//! The login screens: QR, password, TOTP and token forms.
//!
//! Owns its fields outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_platform::{HiddenKey, TextDocument};

/// Which login-form field, if any, has focus. Only one at a time, and only
/// while a form is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginField {
    Email,
    Password,
    Totp,
    /// The bot-token form's single field.
    Token,
}

/// Login screen state. Everything here is used by login code only.
#[derive(Debug)]
pub(crate) struct LoginView {
    /// Which login-form field has focus, if any.
    pub(crate) field: Option<LoginField>,
    /// The form the user is on: it stays put while login runs or fails, so an
    /// error lands back on the same form instead of bouncing to the QR.
    pub(crate) form: Option<LoginField>,
    /// The last login failure, shown on the form so the user knows why and can
    /// retry. Cleared by the next attempt or by leaving the form.
    pub(crate) error: Option<String>,
    /// Per-field failures from the last attempt, as (dotted path, detail).
    /// Shown under each named input; cleared with the general error.
    pub(crate) field_errors: Vec<(String, String)>,
    /// The login form's email contents. Kept across password retries.
    pub(crate) email: TextDocument,
    /// The login form's password or TOTP code, whichever step is shown.
    pub(crate) input: TextDocument,
    /// The hidden code (konami) typed on the QR screen so far. Completed
    /// sequences open the bot-token form; anything else resets it.
    pub(crate) hidden_code: Vec<HiddenKey>,
}

impl LoginView {
    pub(crate) fn new() -> Self {
        LoginView {
            field: None,
            form: None,
            error: None,
            field_errors: Vec::new(),
            email: TextDocument::new(),
            input: TextDocument::new(),
            hidden_code: Vec::new(),
        }
    }
}
