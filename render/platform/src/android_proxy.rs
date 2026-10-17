//! Android's login autofill twins.
//!
//! The IME connection lives in GameActivity's `SurfaceView`, which reports
//! `AUTOFILL_TYPE_NONE`: a credential provider has nothing it can fill
//! there. Two `EditText` siblings sit over the rendered login fields
//! instead (ADR-0017) — one with the username hint, one with the password
//! hint, both in the window so the provider pairs them — and the focused
//! one holds the keyboard while a login field is active.
//!
//! `LoginEditor.kt` owns the views; this layer decides when each twin takes
//! the keyboard and what its text means, the same shape as the iOS proxies
//! in `proxy.rs`. The receiving point is built in `android_on_create` (Java
//! main thread only) and lives for the process. Every call here may come
//! from any thread: each one attaches, and the Kotlin side hops to the UI
//! thread itself for whatever moves a view.

use jni::objects::{Global, JObject, JString, JValue};
use jni::{jni_sig, jni_str};

use crate::ImeProxy;
use crate::ime_parking::{Parking, Slot};

/// The receiving point, matching `LoginEditor.kt`. Built on the Java main
/// thread; never released, because the twins live for the process.
static EDITOR: std::sync::OnceLock<Global<JObject<'static>>> = std::sync::OnceLock::new();

/// What one poll found for one twin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    Text(String),
    /// The IME's action button, or a newline a single-line field cannot hold.
    Submitted(String),
}

/// Builds the receiving point. Java main thread only: views may only be
/// touched there, and the window's content view exists by then.
pub fn install(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    // `from_raw` asserts on null, so the pointers are checked before it.
    if vm.is_null() || activity.is_null() {
        tracing::warn!("onCreate handed over no JVM; password autofill stays off");
        return;
    }
    // Safe: the shell hands over pointers owned by the running `onCreate`.
    let vm = unsafe { jni::JavaVM::from_raw(vm.cast()) };
    let done = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let activity = unsafe { JObject::from_raw(env, activity.cast()) };
        let editor = env.new_object(
            jni_str!("dev/gumicord/app/LoginEditor"),
            jni_sig!("(Landroid/app/Activity;)V"),
            &[JValue::from(&activity)],
        )?;
        // The global reference is dropped never on purpose: the twins
        // outlive every window, and the JVM owns the objects.
        let editor = env.new_global_ref(editor)?;
        if EDITOR.set(editor).is_err() {
            tracing::warn!("the login twins were installed twice");
        }
        Ok(())
    });
    if let Err(e) = done {
        tracing::warn!(%e, "the login twins could not be created; password autofill stays off");
    }
}

/// Whether the twins can be used at all. Without them the login fields fall
/// back to GameTextInput: typing still works, autofill does not.
pub fn available() -> bool {
    EDITOR.get().is_some()
}

/// Runs one call against the receiving point. Any thread may ask: the
/// attach is a no-op when this thread already holds one.
fn call<T>(
    what: &'static str,
    f: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> Result<T, jni::errors::Error>,
) -> Option<T> {
    let editor = EDITOR.get()?;
    let ctx = ndk_context::android_context();
    // `from_raw` asserts on null, and a shell that never set the context
    // has exactly that.
    if ctx.vm().is_null() {
        return None;
    }
    // Safe: the shell set both pointers up before the first frame ran.
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) };
    match vm.attach_current_thread(|env| f(env, editor.as_obj())) {
        Ok(value) => Some(value),
        Err(e) => {
            tracing::warn!(%e, what, "login twin call failed");
            None
        }
    }
}

/// What one twin's text says since the last read.
#[derive(Debug, PartialEq, Eq)]
enum Diff {
    /// Nothing new.
    Same,
    /// A fill or a keystroke landed.
    Changed(String),
    /// A newline the single-line field could not hold: the action button,
    /// or a paste from somewhere that had one. The caller writes [`Self::clean`]
    /// back before reporting it.
    Acted(String),
}

impl Diff {
    fn clean(text: &str) -> String {
        text.replace('\n', "")
    }

    fn of(current: &str, last: &str) -> Diff {
        if current == last {
            return Diff::Same;
        }
        let clean = Diff::clean(current);
        if clean != current {
            return Diff::Acted(clean);
        }
        Diff::Changed(current.to_owned())
    }
}

fn index_of(kind: ImeProxy) -> i32 {
    match kind {
        ImeProxy::Username => 0,
        ImeProxy::Password => 1,
    }
}

fn kind_of(index: i32) -> Option<ImeProxy> {
    match index {
        0 => Some(ImeProxy::Username),
        1 => Some(ImeProxy::Password),
        _ => None,
    }
}

fn slot_of(kind: ImeProxy) -> Slot {
    match kind {
        ImeProxy::Username => Slot::Username,
        ImeProxy::Password => Slot::Password,
    }
}

/// The two twins, and the rules around them.
#[derive(Debug, Default)]
pub struct AndroidProxy {
    /// Which login field owns the keyboard, if any.
    active: Option<ImeProxy>,
    /// What each twin held at the last read. Our own write-back updates it
    /// too, so its echo never counts as a change.
    last: [String; 2],
    /// Where each twin sits, and whether it sits anywhere yet. The rule and
    /// its bookkeeping live in [`crate::ime_parking`] so they can be checked
    /// without a device; this layer only moves the views.
    parking: Parking,
}

impl AndroidProxy {
    /// Nothing owns the keyboard yet, and both baselines start empty so the
    /// first fill diffs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Moves both twins over the rendered login controls. `None` parks a
    /// twin at a corner pixel and marks it unplaced, so [`poll`](Self::poll)
    /// skips it: half a paired fill beats a stale one replayed into the
    /// wrong document.
    pub fn place(
        &mut self,
        username: Option<gumicord_render::Rect>,
        password: Option<gumicord_render::Rect>,
    ) {
        for (kind, rect) in [
            (ImeProxy::Username, username),
            (ImeProxy::Password, password),
        ] {
            // `set` answers whether the frame moved, so a repeat stays
            // silent and a withdrawn rectangle unplaces the slot.
            if !self.parking.set(slot_of(kind), rect) {
                continue;
            }
            let index = index_of(kind);
            match rect {
                Some(rect) => {
                    call("place", move |env, editor| {
                        env.call_method(
                            editor,
                            jni_str!("place"),
                            jni_sig!("(IFFFF)V"),
                            &[
                                JValue::Int(index),
                                JValue::Float(rect.x),
                                JValue::Float(rect.y),
                                JValue::Float(rect.w),
                                JValue::Float(rect.h),
                            ],
                        )?;
                        Ok(())
                    });
                }
                None => {
                    call("park", move |env, editor| {
                        env.call_method(
                            editor,
                            jni_str!("park"),
                            jni_sig!("(I)V"),
                            &[JValue::Int(index)],
                        )?;
                        Ok(())
                    });
                }
            }
        }
    }

    /// Asks one twin to take the keyboard, or lets both go. `text` seeds a
    /// freshly shown twin; while one stays up it is only read, never
    /// written, so typing never fights the sync.
    pub fn set_active(&mut self, kind: Option<ImeProxy>, text: &str) {
        if self.active == kind {
            return;
        }
        tracing::debug!(?kind, prev = ?self.active, "login twin re-attaching");
        let Some(kind) = kind else {
            self.blur();
            return;
        };
        self.active = Some(kind);
        let index = index_of(kind);
        let seed = text.to_owned();
        call("focus", move |env, editor| {
            let seed = env.new_string(seed)?;
            env.call_method(
                editor,
                jni_str!("focus"),
                jni_sig!("(ILjava/lang/String;)V"),
                &[JValue::Int(index), JValue::from(&seed)],
            )?;
            Ok(())
        });
        // The baseline stays document-side: a fill that lands after this
        // point still diffs and routes on the next poll.
        self.last[index as usize] = text.to_owned();
        // Snapshot the idle sibling too, mirroring the iOS proxies: a
        // paired fill moves both, and the poll has to see whose text
        // actually changed.
        let other = other_of(kind);
        if self.parking.placed(slot_of(other)) {
            self.last[index_of(other) as usize] = self.read(other).unwrap_or_default();
        }
    }

    /// Reads both twins. The provider fills username and password as a
    /// pair, so the idle sibling moves too; watching only the focused one
    /// drops the other half. One event per call; the caller polls every
    /// tick while a field is up.
    pub fn poll(&mut self) -> Option<(ImeProxy, EditorEvent)> {
        self.active?;
        for kind in [ImeProxy::Username, ImeProxy::Password] {
            // A twin with no rectangle has not settled; a fill landing
            // there would be judged against a baseline from before it.
            if !self.parking.placed(slot_of(kind)) {
                continue;
            }
            let Some(current) = self.read(kind) else {
                continue;
            };
            let index = index_of(kind) as usize;
            match Diff::of(&current, &self.last[index]) {
                Diff::Same => continue,
                Diff::Acted(clean) => {
                    // The twin still holds the newline; write it back so
                    // the field, the cache and the baseline agree again.
                    self.write(kind, &clean);
                    self.last[index] = clean.clone();
                    tracing::debug!(?kind, "login twin action");
                    return Some((kind, EditorEvent::Submitted(clean)));
                }
                Diff::Changed(current) => {
                    self.last[index] = current.clone();
                    tracing::debug!(?kind, "login twin fill polled");
                    return Some((kind, EditorEvent::Text(current)));
                }
            }
        }
        None
    }

    /// Whether a twin holds the keyboard.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn active_kind(&self) -> Option<ImeProxy> {
        self.active
    }

    /// Which twin the keyboard sits on, if any. A tap on an overlapping
    /// twin moves it without the app hearing about the press.
    pub fn focused(&self) -> Option<ImeProxy> {
        let index = call("focused", |env, editor| {
            env.call_method(editor, jni_str!("focused"), jni_sig!("()I"), &[])?
                .i()
        })?;
        kind_of(index)
    }

    /// The oldest action button the IME reported, as its twin.
    pub fn poll_action(&mut self) -> Option<ImeProxy> {
        let index = call("pollAction", |env, editor| {
            env.call_method(editor, jni_str!("pollAction"), jni_sig!("()I"), &[])?
                .i()
        })?;
        kind_of(index)
    }

    /// Lets go now rather than on the next redraw, so the keyboard cannot
    /// linger over what comes next. Native text goes too: a refocus seeds
    /// from the document, and a stale fill from before the blur must not
    /// replay into it.
    ///
    /// Steady state is free: this runs on every tick while no login field
    /// is focused, and a JNI round trip (plus a posted runnable) per tick is
    /// not. Without a keyboard to give back there is nothing to tear down.
    pub fn blur(&mut self) {
        let was_active = self.active.is_some();
        if was_active {
            call("blur", |env, editor| {
                env.call_method(editor, jni_str!("blur"), jni_sig!("()V"), &[])?;
                Ok(())
            });
            tracing::debug!("login twins resigned");
        }
        self.last = [String::new(), String::new()];
        self.active = None;
    }

    fn read(&self, kind: ImeProxy) -> Option<String> {
        let index = index_of(kind);
        call("text", move |env, editor| {
            let text = env
                .call_method(
                    editor,
                    jni_str!("text"),
                    jni_sig!("(I)Ljava/lang/String;"),
                    &[JValue::Int(index)],
                )?
                .l()?;
            if text.is_null() {
                return Ok(String::new());
            }
            let text: JString = env.cast_local::<JString>(text)?;
            text.try_to_string(env)
        })
    }

    fn write(&self, kind: ImeProxy, text: &str) {
        let index = index_of(kind);
        let text = text.to_owned();
        call("setText", move |env, editor| {
            let text = env.new_string(text)?;
            env.call_method(
                editor,
                jni_str!("setText"),
                jni_sig!("(ILjava/lang/String;)V"),
                &[JValue::Int(index), JValue::from(&text)],
            )?;
            Ok(())
        });
    }
}

fn other_of(kind: ImeProxy) -> ImeProxy {
    match kind {
        ImeProxy::Username => ImeProxy::Password,
        ImeProxy::Password => ImeProxy::Username,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A twin with no receiver answers nothing, and the poll keeps going.
    #[test]
    fn nothing_is_installed_without_a_shell() {
        assert!(!available());
    }

    /// Steady state: the same text twice is one event, not two.
    #[test]
    fn an_unchanged_field_says_nothing() {
        assert_eq!(Diff::of("secret", "secret"), Diff::Same);
    }

    /// A fill or a keystroke is reported once, with the text it carried.
    #[test]
    fn a_changed_field_reports_its_text() {
        assert_eq!(
            Diff::of("alice@example.com", ""),
            Diff::Changed("alice@example.com".to_owned())
        );
    }

    /// The single-line contract: a newline cannot be typed into these
    /// fields, so one means the action button (or a paste) and the text is
    /// reported cleaned.
    #[test]
    fn a_newline_means_the_action_button() {
        assert_eq!(
            Diff::of("secret\n", "secret"),
            Diff::Acted("secret".to_owned())
        );
    }

    /// A paste carrying a newline lands as text, not as an action, once the
    /// newline is gone from the baseline.
    #[test]
    fn a_paste_with_a_newline_is_reported_cleaned_first() {
        // First poll: the newline cannot stand, so it is reported cleaned.
        assert_eq!(
            Diff::of("two\nlines", "old"),
            Diff::Acted("twolines".to_owned())
        );
        // The write-back and the baseline agree, so its echo is silent.
        assert_eq!(Diff::of("twolines", "twolines"), Diff::Same);
    }

    /// The two halves round-trip through the indices the receiving point
    /// uses; a twin that never existed answers nothing. `-1` is Kotlin's
    /// `UNSET`, the answer for "no field".
    #[test]
    fn the_indices_name_the_two_halves() {
        assert_eq!(
            kind_of(index_of(ImeProxy::Username)),
            Some(ImeProxy::Username)
        );
        assert_eq!(
            kind_of(index_of(ImeProxy::Password)),
            Some(ImeProxy::Password)
        );
        assert_eq!(kind_of(-1), None);
        assert_eq!(kind_of(2), None);
    }
}
