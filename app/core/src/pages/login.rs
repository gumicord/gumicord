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

    /// The main screen is not even built while signed out; visible but
    /// untouchable is the worst state.
    #[test]
    fn the_main_screen_is_not_built_before_login() {
        let a = pending();
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

    /// Progress is always stated, so nothing looks silently stuck.
    #[test]
    fn every_state_says_something() {
        let mut a = pending();
        for event in [
            None,
            Some(LoginEvent::Qr("x".to_owned())),
            Some(LoginEvent::Approved),
            Some(LoginEvent::Failed("接続できない".to_owned())),
        ] {
            if let Some(e) = event {
                a.login.apply_for_test(e);
            }
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
        assert!(a.login_view.field.is_some());

        assert!(a.pressed(&[]));
        assert_eq!(a.login_view.field, None, "欄外を押してもフォーカスが残る");
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
        assert!(matches!(a.login_view.field, Some(LoginField::Email)));
        a.focused_document().unwrap().insert("a@b.c");

        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        assert!(matches!(a.login_view.field, Some(LoginField::Password)));
        a.focused_document().unwrap().insert("secret");

        assert_eq!(a.login_view.email.text(), "a@b.c", "email 欄の内容が消えた");
        assert_eq!(
            a.login_view.input.text(),
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
        assert_eq!(a.login_view.field, None, "送信後もフォーカスが残っている");
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
            a.login_view.field,
            Some(LoginField::Token),
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
        assert_eq!(a.login_view.field, None);
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
        assert_eq!(a.login_view.field, None);
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
        assert_eq!(a.login_view.field, None, "送信後もフォーカスが残っている");
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
        assert!(matches!(a.login_view.field, Some(LoginField::Email)));

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
            a.login_view.input.has_selection(),
            "ログイン欄が選択されていない"
        );
        assert!(!a.chat.input.has_selection(), "コンポーザーが触られた");
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
        assert!(a.pending.is_none(), "解けた captcha が残っている");
    }
}
