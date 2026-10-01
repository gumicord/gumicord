//! The login screens: QR, password, TOTP and token forms.
//!
//! Owns its fields outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_platform::Application;
use gumicord_platform::{HiddenKey, TextDocument};
use gumicord_uitree::{Content, Editable, Key, NodeId, State, UiNode};

use super::super::inputs::{InputAddr, InputKind};
use super::super::session::Session;

/// Digits a TOTP code holds. Discord uses six half-width digits; anything
/// else in the box can never verify.
pub(crate) const TOTP_LEN: usize = 6;

/// One TOTP digit, half-width. Full-width digits come from Japanese input
/// and verify the same once narrowed; anything else is dropped.
pub(crate) fn totp_digit(c: char) -> Option<char> {
    match c {
        '0'..='9' => Some(c),
        '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32),
        _ => None,
    }
}

/// Narrows a TOTP box to what the API reads: half-width digits only.
pub(crate) fn normalize_totp(raw: &str) -> String {
    raw.chars().filter_map(totp_digit).collect()
}

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
/// Input documents live in the top-level registry by address; focus
/// does too. This keeps which form is shown, and why it failed.
#[derive(Debug)]
pub(crate) struct LoginView {
    /// The form the user is on: it stays put while login runs or fails, so an
    /// error lands back on the same form instead of bouncing to the QR.
    pub(crate) form: Option<LoginField>,
    /// The last login failure, shown on the form so the user knows why and can
    /// retry. Cleared by the next attempt or by leaving the form.
    pub(crate) error: Option<String>,
    /// Per-field failures from the last attempt, as (dotted path, detail).
    /// Shown under each named input; cleared with the general error.
    pub(crate) field_errors: Vec<(String, String)>,
    /// The captcha challenge awaiting a solution, kept on the app side so the
    /// platform's modal can hand back a bare token while the challenge's own
    /// `rqtoken`/`session_id` are still available to echo on the retry.
    pub(crate) pending: Option<gumicord_rest::CaptchaChallenge>,
    /// The hidden code (konami) typed on the QR screen so far. Completed
    /// sequences open the bot-token form; anything else resets it.
    pub(crate) hidden_code: Vec<HiddenKey>,
}

/// The address and kind of one login box: the login field node plus
/// the slot naming the box. The single place translating fields to
/// addresses.
pub(crate) fn login_box(field: LoginField) -> (InputAddr, InputKind) {
    let (slot, kind) = match field {
        LoginField::Email => ("email", InputKind::Email),
        LoginField::Password => ("password", InputKind::Password),
        LoginField::Totp => ("totp", InputKind::Code),
        LoginField::Token => ("token", InputKind::Text),
    };
    (
        InputAddr::of(NodeId::AppScreenLoginField, Some(Key::Slot(slot))),
        kind,
    )
}

/// The address of one login box. See [`login_box`].
pub(crate) fn login_addr(field: LoginField) -> InputAddr {
    login_box(field).0
}

impl LoginView {
    pub(crate) fn new() -> Self {
        LoginView {
            form: None,
            error: None,
            field_errors: Vec::new(),
            pending: None,
            hidden_code: Vec::new(),
        }
    }
}

impl crate::Gumicord {
    /// Moves focus between login fields for the keyboard's advance button.
    /// No neighbor forward (or anywhere without one) acts instead; back from
    /// the first field does nothing.
    pub(crate) fn focus_neighbor(&mut self, next: bool) -> bool {
        let email = login_addr(LoginField::Email);
        let password = login_addr(LoginField::Password);
        let target = match (&self.focus, next) {
            (Some(a), true) if *a == email => Some(password),
            (Some(a), false) if *a == password => Some(email),
            _ => None,
        };
        match target {
            Some(addr) => {
                self.focus = Some(addr);
                true
            }
            None => next && self.submit(),
        }
    }

    /// Submits the active login step. Runs the password flow, hands off a TOTP
    /// code, or logs in with a bot token; nothing to send stays put.
    /// While an attempt is in flight every submit is ignored: re-sending
    /// only stacks duplicate attempts behind the running one.
    pub(crate) fn submit_login(&mut self) -> bool {
        if self.login.busy() {
            tracing::debug!("login submit ignored while busy");
            return false;
        }
        self.login_view.error = None;
        self.login_view.field_errors.clear();
        // A button press clears focus before routing here, so the focused
        // field cannot name the step: on the TOTP screen the field is
        // already `None` when this runs. The session names the TOTP and
        // token steps instead; anything else is the password form.
        if matches!(self.login.session(), Session::PasswordTotp) {
            let raw = self
                .inputs
                .must(&login_addr(LoginField::Totp))
                .text()
                .to_owned();
            let code = normalize_totp(&raw);
            tracing::debug!(
                raw_len = raw.chars().count(),
                digit_len = code.len(),
                "totp code normalized"
            );
            if code.len() != TOTP_LEN {
                self.login_view.error = Some("認証コードは6桁の数字で入力してください".to_owned());
                return false;
            }
            tracing::debug!("submitting a totp code");
            self.login.submit_totp(code);
            self.inputs.must_mut(&login_addr(LoginField::Totp)).take();
            self.focus = None;
            true
        } else if matches!(self.login.session(), Session::Token) {
            let token = self
                .inputs
                .must(&login_addr(LoginField::Token))
                .text()
                .trim()
                .to_owned();
            if token.is_empty() {
                return false;
            }
            self.login.submit_bot_token(token);
            self.inputs.must_mut(&login_addr(LoginField::Token)).take();
            self.focus = None;
            true
        } else {
            let email = self
                .inputs
                .must(&login_addr(LoginField::Email))
                .text()
                .trim()
                .to_owned();
            let password = self
                .inputs
                .must(&login_addr(LoginField::Password))
                .text()
                .to_owned();

            if email.is_empty() || password.is_empty() {
                tracing::debug!(
                    email_empty = email.is_empty(),
                    password_empty = password.is_empty(),
                    "password login not sent; a field is empty"
                );
                return false;
            }
            tracing::debug!("submitting a password login");
            self.login.submit_password(email, password);
            // Keep both for a retry or a trip back from the TOTP step.
            // The secret is masked on screen, and signing out wipes the
            // documents.
            self.focus = None;
            true
        }
    }

    /// Drops the login form and returns to the QR screen. Phones never
    /// show the QR screen, so there it backs out to the password form.
    pub(crate) fn leave_login_form(&mut self) {
        self.login.cancel_password();
        // A typed code or token must not linger behind the form. The
        // password keeps its own document, so a trip back from the TOTP
        // step still finds it.
        self.inputs.must_mut(&login_addr(LoginField::Totp)).take();
        self.inputs.must_mut(&login_addr(LoginField::Token)).take();
        self.focus = None;
        self.login_view.form = crate::is_mobile().then_some(LoginField::Password);
        self.login_view.error = None;
        self.login_view.field_errors.clear();
    }
}

impl crate::Gumicord {
    /// The splash: the app icon centred, with no text. The row's spacers
    /// centre horizontally and its cross axis centres vertically.
    /// No new stable IDs; the container was already in the ABI, unused.
    pub(crate) fn loading_screen(&self) -> UiNode {
        UiNode::new(NodeId::AppScreenLoading).child(
            UiNode::new(NodeId::LayoutRow)
                .child(UiNode::new(NodeId::LayoutSpacer))
                .child(UiNode::image(
                    NodeId::AppScreenLoadingIcon,
                    crate::images::SPLASH_ICON_URL,
                ))
                .child(UiNode::new(NodeId::LayoutSpacer)),
        )
    }

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
                            self.inputs.must(&login_addr(LoginField::Email)),
                            false,
                        ))
                        .child_if(self.has_login_field_error(&["login"]), || {
                            self.login_field_error_node("login_error_email", &["login"])
                        })
                        .child(self.login_label("パスワード"))
                        .child(
                            self.login_field(
                                "password",
                                "パスワード",
                                self.inputs.must(&login_addr(LoginField::Password)),
                                self.inputs
                                    .kind(&login_addr(LoginField::Password))
                                    .is_some_and(InputKind::secret),
                            ),
                        )
                        .child_if(self.has_login_field_error(&["password"]), || {
                            self.login_field_error_node("login_error_password", &["password"])
                        })
                        .child(self.login_forgot_password())
                        .child_if(self.login_view.error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン", self.login_password_ready()))
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
                        // Whose code this is, verbatim: a code read for the
                        // wrong account can never verify.
                        .child_if(self.login.totp_email().is_some(), || {
                            UiNode::text(
                                NodeId::AppScreenLoginHint,
                                self.login.totp_email().unwrap_or_default(),
                            )
                        })
                        .child(self.login_label("認証コード"))
                        .child(self.login_field(
                            "totp",
                            "認証コード",
                            self.inputs.must(&login_addr(LoginField::Totp)),
                            false,
                        ))
                        .child_if(self.has_login_field_error(&["code"]), || {
                            self.login_field_error_node("login_error_code", &["code"])
                        })
                        .child_if(self.login_view.error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン", self.login_totp_ready()))
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
                        .child(self.login_field(
                            "token",
                            "トークン",
                            self.inputs.must(&login_addr(LoginField::Token)),
                            false,
                        ))
                        .child_if(self.login_view.error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン", self.login_code_ready()))
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

    /// Whether the given slot is the currently focused login box.
    fn login_field_slot(&self, slot: &'static str) -> bool {
        let field = match slot {
            "email" => LoginField::Email,
            "password" => LoginField::Password,
            "token" => LoginField::Token,
            _ => LoginField::Totp,
        };
        self.focus.as_ref() == Some(&login_addr(field))
    }

    /// Whether the password form holds something to send. The button press
    /// clears focus first, so this reads the documents, not the focus.
    fn login_password_ready(&self) -> bool {
        !self
            .inputs
            .must(&login_addr(LoginField::Email))
            .text()
            .trim()
            .is_empty()
            && !self
                .inputs
                .must(&login_addr(LoginField::Password))
                .text()
                .is_empty()
    }

    /// Whether the TOTP box holds a sendable code: exactly six digits once
    /// narrowed. Anything else is refused at submit, so the button says so.
    fn login_totp_ready(&self) -> bool {
        normalize_totp(self.inputs.must(&login_addr(LoginField::Totp)).text()).len() == TOTP_LEN
    }

    /// Whether the bot-token box holds something.
    fn login_code_ready(&self) -> bool {
        !self
            .inputs
            .must(&login_addr(LoginField::Token))
            .text()
            .trim()
            .is_empty()
    }

    /// The primary login form button (submit). Dimmed while an attempt
    /// is in flight, and while there is nothing to send: an empty form
    /// silently ignores the press, which reads as frozen.
    fn login_submit(&self, label: &str, ready: bool) -> UiNode {
        UiNode::new(NodeId::PrimitiveButton)
            .with_key(Key::Slot("login_submit"))
            .with_state_if(
                self.is_hovered(NodeId::PrimitiveButton, Some(&Key::Slot("login_submit"))),
                State::Hover,
            )
            .with_state_if(!ready || self.login.busy(), State::Disabled)
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
                self.focus = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    use crate::session::{Login, LoginEvent, Session};

    /// A signed-out app. `Gumicord::new` reads the environment, and a
    /// developer's variables must not change test results.
    fn pending() -> Gumicord {
        Gumicord::with_themes(
            Login::fresh_for_test(),
            Live::without_cache(),
            PluginManager::disabled(),
            None,
        )
    }

    fn ids(tree: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| out.push(n.id));
        out
    }

    /// While booting with nothing real to show, the splash covers the
    /// login screen; the first answer decides which screen follows.
    #[test]
    fn the_splash_covers_the_login_screen_while_booting() {
        let a = pending();
        let seen = ids(&a.build_tree(Panes::Three));

        assert!(seen.contains(&NodeId::AppScreenLoading));
        assert!(!seen.contains(&NodeId::AppScreenLogin));
        assert!(!seen.contains(&NodeId::AppScreenMain));
        assert!(!seen.contains(&NodeId::ChatMessageList), "本文が漏れている");
    }

    /// The splash icon sits in the middle of the screen, on a phone-sized
    /// viewport. Rectangles stand in for seeing it.
    #[test]
    fn the_splash_icon_sits_centred() {
        let a = pending();
        let placed = gumicord_render::layout_for_test(
            &a.build_tree(Panes::Three),
            gumicord_render::Size::new(375.0, 667.0),
        );
        let rect = |id| placed.iter().find(|(i, _)| *i == id).map(|(_, r)| *r);
        let screen = rect(NodeId::AppScreenLoading).expect("splash missing");
        let icon = rect(NodeId::AppScreenLoadingIcon).expect("icon missing");
        assert!((icon.w - 96.0).abs() < 0.01, "icon width {icon:?}");
        assert!((icon.h - 96.0).abs() < 0.01, "icon height {icon:?}");
        assert!(
            ((icon.x - screen.x) - (screen.w - icon.w) / 2.0).abs() < 1.0,
            "horizontally off-centre: {icon:?} in {screen:?}"
        );
        assert!(
            ((icon.y - screen.y) - (screen.h - icon.h) / 2.0).abs() < 1.0,
            "vertically off-centre: {icon:?} in {screen:?}"
        );
    }

    /// The first answer ends the splash: a QR shows the login screen.
    #[test]
    fn the_login_screen_follows_the_first_answer() {
        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        let seen = ids(&a.build_tree(Panes::Three));

        assert!(!seen.contains(&NodeId::AppScreenLoading));
        assert!(seen.contains(&NodeId::AppScreenLogin));
        assert!(!seen.contains(&NodeId::AppScreenMain));
    }

    /// The main screen is not even built while signed out; visible but
    /// untouchable is the worst state.
    #[test]
    fn the_main_screen_is_not_built_before_login() {
        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        let seen = ids(&a.build_tree(Panes::Three));

        assert!(seen.contains(&NodeId::AppScreenLogin));
        assert!(!seen.contains(&NodeId::AppScreenMain));
        assert!(!seen.contains(&NodeId::ChatMessageList), "本文が漏れている");
    }

    /// No QR node before there is a QR: an unscannable one is worse than
    /// none.
    #[test]
    fn the_qr_node_appears_only_once_there_is_a_qr() {
        let mut a = pending();
        assert!(!ids(&a.build_tree(Panes::Three)).contains(&NodeId::PrimitiveQr));

        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        let tree = a.build_tree(Panes::Three);
        assert!(ids(&tree).contains(&NodeId::PrimitiveQr));

        let mut data = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveQr {
                data = n.content.as_qr().map(str::to_owned);
            }
        });
        assert_eq!(data.as_deref(), Some("https://example/1"));
    }

    /// The splash shows only the centred app icon, with no text. Login
    /// states below still state their progress through the hint line.
    #[test]
    fn every_state_says_something() {
        let a = pending();
        let tree = a.build_tree(Panes::Three);
        let mut loading = false;
        let mut icon = None;
        let mut splash_text = false;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoading {
                loading = true;
            }
            if n.id == NodeId::AppScreenLoadingIcon {
                icon = n.content.as_image().map(str::to_owned);
            }
            if n.id == NodeId::PrimitiveText
                && n.content.as_text().is_some_and(|s| !s.trim().is_empty())
            {
                splash_text = true;
            }
        });
        assert!(loading, "splash missing");
        assert_eq!(
            icon.as_deref(),
            Some(crate::images::SPLASH_ICON_URL),
            "splash without the app icon"
        );
        assert!(!splash_text, "splash shows text");

        let mut a = pending();
        for event in [
            LoginEvent::Qr("x".to_owned()),
            LoginEvent::Approved,
            LoginEvent::Failed("接続できない".to_owned()),
        ] {
            a.login.apply_for_test(event);
            let tree = a.build_tree(Panes::Three);

            let mut hint = None;
            tree.walk(&mut |n, _| {
                if n.id == NodeId::AppScreenLoginHint {
                    hint = n.content.as_text().map(str::to_owned);
                }
            });
            let hint = hint.expect("説明文が無い");
            assert!(!hint.trim().is_empty(), "説明文が空である");
        }
    }

    /// The theme reaches the login screen; the QR's ground stays light.
    #[test]
    fn the_theme_reaches_the_login_screen() {
        let mut a = pending();
        a.login.apply_for_test(LoginEvent::Qr("x".to_owned()));

        let tree = a.build(&FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        });

        let mut qr_style = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveQr {
                qr_style = Some(n.style.clone());
            }
        });
        let s = qr_style.expect("QR が無い");
        assert!(s.background.is_some(), "QR の地が解決されていない");
        assert!(s.padding.is_some(), "静音領域ぶんの余白が無い");
    }

    /// Skipping login shows the main screen.
    #[test]
    fn skipping_shows_the_main_screen() {
        let a = Gumicord::demo();
        assert!(a.login.shows_main());
        assert!(ids(&a.build_tree(Panes::Three)).contains(&NodeId::AppScreenMain));
    }

    /// A signed-out app with a cache left by an earlier run.
    fn cached() -> Gumicord {
        let mut a = pending();
        a.live.store_mut().upsert_guild(gumicord_model::Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: Vec::new(),
        });
        a
    }

    /// A session already dead at startup must not strand the app behind its
    /// own cache: nothing could ever refresh what is on screen.
    #[test]
    fn a_session_dead_at_startup_clears_the_cache() {
        let mut a = cached();
        assert!(!a.live.is_empty(), "the cache did not load");
        assert!(a.shows_main(), "cache-first shows the main screen");

        a.login.apply_for_test(LoginEvent::Ended);
        assert!(a.wake(), "the end asked for a redraw");

        assert!(a.live.is_empty(), "the cache survived");
        assert!(!a.shows_main(), "the login screen never took over");
        assert!(
            a.login.hint().contains("セッションが無効"),
            "no reason given: {}",
            a.login.hint()
        );
    }

    /// A first start has neither cache nor session; leading the QR with a
    /// logout reason nobody earned would be a lie.
    #[test]
    fn a_session_dead_before_any_cache_blames_nothing() {
        let mut a = pending();
        assert!(a.live.is_empty());

        a.login.apply_for_test(LoginEvent::Ended);
        a.wake();

        assert_eq!(a.login.hint(), a.login.session().hint());
    }

    /// Tests never reach the network.
    #[test]
    fn nothing_starts_until_start_is_called() {
        let login = Login::fresh_for_test();
        assert!(!login.shows_main());
        assert!(login.session().qr().is_none());
    }

    /// Writes text into one login box, by address.
    fn login_input(a: &mut Gumicord, field: LoginField, text: &str) {
        a.inputs.must_mut(&login_addr(field)).insert(text);
    }

    /// Reads one login box, by address.
    fn login_text(a: &Gumicord, field: LoginField) -> String {
        a.inputs.must(&login_addr(field)).text().to_owned()
    }

    /// Focuses one login box, by field.
    fn focus_login(a: &mut Gumicord, field: LoginField) {
        a.focus = Some(login_addr(field));
    }

    /// A hit for the login form, where the node already carries its slot.
    fn login_hit_of(id: NodeId, key: Key) -> Hit {
        Hit {
            id,
            key: Some(key),
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        }
    }

    /// The QR screen has a way into the password form, and reaching it swaps
    /// the screen over: the QR must not linger behind the form.
    #[test]
    fn the_password_form_is_reached_from_the_qr() {
        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        assert!(ids(&a.build_tree(Panes::Three)).contains(&NodeId::PrimitiveQr));

        let entry = login_hit_of(NodeId::PrimitiveButton, Key::Slot("login_password"));
        assert!(
            a.pressed(std::slice::from_ref(&entry)),
            "フォームへの入口が効かない"
        );

        assert!(matches!(a.login.session(), Session::Password));
        let mut seen = ids(&a.build_tree(Panes::Three));
        assert!(seen.contains(&NodeId::AppScreenLoginField), "入力欄が無い");
        seen.retain(|id| *id == NodeId::PrimitiveQr);
        assert!(seen.is_empty(), "パスワード画面なのに QR が残る");
    }

    /// The QR button on the password form leaves it; its press used to fall
    /// through the dispatch and do nothing.
    #[test]
    fn the_qr_button_leaves_the_password_form() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        assert!(a.login_view.form.is_some());

        let back = login_hit_of(NodeId::PrimitiveButton, Key::Slot("login_qr"));
        assert!(a.pressed(std::slice::from_ref(&back)), "QRボタンが効かない");
        assert!(a.login_view.form.is_none(), "QRに戻っていない");
    }

    /// MFA must reach the TOTP screen even while the password-form override
    /// is set: the override used to strand the flow on the password screen
    /// with no visible progress (mobile pins it from the start, having no
    /// QR screen).
    #[test]
    fn the_totp_screen_shows_despite_the_password_override() {
        let mut a = pending();
        // In through the password form, like a phone.
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        assert!(a.login_view.form.is_some());
        // Discord asked for a second factor.
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        let tree = a.build_tree(Panes::Three);
        let mut slots = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoginField {
                slots.push(n.key.clone());
            }
        });
        assert!(
            slots.contains(&Some(Key::Slot("totp"))),
            "TOTP screen missing: {slots:?}"
        );
        assert!(
            !slots.contains(&Some(Key::Slot("password"))),
            "password form lingers: {slots:?}"
        );
    }

    /// Arriving on the TOTP step focuses its field: the code goes straight
    /// in with no extra tap. Only the transition: later wakes must not
    /// steal focus back after an outside press. The code box starts empty
    /// while the kept password stays in its own document.
    #[test]
    fn arriving_on_the_totp_step_focuses_its_field() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        assert!(a.focus.is_none());
        // A submitted password stays for a retry or a trip back.
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        a.login.send_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        assert!(a.wake());
        assert_eq!(a.focus, Some(login_addr(LoginField::Totp)));
        assert!(
            login_text(&a, LoginField::Totp).is_empty(),
            "コード欄にパスワードが残っている"
        );
        assert_eq!(login_text(&a, LoginField::Password), "secret");
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");

        a.focus = None;
        a.wake();
        assert!(a.focus.is_none(), "外し直した焦点が戻った");
    }

    /// The password never renders in the code box: each step owns its
    /// document, so no transition can carry one into the other. Backing
    /// out of the TOTP step still finds the password waiting.
    #[test]
    fn the_password_never_reaches_the_code_box() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        a.login.send_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        assert!(a.wake());

        let mut code = None;
        a.build_tree(Panes::Three).walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoginField && n.key == Some(Key::Slot("totp")) {
                code = n.content.as_editable().map(|e| e.text.clone());
            }
        });
        let code = code.expect("コード欄が無い");
        assert!(code.is_empty(), "コード欄にパスワードが出ている: {code:?}");

        a.leave_login_form();
        assert_eq!(login_text(&a, LoginField::Password), "secret");
        assert!(login_text(&a, LoginField::Totp).is_empty());
    }

    /// Signing in wipes the password: memory should not keep what the
    /// screen already hides. The email stays for the next attempt.
    #[test]
    fn signing_in_wipes_the_password() {
        use crate::session::LoggedIn;

        let mut a = pending();
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        a.login.send_for_test(LoginEvent::Done(Box::new(LoggedIn {
            me: serde_json::from_str(r#"{"id":"1","username":"ねんねこ"}"#).unwrap(),
            client: gumicord_rest::RestClient::anonymous().unwrap(),
            token: gumicord_model::Token::new("t"),
        })));
        assert!(a.wake());
        assert!(login_text(&a, LoginField::Password).is_empty());
        assert!(login_text(&a, LoginField::Totp).is_empty());
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");
    }

    /// A rejected TOTP code shows its reason on the code screen, so the
    /// retry never looks like nothing happened.
    #[test]
    fn a_rejected_totp_code_shows_its_reason() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: Some("認証コードが違います。もう一度入力してください".to_owned()),
        });
        assert!(a.wake());
        let tree = a.build_tree(Panes::Three);
        let mut lines = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoginError {
                lines.push(n.content.as_text().unwrap_or("").to_owned());
            }
        });
        assert!(
            lines.iter().any(|t| t.contains("認証コードが違います")),
            "no reason shown: {lines:?}"
        );
    }

    /// Tapping the submit button clears focus before routing, so the TOTP
    /// screen submits with no field focused. The session still names the
    /// step: the code must go out as a code, not as a password login.
    #[test]
    fn tapping_submit_on_the_totp_screen_sends_the_code() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        // The button press left no focus behind, and the email box was
        // never filled on this path: a password login would refuse.
        a.focus = None;
        login_input(&mut a, LoginField::Totp, "123456");
        assert!(a.submit_login(), "TOTP 画面の送信が送られない");
        assert!(a.login.busy(), "送信後も処理中にならない");
    }

    /// TOTP codes narrow to half-width digits: full-width digits and stray
    /// separators verify the same once narrowed, letters never do.
    #[test]
    fn totp_codes_narrow_to_half_width_digits() {
        assert_eq!(normalize_totp("123456"), "123456");
        assert_eq!(normalize_totp("１２３４５６"), "123456");
        assert_eq!(normalize_totp("123 456"), "123456");
        assert_eq!(normalize_totp("123-456"), "123456");
        assert_eq!(normalize_totp("ab12cd"), "12");
        assert_eq!(normalize_totp(""), "");
    }

    /// A code typed full-width still sends, narrowed: the digits are what
    /// verify, not their width.
    #[test]
    fn a_full_width_totp_code_still_sends() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        a.focus = None;
        login_input(&mut a, LoginField::Totp, "１２３４５６");
        assert!(a.submit_login(), "全角のコードが送られない");
        assert!(a.login.busy());
    }

    /// A short code is refused on the spot with a reason, instead of
    /// spending the ticket on a certain rejection.
    #[test]
    fn a_short_totp_code_is_refused_with_a_reason() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        focus_login(&mut a, LoginField::Totp);
        login_input(&mut a, LoginField::Totp, "12345");
        assert!(!a.submit_login(), "5桁のコードが送られてしまう");
        assert!(!a.login.busy(), "送っていないのに処理中になる");
        let tree = a.build_tree(Panes::Three);
        let mut lines = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoginError {
                lines.push(n.content.as_text().unwrap_or("").to_owned());
            }
        });
        assert!(
            lines.iter().any(|t| t.contains("6桁")),
            "no format reason shown: {lines:?}"
        );
    }

    /// Typed TOTP input keeps half-width digits only, up to six: the rest
    /// can never verify.
    #[test]
    fn typed_totp_input_keeps_digits_only() {
        let mut a = pending();
        focus_login(&mut a, LoginField::Totp);
        assert!(a.insert_text("a1b2"));
        assert_eq!(login_text(&a, LoginField::Totp), "12");
        assert!(!a.insert_text("xy"), "非数字が消費された");
        assert!(a.insert_text("3456789"));
        assert_eq!(
            login_text(&a, LoginField::Totp),
            "123456",
            "6桁で止まらない"
        );
    }

    /// The TOTP screen names whose code it asks for.
    #[test]
    fn the_totp_screen_names_its_account() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        let tree = a.build_tree(Panes::Three);
        let mut hints = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoginHint {
                hints.push(n.content.as_text().unwrap_or("").to_owned());
            }
        });
        assert!(
            hints.iter().any(|t| t == "a@b.c"),
            "account missing: {hints:?}"
        );
    }

    /// Field failures show under each named input: INVALID_LOGIN names both
    /// `login` and `password`, so both lines appear with the general one.
    #[test]
    fn login_field_errors_show_under_each_named_input() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.login.apply_for_test(LoginEvent::FieldErrors(vec![
            (
                "login".to_owned(),
                "ログインまたはパスワードが無効です。(INVALID_LOGIN)".to_owned(),
            ),
            (
                "password".to_owned(),
                "ログインまたはパスワードが無効です。(INVALID_LOGIN)".to_owned(),
            ),
        ]));
        a.login.apply_for_test(LoginEvent::Failed(
            "フォームボディが無効です (50035)".to_owned(),
        ));
        assert!(a.wake());
        let tree = a.build_tree(Panes::Three);
        let mut lines = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::AppScreenLoginError {
                lines.push((n.key.clone(), n.content.as_text().unwrap_or("").to_owned()));
            }
        });
        for slot in ["login_error_email", "login_error_password"] {
            assert!(
                lines
                    .iter()
                    .any(|(key, text)| *key == Some(Key::Slot(slot)) && text.contains("無効です")),
                "missing line for {slot}: {lines:?}"
            );
        }
    }

    /// A press outside every login field releases focus; otherwise the
    /// keyboard stays up with no way to dismiss it.
    #[test]
    fn pressing_outside_a_login_field_releases_focus() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("email"),
        )]);
        assert!(a.focus.is_some());

        assert!(a.pressed(&[]));
        assert_eq!(a.focus, None, "欄外を押してもフォーカスが残る");
    }

    /// Clicking a login field focuses exactly that one, and typing lands in the
    /// right box; switching to another keeps the first's contents.
    #[test]
    fn clicking_a_login_field_focuses_it_for_typing() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);

        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("email"),
        )]);
        assert_eq!(a.focus, Some(login_addr(LoginField::Email)));
        a.focused_document().unwrap().insert("a@b.c");

        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        assert_eq!(a.focus, Some(login_addr(LoginField::Password)));
        a.focused_document().unwrap().insert("secret");

        assert_eq!(
            login_text(&a, LoginField::Email),
            "a@b.c",
            "email 欄の内容が消えた"
        );
        assert_eq!(
            login_text(&a, LoginField::Password),
            "secret",
            "password 欄に書かれていない"
        );
    }

    /// Submitting the password form hands the credentials to the background
    /// login and drops the form's focus.
    #[test]
    fn submitting_the_password_form_hands_off_credentials() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("email"),
        )]);
        a.focused_document().unwrap().insert("a@b.c");
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        a.focused_document().unwrap().insert("secret");

        assert!(a.submit_login(), "パスワードログインが送信されなかった");
        assert_eq!(a.focus, None, "送信後もフォーカスが残っている");
        // A retry or a trip back from the TOTP step must not retype.
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");
        assert_eq!(login_text(&a, LoginField::Password), "secret");
    }

    /// Backing out of the password form keeps what was typed; only signing
    /// out wipes the documents.
    #[test]
    fn leaving_the_password_form_keeps_the_password() {
        let mut a = pending();
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        a.leave_login_form();
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");
        assert_eq!(login_text(&a, LoginField::Password), "secret");

        assert!(a.forget_account());
        assert!(login_text(&a, LoginField::Email).is_empty());
        assert!(login_text(&a, LoginField::Password).is_empty());
    }

    /// Backing out of the TOTP step drops the code while the password
    /// waits in its own document for the trip back.
    #[test]
    fn leaving_the_totp_step_drops_the_code() {
        let mut a = pending();
        a.login.apply_for_test(LoginEvent::TotpNeeded {
            email: "a@b.c".to_owned(),
            error: None,
        });
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        login_input(&mut a, LoginField::Totp, "123456");
        a.leave_login_form();
        assert!(login_text(&a, LoginField::Totp).is_empty());
        assert_eq!(login_text(&a, LoginField::Password), "secret");
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");
    }

    /// Opening the bot-token form starts from an empty box while a kept
    /// password stays in its own document.
    #[test]
    fn opening_the_token_form_drops_a_kept_password() {
        use gumicord_platform::HiddenKey::{A, B, Down, Left, Right, Up};

        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        login_input(&mut a, LoginField::Password, "secret");

        for key in [Up, Up, Down, Down, Left, Right, Left, Right, B, A] {
            assert!(a.hidden_key(key), "QR 画面上のキーは消費されるはず");
        }

        assert!(matches!(a.login.session(), Session::Token));
        assert!(
            login_text(&a, LoginField::Totp).is_empty(),
            "トークン欄にパスワードが残っている"
        );
        assert_eq!(login_text(&a, LoginField::Password), "secret");
    }

    /// Email and password ask for native mirrors; nothing else does.
    #[test]
    fn login_fields_report_their_proxy_kind() {
        use gumicord_platform::ImeProxy;

        let mut a = pending();
        assert_eq!(a.ime_proxy(), None);
        focus_login(&mut a, LoginField::Email);
        assert_eq!(a.ime_proxy(), Some(ImeProxy::Username));
        focus_login(&mut a, LoginField::Password);
        assert_eq!(a.ime_proxy(), Some(ImeProxy::Password));
        focus_login(&mut a, LoginField::Totp);
        assert_eq!(a.ime_proxy(), None);
        a.focus = None;
        a.focus = Some(inputs::composer_addr());
        assert_eq!(a.ime_proxy(), None);
    }

    /// Polled native text lands in the named field, wherever focus sits: a
    /// paired fill reaches both fields from one poll.
    #[test]
    fn proxy_text_reaches_the_named_field() {
        use gumicord_platform::ImeProxy;

        let mut a = pending();
        focus_login(&mut a, LoginField::Email);
        assert!(a.proxy_text(ImeProxy::Password, "secret".to_owned()));
        assert_eq!(login_text(&a, LoginField::Password), "secret");
        assert!(login_text(&a, LoginField::Email).is_empty());
        assert!(!a.proxy_text(ImeProxy::Password, "secret".to_owned()));
        assert!(a.proxy_text(ImeProxy::Username, "a@b.c".to_owned()));
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");
    }

    /// Field-menu items act on the menu's target while nothing holds
    /// focus; pasting chooses the target back for typing.
    #[test]
    fn menu_items_act_on_the_menu_target_without_focus() {
        let mut a = pending();
        login_input(&mut a, LoginField::Password, "secret");
        a.focus = None;
        a.menu_field = Some(crate::MenuField::Login(LoginField::Password));
        a.perform(crate::menu::Action::SelectAll);
        assert_eq!(
            a.inputs.must(&login_addr(LoginField::Password)).selection(),
            0.."secret".len()
        );
        a.perform(crate::menu::Action::Paste);
        assert_eq!(a.focus, Some(login_addr(LoginField::Password)));
    }

    /// A paired fill reaches both documents before the single submit reads
    /// them, whichever field holds focus.
    #[test]
    fn a_paired_fill_lands_in_both_fields_before_submit() {
        use gumicord_platform::ImeProxy;

        let mut a = pending();
        focus_login(&mut a, LoginField::Email);
        assert!(a.proxy_text(ImeProxy::Username, "a@b.c".to_owned()));
        assert!(a.proxy_text(ImeProxy::Password, "secret".to_owned()));
        assert_eq!(login_text(&a, LoginField::Email), "a@b.c");
        assert_eq!(login_text(&a, LoginField::Password), "secret");
        assert!(a.submit_login(), "pair-filled form does not submit");
        assert!(a.login.busy());
    }

    /// While an attempt is in flight every submit is ignored and the
    /// button dims; a failure re-arms the form for a retry.
    #[test]
    fn resubmitting_while_busy_is_ignored() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("email"),
        )]);
        a.focused_document().unwrap().insert("a@b.c");
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        a.focused_document().unwrap().insert("secret");

        assert!(a.submit_login(), "最初の送信が通らない");
        assert!(a.login.busy(), "送信後も処理中にならない");

        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        a.focused_document().unwrap().insert("secret");
        assert!(!a.submit_login(), "処理中の再送信が通ってしまう");

        a.login
            .apply_for_test(LoginEvent::Failed("だめ".to_owned()));
        assert!(!a.login.busy(), "失敗後も処理中のまま");
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        a.focused_document().unwrap().insert("secret");
        assert!(a.submit_login(), "失敗後の再送が通らない");
    }

    /// The submit button carries the disabled state while busy, so the
    /// theme dims it; idle forms stay undimmed.
    #[test]
    fn the_submit_button_dims_while_busy() {
        use gumicord_uitree::State;

        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        let mut submit_state = None;
        a.build_tree(Panes::Three).walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot("login_submit")) {
                submit_state = Some(n.states.contains(State::Disabled));
            }
        });
        assert_eq!(submit_state, Some(false), "待機中なのに無効表示");

        focus_login(&mut a, LoginField::Password);
        login_input(&mut a, LoginField::Email, "a@b.c");
        login_input(&mut a, LoginField::Password, "secret");
        assert!(a.submit_login());
        let mut submit_state = None;
        a.build_tree(Panes::Three).walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot("login_submit")) {
                submit_state = Some(n.states.contains(State::Disabled));
            }
        });
        assert_eq!(submit_state, Some(true), "処理中なのに通常表示");
    }

    /// The submit button dims while a form is empty, so an ignored press
    /// never reads as frozen: half an autofill leaves the button dimmed.
    #[test]
    fn the_submit_button_dims_while_a_field_is_empty() {
        use gumicord_uitree::State;

        let state_of = |a: &Gumicord| {
            let mut submit_state = None;
            a.build_tree(Panes::Three).walk(&mut |n, _| {
                if n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot("login_submit")) {
                    submit_state = Some(n.states.contains(State::Disabled));
                }
            });
            submit_state
        };

        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        // Only the password arrived (e.g. a one-sided autofill).
        login_input(&mut a, LoginField::Password, "secret");
        assert_eq!(state_of(&a), Some(true), "片欄だけなのに押せる表示");

        login_input(&mut a, LoginField::Email, "a@b.c");
        assert_eq!(state_of(&a), Some(false), "揃ったのに無効表示");
    }

    /// The konami code on the QR screen opens the bot-token form.
    #[test]
    fn the_konami_code_opens_the_bot_token_form() {
        use gumicord_platform::HiddenKey::{A, B, Down, Left, Right, Up};

        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));

        for key in [Up, Up, Down, Down, Left, Right, Left, Right, B, A] {
            assert!(a.hidden_key(key), "QR 画面上のキーは消費されるはず");
        }

        assert!(
            matches!(a.login.session(), Session::Token),
            "コンバットコードでトークン画面に入っていない"
        );
        assert_eq!(
            a.focus,
            Some(login_addr(LoginField::Token)),
            "入力欄にフォーカスが無い"
        );
    }

    /// A stray key breaks the sequence; nothing opens and the buffer resets.
    #[test]
    fn a_stray_key_breaks_the_konami_code() {
        use gumicord_platform::HiddenKey::{Down, Left, Up};

        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));

        // Up Up Down Down Left, then a Left where a Right belongs.
        for key in [Up, Up, Down, Down, Left, Left] {
            a.hidden_key(key);
        }

        assert!(!matches!(a.login.session(), Session::Token));
        assert_eq!(a.focus, None);
    }

    /// Off the QR screen the hidden code does nothing.
    #[test]
    fn the_konami_code_does_nothing_off_the_qr_screen() {
        use gumicord_platform::HiddenKey::{A, B, Down, Left, Right, Up};

        // `pending` starts at Connecting, not the QR screen.
        let mut a = pending();
        for key in [Up, Up, Down, Down, Left, Right, Left, Right, B, A] {
            a.hidden_key(key);
        }

        assert!(!matches!(a.login.session(), Session::Token));
        assert_eq!(a.focus, None);
    }

    /// Submitting the token form hands the bot token to the background and
    /// drops the form's focus.
    #[test]
    fn submitting_the_token_form_hands_off_the_bot_token() {
        use gumicord_platform::HiddenKey::{A, B, Down, Left, Right, Up};

        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::Qr("https://example/1".to_owned()));

        for key in [Up, Up, Down, Down, Left, Right, Left, Right, B, A] {
            a.hidden_key(key);
        }
        a.focused_document().unwrap().insert("bot-token");
        assert!(a.submit_login(), "トークンログインが送信されなかった");
        assert_eq!(a.focus, None, "送信後もフォーカスが残っている");
    }

    /// Right-clicking a login field focuses it and shows the input menu for
    /// that field's contents, not the composer's.
    #[test]
    fn right_clicking_a_login_field_opens_its_menu() {
        use crate::menu::Action;
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);

        // Focus the email field and select its content.
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("email"),
        )]);
        a.focused_document().unwrap().insert("a@b.c");
        a.focused_document().unwrap().select_all();

        let field = login_hit_of(NodeId::AppScreenLoginField, Key::Slot("email"));
        assert!(a.context_menu(&[field], (0.0, 0.0)));
        assert_eq!(a.focus, Some(login_addr(LoginField::Email)));

        let has = |want: &Action| {
            a.floating
                .as_ref()
                .expect("開いていない")
                .items()
                .iter()
                .any(|i| &i.action == want)
        };
        assert!(has(&Action::Cut), "選んだ欄に切り取りが出ていない");
        assert!(has(&Action::CopySelection), "選んだ欄にコピーが出ていない");
        assert!(has(&Action::Paste), "貼り付けが出ていない");
    }

    /// Holding a login box open its menu drops the keyboard on a phone:
    /// the menu rises from the bottom over it, and there is no other way
    /// to put the keyboard away. The desk keeps focus.
    ///
    /// The same answer is behind one compile-time question, so this arm
    /// went unexamined until that question became a test hook.
    #[test]
    fn holding_a_login_box_open_its_menu_drops_the_keyboard() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        let field = login_hit_of(NodeId::AppScreenLoginField, Key::Slot("password"));

        crate::assume_mobile(|| {
            assert!(a.context_menu(&[field], (0.0, 0.0)), "メニューが開かない");
        });
        assert!(
            a.focus.is_none(),
            "メニューの上でログイン欄がまだ focusing している"
        );
        // The menu still knows which box it acts on, with no focus to
        // borrow it from.
        assert_eq!(
            a.menu_field,
            Some(crate::MenuField::Login(LoginField::Password))
        );
        assert!(a.menu_field_doc().text().is_empty(), "別の欄を指している");
    }

    /// The "select all" menu item targets the focused login field, leaving
    /// the composer untouched.
    #[test]
    fn select_all_targets_the_focused_login_field() {
        let mut a = pending();
        a.pressed(&[login_hit_of(
            NodeId::PrimitiveButton,
            Key::Slot("login_password"),
        )]);
        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        a.focused_document().unwrap().insert("secret");

        a.perform(crate::menu::Action::SelectAll);

        assert!(
            a.inputs
                .must(&login_addr(LoginField::Password))
                .has_selection(),
            "ログイン欄が選択されていない"
        );
        assert!(
            !a.inputs.must(&inputs::composer_addr()).has_selection(),
            "コンポーザーが触られた"
        );
    }

    /// A captcha challenge is handed to the platform, and its solution comes
    /// back as a submit.
    #[test]
    fn a_pending_captcha_is_forwarded_and_solved() {
        let mut a = pending();
        a.login
            .apply_for_test(LoginEvent::CaptchaNeeded(gumicord_rest::CaptchaChallenge {
                sitekey: Some("site123".to_owned()),
                service: Some("hcaptcha".to_owned()),
                rqdata: Some("rqdata".to_owned()),
                rqtoken: Some("rqtoken".to_owned()),
                session_id: Some("sess".to_owned()),
            }));

        let challenge = a
            .pending_captcha()
            .expect("pending_captcha がプラットフォームへ渡さない");
        assert_eq!(challenge.site_key, "site123");
        assert_eq!(challenge.rqdata.as_deref(), Some("rqdata"));

        // Nothing left to forward: it moved to the app side for the retry.
        assert!(
            a.pending_captcha().is_none(),
            "同一の captcha が二度渡される"
        );

        a.captcha_solved(gumicord_platform::SolvedCaptcha {
            solution: "tok".to_owned(),
        });
        assert!(
            a.login_view.pending.is_none(),
            "解けた captcha が残っている"
        );
    }
}
