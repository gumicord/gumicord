//! The main chat screen: lists, messages, composer and navigation.
//!
//! Owns its state outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_model::ChannelId;
use gumicord_platform::TextDocument;

pub mod composer;
pub mod lists;
pub mod menus;
pub mod rows;

#[cfg(test)]
pub(crate) mod tests;

// ═══════════════════════════════════════════════════════════════════════
//  Display rows
//
//  Live data meets the tree builder here.
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
