//! The login screens: QR, password, TOTP and token forms.
//!
//! Owns its fields outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_platform::{HiddenKey, TextDocument};
use gumicord_platform::Application;
use gumicord_uitree::{Content, Editable, Key, NodeId, State, UiNode};

use super::super::session::Session;

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

impl crate::Gumicord {
    /// Moves focus between login fields for the keyboard's advance button.
    /// No neighbor forward (or anywhere without one) acts instead; back from
    /// the first field does nothing.
    pub(crate) fn focus_neighbor(&mut self, next: bool) -> bool {
        let target = match (self.login_view.field, next) {
            (Some(LoginField::Email), true) => Some(LoginField::Password),
            (Some(LoginField::Password), false) => Some(LoginField::Email),
            _ => None,
        };
        match target {
            Some(field) => {
                self.login_view.field = Some(field);
                true
            }
            None => next && self.submit(),
        }
    }

    /// Submits the active login step. Runs the password flow, hands off a TOTP
    /// code, or logs in with a bot token; nothing to send stays put.
    pub(crate) fn submit_login(&mut self) -> bool {
        self.login_view.error = None;
        self.login_view.field_errors.clear();
        match self.login_view.field {
            Some(LoginField::Token) => {
                let token = self.login_view.input.text().trim().to_owned();
                if token.is_empty() {
                    return false;
                }
                self.login.submit_bot_token(token);
                self.login_view.input.take();
                self.login_view.field = None;
                true
            }
            Some(LoginField::Email) | Some(LoginField::Password) | None => {
                let email = self.login_view.email.text().trim().to_owned();
                let password = self.login_view.input.text().to_owned();

                if email.is_empty() || password.is_empty() {
                    return false;
                }
                self.login.submit_password(email, password);
                // Keep the email for a retry; the password is a secret that
                // has done its job.
                self.login_view.input.take();
                self.login_view.field = None;
                true
            }
            Some(LoginField::Totp) => {
                let code = self.login_view.input.text().trim().to_owned();
                if code.is_empty() {
                    return false;
                }
                self.login.submit_totp(code);
                self.login_view.input.take();
                self.login_view.field = None;
                true
            }
        }
    }

    /// Drops the login form and returns to the QR screen. Phones never
    /// show the QR screen, so there it backs out to the password form.
    pub(crate) fn leave_login_form(&mut self) {
        self.login.cancel_password();
        self.login_view.field = None;
        self.login_view.form = crate::is_mobile().then_some(LoginField::Password);
        self.login_view.error = None;
        self.login_view.field_errors.clear();
        self.login_view.input.take();
    }
}

impl crate::Gumicord {
    /// The login screen. Shows a QR by default, the password form when that
    /// was chosen, or the TOTP step when a second factor is needed.
    pub(crate) fn login_screen(&self) -> UiNode {
        let s = self.login.session();
        let mut screen = UiNode::new(NodeId::AppScreenLogin);

        // A form the user chose stays up while the login runs or even fails, so
        // its error is shown on the form, not bounced to the QR.
        //
        // The override pins only the password form: TOTP and token steps
        // always follow the session, or the override strands the flow on
        // the password screen (mobile never shows QR, so it is always set
        // there).
        let form = match s {
            Session::PasswordTotp => Some(LoginField::Totp),
            Session::Token => Some(LoginField::Token),
            _ => self.login_view.form.or(match s {
                Session::Password => Some(LoginField::Password),
                _ => None,
            }),
        };

        match form {
            Some(LoginField::Email | LoginField::Password) => {
                screen = screen.child(self.login_card(|_| {
                    UiNode::new(NodeId::LayoutColumn)
                        .with_key(Key::Slot("list"))
                        .child(UiNode::text(
                            NodeId::AppScreenLoginTitle,
                            "Discordにログイン",
                        ))
                        .child(self.login_label("メールアドレス"))
                        .child(self.login_field(
                            "email",
                            "メールアドレス",
                            &self.login_view.email,
                            false,
                        ))
                        .child_if(self.has_login_field_error(&["login"]), || {
                            self.login_field_error_node("login_error_email", &["login"])
                        })
                        .child(self.login_label("パスワード"))
                        .child(self.login_field("password", "パスワード", &self.login_view.input, true))
                        .child_if(self.has_login_field_error(&["password"]), || {
                            self.login_field_error_node("login_error_password", &["password"])
                        })
                        .child(self.login_forgot_password())
                        .child_if(self.login_view.error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン"))
                        .child(self.login_divider())
                        // No QR screen on phones, so nowhere to go back to.
                        .child_if(!crate::is_mobile(), || self.login_qr_button())
                        .child(self.login_register_link())
                }))
            }

            Some(LoginField::Totp) => {
                screen = screen.child(self.login_card(|_| {
                    UiNode::new(NodeId::LayoutColumn)
                        .with_key(Key::Slot("list"))
                        .child(UiNode::text(
                            NodeId::AppScreenLoginTitle,
                            "認証コードを入力",
                        ))
                        .child(self.login_label("認証コード"))
                        .child(self.login_field("totp", "認証コード", &self.login_view.input, false))
                        .child_if(self.has_login_field_error(&["code"]), || {
                            self.login_field_error_node("login_error_code", &["code"])
                        })
                        .child_if(self.login_view.error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン"))
                        .child(self.login_secondary("戻る", "login_back"))
                }))
            }

            Some(LoginField::Token) => {
                screen = screen.child(self.login_card(|_| {
                    UiNode::new(NodeId::LayoutColumn)
                        .with_key(Key::Slot("list"))
                        .child(UiNode::text(
                            NodeId::AppScreenLoginTitle,
                            "ボットトークンでログイン",
                        ))
                        .child(self.login_label("トークン"))
                        .child(self.login_field("token", "トークン", &self.login_view.input, false))
                        .child_if(self.login_view.error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン"))
                        .child(self.login_secondary("戻る", "login_back"))
                }))
            }

            None => {
                // Default: QR code screen with option to use password login
                screen = screen.child(
                    UiNode::new(NodeId::LayoutRow)
                        .child(UiNode::new(NodeId::LayoutSpacer))
                        .child(
                            UiNode::new(NodeId::LayoutColumn)
                                .with_key(Key::Slot("qr_column"))
                                .child(UiNode::text(
                                    NodeId::AppScreenLoginTitle,
                                    "QR コードでログイン",
                                ))
                                .child_if(s.qr().is_some(), || {
                                    UiNode::qr(NodeId::PrimitiveQr, s.qr().unwrap_or_default())
                                })
                                .child(UiNode::text(NodeId::AppScreenLoginHint, self.login.hint()))
                                .child(
                                    self.login_secondary("パスワードでログイン", "login_password"),
                                ),
                        )
                        .child(UiNode::new(NodeId::LayoutSpacer)),
                )
            }
        }

        screen
    }
}

impl crate::Gumicord {
    /// Wraps children in a centered login card container.
    fn login_card(&self, build: impl FnOnce(UiNode) -> UiNode) -> UiNode {
        UiNode::new(NodeId::AppScreenLoginCard).child(build(UiNode::new(NodeId::LayoutSpacer)))
    }

    /// A small label above a login-field box.
    fn login_label(&self, text: &str) -> UiNode {
        UiNode::text(NodeId::AppScreenLoginLabel, text)
    }

    /// "パスワードを忘れた場合" link under the password field.
    fn login_forgot_password(&self) -> UiNode {
        UiNode::new(NodeId::AppScreenLoginForgot)
            .with_content(Content::Text("パスワードを忘れた場合".into()))
            .with_key(Key::Slot("login_forgot_password"))
    }

    /// An error line below a login form. The message is always present when
    /// this is called.
    fn login_error_node(&self) -> UiNode {
        UiNode::text(
            NodeId::AppScreenLoginError,
            self.login_view.error.as_deref().unwrap_or_default(),
        )
    }

    /// Whether any field failure names one of these Discord paths (`login`
    /// for email, `password`, `code` for TOTP).
    fn has_login_field_error(&self, segments: &[&str]) -> bool {
        self.login_view.field_errors.iter().any(|(path, _)| {
            segments
                .iter()
                .any(|s| path == s || path.starts_with(&format!("{s}.")))
        })
    }

    /// An error line below one login field. Shares the general error's node
    /// so the styling matches; the slot tells same-id siblings apart. Only
    /// call when [`Self::has_login_field_error`] holds.
    fn login_field_error_node(&self, slot: &'static str, segments: &[&str]) -> UiNode {
        let detail = self
            .login_view
            .field_errors
            .iter()
            .filter(|(path, _)| {
                segments
                    .iter()
                    .any(|s| path == s || path.starts_with(&format!("{s}.")))
            })
            .map(|(_, detail)| detail.clone())
            .collect::<Vec<_>>()
            .join(" / ");
        UiNode::text(NodeId::AppScreenLoginError, detail).with_key(Key::Slot(slot))
    }

    /// "または" divider with lines on both sides.
    fn login_divider(&self) -> UiNode {
        UiNode::text(NodeId::AppScreenLoginDivider, "または")
    }

    /// QR code login button.
    fn login_qr_button(&self) -> UiNode {
        UiNode::new(NodeId::AppScreenLoginQrButton)
            .with_key(Key::Slot("login_qr"))
            .child(UiNode::text(NodeId::PrimitiveText, "QRコードでログイン"))
    }

    /// "アカウントを作成" link at the bottom.
    fn login_register_link(&self) -> UiNode {
        UiNode::new(NodeId::AppScreenLoginRegister)
            .with_content(Content::Text("アカウントを作成".into()))
            .with_key(Key::Slot("login_register"))
    }

    /// One editable box on the login form. `slot` picks email/password/totp/
    /// token; a focused field carries the focus state. When `mask` is true the
    /// text is replaced by bullets while the real content stays in `doc`.
    fn login_field(
        &self,
        slot: &'static str,
        placeholder: &str,
        doc: &TextDocument,
        mask: bool,
    ) -> UiNode {
        let (text, caret, selection) = if mask {
            Self::masked(doc)
        } else {
            (doc.text().to_owned(), doc.caret(), doc.selection())
        };
        UiNode::editable(
            NodeId::AppScreenLoginField,
            Editable {
                text,
                caret,
                selection,
                composing: if mask { None } else { doc.composing() },
                placeholder: placeholder.to_owned(),
            },
        )
        .with_key(Key::Slot(slot))
        .with_state_if(self.login_field_slot(slot), State::Focus)
    }

    /// A bullet-substituted view of a secret field: one `•` per character,
    /// with caret and selection remapped so the caret sits in the right place.
    fn masked(doc: &TextDocument) -> (String, usize, std::ops::Range<usize>) {
        const BULLET: &str = "•";
        let text = BULLET.repeat(doc.text().chars().count());
        let map = |byte: usize| doc.text()[..byte].chars().count() * 3;
        let sel = doc.selection();
        (text, map(doc.caret()), map(sel.start)..map(sel.end))
    }

    /// Whether the given slot is the currently focused login field.
    fn login_field_slot(&self, slot: &'static str) -> bool {
        matches!(
            (self.login_view.field, slot),
            (Some(LoginField::Email), "email")
                | (Some(LoginField::Password), "password")
                | (Some(LoginField::Totp), "totp")
                | (Some(LoginField::Token), "token")
        )
    }

    /// The primary login form button (submit).
    fn login_submit(&self, label: &str) -> UiNode {
        UiNode::new(NodeId::PrimitiveButton)
            .with_key(Key::Slot("login_submit"))
            .with_state_if(
                self.is_hovered(NodeId::PrimitiveButton, Some(&Key::Slot("login_submit"))),
                State::Hover,
            )
            .child(UiNode::text(NodeId::PrimitiveText, label))
    }

    /// A quiet, secondary login form button (entry or back).
    fn login_secondary(&self, label: &str, slot: &'static str) -> UiNode {
        UiNode::new(NodeId::PrimitiveButton)
            .with_key(Key::Slot(slot))
            .with_state_if(
                self.is_hovered(NodeId::PrimitiveButton, Some(&Key::Slot(slot))),
                State::Hover,
            )
            .child(UiNode::text(NodeId::PrimitiveText, label))
    }

    /// Acts on a login-form button press. `false` keeps the UI as it was.
    pub(crate) fn login_button(&mut self, slot: &str) -> bool {
        match slot {
            // From the QR screen into the password form.
            "login_password" => {
                self.login.start_password();
                self.login_view.field = None;
                self.login_view.form = Some(LoginField::Password);
                true
            }
            "login_submit" => self.submit_login(),
            // Back to the QR screen, abandoning the password login.
            "login_back" => {
                self.leave_login_form();
                true
            }
            // "パスワードを忘れた場合" - for now just acknowledge, could open a flow later
            "login_forgot_password" => true,
            // QRコードでログイン button - go back to QR screen
            "login_qr" => {
                self.leave_login_form();
                true
            }
            // アカウントを作成 - for now just acknowledge
            "login_register" => true,
            _ => false,
        }
    }
}
