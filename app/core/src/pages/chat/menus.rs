//! Chat menus: field, message, channel, guild and user menus.
use super::super::login::LoginField;
use gumicord_model::ChannelId;
use gumicord_platform::TextDocument;

impl crate::Gumicord {
    /// The document a field menu and its items act on: the focused login field
    /// if any, else the composer. Read-only; [`focused_document`] is the
    /// mutable twin used to actually edit it.
    ///
    /// [`focused_document`]: crate::Application::focused_document
    pub(crate) fn field_doc(&self) -> &TextDocument {
        match self.login_view.field {
            Some(LoginField::Email) => &self.login_view.email,
            Some(LoginField::Password | LoginField::Totp | LoginField::Token) => {
                &self.login_view.input
            }
            None => &self.chat.input,
        }
    }

    /// A field's menu, desktop only. Lists only what would do something.
    pub(crate) fn field_menu(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let doc = self.field_doc();
        let mut items = Vec::new();

        if !doc.selection().is_empty() {
            items.push(Item::new(Action::Cut, "切り取り").icon("cut"));
            items.push(Item::new(Action::CopySelection, "コピー").icon("copy"));
        }
        // Not read: opening the clipboard takes it from other programs, which
        // is not something to do every time a menu opens.
        items.push(Item::new(Action::Paste, "貼り付け").icon("paste"));

        if !doc.is_empty() {
            items.push(Item::new(Action::SelectAll, "すべて選択").icon("select_all"));
        }
        items
    }
}
impl crate::Gumicord {
    /// A message's menu. Only what can actually be done: a greyed row adds to
    /// the search for a usable one.
    pub(crate) fn message_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let mut items = Vec::new();

        items.push(Item::new(Action::Reply(id), "返信").icon("reply"));

        // Only our own: the server would return 403 anyway, but not offering
        // it comes first.
        if self.is_mine(id) {
            items.push(Item::new(Action::Edit(id), "編集").icon("edit"));
        }

        // The raw body, since `**bold**` is what the author actually typed.
        if let Some(text) = self.raw_body(id) {
            items.push(Item::new(Action::Copy(text), "本文をコピー").icon("copy"));
        }
        items.push(Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id"));

        if self.is_mine(id) {
            items.push(Item::new(Action::Delete(id), "削除").icon("trash").danger());
        }
        items
    }

    /// Whether a message is ours. False when signed out: turning "unknown"
    /// into "ours" would offer edit and delete on other people's messages.
    pub(crate) fn is_mine(&self, id: u64) -> bool {
        let Some(me) = self.login.session().logged_in().map(|l| l.me.user.id) else {
            return false;
        };
        self.live
            .store()
            .messages(ChannelId::from(self.chat.selected_channel))
            .iter()
            .any(|m| m.id.get() == id && m.author.id == me)
    }

    /// A message's body as typed.
    ///
    /// Not taken from the built nodes: those hold parsed text, where `<@123>`
    /// has already become a display name.
    pub(crate) fn raw_body(&self, id: u64) -> Option<String> {
        if !self.uses_live() {
            return None;
        }
        self.live
            .store()
            .messages(ChannelId::from(self.chat.selected_channel))
            .iter()
            .find(|m| m.id.get() == id)
            .map(|m| m.content.clone())
    }

    pub(crate) fn channel_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let mut items = Vec::new();
        if self.live.store().is_unread(ChannelId::from(id)) {
            items.push(Item::new(Action::MarkRead(id), "既読にする").icon("check"));
        }
        items.push(Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id"));
        items
    }

    pub(crate) fn guild_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        vec![Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id")]
    }

    /// The menu on our own panel. Only offered while actually signed in.
    pub(crate) fn user_menu(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let Some(l) = self.login.session().logged_in() else {
            return Vec::new();
        };
        let mut items =
            vec![Item::new(Action::Copy(l.me.user.id.to_string()), "ID をコピー").icon("id")];

        if let Ok(store) = gumicord_platform::SecretStore::new()
            && let Ok(index) = crate::account::AccountsIndex::load(&store)
        {
            let current_key = crate::account::AccountKey::new(l.me.user.id, l.token.is_bot());
            for acc in &index.accounts {
                let is_current = acc.key == current_key;
                let id_str = acc.key.id.to_string();
                let suffix = &id_str[id_str.len().saturating_sub(4)..];
                let label = format!("{} (…{})", acc.display_name, suffix);
                let mut item = Item::new(Action::SwitchAccount(acc.key), label);
                if is_current {
                    item = item.icon("check").selected(true);
                }
                items.push(item);
            }
        }

        items.push(Item::new(Action::AddAccount, "アカウントを追加"));
        items.push(
            Item::new(Action::LogOut, "ログアウト")
                .icon("logout")
                .danger(),
        );
        items
    }
}
