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
    /// Whether a reply mentions its target. Discord's default is on.
    pub(crate) reply_mention: bool,
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
    /// The drawer's slide progress, 0 (shut) to 1 (open). The flag above
    /// owns the tree; this owns where it stands while opening, closing,
    /// or following the finger.
    pub(crate) drawer_slide: f32,
    /// Where the slide is going. The flag flips only when a closing slide
    /// lands, so the drawer coasts out instead of vanishing.
    pub(crate) drawer_target: f32,
    /// The progress an animation started from; a reversal continues from
    /// what is on screen instead of jumping.
    pub(crate) drawer_anim_from: f32,
    /// When the current animation started. `None` while the finger drives.
    pub(crate) drawer_anim_start: Option<std::time::Instant>,
    /// Whether the finger drives the slide; time animation stays off.
    pub(crate) drawer_drag: bool,
    /// The progress a drag started from: shut for an opening drag, what is
    /// on screen for a closing one.
    pub(crate) drawer_drag_from: f32,
}

impl ChatView {
    pub(crate) fn new(guild: u64, channel: u64) -> Self {
        ChatView {
            selected_guild: guild,
            reveals: crate::markdown::Reveals::default(),
            composing: Composing::New,
            reply_mention: true,
            selected_channel: channel,
            input_focused: false,
            input: TextDocument::new(),
            pending_reveal: None,
            pending_jump: None,
            a11y_message: None,
            drawer_open: false,
            member_sheet_open: false,
            drawer_slide: 1.0,
            drawer_target: 1.0,
            drawer_anim_from: 1.0,
            drawer_anim_start: None,
            drawer_drag: false,
            drawer_drag_from: 0.0,
        }
    }
}

/// How long the drawer takes to coast open or shut, in milliseconds.
pub(crate) const DRAWER_ANIM_MS: f32 = 220.0;

/// A sideways release fast enough to decide the drawer on its own.
pub(crate) const DRAWER_FLING_PX_S: f32 = 200.0;

/// Fast out, quiet in. Mirrors the renderer's motion curve, which lives
/// in another crate; duplicating three lines beats coupling to it.
fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

impl crate::Gumicord {
    /// Opens the navigation drawer. Only where the lists hide and past
    /// login; elsewhere there is nothing to drawer over. Slides in from
    /// the edge rather than appearing.
    pub(crate) fn open_drawer(&mut self) -> bool {
        if self.chat.drawer_open {
            // Reopening mid-close: swing back instead of refusing.
            if self.chat.drawer_target == 0.0 {
                self.chat.drawer_target = 1.0;
                self.chat.drawer_anim_from = self.chat.drawer_slide;
                self.chat.drawer_anim_start = Some(std::time::Instant::now());
                self.chat.drawer_drag = false;
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
        self.chat.drawer_slide = 0.0;
        self.chat.drawer_target = 1.0;
        self.chat.drawer_anim_from = 0.0;
        self.chat.drawer_anim_start = Some(std::time::Instant::now());
        self.chat.drawer_drag = false;
        true
    }

    pub(crate) fn close_drawer(&mut self) -> bool {
        if !self.chat.drawer_open {
            return false;
        }
        // Slide out; the flag flips when the slide lands (see build()),
        // so the drawer coasts instead of vanishing.
        self.chat.drawer_target = 0.0;
        self.chat.drawer_anim_from = self.chat.drawer_slide;
        self.chat.drawer_anim_start = Some(std::time::Instant::now());
        self.chat.drawer_drag = false;
        true
    }

    /// Advances the drawer slide towards its target. Returns whether it is
    /// still moving. A landed closing slide flips the flag, which is what
    /// finally removes the drawer from the tree.
    pub(crate) fn advance_drawer(&mut self, now: std::time::Instant) -> bool {
        if !self.chat.drawer_open || self.chat.drawer_drag {
            return false;
        }
        let target = self.chat.drawer_target;
        if (self.chat.drawer_slide - target).abs() < 0.001 {
            self.chat.drawer_slide = target;
            if target == 0.0 {
                self.chat.drawer_open = false;
            }
            return false;
        }
        let Some(start) = self.chat.drawer_anim_start else {
            self.chat.drawer_slide = target;
            return false;
        };
        let t = (now.saturating_duration_since(start).as_secs_f32() * 1000.0 / DRAWER_ANIM_MS)
            .clamp(0.0, 1.0);
        self.chat.drawer_slide =
            self.chat.drawer_anim_from + (target - self.chat.drawer_anim_from) * ease_out_cubic(t);
        if t >= 1.0 {
            self.chat.drawer_slide = target;
            if target == 0.0 {
                self.chat.drawer_open = false;
            }
            return false;
        }
        true
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
        self.chat.drawer_slide = 0.0;
        self.chat.drawer_target = 1.0;
        self.chat.drawer_drag = true;
        self.chat.drawer_drag_from = 0.0;
        self.chat.drawer_anim_start = None;
        true
    }

    /// Starts a finger-driven drawer close from inside the open drawer.
    /// The slide holds where it stands and follows the finger from there.
    pub(crate) fn drawer_close_drag_start(&mut self) -> bool {
        if !self.chat.drawer_open || self.panes().guilds() || !self.shows_main() {
            return false;
        }
        self.chat.drawer_drag = true;
        self.chat.drawer_drag_from = self.chat.drawer_slide;
        self.chat.drawer_anim_start = None;
        true
    }

    /// Whether a touch inside the open drawer could start a close drag:
    /// narrow, open, and past login. Side-effect free.
    pub(crate) fn drawer_close_drag_maybe(&self) -> bool {
        self.chat.drawer_open && !self.panes().guilds() && self.shows_main()
    }

    /// Follows the finger: progress is the drag distance from the start
    /// over the drawer width, falling from where the drag began. A stale
    /// width still opens; only the mapping stretches.
    pub(crate) fn drawer_drag_move(&mut self, dx: f32, width: f32) -> bool {
        if !self.chat.drawer_drag {
            return false;
        }
        let w = if width > 0.0 { width } else { 300.0 };
        let next = (self.chat.drawer_drag_from + dx / w).clamp(0.0, 1.0);
        if (next - self.chat.drawer_slide).abs() < 0.0005 {
            return false;
        }
        self.chat.drawer_slide = next;
        true
    }

    /// Lets go: a fast flick right finishes opening, a fast flick left
    /// falls back, and a slow release follows whichever half it is on.
    pub(crate) fn drawer_drag_end(&mut self, velocity: f32) -> bool {
        if !self.chat.drawer_drag {
            return false;
        }
        self.chat.drawer_drag = false;
        self.chat.drawer_target = if velocity > DRAWER_FLING_PX_S
            || (velocity >= -DRAWER_FLING_PX_S && self.chat.drawer_slide > 0.5)
        {
            1.0
        } else {
            0.0
        };
        self.chat.drawer_anim_from = self.chat.drawer_slide;
        self.chat.drawer_anim_start = Some(std::time::Instant::now());
        true
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
        self.sheet_slide = 0.0;
        self.sheet_target = 1.0;
        self.sheet_anim_from = 0.0;
        self.sheet_anim_start = Some(std::time::Instant::now());
        self.sheet_drag = false;
        self.sheet_closing = None;
        true
    }

    /// Closes the member sheet. Slides out; the flag flips when the slide
    /// lands, so the sheet coasts instead of vanishing.
    pub(crate) fn close_member_sheet(&mut self) -> bool {
        if !self.chat.member_sheet_open {
            return false;
        }
        self.sheet_target = 0.0;
        self.sheet_anim_from = self.sheet_slide;
        self.sheet_anim_start = Some(std::time::Instant::now());
        self.sheet_drag = false;
        self.sheet_closing = Some(crate::SheetCloser::Member);
        true
    }

    /// Advances the sheet slide towards its target. Returns whether it is
    /// still moving. A landed closing slide clears whichever surface was
    /// closing, which finally removes the sheet from the tree.
    pub(crate) fn advance_sheet(&mut self, now: std::time::Instant) -> bool {
        if self.sheet_drag {
            return false;
        }
        let target = self.sheet_target;
        if (self.sheet_slide - target).abs() < 0.001 {
            self.sheet_slide = target;
            if target == 0.0 {
                self.land_sheet();
            }
            return false;
        }
        let Some(start) = self.sheet_anim_start else {
            self.sheet_slide = target;
            return false;
        };
        let t = (now.saturating_duration_since(start).as_secs_f32() * 1000.0
            / crate::SHEET_ANIM_MS)
            .clamp(0.0, 1.0);
        self.sheet_slide =
            self.sheet_anim_from + (target - self.sheet_anim_from) * ease_out_cubic(t);
        if t >= 1.0 {
            self.sheet_slide = target;
            if target == 0.0 {
                self.land_sheet();
            }
            return false;
        }
        true
    }

    /// Clears whichever surface a closing slide was hiding.
    fn land_sheet(&mut self) {
        match self.sheet_closing.take() {
            Some(crate::SheetCloser::Member) => {
                self.chat.member_sheet_open = false;
            }
            Some(crate::SheetCloser::Menu) => {
                if matches!(self.floating, Some(crate::menu::Floating::Menu(_))) {
                    self.floating = None;
                }
            }
            None => {}
        }
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
        self.sheet_drag = true;
        self.sheet_drag_from = self.sheet_slide;
        self.sheet_anim_start = None;
        true
    }

    /// Follows the finger: progress is the drag distance from the start
    /// over the sheet height, falling from where the drag began. Down is
    /// positive on screen and closes the sheet.
    pub(crate) fn sheet_drag_move(&mut self, dy: f32, height: f32) -> bool {
        if !self.sheet_drag {
            return false;
        }
        let h = if height > 0.0 { height } else { 300.0 };
        let next = (self.sheet_drag_from - dy / h).clamp(0.0, 1.0);
        if (next - self.sheet_slide).abs() < 0.0005 {
            return false;
        }
        self.sheet_slide = next;
        true
    }

    /// Lets go: a fast flick down finishes closing, a fast flick up swings
    /// back open, and a slow release follows whichever half it is on.
    pub(crate) fn sheet_drag_end(&mut self, velocity: f32) -> bool {
        if !self.sheet_drag {
            return false;
        }
        self.sheet_drag = false;
        self.sheet_target = if velocity > crate::SHEET_FLING_PX_S
            || (velocity >= -crate::SHEET_FLING_PX_S && self.sheet_slide < 0.5)
        {
            0.0
        } else {
            1.0
        };
        self.sheet_anim_from = self.sheet_slide;
        self.sheet_anim_start = Some(std::time::Instant::now());
        if self.sheet_target == 0.0 && self.sheet_closing.is_none() {
            self.sheet_closing = if self.chat.member_sheet_open {
                Some(crate::SheetCloser::Member)
            } else {
                Some(crate::SheetCloser::Menu)
            };
        }
        true
    }
}
