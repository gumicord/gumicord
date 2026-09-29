//! The main chat screen: lists, messages, composer and navigation.
//!
//! Owns its state outright; the shell only routes presses here and reads
//! the built subtree.

use gumicord_model::ChannelId;

use super::overlays::{SheetCloser, SlideState};
use crate::inputs::composer_addr;

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
    /// Whether a reply mentions its target. Discord's default is on.
    pub(crate) reply_mention: bool,
    pub(crate) selected_channel: u64,
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
    /// The drawer's slide progress, 0 (shut) to 1 (open). The flag above
    /// owns the tree; this owns where it stands while opening, closing,
    /// or following the finger. Same channel physics as the bottom sheet.
    pub(crate) drawer: SlideState,
}

impl ChatView {
    pub(crate) fn new(guild: u64, channel: u64) -> Self {
        ChatView {
            selected_guild: guild,
            reveals: crate::markdown::Reveals::default(),
            composing: Composing::New,
            reply_mention: true,
            selected_channel: channel,
            pending_reveal: None,
            pending_jump: None,
            a11y_message: None,
            drawer_open: false,
            member_sheet_open: false,
            drawer: SlideState::new(),
        }
    }
}

impl crate::Gumicord {
    /// Opens the navigation drawer. Only where the lists hide and past
    /// login; elsewhere there is nothing to drawer over. Slides in from
    /// the edge rather than appearing.
    pub(crate) fn open_drawer(&mut self) -> bool {
        if self.chat.drawer_open {
            // Reopening mid-close: swing back instead of refusing.
            if self.chat.drawer.target == 0.0 {
                self.chat.drawer.retarget(1.0);
                return true;
            }
            return false;
        }
        if self.panes().guilds() || !self.shows_main() {
            return false;
        }
        // The drawer holds no text fields; opening it over the keyboard
        // strands both.
        self.release_text_focus();
        self.chat.drawer_open = true;
        self.chat.member_sheet_open = false;
        self.chat.drawer.rise();
        true
    }

    pub(crate) fn close_drawer(&mut self) -> bool {
        if !self.chat.drawer_open {
            return false;
        }
        // Slide out; the flag flips when the slide lands (see build()),
        // so the drawer coasts instead of vanishing.
        self.chat.drawer.retarget(0.0);
        true
    }

    /// Advances the drawer slide towards its target. Returns whether it is
    /// still moving. A landed closing slide flips the flag, which is what
    /// finally removes the drawer from the tree.
    pub(crate) fn advance_drawer(&mut self, now: std::time::Instant) -> bool {
        if !self.chat.drawer_open {
            return false;
        }
        let moving = self.chat.drawer.advance(now);
        if !moving
            && !self.chat.drawer.drag
            && self.chat.drawer.slide == 0.0
            && self.chat.drawer.target == 0.0
        {
            self.chat.drawer_open = false;
        }
        moving
    }

    /// Whether a touch at x could start a drawer drag: narrow, closed,
    /// and past login. Side-effect free; the drag itself starts below.
    pub(crate) fn drawer_drag_maybe(&self, x: f32) -> bool {
        !self.chat.drawer_open
            && x <= crate::DRAWER_EDGE
            && !self.panes().guilds()
            && self.shows_main()
    }

    /// Starts a finger-driven drawer open. The drawer enters the tree shut
    /// and follows the finger from there.
    pub(crate) fn drawer_drag_start(&mut self) -> bool {
        if self.chat.drawer_open || self.panes().guilds() || !self.shows_main() {
            return false;
        }
        self.release_text_focus();
        self.chat.drawer_open = true;
        self.chat.member_sheet_open = false;
        self.chat.drawer.rise();
        self.chat.drawer.drag_start();
        true
    }

    /// Starts a finger-driven drawer close from inside the open drawer.
    /// The slide holds where it stands and follows the finger from there.
    pub(crate) fn drawer_close_drag_start(&mut self) -> bool {
        if !self.chat.drawer_open || self.panes().guilds() || !self.shows_main() {
            return false;
        }
        self.chat.drawer.drag_start();
        true
    }

    /// Whether a touch inside the open drawer could start a close drag:
    /// narrow, open, and past login. Side-effect free.
    pub(crate) fn drawer_close_drag_maybe(&self) -> bool {
        self.chat.drawer_open && !self.panes().guilds() && self.shows_main()
    }

    /// Follows the finger: progress is the drag distance from the start
    /// over the drawer width, rising from where the drag began. A stale
    /// width still opens; only the mapping stretches.
    pub(crate) fn drawer_drag_move(&mut self, dx: f32, width: f32) -> bool {
        // Right-positive motion opens the drawer: opening-positive.
        self.chat.drawer.drag_move(dx, width)
    }

    /// Lets go: a fast flick right finishes opening, a fast flick left
    /// falls back, and a slow release follows whichever half it is on.
    pub(crate) fn drawer_drag_end(&mut self, velocity: f32) -> bool {
        // The drawer opens to the right; it has no closer to land.
        self.chat.drawer.drag_end(velocity, false, None)
    }

    /// A message swiped left starts a reply, like the menu does.
    pub(crate) fn start_reply(&mut self, id: u64) {
        self.chat.composing = Composing::Reply(id);
        self.focus = Some(composer_addr());
        self.chat.a11y_message = Some(id);
    }

    /// Drives one message row left with the finger. Gestures for anything
    /// but the chat never reach here; overlays own their touches while
    /// open. Only the matching row moves; anything else is ignored.
    pub(crate) fn message_swipe_move(&mut self, id: u64, dx: f32) -> bool {
        if self.chat.drawer_open
            || self.chat.member_sheet_open
            || self.floating.is_some()
            || self.settings.open
        {
            return false;
        }
        let dx = dx.clamp(SWIPE_REPLY_CLAMP, 0.0);
        match &mut self.message_swipe {
            // A fresh touch mid-return takes over where the row stands.
            Some(swipe) if swipe.id == id => {
                if (swipe.dx - dx).abs() < 0.5 {
                    return false;
                }
                swipe.dx = dx;
                swipe.from = dx;
                swipe.start = None;
                true
            }
            Some(_) => false,
            None => {
                self.message_swipe = Some(MessageSwipe::driving(id, dx));
                true
            }
        }
    }

    /// The finger lifted off a driven row: past the threshold starts a
    /// reply, otherwise the row springs back.
    pub(crate) fn message_swipe_end(&mut self, id: u64, now: std::time::Instant) -> bool {
        let reply = match &self.message_swipe {
            Some(swipe) if swipe.id == id => swipe.dx <= SWIPE_REPLY_THRESHOLD,
            _ => return false,
        };
        if reply {
            self.start_reply(id);
        }
        if let Some(swipe) = self.message_swipe.as_mut().filter(|s| s.id == id) {
            swipe.release(now);
        }
        true
    }

    /// Advances a springing-back row. True while frames must keep coming.
    pub(crate) fn poll_message_swipe(&mut self, now: std::time::Instant) -> bool {
        let Some(swipe) = self.message_swipe.as_mut() else {
            return false;
        };
        if !swipe.returning() {
            return false;
        }
        if swipe.advance(now) {
            return true;
        }
        self.message_swipe = None;
        true
    }

    /// The row currently offset, if any. Pushed to the renderer every frame.
    pub(crate) fn message_swipe_offset(&self) -> Option<(u64, f32)> {
        self.message_swipe.as_ref().map(|s| (s.id, s.dx))
    }

    /// Opens the member list as a bottom sheet. Only where the member
    /// pane hides and past login. Rises from the bottom rather than
    /// appearing.
    pub(crate) fn open_member_sheet(&mut self) -> bool {
        if self.chat.member_sheet_open || self.panes().members() || !self.shows_main() {
            return false;
        }
        // The sheet holds no text fields; opening it over the keyboard
        // strands both.
        self.release_text_focus();
        self.chat.member_sheet_open = true;
        self.chat.drawer_open = false;
        self.sheet.rise();
        true
    }

    /// Closes the member sheet. Slides out; the flag flips when the slide
    /// lands, so the sheet coasts instead of vanishing.
    pub(crate) fn close_member_sheet(&mut self) -> bool {
        if !self.chat.member_sheet_open {
            return false;
        }
        self.sheet.start_close(SheetCloser::Member);
        true
    }

    /// Advances the sheet slide towards its target. Returns whether it is
    /// still moving. A landed closing slide clears whichever surface was
    /// closing, which finally removes the sheet from the tree.
    pub(crate) fn advance_sheet(&mut self, now: std::time::Instant) -> bool {
        let moving = self.sheet.advance(now);
        if moving {
            return true;
        }
        let Some(closer) = self.sheet.take_closer() else {
            return false;
        };
        match closer {
            SheetCloser::Member => {
                self.chat.member_sheet_open = false;
            }
            SheetCloser::Menu => {
                if matches!(self.floating, Some(crate::menu::Floating::Menu(_))) {
                    self.floating = None;
                }
            }
        }
        false
    }

    /// Whether a touch on the sheet handle could start a drag: a sheet
    /// (member or menu) is open and narrow. Side-effect free.
    pub(crate) fn sheet_drag_maybe(&self) -> bool {
        use crate::menu::Floating;

        (self.chat.member_sheet_open
            || matches!(self.floating, Some(Floating::Menu(_)))
                && self.panes().present() == crate::menu::Present::Sheet)
            && self.shows_main()
    }

    /// Starts a finger-driven sheet close from the handle. The slide holds
    /// where it stands and follows the finger from there.
    pub(crate) fn sheet_drag_start(&mut self) -> bool {
        if !self.sheet_drag_maybe() {
            return false;
        }
        self.sheet.drag_start();
        true
    }

    /// Follows the finger: progress is the drag distance from the start
    /// over the sheet height, falling from where the drag began. Down is
    /// positive on screen and closes the sheet.
    pub(crate) fn sheet_drag_move(&mut self, dy: f32, height: f32) -> bool {
        // Down-positive screen motion shuts the sheet: opening-negative.
        self.sheet.drag_move(-dy, height)
    }

    /// Lets go: a fast flick down finishes closing, a fast flick up swings
    /// back open, and a slow release follows whichever half it is on.
    pub(crate) fn sheet_drag_end(&mut self, velocity: f32) -> bool {
        let fallback = if self.chat.member_sheet_open {
            SheetCloser::Member
        } else {
            SheetCloser::Menu
        };
        self.sheet.drag_end(velocity, true, Some(fallback))
    }
}

/// How far left a message follows the finger before stopping.
pub(crate) const SWIPE_REPLY_CLAMP: f32 = -96.0;

/// Release past this starts a reply instead of springing back.
pub(crate) const SWIPE_REPLY_THRESHOLD: f32 = -64.0;

/// Spring-back duration, in milliseconds.
pub(crate) const SWIPE_RETURN_MS: f32 = 180.0;

/// One message row driven by a finger: it follows left, then either
/// starts a reply or springs back. Single-finger only, like the rest
/// of the touch layer.
#[derive(Debug, Clone, Copy)]
pub(crate) struct MessageSwipe {
    id: u64,
    dx: f32,
    from: f32,
    start: Option<std::time::Instant>,
}

impl MessageSwipe {
    fn driving(id: u64, dx: f32) -> Self {
        MessageSwipe {
            id,
            dx,
            from: dx,
            start: None,
        }
    }

    fn release(&mut self, now: std::time::Instant) {
        self.from = self.dx;
        self.start = Some(now);
    }

    fn returning(&self) -> bool {
        self.start.is_some()
    }

    /// Advances a return towards the anchor. Returns false once arrived.
    fn advance(&mut self, now: std::time::Instant) -> bool {
        let Some(start) = self.start else {
            return false;
        };
        let t = (now.saturating_duration_since(start).as_secs_f32() * 1000.0 / SWIPE_RETURN_MS)
            .clamp(0.0, 1.0);
        self.dx = self.from * (1.0 - super::overlays::ease_out_cubic(t));
        t < 1.0
    }
}
