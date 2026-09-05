//! Loading, chaining, and disabling plugins.
//!
//! `PluginSet` owns the ordered hosts and runs the P6 chain synchronously.
//! `PluginManager` pins one on a worker thread and trades latest-only trees
//! with the main thread, so a runaway plugin lags effects instead of frames:
//! whatever is newest when a frame starts is what gets drawn.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gumicord_uitree::UiNode;

use crate::convert::PatchContext;
use crate::host::{PluginHost, PluginSource};
use crate::manifest::Manifest;
use crate::storage::Storage;

/// A full chain slower than this still applies, but warns.
///
/// Dropping slow results instead would starve a merely slow plugin forever;
/// the interrupt deadline already bounds true hangs.
const FRAME_BUDGET: Duration = Duration::from_millis(8);
/// Slow-chain warnings are noise at frame rate; one voice per minute.
const WARN_THROTTLE: Duration = Duration::from_secs(60);

/// What the worker owes the main thread.
#[derive(Debug)]
pub enum ManagerEvent {
    /// The latest completed full-chain output. Superseded ones never send.
    Patched(Box<UiNode>),
    /// A plugin failed chronically and was unloaded.
    Disabled {
        id: String,
        failures: usize,
        reason: String,
    },
    /// A first-seen manifest asks for capabilities.
    NeedsApproval {
        id: String,
        name: String,
        capabilities: Vec<String>,
    },
    /// Anything else worth a log line on the main side.
    Warned { message: String },
}

/// One row of the settings screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginState {
    pub id: String,
    pub name: String,
    pub version: String,
    pub state: PluginStateKind,
    pub capabilities: Vec<String>,
    /// Whether the manifest declares a settings page.
    pub has_settings: bool,
}

/// Where a plugin stands. Refusals and switches persist; anything else is
/// recomputed from what is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginStateKind {
    /// Running and patching.
    Loaded,
    /// Turned off by the user; grants kept.
    Disabled,
    /// Refused; never loads until approved again.
    Denied,
    /// Unseen capabilities; the approval dialog owns it.
    NeedsApproval,
    /// Granted but absent: loading failed, and the warning went out already.
    LoadFailed,
}

enum Command {
    Submit {
        tree: Box<UiNode>,
        ctx: PatchContext,
    },
    Approve {
        id: String,
        granted: Vec<String>,
    },
    Deny {
        id: String,
    },
    Reload {
        id: String,
    },
    Unload {
        id: String,
    },
    Disable {
        id: String,
    },
    Enable {
        id: String,
    },
    Reapprove {
        id: String,
    },
    ListStates {
        reply: mpsc::Sender<Vec<PluginState>>,
    },
    SettingsTree {
        id: String,
        reply: mpsc::Sender<Option<UiNode>>,
    },
    Shutdown,
}

struct LoadedPlugin {
    manifest: Manifest,
    host: PluginHost,
}

struct PluginSet {
    plugins_dir: PathBuf,
    plugins: Vec<LoadedPlugin>,
    known: HashMap<String, (PathBuf, Manifest)>,
    grants: HashMap<String, Vec<String>>,
    disabled: HashSet<String>,
}

impl PluginSet {
    fn open(plugins_dir: &Path) -> (Self, Vec<ManagerEvent>) {
        let mut events = Vec::new();
        if let Err(e) = std::fs::create_dir_all(plugins_dir) {
            events.push(ManagerEvent::Warned {
                message: format!(
                    "plugins directory {} unusable: {e}; running without plugins",
                    plugins_dir.display()
                ),
            });
        }
        let (grants, disabled) = load_grants(plugins_dir);
        (
            PluginSet {
                plugins_dir: plugins_dir.to_owned(),
                plugins: Vec::new(),
                known: HashMap::new(),
                grants,
                disabled,
            },
            events,
        )
    }

    /// Finds plugin directories and loads what needs no asking.
    fn scan(&mut self) -> Vec<ManagerEvent> {
        let mut events = Vec::new();
        let mut dirs = Vec::new();
        match std::fs::read_dir(&self.plugins_dir) {
            Err(e) => {
                events.push(ManagerEvent::Warned {
                    message: format!("cannot list {}: {e}", self.plugins_dir.display()),
                });
                return events;
            }
            Ok(entries) => {
                for entry in entries.flatten() {
                    let dir = entry.path();
                    if dir.is_dir() && dir.join("manifest.json").is_file() {
                        dirs.push(dir);
                    }
                }
            }
        }
        dirs.sort();
        for dir in dirs {
            let manifest = match Manifest::load(&dir) {
                Err(e) => {
                    events.push(ManagerEvent::Warned {
                        message: format!("skipping {}: {e}", dir.display()),
                    });
                    continue;
                }
                Ok(m) => m,
            };
            let id = manifest.id.clone();
            self.known
                .insert(id.clone(), (dir.clone(), manifest.clone()));
            if self.disabled.contains(&id) {
                continue;
            }
            if manifest.capabilities.is_empty() {
                events.extend(self.load_host(&id));
            } else {
                match self.grants.get(&id) {
                    None => events.push(ManagerEvent::NeedsApproval {
                        id,
                        name: manifest.name.clone(),
                        capabilities: manifest.capabilities.clone(),
                    }),
                    Some(granted) if granted.is_empty() => {}
                    Some(_) => events.extend(self.load_host(&id)),
                }
            }
        }
        events
    }

    fn approve(&mut self, id: &str, granted: &[String]) -> Vec<ManagerEvent> {
        let Some((_, manifest)) = self.known.get(id).cloned() else {
            return vec![warn(format!("approve for unknown plugin {id}"))];
        };
        let granted: Vec<String> = granted
            .iter()
            .filter(|c| manifest.capabilities.contains(c))
            .cloned()
            .collect();
        self.grants.insert(id.to_owned(), granted);
        let mut events = self.save_grants();
        events.extend(self.load_host(id));
        events
    }

    fn deny(&mut self, id: &str) -> Vec<ManagerEvent> {
        if !self.known.contains_key(id) {
            return vec![warn(format!("deny for unknown plugin {id}"))];
        }
        self.grants.insert(id.to_owned(), Vec::new());
        self.unload(id);
        self.save_grants()
    }

    /// Turns a plugin off and remembers it. Grants stay: enabling again
    /// resumes where it left off, without asking twice.
    fn disable(&mut self, id: &str) -> Vec<ManagerEvent> {
        if !self.known.contains_key(id) {
            return vec![warn(format!("disable for unknown plugin {id}"))];
        }
        self.disabled.insert(id.to_owned());
        self.unload(id);
        self.save_grants()
    }

    /// Turns a plugin back on. A denied plugin stays denied: refusing and
    /// then enabling must not silently grant. Unasked capabilities ask.
    fn enable(&mut self, id: &str) -> Vec<ManagerEvent> {
        let Some((_, manifest)) = self.known.get(id).cloned() else {
            return vec![warn(format!("enable for unknown plugin {id}"))];
        };
        if self.grants.get(id).is_some_and(Vec::is_empty) {
            return vec![warn(format!("plugin {id} is denied; approve it first"))];
        }
        self.disabled.remove(id);
        let mut events = self.save_grants();
        if manifest.capabilities.is_empty() || self.grants.contains_key(id) {
            events.extend(self.load_host(id));
        } else {
            events.push(ManagerEvent::NeedsApproval {
                id: id.to_owned(),
                name: manifest.name.clone(),
                capabilities: manifest.capabilities.clone(),
            });
        }
        events
    }

    /// Asks again after a denial: forgets the refusal and goes through the
    /// approval dialog like a first sighting.
    fn reapprove(&mut self, id: &str) -> Vec<ManagerEvent> {
        let Some((_, manifest)) = self.known.get(id).cloned() else {
            return vec![warn(format!("approve for unknown plugin {id}"))];
        };
        self.grants.remove(id);
        self.disabled.remove(id);
        let mut events = self.save_grants();
        if manifest.capabilities.is_empty() {
            events.extend(self.load_host(id));
        } else {
            events.push(ManagerEvent::NeedsApproval {
                id: id.to_owned(),
                name: manifest.name.clone(),
                capabilities: manifest.capabilities.clone(),
            });
        }
        events
    }

    fn reload(&mut self, id: &str) -> Vec<ManagerEvent> {
        let Some((dir, _)) = self.known.get(id).cloned() else {
            return vec![warn(format!("reload for unknown plugin {id}"))];
        };
        if self.grants.get(id).is_some_and(Vec::is_empty) {
            return vec![warn(format!("plugin {id} is denied; not reloading"))];
        }
        if self.disabled.contains(id) {
            return vec![warn(format!("plugin {id} is disabled; not reloading"))];
        }
        match Manifest::load(&dir) {
            Err(e) => vec![warn(format!("reload of {id} failed: {e}"))],
            Ok(manifest) => {
                self.known.insert(id.to_owned(), (dir, manifest));
                self.unload(id);
                self.load_host(id)
            }
        }
    }

    fn unload(&mut self, id: &str) {
        self.plugins.retain(|p| p.manifest.id != id);
    }

    /// Reads one plugin's settings page, if it declares one. Display-only:
    /// controls sit inert until the settings event channel arrives.
    fn settings_tree(&self, id: &str) -> Option<UiNode> {
        let (dir, manifest) = self.known.get(id)?.clone();
        let settings = manifest.settings.as_ref()?;
        let bytes = std::fs::read(dir.join(settings)).ok()?;
        let source = match settings.rsplit_once('.') {
            Some((_, "js")) => PluginSource::Js(bytes),
            Some((_, "qjsc")) => PluginSource::Bytecode(bytes),
            _ => return None,
        };
        let storage = Storage::load(&dir).ok()?;
        let granted: HashSet<String> = self
            .grants
            .get(id)
            .map(|g| {
                g.iter()
                    .filter(|c| manifest.capabilities.contains(c))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        match PluginHost::settings_tree(id, &granted, source, storage) {
            Ok(tree) => tree,
            Err(e) => {
                tracing::warn!(plugin = %id, %e, "settings page failed");
                None
            }
        }
    }
    /// One row per known plugin for the settings screen.
    fn states(&self) -> Vec<PluginState> {
        let mut ids: Vec<&String> = self.known.keys().collect();
        ids.sort();
        ids.into_iter()
            .map(|id| {
                let (_, manifest) = &self.known[id];
                let state = if self.disabled.contains(id) {
                    PluginStateKind::Disabled
                } else if self.grants.get(id).is_some_and(Vec::is_empty) {
                    PluginStateKind::Denied
                } else if self.plugins.iter().any(|p| &p.manifest.id == id) {
                    PluginStateKind::Loaded
                } else if !manifest.capabilities.is_empty() && !self.grants.contains_key(id) {
                    PluginStateKind::NeedsApproval
                } else {
                    // Granted but absent: loading failed, and the warning
                    // went out with the event.
                    PluginStateKind::LoadFailed
                };
                PluginState {
                    id: id.clone(),
                    name: manifest.name.clone(),
                    version: manifest.version.clone(),
                    state,
                    capabilities: manifest.capabilities.clone(),
                    has_settings: manifest.settings.is_some(),
                }
            })
            .collect()
    }

    /// Builds a host when the grants cover it. Already-loaded hosts stay.
    fn load_host(&mut self, id: &str) -> Vec<ManagerEvent> {
        if self.plugins.iter().any(|p| p.manifest.id == id) {
            return Vec::new();
        }
        let Some((dir, manifest)) = self.known.get(id).cloned() else {
            return vec![warn(format!("load for unknown plugin {id}"))];
        };
        let granted: HashSet<String> = self
            .grants
            .get(id)
            .map(|g| {
                g.iter()
                    .filter(|c| manifest.capabilities.contains(c))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let storage = match Storage::load(&dir) {
            Err(e) => {
                return vec![warn(format!("plugin {id} storage unreadable: {e}"))];
            }
            Ok(s) => s,
        };
        let bytes = match std::fs::read(manifest.entry_path(&dir)) {
            Err(e) => {
                return vec![warn(format!("plugin {id} entry unreadable: {e}"))];
            }
            Ok(b) => b,
        };
        let extension = manifest
            .entry
            .rsplit('.')
            .next()
            .unwrap_or_default()
            .to_owned();
        let source = match PluginSource::from_extension(bytes, &extension) {
            Err(reason) => {
                return vec![warn(format!("plugin {id} entry rejected: {reason}"))];
            }
            Ok(s) => s,
        };
        match PluginHost::load(id, &granted, source, storage) {
            Err(e) => vec![warn(format!("plugin {id} failed to load: {e}"))],
            Ok(host) => {
                tracing::info!(
                    plugin = %id,
                    patches = host.patch_count(),
                    "plugin loaded"
                );
                self.plugins.push(LoadedPlugin { manifest, host });
                Vec::new()
            }
        }
    }

    /// Runs the P6 chain: each plugin sees the previous output, in load
    /// order. A plugin that fails keeps the tree as it found it; one that
    /// fails chronically is unloaded with an event.
    fn apply_all(&mut self, mut tree: UiNode, ctx: &PatchContext) -> (UiNode, Vec<ManagerEvent>) {
        let mut events = Vec::new();
        for plugin in &self.plugins {
            match plugin.host.apply_tree(&tree, ctx) {
                Ok(next) => tree = next,
                Err(e) => {
                    plugin.host.record_failure();
                    events.push(ManagerEvent::Warned {
                        message: format!("plugin {} failed a frame: {e}", plugin.manifest.id),
                    });
                }
            }
        }
        let mut disabled = Vec::new();
        self.plugins.retain(|p| match p.host.failure_tripped() {
            Some(failures) => {
                disabled.push((p.manifest.id.clone(), failures));
                false
            }
            None => true,
        });
        for (id, failures) in disabled {
            events.push(ManagerEvent::Disabled {
                id: id.clone(),
                failures,
                reason: "failed repeatedly; unloaded".to_owned(),
            });
            tracing::error!(plugin = %id, failures, "plugin disabled after chronic failures");
        }
        (tree, events)
    }

    fn save_grants(&self) -> Vec<ManagerEvent> {
        let path = self.plugins_dir.join("grants.json");
        let stored = StoredGrants {
            grants: self.grants.clone(),
            disabled: {
                let mut ids: Vec<String> = self.disabled.iter().cloned().collect();
                ids.sort();
                ids
            },
        };
        match serde_json::to_string_pretty(&stored)
            .map_err(|e| e.to_string())
            .and_then(|raw| write_atomically(&path, raw.as_bytes()).map_err(|e| e.to_string()))
        {
            Ok(()) => Vec::new(),
            Err(reason) => vec![warn(format!(
                "cannot save {}: {reason}; approvals last until restart",
                path.display()
            ))],
        }
    }
}

fn warn(message: String) -> ManagerEvent {
    ManagerEvent::Warned { message }
}

/// Writes beside the target and renames over it, so a concurrent reader
/// never sees a half-written file.
fn write_atomically(path: &std::path::Path, raw: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, raw)?;
    std::fs::rename(&tmp, path)
}

fn load_grants(plugins_dir: &Path) -> (HashMap<String, Vec<String>>, HashSet<String>) {
    let empty = || (HashMap::new(), HashSet::new());
    let Ok(raw) = std::fs::read_to_string(plugins_dir.join("grants.json")) else {
        return empty();
    };
    // Flat first: the shaped read ignores unknown fields, so it would
    // swallow an old file into emptiness. A plugin id can never be
    // "grants" or "disabled" (no dots), so the shapes never collide.
    if let Ok(flat) = serde_json::from_str::<HashMap<String, Vec<String>>>(&raw) {
        return (flat, HashSet::new());
    }
    if let Ok(shaped) = serde_json::from_str::<StoredGrants>(&raw) {
        return (shaped.grants, shaped.disabled.into_iter().collect());
    }
    empty()
}

/// `grants.json` on disk. `disabled` joined later; old flat files still read.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct StoredGrants {
    #[serde(default)]
    grants: HashMap<String, Vec<String>>,
    #[serde(default)]
    disabled: Vec<String>,
}

/// The main-thread handle. Everything cross-thread is latest-only: sending
/// never blocks, and draining keeps only the newest output.
pub struct PluginManager {
    cmd: Option<mpsc::Sender<Command>>,
    events: mpsc::Receiver<ManagerEvent>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PluginManager {
    /// Starts the worker and loads what needs no asking.
    pub fn start(plugins_dir: PathBuf) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("gumicord-plugins".to_owned())
            .spawn(move || run_worker(plugins_dir, cmd_rx, ev_tx))
            .expect("plugin worker thread spawns");
        PluginManager {
            cmd: Some(cmd_tx),
            events: ev_rx,
            worker: Some(worker),
        }
    }

    /// Safe mode: every call is a no-op, nothing ever loads.
    pub fn disabled() -> Self {
        let (_, ev_rx) = mpsc::channel();
        PluginManager {
            cmd: None,
            events: ev_rx,
            worker: None,
        }
    }

    /// Hands the latest tree over; older unhandled ones are dropped.
    pub fn submit(&self, tree: &UiNode, ctx: &PatchContext) {
        let Some(cmd) = &self.cmd else { return };
        let _ = cmd.send(Command::Submit {
            tree: Box::new(tree.clone()),
            ctx: ctx.clone(),
        });
    }

    /// Takes every pending event without blocking.
    pub fn drain(&self) -> Vec<ManagerEvent> {
        self.events.try_iter().collect()
    }

    pub fn approve(&self, id: &str, granted: &[String]) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Approve {
                id: id.to_owned(),
                granted: granted.to_vec(),
            });
        }
    }

    pub fn deny(&self, id: &str) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Deny { id: id.to_owned() });
        }
    }

    pub fn reload(&self, id: &str) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Reload { id: id.to_owned() });
        }
    }

    pub fn unload(&self, id: &str) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Unload { id: id.to_owned() });
        }
    }

    pub fn disable(&self, id: &str) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Disable { id: id.to_owned() });
        }
    }

    pub fn enable(&self, id: &str) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Enable { id: id.to_owned() });
        }
    }

    pub fn reapprove(&self, id: &str) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Reapprove { id: id.to_owned() });
        }
    }

    /// One plugin's settings tree, for the settings screen. Blocking, but
    /// settings open rarely; frames never call it.
    pub fn settings_tree(&self, id: &str) -> Option<UiNode> {
        let Some(cmd) = &self.cmd else {
            return None;
        };
        let (tx, rx) = mpsc::channel();
        if cmd
            .send(Command::SettingsTree {
                id: id.to_owned(),
                reply: tx,
            })
            .is_err()
        {
            return None;
        }
        rx.recv().unwrap_or(None)
    }

    /// One row per known plugin, for the settings screen. Blocking, but the
    /// settings screen opens rarely; frames never call it.
    pub fn plugin_states(&self) -> Vec<PluginState> {
        let Some(cmd) = &self.cmd else {
            return Vec::new();
        };
        let (tx, rx) = mpsc::channel();
        if cmd.send(Command::ListStates { reply: tx }).is_err() {
            return Vec::new();
        }
        rx.recv().unwrap_or_default()
    }
}

impl Drop for PluginManager {
    fn drop(&mut self) {
        if let Some(cmd) = &self.cmd {
            let _ = cmd.send(Command::Shutdown);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn run_worker(
    plugins_dir: PathBuf,
    cmd_rx: mpsc::Receiver<Command>,
    ev_tx: mpsc::Sender<ManagerEvent>,
) {
    let send = |e: ManagerEvent| {
        let _ = ev_tx.send(e);
    };
    let (mut set, initial) = PluginSet::open(&plugins_dir);
    for e in initial {
        send(e);
    }
    for e in set.scan() {
        send(e);
    }

    let mut pending: Option<(Box<UiNode>, PatchContext)> = None;
    let mut last_slow_warn = None;
    loop {
        if pending.is_none() {
            match cmd_rx.recv() {
                Err(_) => break,
                Ok(cmd) => {
                    if handle_command(cmd, &mut pending, &mut set, &send) {
                        break;
                    }
                }
            }
        }
        let mut shutdown = false;
        for cmd in cmd_rx.try_iter() {
            if handle_command(cmd, &mut pending, &mut set, &send) {
                shutdown = true;
                break;
            }
        }
        if shutdown {
            break;
        }
        if let Some((tree, ctx)) = pending.take() {
            let started = Instant::now();
            let (out, events) = set.apply_all(*tree, &ctx);
            for e in events {
                send(e);
            }
            if started.elapsed() > FRAME_BUDGET
                && last_slow_warn.is_none_or(|t| t + WARN_THROTTLE < Instant::now())
            {
                last_slow_warn = Some(Instant::now());
                send(ManagerEvent::Warned {
                    message: format!(
                        "plugin chain took {:?}; effects lag a frame",
                        started.elapsed()
                    ),
                });
            }
            send(ManagerEvent::Patched(Box::new(out)));
        }
    }
}

/// Runs one command; true means shut down.
fn handle_command(
    cmd: Command,
    pending: &mut Option<(Box<UiNode>, PatchContext)>,
    set: &mut PluginSet,
    send: &impl Fn(ManagerEvent),
) -> bool {
    match cmd {
        Command::Submit { tree, ctx } => *pending = Some((tree, ctx)),
        Command::Approve { id, granted } => {
            for e in set.approve(&id, &granted) {
                send(e);
            }
        }
        Command::Deny { id } => {
            for e in set.deny(&id) {
                send(e);
            }
        }
        Command::Reload { id } => {
            for e in set.reload(&id) {
                send(e);
            }
        }
        Command::Unload { id } => set.unload(&id),
        Command::Disable { id } => {
            for e in set.disable(&id) {
                send(e);
            }
        }
        Command::Enable { id } => {
            for e in set.enable(&id) {
                send(e);
            }
        }
        Command::Reapprove { id } => {
            for e in set.reapprove(&id) {
                send(e);
            }
        }
        Command::ListStates { reply } => {
            let _ = reply.send(set.states());
        }
        Command::SettingsTree { id, reply } => {
            let _ = reply.send(set.settings_tree(&id));
        }
        Command::Shutdown => return true,
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::FAILURE_THRESHOLD;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-plugin-manager-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn plugin_dir(root: &Path, id: &str, source: &str, capabilities: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{id}","name":"{id}","version":"1.0.0","capabilities":[{capabilities}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.js"), source).unwrap();
        dir
    }

    /// Patches chain in load order, each seeing the previous output.
    #[test]
    fn plugins_chain_in_load_order() {
        let root = dir("chain");
        plugin_dir(
            &root,
            "com.example.a",
            r#"globalThis.__gumicord_apply = (n) => ({...n, children: [...(n.children ?? []), {id: "primitive.text", props: {value: "A"}}]});"#,
            "",
        );
        plugin_dir(
            &root,
            "com.example.b",
            r#"globalThis.__gumicord_apply = (n) => ({...n, children: [...(n.children ?? []), {id: "primitive.text", props: {value: "B"}}]});"#,
            "",
        );
        let (mut set, _) = PluginSet::open(&root);
        let events = set.scan();
        assert!(events.is_empty(), "{events:?}");

        let tree = UiNode::new(gumicord_uitree::NodeId::ChatMessageContent);
        let (out, events) = set.apply_all(tree, &PatchContext::empty());
        assert!(events.is_empty(), "{events:?}");
        let texts: Vec<_> = out
            .children
            .iter()
            .filter_map(|c| c.content.as_text().map(str::to_owned))
            .collect();
        assert_eq!(texts, ["A", "B"]);
    }

    /// A denied plugin never loads; an unseen one asks first.
    #[test]
    fn approval_gates_capability_plugins() {
        let root = dir("approval");
        plugin_dir(
            &root,
            "com.example.free",
            r#"globalThis.__gumicord_apply = (n) => n;"#,
            "",
        );
        plugin_dir(
            &root,
            "com.example.needy",
            r#"globalThis.__gumicord_apply = (n) => n;"#,
            r#""storage""#,
        );
        let (mut set, _) = PluginSet::open(&root);
        let events = set.scan();
        assert_eq!(set.plugins.len(), 1, "only the free one loads");
        let approvals: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ManagerEvent::NeedsApproval { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(approvals, ["com.example.needy"]);

        set.deny("com.example.needy");
        assert!(
            set.plugins
                .iter()
                .all(|p| p.manifest.id != "com.example.needy")
        );

        // Denial persists as an empty grant list.
        let (denied, _) = PluginSet::open(&root);
        assert_eq!(denied.grants.get("com.example.needy"), Some(&Vec::new()));

        set.approve("com.example.needy", &["storage".to_owned()]);
        assert!(
            set.plugins
                .iter()
                .any(|p| p.manifest.id == "com.example.needy")
        );

        let (again, _) = PluginSet::open(&root);
        assert_eq!(
            again.grants.get("com.example.needy"),
            Some(&vec!["storage".to_owned()])
        );
    }

    /// An SDK-style spread patch keeps bodies: what goes out must come
    /// back, or every wrap silently erases text.
    #[test]
    fn spread_patches_preserve_bodies() {
        use gumicord_uitree::{NodeId, UiNode};
        let root = dir("spread");
        plugin_dir(
            &root,
            "com.example.spread",
            r#"globalThis.__gumicord_apply = (n) => {
                if (n.id !== "chat.message.content") return n;
                return { ...n, children: [...(n.children ?? []), { id: "primitive.badge", props: { text: "hi" } }] };
            };"#,
            "",
        );
        let (mut set, _) = PluginSet::open(&root);
        assert!(set.scan().is_empty());
        let tree = UiNode::text(NodeId::ChatMessageContent, "hello");
        let (out, events) = set.apply_all(tree, &PatchContext::empty());
        assert!(events.is_empty(), "{events:?}");
        assert_eq!(out.content.as_text(), Some("hello"), "body lost");
        assert_eq!(out.children.len(), 1);
        assert_eq!(out.children[0].content.as_text(), Some("hi"));
    }

    /// The worker thread delivers patches back to the main thread.
    #[test]
    fn worker_delivers_patches() {
        use gumicord_uitree::{NodeId, UiNode};
        let root = dir("worker");
        plugin_dir(
            &root,
            "com.example.w",
            r#"globalThis.__gumicord_apply = (n) => {
                const walk = (x) => {
                    const kids = (x.children ?? []).map(walk);
                    const cur = kids.length ? { ...x, children: kids } : x;
                    if (cur.id !== "chat.message.content") return cur;
                    return { ...cur, children: [...(cur.children ?? []), { id: "primitive.badge", props: { text: "hi" } }] };
                };
                return walk(n);
            };"#,
            "",
        );
        let manager = PluginManager::start(root);
        let tree = UiNode::text(NodeId::ChatMessageContent, "hello");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            manager.submit(&tree, &PatchContext::empty());
            let mut badges = 0;
            let mut warnings = Vec::new();
            for e in manager.drain() {
                match e {
                    ManagerEvent::Patched(out) => {
                        out.walk(&mut |n, _| {
                            if n.id == NodeId::PrimitiveBadge {
                                badges += 1;
                            }
                        });
                    }
                    other => warnings.push(format!("{other:?}")),
                }
            }
            if badges > 0 {
                return;
            }
            if std::time::Instant::now() > deadline {
                panic!("no patched tree in 10s; {warnings:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A plugin failing every frame is unloaded with an event.
    #[test]
    fn chronic_failure_disables() {
        let root = dir("disable");
        plugin_dir(
            &root,
            "com.example.bad",
            r#"globalThis.__gumicord_apply = (n) => { throw new Error("boom"); };"#,
            "",
        );
        let (mut set, _) = PluginSet::open(&root);
        assert!(set.scan().is_empty());
        let tree = UiNode::new(gumicord_uitree::NodeId::ChatMessageContent);
        let mut disabled = false;
        for _ in 0..FAILURE_THRESHOLD + 1 {
            let (kept, events) = set.apply_all(tree.clone(), &PatchContext::empty());
            assert_eq!(kept.id, tree.id, "output falls back to the input");
            disabled |= events
                .iter()
                .any(|e| matches!(e, ManagerEvent::Disabled { .. }));
        }
        assert!(disabled, "chronic failure unloads the plugin");
        assert!(set.plugins.is_empty());
    }

    fn state_of(set: &PluginSet, id: &str) -> PluginStateKind {
        set.states()
            .into_iter()
            .find(|s| s.id == id)
            .expect("known plugin missing")
            .state
    }

    /// Disabling remembers, enabling resumes grants, and denying needs a new
    /// approval. The settings screen reads the same states it acts on.
    #[test]
    fn disabling_enabling_and_reapproving() {
        let root = dir("states");
        plugin_dir(
            &root,
            "com.example.a",
            "globalThis.__gumicord_apply = (n) => n;",
            r#""log""#,
        );
        let (mut set, _) = PluginSet::open(&root);
        let events = set.scan();
        assert!(
            events.iter().any(
                |e| matches!(e, ManagerEvent::NeedsApproval { id, .. } if id == "com.example.a")
            ),
            "unseen capabilities ask: {events:?}"
        );
        assert_eq!(
            state_of(&set, "com.example.a"),
            PluginStateKind::NeedsApproval
        );

        set.approve("com.example.a", &["log".to_owned()]);
        assert_eq!(state_of(&set, "com.example.a"), PluginStateKind::Loaded);

        set.disable("com.example.a");
        assert_eq!(state_of(&set, "com.example.a"), PluginStateKind::Disabled);
        assert!(set.plugins.is_empty(), "disabled unloads");

        // Grants survived the switch: enabling resumes without asking.
        let events = set.enable("com.example.a");
        assert!(events.is_empty(), "{events:?}");
        assert_eq!(state_of(&set, "com.example.a"), PluginStateKind::Loaded);

        set.deny("com.example.a");
        assert_eq!(state_of(&set, "com.example.a"), PluginStateKind::Denied);

        // Enabling never overrides a refusal.
        let events = set.enable("com.example.a");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, ManagerEvent::Warned { .. })),
            "silent grant: {events:?}"
        );
        assert_eq!(state_of(&set, "com.example.a"), PluginStateKind::Denied);

        // Asking again forgets the refusal like a first sighting.
        let events = set.reapprove("com.example.a");
        assert!(
            events.iter().any(
                |e| matches!(e, ManagerEvent::NeedsApproval { id, .. } if id == "com.example.a")
            ),
            "no second ask: {events:?}"
        );
        assert_eq!(
            state_of(&set, "com.example.a"),
            PluginStateKind::NeedsApproval
        );
    }

    /// Old flat grants files still read; disabled survives a restart.
    #[test]
    fn grants_files_old_and_new() {
        let root = dir("grants-shape");
        std::fs::write(root.join("grants.json"), r#"{"com.example.a":["log"]}"#).unwrap();
        let (grants, disabled) = load_grants(&root);
        assert_eq!(grants.get("com.example.a").unwrap(), &["log"]);
        assert!(disabled.is_empty());

        let root = dir("grants-disabled");
        plugin_dir(
            &root,
            "com.example.a",
            "globalThis.__gumicord_apply = (n) => n;",
            "",
        );
        let (mut set, _) = PluginSet::open(&root);
        assert!(set.scan().is_empty());
        set.disable("com.example.a");
        drop(set);

        let (mut set, _) = PluginSet::open(&root);
        let events = set.scan();
        assert!(
            events.iter().all(|e| !matches!(
                e,
                ManagerEvent::NeedsApproval { .. } | ManagerEvent::Patched(_)
            )),
            "disabled plugin asked or ran: {events:?}"
        );
        assert_eq!(
            set.states()[0].state,
            PluginStateKind::Disabled,
            "disabled did not survive"
        );
    }

    /// A declared settings page reads; anything else is no page. The
    /// settings screen embeds what comes back verbatim.
    #[test]
    fn settings_trees_read() {
        use gumicord_uitree::NodeId;
        let root = dir("settings-page");
        let plain = plugin_dir(
            &root,
            "com.example.plain",
            "globalThis.__gumicord_apply = (n) => n;",
            "",
        );
        let _ = plain;
        let with = root.join("com.example.paged");
        std::fs::create_dir_all(&with).unwrap();
        std::fs::write(
            with.join("manifest.json"),
            r#"{"id":"com.example.paged","name":"Paged","version":"1.0.0","capabilities":[],"settings":"settings.js"}"#,
        )
        .unwrap();
        std::fs::write(
            with.join("plugin.js"),
            "globalThis.__gumicord_apply = (n) => n;",
        )
        .unwrap();
        std::fs::write(
            with.join("settings.js"),
            r#"globalThis.__gumicord_settings = () => ({ id: "primitive.text", props: { value: "hi" } });"#,
        )
        .unwrap();

        let (mut set, _) = PluginSet::open(&root);
        assert!(set.scan().is_empty());

        let states = set.states();
        assert!(
            states
                .iter()
                .find(|s| s.id == "com.example.paged")
                .is_some_and(|s| s.has_settings)
        );
        assert!(
            states
                .iter()
                .find(|s| s.id == "com.example.plain")
                .is_some_and(|s| !s.has_settings)
        );

        let tree = set.settings_tree("com.example.paged").expect("no page");
        let mut texts = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveText {
                texts.extend(n.content.as_text().map(str::to_owned));
            }
        });
        assert_eq!(texts, ["hi"]);

        assert_eq!(set.settings_tree("com.example.plain"), None);
        assert_eq!(set.settings_tree("com.example.missing"), None);

        // A throwing page is no page, not a crash.
        std::fs::write(
            with.join("settings.js"),
            "globalThis.__gumicord_settings = () => { throw new Error('boom'); };",
        )
        .unwrap();
        assert_eq!(set.settings_tree("com.example.paged"), None);
    }

    /// Reload re-reads the files: editing code alone changes nothing until
    /// it runs. Hot reload watches theme files, not plugin code.
    #[test]
    fn reloading_picks_up_edited_code() {
        use gumicord_uitree::{NodeId, UiNode};
        let root = dir("reload-code");
        let plugin = plugin_dir(
            &root,
            "com.example.r",
            r#"globalThis.__gumicord_apply = (n) => ({...n, children: [...(n.children ?? []), {id: "primitive.text", props: {value: "v1"}}]});"#,
            "",
        );
        let (mut set, _) = PluginSet::open(&root);
        assert!(set.scan().is_empty());

        let texts = |set: &mut PluginSet| {
            let tree = UiNode::new(NodeId::ChatMessageContent);
            let (out, _) = set.apply_all(tree, &PatchContext::empty());
            out.children
                .iter()
                .filter_map(|c| c.content.as_text().map(str::to_owned))
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&mut set), ["v1"]);

        std::fs::write(
            plugin.join("plugin.js"),
            r#"globalThis.__gumicord_apply = (n) => ({...n, children: [...(n.children ?? []), {id: "primitive.text", props: {value: "v2"}}]});"#,
        )
        .unwrap();
        // Still the old code: nothing watches plugin files.
        assert_eq!(texts(&mut set), ["v1"]);

        assert!(set.reload("com.example.r").is_empty());
        assert_eq!(texts(&mut set), ["v2"]);
    }
}
