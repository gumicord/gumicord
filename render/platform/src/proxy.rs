//! Invisible login fields for iOS password autofill.
//!
//! winit's view only speaks `UIKeyInput`, which the password manager cannot
//! fill into. Two hidden `UITextField` siblings (username + password, so the
//! manager pairs them) receive the fill; this layer polls their text into
//! the app's documents. Visible editing stays in our rendered fields: these
//! draw nothing (clear text, clear caret) and stand in a strip beside the
//! field rather than over it, so they can stay touchable — which AutoFill
//! requires — without standing in a finger's way (see [`Proxy::place`]).
//!
//! Polled, not delegated: a delegate class from Rust is a maintenance
//! burden, and a frame of latency is invisible on a login form.

use objc2::rc::Retained;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_ui_kit::{
    UIColor, UIKeyboardType, UITextAutocapitalizationType, UITextAutocorrectionType,
    UITextBorderStyle, UITextContentTypePassword, UITextContentTypeUsername, UITextField,
    UITextInputTraits, UIView,
};

/// What the poll found. A return key arrives as a newline inside the text;
/// login fields are single-line, so that means submit.
pub enum ProxyEvent {
    Text(String),
    Submitted(String),
}

pub struct Proxy {
    user: Retained<UITextField>,
    pass: Retained<UITextField>,
    active: Option<super::ImeProxy>,
    last: [String; 2],
    parent: Option<std::ptr::NonNull<std::ffi::c_void>>,
    /// Where the fields were last parked; setting the same frame every
    /// tick churns layout for no reason.
    placed: [(f64, f64, f64, f64); 2],
}

fn idx(kind: super::ImeProxy) -> usize {
    match kind {
        super::ImeProxy::Username => 0,
        super::ImeProxy::Password => 1,
    }
}

fn make_field() -> Retained<UITextField> {
    let field: Retained<UITextField> = unsafe { msg_send![class!(UITextField), new] };
    // Off-screen until parked, never untouchable: a view that takes no
    // interaction may refuse first responder, and AutoFill offers nothing
    // to a field it cannot touch.
    field.setFrame(NSRect::new(NSPoint::new(-5.0, -5.0), NSSize::new(1.0, 1.0)));
    // Invisible but present: hidden and alpha-zero fields are ignored by
    // password autofill, so the password half of a paired fill never lands.
    // No border, and clear text and caret instead: the app draws those, so
    // where these sit costs nothing (see `place`).
    field.setBorderStyle(UITextBorderStyle::None);
    field.setTextColor(Some(&UIColor::clearColor()));
    // Tints the caret: only MainThreadOnly is unsafe, and this runs there.
    unsafe { field.setTintColor(Some(&UIColor::clearColor())) };
    field.setAutocapitalizationType(UITextAutocapitalizationType::None);
    field.setAutocorrectionType(UITextAutocorrectionType::No);
    field
}

fn configure(field: &UITextField, kind: super::ImeProxy) {
    match kind {
        // Reaching into the statics needs unsafe: nothing checks them.
        super::ImeProxy::Username => unsafe {
            field.setTextContentType(Some(UITextContentTypeUsername));
            field.setKeyboardType(UIKeyboardType::EmailAddress);
            field.setSecureTextEntry(false);
        },
        super::ImeProxy::Password => unsafe {
            field.setTextContentType(Some(UITextContentTypePassword));
            field.setKeyboardType(UIKeyboardType::Default);
            field.setSecureTextEntry(true);
        },
    }
}

fn field_text(field: &UITextField) -> String {
    field.text().map(|s| s.to_string()).unwrap_or_default()
}

fn set_text(field: &UITextField, text: &str) {
    field.setText(Some(&NSString::from_str(text)));
}

impl Proxy {
    pub fn new() -> Self {
        let user = make_field();
        let pass = make_field();
        configure(&user, super::ImeProxy::Username);
        configure(&pass, super::ImeProxy::Password);
        Proxy {
            user,
            pass,
            active: None,
            last: [String::new(), String::new()],
            parent: None,
            placed: [(-5.0, -5.0, 1.0, 1.0), (-5.0, -5.0, 1.0, 1.0)],
        }
    }

    fn field(&self, kind: super::ImeProxy) -> &UITextField {
        match kind {
            super::ImeProxy::Username => &self.user,
            super::ImeProxy::Password => &self.pass,
        }
    }

    /// Shows the wanted field under the given view. Both siblings stay
    /// attached while either is up: the manager pairs a username field
    /// with a password field by proximity, and a lone field fills alone.
    /// Steady state does nothing: re-attaching every frame resigns the
    /// first responder (killing the keyboard) and churns layout. Only a
    /// new parent (the view is recreated across suspend/resume) or a new
    /// kind re-attaches.
    /// `text` seeds a freshly shown field; while one stays up it is only
    /// read, never written, so typing never fights the sync.
    pub fn set_active(&mut self, parent: &UIView, kind: Option<super::ImeProxy>, text: &str) {
        let ptr = std::ptr::NonNull::from(parent).cast::<std::ffi::c_void>();
        if self.active == kind && self.parent == Some(ptr) {
            return;
        }
        tracing::debug!(?kind, prev = ?self.active, "proxy re-attaching");
        for field in [&self.user, &self.pass] {
            field.resignFirstResponder();
            field.removeFromSuperview();
            // Becoming first responder refuses without interaction, and it
            // stays on: AutoFill offers nothing to a field it cannot touch.
            field.setUserInteractionEnabled(true);
        }
        self.active = kind;
        self.parent = kind.map(|_| ptr);
        if let Some(kind) = kind {
            for field in [&self.user, &self.pass] {
                parent.addSubview(field);
            }
            let field = self.field(kind);
            // Seed only an empty native from the doc: the OS may have
            // filled it between the focus tap and this tick, and
            // overwriting that with a stale doc loses the fill. The
            // baseline below stays doc-side, so a kept fill still
            // diffs and routes on the next poll.
            if field_text(field).is_empty() && !text.is_empty() {
                set_text(field, text);
            }
            // Either the call took it or it already holds it; anything
            // else means no keyboard from here.
            let became = field.becomeFirstResponder() || field.isFirstResponder();
            tracing::debug!(?kind, became, placed = ?self.placed, "proxy field shown");
            // A refused responder means no keyboard from here; detach
            // instead of sitting focused with no keyboard.
            if !became {
                for field in [&self.user, &self.pass] {
                    field.removeFromSuperview();
                }
                self.active = None;
            } else {
                // Both twins stay touchable. AutoFill offers nothing to a
                // field it cannot touch, and the one holding the keyboard is
                // exactly the field it has to offer to — so making it
                // untouchable here is what makes the button never appear.
                // The pair is parked out of the way instead (see `place`).
                self.last[idx(kind)] = text.to_owned();
                // Snapshot the sibling too: a paired fill moves both, and
                // the poll below must see whose text actually changed.
                let other = match kind {
                    super::ImeProxy::Username => super::ImeProxy::Password,
                    super::ImeProxy::Password => super::ImeProxy::Username,
                };
                self.last[idx(other)] = field_text(self.field(other));
            }
        }
    }

    /// Parks both fields in a strip beside the one the user is on.
    ///
    /// AutoFill offers credentials only to fields it can touch, and writes a
    /// paired fill into both, so both twins stay in the window and stay
    /// touchable — which is the whole reason they are not left covering a
    /// visible login box and swallowing its taps.
    ///
    /// Their frame is otherwise free: the text and the caret are clear, the
    /// app draws those, and nothing here is ever seen. So they go where
    /// they can do least harm — one pixel wide, hard against the focused
    /// field's far edge, the two of them touching, which is as close as the
    /// manager's pairing wants. That pixel is the only thing a finger can
    /// land on.
    ///
    /// Inside the parent on purpose: off-screen, a field may refuse first
    /// responder and the keyboard never comes, which is why the strip
    /// flips to the field's other edge rather than leave the window.
    ///
    /// `rect` is that strip; `None` leaves both where they are.
    pub fn place(&mut self, rect: Option<(f64, f64, f64, f64)>) {
        let Some(rect) = rect else {
            return;
        };
        for kind in [super::ImeProxy::Username, super::ImeProxy::Password] {
            if self.placed[idx(kind)] != rect {
                let view = self.field(kind);
                view.setFrame(NSRect::new(
                    NSPoint::new(rect.0, rect.1),
                    NSSize::new(rect.2, rect.3),
                ));
                self.placed[idx(kind)] = rect;
            }
        }
    }

    /// Reads both fields. The manager fills username and password as a
    /// pair, so the idle sibling moves too; watching only the focused
    /// one drops the other half of the fill. One event per call; the
    /// caller polls every frame while up.
    pub fn poll(&mut self) -> Option<(super::ImeProxy, ProxyEvent)> {
        self.active?;
        for kind in [super::ImeProxy::Username, super::ImeProxy::Password] {
            let current = field_text(self.field(kind));
            if current == self.last[idx(kind)] {
                continue;
            }
            // Return is the only way a newline reaches a login field.
            let clean = current.replace('\n', "");
            if clean != current {
                let field = self.field(kind);
                set_text(field, &clean);
                self.last[idx(kind)] = clean.clone();
                tracing::debug!(?kind, "proxy submitted");
                return Some((kind, ProxyEvent::Submitted(clean)));
            }
            self.last[idx(kind)] = current.clone();
            tracing::debug!(?kind, "proxy fill polled");
            return Some((kind, ProxyEvent::Text(current)));
        }
        None
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Drops focus immediately, without needing the parent view. Used
    /// when the app already cleared focus (outside press, submit) so the
    /// keyboard hides on this event instead of the next redraw.
    pub fn blur(&mut self) {
        let was_active = self.active.is_some();
        // Whether resigning took first responder away tells a stuck
        // keyboard apart from a slow one.
        let user = self.user.resignFirstResponder();
        let pass = self.pass.resignFirstResponder();
        // Drop native text too: a refocus seeds from the document, so a
        // stale fill from before the blur must never replay into it.
        for field in [&self.user, &self.pass] {
            set_text(field, "");
            field.removeFromSuperview();
        }
        self.last = [String::new(), String::new()];
        if was_active {
            let held = self.user.isFirstResponder() || self.pass.isFirstResponder();
            tracing::debug!(user, pass, held, "proxy resigned");
        }
        self.active = None;
        self.parent = None;
    }
}

/// winit's view, the only legal parent. `None` when the handle is missing.
pub fn parent_view(window: &winit::window::Window) -> Option<&UIView> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    match handle.as_raw() {
        RawWindowHandle::UiKit(handle) => {
            Some(unsafe { &*handle.ui_view.as_ptr().cast::<UIView>() })
        }
        _ => None,
    }
}
