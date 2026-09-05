//! One plugin's QuickJS home: a `Runtime` + `Context` pair.
//!
//! Everything JavaScript runs inside [`Context::with`]; values never leave
//! the closure they were born in. Reloading drops the whole host and builds
//! another — state never leaks across, while storage lives outside and
//! survives.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use rquickjs::{Context, Ctx, FromJs, Function, Module, Object, Runtime, Value};

use crate::PluginError;
use crate::storage::Storage;

/// How long one apply may run before it is killed.
///
/// S3 stopped a runaway 0.2 ms past this. Generous on purpose: this kills
/// hangs, while ordinary slowness is handled per frame elsewhere.
pub const INTERRUPT_BUDGET: std::time::Duration = std::time::Duration::from_millis(100);

/// Bytes are cheap beside what a hung frame costs.
const MEMORY_LIMIT_BYTES: usize = 32 * 1024 * 1024;
const MAX_STACK_BYTES: usize = 512 * 1024;

/// Plugin code, either form `EXT-031` asks for.
#[derive(Debug, Clone)]
pub enum PluginSource {
    /// Classic script, evaluated directly.
    Js(Vec<u8>),
    /// Precompiled bytecode, evaluated as a module.
    Bytecode(Vec<u8>),
}

impl PluginSource {
    pub fn from_extension(code: Vec<u8>, extension: &str) -> Result<Self, String> {
        match extension {
            "js" => Ok(PluginSource::Js(code)),
            "qjsc" => Ok(PluginSource::Bytecode(code)),
            other => Err(format!("unsupported plugin entry: .{other}")),
        }
    }
}

type Shared<T> = Rc<RefCell<T>>;

/// One plugin's isolated world. Stays on the thread that built it.
pub struct PluginHost {
    id: String,
    context: Context,
    deadline: Shared<Option<Instant>>,
    storage: Shared<Storage>,
    failures: Shared<FailureLog>,
    // Kept alive: bytecode modules borrow these bytes.
    bytecode: Option<Vec<u8>>,
}

impl PluginHost {
    /// Builds the world, injects the granted capabilities, and evaluates
    /// the entry. Undeclared APIs are not injected at all: calling one is
    /// a `TypeError` in JS, never a host-side refusal.
    pub fn load(
        id: &str,
        granted: &HashSet<String>,
        source: PluginSource,
        storage: Storage,
    ) -> Result<Self, PluginError> {
        let fail = |reason: String| PluginError::LoadFailed {
            id: id.to_owned(),
            reason,
        };
        let runtime = Runtime::new().map_err(|e| fail(e.to_string()))?;
        runtime.set_memory_limit(MEMORY_LIMIT_BYTES);
        runtime.set_max_stack_size(MAX_STACK_BYTES);
        let deadline: Shared<Option<Instant>> = Rc::new(RefCell::new(None));
        {
            let stop_at = Rc::clone(&deadline);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                stop_at.borrow().is_some_and(|t| Instant::now() > t)
            })));
        }
        let context = Context::full(&runtime).map_err(|e| fail(e.to_string()))?;
        let mut host = PluginHost {
            id: id.to_owned(),
            context,
            deadline,
            storage: Rc::new(RefCell::new(storage)),
            failures: Rc::new(RefCell::new(FailureLog::default())),
            bytecode: None,
        };
        host.inject(granted).map_err(|e| fail(e.to_string()))?;
        match source {
            PluginSource::Js(code) => host.eval_source(&code).map_err(&fail)?,
            PluginSource::Bytecode(bytes) => host.eval_bytecode(bytes).map_err(fail)?,
        }
        Ok(host)
    }

    /// The plugin's id, for logs and storage namespaces.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Moves the kill-switch. `None` disarms it.
    pub fn set_deadline(&self, at: Option<Instant>) {
        *self.deadline.borrow_mut() = at;
    }

    /// Whether failures tripped the disable threshold, and how many stand.
    pub fn failure_tripped(&self) -> Option<usize> {
        let log = self.failures.borrow();
        log.tripped().then(|| log.count())
    }

    /// Records a failure the JS side swallowed or the boundary rejected.
    pub fn record_failure(&self) {
        self.failures.borrow_mut().record();
    }

    /// Runs one subtree through the plugin: convert in, call, convert out.
    ///
    /// Conversion failures count like patch failures: the caller keeps the
    /// pre-plugin tree either way.
    /// Runs one subtree through the plugin: convert in, call, convert out.
    ///
    /// Conversion failures count like patch failures: the caller keeps the
    /// pre-plugin tree either way. The entry is `__gumicord_apply(node,
    /// ctx)`, installed by the SDK; anything it throws surfaces as an
    /// error.
    pub fn apply_tree(
        &self,
        tree: &gumicord_uitree::UiNode,
        ctx: &crate::convert::PatchContext,
    ) -> Result<gumicord_uitree::UiNode, PluginError> {
        self.context.with(|ctx_js| {
            let (node, js_ctx) = crate::convert::apply_args(ctx_js.clone(), tree, ctx)
                .map_err(|e| with_id(&self.id, e))?;
            self.set_deadline(Some(Instant::now() + INTERRUPT_BUDGET));
            let result: Result<Object, PluginError> = (|| {
                let entry: Function = ctx_js.globals().get("__gumicord_apply").map_err(|_| {
                    PluginError::NoPatchEntry {
                        id: self.id.clone(),
                    }
                })?;
                entry
                    .call::<_, Object>((node, js_ctx))
                    .map_err(|e| PluginError::PatchFailed {
                        id: self.id.clone(),
                        node: String::new(),
                        reason: js_error_text(&ctx_js, e),
                    })
            })();
            self.set_deadline(None);
            let out = result.map_err(|e| with_id(&self.id, e))?;
            crate::convert::apply_result(&ctx_js, tree, out.as_ref().clone())
                .map_err(|e| with_id(&self.id, e))
        })
    }

    /// How many patches the plugin registered, best effort.
    pub fn patch_count(&self) -> usize {
        self.context.with(|ctx| {
            ctx.globals()
                .get::<_, Function>("__gumicord_patch_count")
                .and_then(|f| f.call::<_, usize>(()))
                .unwrap_or(0)
        })
    }

    /// Reads the settings page in a throwaway world: same code, fresh
    /// globals, storage writes refused. Nothing here may disturb the patch
    /// context, which keeps running beside it.
    pub fn settings_tree(
        id: &str,
        granted: &HashSet<String>,
        source: PluginSource,
        storage: Storage,
    ) -> Result<Option<gumicord_uitree::UiNode>, PluginError> {
        let fail = |reason: String| PluginError::LoadFailed {
            id: id.to_owned(),
            reason,
        };
        let runtime = Runtime::new().map_err(|e| fail(e.to_string()))?;
        runtime.set_memory_limit(MEMORY_LIMIT_BYTES);
        runtime.set_max_stack_size(MAX_STACK_BYTES);
        let deadline: Shared<Option<Instant>> = std::rc::Rc::new(RefCell::new(None));
        {
            let stop_at = Rc::clone(&deadline);
            runtime.set_interrupt_handler(Some(Box::new(move || {
                stop_at.borrow().is_some_and(|t| Instant::now() > t)
            })));
        }
        let context = Context::full(&runtime).map_err(|e| fail(e.to_string()))?;
        let mut host = PluginHost {
            id: id.to_owned(),
            context,
            deadline,
            storage: Rc::new(RefCell::new(storage)),
            failures: Rc::new(RefCell::new(FailureLog::default())),
            bytecode: None,
        };
        host.inject_with(granted, false)
            .map_err(|e| fail(e.to_string()))?;
        match source {
            PluginSource::Js(code) => host.eval_source(&code).map_err(&fail)?,
            PluginSource::Bytecode(bytes) => host.eval_bytecode(bytes).map_err(fail)?,
        }
        host.context.with(|ctx| {
            let entry: Option<Function> = ctx.globals().get("__gumicord_settings").ok();
            let Some(entry) = entry else {
                return Ok(None);
            };
            host.set_deadline(Some(Instant::now() + INTERRUPT_BUDGET));
            let out: Value = entry
                .call::<_, Object>(())
                .map(Value::from_object)
                .map_err(|e| fail(js_error_text(&ctx, e)))?;
            host.set_deadline(None);
            let empty = std::collections::HashMap::new();
            crate::convert::js_to_node(&ctx, &out, &empty)
                .map(Some)
                .map_err(|e| with_id(&host.id, e))
        })
    }

    fn inject(&self, granted: &HashSet<String>) -> rquickjs::Result<()> {
        self.inject_with(granted, true)
    }

    /// Injects capabilities, optionally without storage writes. Settings
    /// pages display: they may read, never save, until the event channel
    /// for settings arrives.
    fn inject_with(&self, granted: &HashSet<String>, writable: bool) -> rquickjs::Result<()> {
        self.context.with(|ctx| {
            let host = Object::new(ctx.clone())?;

            if granted.contains("log") {
                let id = self.id.clone();
                host.set(
                    "log",
                    Function::new(ctx.clone(), move |level: String, msg: String| {
                        match level.as_str() {
                            "warn" => tracing::warn!(plugin = %id, "{msg}"),
                            "error" => tracing::error!(plugin = %id, "{msg}"),
                            _ => tracing::info!(plugin = %id, "{msg}"),
                        }
                    })?,
                )?;
            }
            if granted.contains("storage") {
            let storage = Rc::clone(&self.storage);
            host.set(
                "storage_get",
                    Function::new(ctx.clone(), move |key: String| {
                        storage.borrow().get(&key).map(str::to_owned)
                    })?,
                )?;
                let storage = Rc::clone(&self.storage);
                let plugin = self.id.clone();
                host.set(
                    "storage_set",
                    Function::new(ctx.clone(), move |key: String, value: String| {
                        if !writable {
                            tracing::warn!(
                                plugin = %plugin,
                                "settings pages cannot save yet; ignoring the write"
                            );
                            return;
                        }
                        if let Err(e) = storage.borrow_mut().set(&key, &value) {
                            tracing::warn!(plugin = %plugin, %e, "storage_set lost a write");
                        }
                    })?,
                )?;
                let storage = Rc::clone(&self.storage);
                let plugin = self.id.clone();
                host.set(
                    "storage_remove",
                    Function::new(ctx.clone(), move |key: String| {
                        if !writable {
                            tracing::warn!(
                                plugin = %plugin,
                                "settings pages cannot save yet; ignoring the write"
                            );
                            return;
                        }
                        if let Err(e) = storage.borrow_mut().remove(&key) {
                            tracing::warn!(plugin = %plugin, %e, "storage_remove lost a write");
                        }
                    })?,
                )?;
            }

            // Plumbing, not a capability: node presence reveals nothing
            // about the user, and failure accounting needs a way back in.
            host.set(
                "node_exists",
                Function::new(ctx.clone(), move |id: String| {
                    id.parse::<gumicord_uitree::NodeId>().is_ok()
                })?,
            )?;
            let failures = Rc::clone(&self.failures);
            let plugin = self.id.clone();
            host.set(
                "patch_failed",
                Function::new(ctx.clone(), move |node: String| {
                    failures.borrow_mut().record();
                    tracing::warn!(plugin = %plugin, %node, "a patch threw and its node was restored");
                })?,
            )?;

            ctx.globals().set("__gumicord_host", host)?;
            Ok(())
        })
    }

    fn eval_source(&self, code: &[u8]) -> Result<(), String> {
        self.context.with(|ctx| {
            ctx.eval::<rquickjs::Value, _>(code.to_vec())
                .map(|_| ())
                .map_err(|e| js_error_text(&ctx, e))
        })
    }

    fn eval_bytecode(&mut self, bytes: Vec<u8>) -> Result<(), String> {
        // `Module::load` borrows these bytes for the module's lifetime.
        self.bytecode = Some(bytes);
        self.context.with(|ctx| {
            let stored = self.bytecode.as_ref().expect("just stored");
            // SAFETY: invalid bytes error out instead.
            let module =
                unsafe { Module::load(ctx.clone(), stored) }.map_err(|e| js_error_text(&ctx, e))?;
            let (_, promise) = module.eval().map_err(|e| js_error_text(&ctx, e))?;
            for _ in 0..10_000 {
                if !ctx.execute_pending_job() {
                    break;
                }
            }
            promise.finish::<()>().map_err(|e| js_error_text(&ctx, e))?;
            Ok(())
        })
    }
}

/// Consecutive failure times; old ones fall out of the window.
#[derive(Debug, Default)]
pub struct FailureLog {
    times: std::collections::VecDeque<Instant>,
}

/// Failures inside this window disable the plugin.
pub const FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// Failures inside the window that disable the plugin.
pub const FAILURE_THRESHOLD: usize = 100;

impl FailureLog {
    pub fn record(&mut self) {
        let now = Instant::now();
        self.times
            .retain(|t| now.duration_since(*t) < FAILURE_WINDOW);
        self.times.push_back(now);
    }

    pub fn tripped(&self) -> bool {
        let now = Instant::now();
        self.times
            .iter()
            .filter(|t| now.duration_since(**t) < FAILURE_WINDOW)
            .count()
            >= FAILURE_THRESHOLD
    }

    pub fn count(&self) -> usize {
        self.times.len()
    }
}

/// Attributes conversion errors, which carry no plugin id yet.
fn with_id(id: &str, error: PluginError) -> PluginError {
    match error {
        PluginError::BadPatchOutput { reason, .. } => PluginError::BadPatchOutput {
            id: id.to_owned(),
            reason,
        },
        other => other,
    }
}

/// The JS exception's own message when there is one.
fn js_error_text(ctx: &Ctx, e: rquickjs::Error) -> String {
    if matches!(e, rquickjs::Error::Exception) {
        let value = ctx.catch();
        if let Ok(text) = String::from_js(ctx, value) {
            return text;
        }
    }
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn host(source: &str, granted: &[&str]) -> PluginHost {
        let dir = std::env::temp_dir().join(format!(
            "gumicord-plugin-host-{}-{}",
            source.len(),
            granted.len()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let granted = granted.iter().map(ToString::to_string).collect();
        PluginHost::load(
            "com.example.t",
            &granted,
            PluginSource::Js(source.as_bytes().to_vec()),
            Storage::load(&dir).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn source_evaluates_and_registers() {
        let h = host(
            r#"globalThis.__gumicord_apply = (n) => n;
               globalThis.__gumicord_patch_count = () => 3;"#,
            &[],
        );
        assert_eq!(h.patch_count(), 3);
    }

    #[test]
    fn syntax_errors_fail_the_load() {
        let dir = std::env::temp_dir().join("gumicord-plugin-host-syntax");
        let _ = std::fs::remove_dir_all(&dir);
        let err = match PluginHost::load(
            "com.example.t",
            &HashSet::new(),
            PluginSource::Js(b"function broken( {".to_vec()),
            Storage::load(&dir).unwrap(),
        ) {
            Ok(_) => panic!("broken source loaded"),
            Err(e) => e,
        };
        assert!(matches!(err, PluginError::LoadFailed { .. }), "{err}");
    }

    #[test]
    fn undeclared_apis_do_not_exist() {
        let h = host(r#"globalThis.__gumicord_apply = (n) => n;"#, &[]);
        let has: bool = h
            .context
            .with(|ctx| {
                ctx.eval::<bool, _>(
                    r#"typeof __gumicord_host.log === "function" ||
                   typeof __gumicord_host.storage_get === "function""#,
                )
            })
            .unwrap();
        assert!(!has, "an undeclared API is reachable");
        let plumbing: bool = h
            .context
            .with(|ctx| ctx.eval::<bool, _>("typeof __gumicord_host.node_exists === 'function'"))
            .unwrap();
        assert!(plumbing);
    }

    #[test]
    fn declared_storage_round_trips_through_js() {
        let h = host(
            r#"globalThis.__gumicord_apply = (n) => n;
               __gumicord_host.storage_set("k", "v");"#,
            &["storage"],
        );
        let back: String = h
            .context
            .with(|ctx| ctx.eval(r#"__gumicord_host.storage_get("k")"#))
            .unwrap();
        assert_eq!(back, "v");
        let missing: Option<String> = h
            .context
            .with(|ctx| ctx.eval(r#"__gumicord_host.storage_get("nope")"#))
            .unwrap();
        assert_eq!(missing, None);
    }

    #[test]
    fn an_infinite_loop_is_stopped() {
        let h = host(r#"globalThis.__gumicord_apply = (n) => n;"#, &[]);
        h.set_deadline(Some(Instant::now()));
        let started = Instant::now();
        let err = h
            .context
            .with(|ctx| ctx.eval::<(), _>("while (true) {}"))
            .unwrap_err();
        let _ = err;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "ran away: {:?}",
            started.elapsed()
        );
        h.set_deadline(None);
    }

    #[test]
    fn failures_trip_only_when_chronic() {
        let mut log = FailureLog::default();
        assert!(!log.tripped());
        for _ in 0..FAILURE_THRESHOLD {
            log.record();
        }
        assert!(log.tripped());
    }
}
