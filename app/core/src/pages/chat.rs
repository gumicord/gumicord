//! The main chat screen: lists, messages, composer and navigation.
//!
//! Owns its state outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_model::ChannelId;
use gumicord_platform::TextDocument;

/// What the composer is doing.
///
/// One field serves all three: new, reply and edit are all "type and press
/// enter", and separate fields would mean retyping after realising it was a
/// reply. Which one is active must be visible — sending a new message while
/// meaning to edit cannot be undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Composing {
    /// Writing something new.
    #[default]
    New,
    /// Replying to a message.
    Reply(u64),
    /// Editing a message.
    Edit(u64),
}

impl Composing {
    pub(crate) fn target(self) -> Option<u64> {
        match self {
            Composing::New => None,
            Composing::Reply(id) | Composing::Edit(id) => Some(id),
        }
    }
}

/// Main-screen state. Everything here is used by chat code only.
pub(crate) struct ChatView {
    pub(crate) selected_guild: u64,
    /// Which spoilers stand open, whole messages or single runs.
    pub(crate) reveals: crate::markdown::Reveals,
    /// What the composer is doing.
    pub(crate) composing: Composing,
    pub(crate) selected_channel: u64,
    /// Whether the composer has focus.
    pub(crate) input_focused: bool,
    /// The composer's contents.
    pub(crate) input: TextDocument,
    /// A jump waiting for the next frame: the renderer knows where the
    /// message landed last frame, this layer only knows it was pressed.
    pub(crate) pending_reveal: Option<u64>,
    /// A jump waiting for its messages: fetched around the target when it
    /// is not loaded. Dropped when the channel moves on.
    pub(crate) pending_jump: Option<(ChannelId, u64)>,
    /// The last pressed message, for the screen reader to follow.
    pub(crate) a11y_message: Option<u64>,
    /// The navigation drawer, for widths that hide the lists.
    pub(crate) drawer_open: bool,
    /// The member list as a bottom sheet, for widths that hide it.
    pub(crate) member_sheet_open: bool,
}

impl ChatView {
    pub(crate) fn new(guild: u64, channel: u64) -> Self {
        ChatView {
            selected_guild: guild,
            reveals: crate::markdown::Reveals::default(),
            composing: Composing::New,
            selected_channel: channel,
            input_focused: false,
            input: TextDocument::new(),
            pending_reveal: None,
            pending_jump: None,
            a11y_message: None,
            drawer_open: false,
            member_sheet_open: false,
        }
    }
}

impl crate::Gumicord {
    /// Opens the navigation drawer. Only where the lists hide and past
    /// login; elsewhere there is nothing to drawer over.
    pub(crate) fn open_drawer(&mut self) -> bool {
        if self.chat.drawer_open || self.panes().guilds() || !self.shows_main() {
            return false;
        }
        self.chat.drawer_open = true;
        self.chat.member_sheet_open = false;
        true
    }

    pub(crate) fn close_drawer(&mut self) -> bool {
        std::mem::replace(&mut self.chat.drawer_open, false)
    }

    /// Opens the member list as a bottom sheet. Only where the member
    /// pane hides and past login.
    pub(crate) fn open_member_sheet(&mut self) -> bool {
        if self.chat.member_sheet_open || self.panes().members() || !self.shows_main() {
            return false;
        }
        self.chat.member_sheet_open = true;
        self.chat.drawer_open = false;
        true
    }

    pub(crate) fn close_member_sheet(&mut self) -> bool {
        std::mem::replace(&mut self.chat.member_sheet_open, false)
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
