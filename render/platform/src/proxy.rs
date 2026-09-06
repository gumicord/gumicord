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
    last: String,
}

fn make_field() -> Retained<UITextField> {
    let field: Retained<UITextField> = unsafe { msg_send![class!(UITextField), new] };
    field.setFrame(NSRect::new(NSPoint::ZERO, NSSize::new(1.0, 1.0)));
    field.setAlpha(0.0);
    field.setUserInteractionEnabled(false);
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
            last: String::new(),
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
    /// The parent is re-attached every time: the window (and its view) is
    /// recreated across suspend/resume, and a stale parent draws nothing.
    /// `text` seeds a freshly shown field; while one stays up it is only
    /// read, never written, so typing never fights the sync.
    pub fn set_active(&mut self, parent: &UIView, kind: Option<super::ImeProxy>, text: &str) {
        if self.active == kind {
            if kind.is_some() {
                for field in [&self.user, &self.pass] {
                    field.removeFromSuperview();
                    parent.addSubview(field);
                }
            }
            return;
        }
        for field in [&self.user, &self.pass] {
            field.resignFirstResponder();
            field.removeFromSuperview();
        }
        self.active = kind;
        if let Some(kind) = kind {
            for field in [&self.user, &self.pass] {
                parent.addSubview(field);
            }
            let field = self.field(kind);
            set_text(field, text);
            field.becomeFirstResponder();
            self.last = text.to_owned();
        }
    }

    /// Reads the active field. Returns text when it moved, or a submit when
    /// a return key arrived inside it.
    pub fn poll(&mut self) -> Option<(super::ImeProxy, ProxyEvent)> {
        let kind = self.active?;
        let current = field_text(self.field(kind));
        if current == self.last {
            return None;
        }
        // Return is the only way a newline reaches a login field.
        let clean = current.replace('\n', "");
        if clean != current {
            let field = self.field(kind);
            set_text(field, &clean);
            self.last = clean.clone();
            return Some((kind, ProxyEvent::Submitted(clean)));
        }
        self.last = current.clone();
        Some((kind, ProxyEvent::Text(current)))
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
