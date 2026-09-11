//! The main chat screen: lists, messages, composer and navigation.
//!
//! Owns its state outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_model::ChannelId;
use gumicord_platform::TextDocument;
use gumicord_uitree::{Key, NodeId, State, UiNode};
use gumicord_uitree::value::Color;

use super::login::LoginField;

// ═══════════════════════════════════════════════════════════════════════
//  Display rows
//
//  Demo and live data meet here, so the tree builder never has to ask which
//  one it is holding.
// ═══════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone)]
pub(crate) struct GuildRow {
    pub(crate) id: u64,
    pub(crate) name: String,
    /// Icon URL; the initials stand in when absent.
    pub(crate) icon: Option<String>,
    pub(crate) unread: bool,
    pub(crate) mentions: u32,
    /// The folder id, when this row is a folder header.
    pub(crate) folder_of_own: Option<u64>,
    /// Whether it sits inside a folder; the indent is the theme's.
    pub(crate) in_folder: bool,
    /// Whether the folder is folded.
    pub(crate) collapsed: bool,
    /// The folder's colour; where it lands is the theme's call.
    pub(crate) tint: Option<u32>,
    /// What the folder holds: children when open, tiles when folded.
    pub(crate) members: Vec<GuildRow>,
}

#[derive(Debug, Clone)]
pub(crate) struct ChannelRow {
    pub(crate) id: u64,
    pub(crate) name: String,
    pub(crate) icon: &'static str,
    pub(crate) topic: Option<String>,
    pub(crate) unread: bool,
    pub(crate) mentions: u32,
    /// A category heading; nothing opens.
    pub(crate) category: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct MessageRow {
    pub(crate) id: u64,
    pub(crate) author: String,
    /// Avatar URL; the initials stand in when absent.
    pub(crate) avatar: Option<String>,
    /// The role colour; where it lands is the theme's call.
    pub(crate) tint: Option<u32>,
    pub(crate) time: String,
    /// Local day label, also the grouping key: equal strings share a day.
    pub(crate) day: String,
    /// Whole seconds, to tell a live run from yesterday's tail.
    pub(crate) unix: i64,
    /// The parsed body. The raw string is deliberately absent: holding both
    /// invites drawing from the wrong one, and only the reader would notice.
    pub(crate) blocks: Vec<gumicord_markdown::Block>,
    pub(crate) mentioned: bool,
    /// The answered message, if this replies to one (FR-028). Display only:
    /// pressing it to jump there is a later piece.
    pub(crate) reply: Option<ReplyRef>,
}

/// Who and what a reply answers: one line, like Discord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplyRef {
    pub(crate) author: String,
    pub(crate) snippet: String,
    /// Small avatar URL; everyone has one, default included.
    pub(crate) avatar: Option<String>,
    /// The answered message, to jump to on press.
    pub(crate) target: u64,
}

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
    /// The guild list.
    pub(crate) fn guild_list(&self) -> UiNode {
        let mut list = UiNode::new(NodeId::NavGuildList).child(
            UiNode::text(NodeId::NavGuildListHome, "DM").with_state_if(
                self.is_hovered(NodeId::NavGuildListHome, None),
                State::Hover,
            ),
        );

        for g in self.guild_rows() {
            // The folder header; pressing it folds.
            if g.folder_of_own.is_some() {
                list = list.child(self.folder_face(&g));
                continue;
            }
            // Contents belong to the folder; emitting them as siblings too
            // would duplicate them.
            if g.in_folder {
                continue;
            }

            list = list.child(self.guild_item(&g));
        }
        list.children(self.scrollbar(NodeId::NavGuildList))
    }

    /// One guild, identical inside and outside a folder.
    pub(crate) fn guild_item(&self, g: &GuildRow) -> UiNode {
        let selected = g.id == self.chat.selected_guild;
        let hovered = self.hovered_id(NodeId::NavGuildListItem, g.id);

        // The container is wider than the icon, leaving a lane at the left
        // for the pill.
        let icon = crate::face(NodeId::NavGuildListItemIcon, g.icon.as_deref(), &g.name)
            .with_data(g.id)
            .with_state_if(selected, State::Selected)
            .with_state_if(hovered, State::Hover)
            .with_state_if(g.in_folder, State::Grouped);

        UiNode::new(NodeId::NavGuildListItem)
            .with_id_key(g.id)
            .with_data(g.id)
            .with_state_if(selected, State::Selected)
            .with_state_if(g.unread, State::Unread)
            .with_state_if(g.mentions > 0, State::Mentioned)
            // Carried as state, not a spacer node: a spacer bakes in the
            // indent and takes it away from the theme.
            .with_state_if(g.in_folder, State::Grouped)
            .with_state_if(hovered, State::Hover)
            .children(self.guild_pill(g, selected, hovered))
            .child(icon)
            // Counts only; the pill already says there is something unread.
            .children((g.mentions > 0).then(|| {
                UiNode::text(NodeId::NavGuildListItemBadge, g.mentions.to_string()).with_data(g.id)
            }))
    }

    /// The pill at a guild's left edge.
    ///
    /// ```text
    ///   ▍◯   selected   tall
    ///   ▪◯   unread     a dot
    ///   ▎◯   hovered    in between
    ///    ◯   otherwise  absent
    /// ```
    ///
    /// Absent rather than zero-height when it would say nothing, so a visible
    /// pill always means something. The size is the theme's; this only says
    /// why it is there.
    pub(crate) fn guild_pill(&self, g: &GuildRow, selected: bool, hovered: bool) -> Option<UiNode> {
        if !selected && !hovered && !g.unread {
            return None;
        }
        Some(
            UiNode::new(NodeId::NavGuildListItemPill)
                .with_data(g.id)
                .with_state_if(selected, State::Selected)
                .with_state_if(g.unread, State::Unread)
                .with_state_if(hovered, State::Hover),
        )
    }

    /// One folder. Open, it wraps its contents.
    ///
    /// ```text
    ///   folded          open
    ///   ┌───────┐      ┌───────┐   one background
    ///   │ ▢ ▢ │      │   ▱   │   behind both
    ///   │ ▢ ▢ │      │  ▢   │
    ///   └───────┘      │  ▢   │
    ///                     └───────┘
    /// ```
    ///
    /// Folded, it tiles the icons inside, so what was folded away is visible
    /// without unfolding.
    ///
    /// No tiles while open, or the same icons appear twice. Contents stay
    /// children, or the background stops covering them and the folder's extent
    /// becomes invisible.
    pub(crate) fn folder_face(&self, row: &GuildRow) -> UiNode {
        let id = row.folder_of_own.unwrap_or(row.id);
        // Only carried; where it lands is the theme's call.
        let tint = row.tint.map(Color::from_rgb);
        let node = UiNode::new(NodeId::NavGuildListFolder)
            .with_id_key(id)
            .with_tint_opt(tint)
            .with_state_if(row.collapsed, State::Collapsed)
            .with_state_if(
                self.hovered_id(NodeId::NavGuildListFolder, id),
                State::Hover,
            );

        if !row.collapsed {
            return node
                .child(UiNode::icon(NodeId::NavGuildListFolderIcon, "folder").with_tint_opt(tint))
                .children(row.members.iter().map(|m| self.guild_item(m)));
        }

        // Rows and columns; there is no grid primitive.
        let mut grid = UiNode::new(NodeId::LayoutColumn);
        for pair in row.members.chunks(2).take(crate::FOLDER_TILES / 2) {
            let mut line = UiNode::new(NodeId::LayoutRow);
            for m in pair {
                line = line.child(
                    crate::face(NodeId::NavGuildListItemIcon, m.icon.as_deref(), &m.name)
                        .with_id_key(m.id)
                        // `collapsed`, not `grouped`.
                        //
                        // `grouped` means a guild inside an open folder,
                        // drawn at normal size; this is a tile on a folded
                        // one. Sharing a state would break one while fixing
                        // the other. The size is the theme's.
                        .with_state(State::Collapsed),
                );
            }
            grid = grid.child(line);
        }
        node.child(grid)
    }
}

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
