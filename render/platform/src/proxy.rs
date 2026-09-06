//! Invisible login fields for iOS password autofill.
//!
//! winit's view only speaks `UIKeyInput`, which the password manager cannot
//! fill into. Two hidden `UITextField` siblings (username + password, so the
//! manager pairs them) receive the fill; this layer polls their text into
//! the app's documents. Visible editing stays in our rendered fields: the
//! proxies are a single transparent pixel and never take touches.
//!
//! Polled, not delegated: a delegate class from Rust is a maintenance
//! burden, and a frame of latency is invisible on a login form.

use objc2::rc::Retained;
use objc2::{class, msg_send};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_ui_kit::{
    UIKeyboardType, UITextAutocapitalizationType, UITextAutocorrectionType,
    UITextContentTypePassword, UITextContentTypeUsername, UITextField, UITextInputTraits, UIView,
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
}

fn idx(kind: super::ImeProxy) -> usize {
    match kind {
        super::ImeProxy::Username => 0,
        super::ImeProxy::Password => 1,
    }
}

fn make_field() -> Retained<UITextField> {
    let field: Retained<UITextField> = unsafe { msg_send![class!(UITextField), new] };
    // Off-screen, not untouchable: a view that takes no interaction may
    // refuse first responder, and outside the parent bounds no touch lands.
    field.setFrame(NSRect::new(NSPoint::new(-5.0, -5.0), NSSize::new(1.0, 1.0)));
    field.setAlpha(0.0);
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
        for field in [&self.user, &self.pass] {
            field.resignFirstResponder();
            field.removeFromSuperview();
        }
        self.active = kind;
        self.parent = kind.map(|_| ptr);
        if let Some(kind) = kind {
            for field in [&self.user, &self.pass] {
                parent.addSubview(field);
            }
            let field = self.field(kind);
            set_text(field, text);
            field.becomeFirstResponder();
            // A refused responder means no keyboard from here; drop back to
            // winit's field instead of sitting focused with no keyboard.
            if !field.isFirstResponder() {
                self.active = None;
            } else {
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

    /// Reads both fields. The manager fills username and password as a
    /// pair, so the idle sibling moves too — watching only the focused
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
                return Some((kind, ProxyEvent::Submitted(clean)));
            }
            self.last[idx(kind)] = current.clone();
            return Some((kind, ProxyEvent::Text(current)));
        }
        None
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
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
