//! The application layer: screens, app state, and the wiring between layers.
//!
//! It owns the order of the frame pipeline, which the extension semantics
//! depend on:
//!
//! ```text
//! [1] input
//! [2] state update              gumicord-store
//! [3] build the UITree          gumicord-uitree
//! [4] plugin structure pass     gumicord-plugin
//! [5] theme resolution          gumicord-theme
//! [6] plugin style pass         gumicord-plugin
//! [7] layout                    gumicord-render
//! [8] draw commands -> GPU
//! [9] accessibility tree        gumicord-platform
//! ```
//!
//! [4] precedes [5] so themes apply to nodes plugins inserted. [6] follows
//! [5] because plugins win when the two disagree.
//!
//! [4], [6] and [9] do not exist yet, and [3] rebuilds the whole tree every
//! frame rather than diffing.
//!
//! There are two screens. A cache, a login, or `GUMICORD_SKIP_LOGIN` all lead
//! to the main screen; nothing leads to the login screen. Having a cache
//! skips waiting for login, since READY takes closer to a second and would
//! blow the cold-start budget — and because signing out deletes the cache,
//! its presence is itself proof of a previous session on this account.
//!
//! `uses_live()` is the single place that distinguishes demo data from real
//! data. The row types absorb the difference so the tree builder never asks.

pub mod demo;
pub mod images;
pub mod live;
pub mod markdown;
pub mod menu;
pub mod session;

use std::borrow::Cow;

use gumicord_model::{ChannelId, GuildId, MessageId, RoleId, UserId};
use gumicord_platform::{Application, FrameCx, TextDocument, Waker};
use gumicord_render::Hit;
use gumicord_store::{ChannelEntry, GuildEntry};
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::value::Color;
use gumicord_uitree::{Editable, Key, NodeId, State, UiNode};
use live::Live;
use session::Login;

/// The default theme, embedded rather than loaded: the app has to run even
/// when no theme file can be read.
const DEFAULT_THEME: &str = include_str!("../../../examples/themes/midnight/theme.json");

/// Swaps the theme, for comparing themes while writing one. Goes away once
/// there is a settings screen and hot reload.
const THEME_ENV: &str = "GUMICORD_THEME";

/// Image sizes requested from the CDN, in logical px, matching what is drawn.
///
/// Never ask for more than is drawn: the atlas is one 2048-square page, and
/// requesting 128px for something drawn at 40px costs ten times the area. It
/// has overflowed in practice.
const GUILD_ICON_PX: f32 = 48.0;
const MESSAGE_AVATAR_PX: f32 = 40.0;
/// The user panel and the member list.
const SMALL_AVATAR_PX: f32 = 32.0;

/// Icons tiled inside a folded folder, 2x2 as in Discord.
///
/// Raising this needs the theme's `grouped` size raised too, or the tiles no
/// longer fit inside the folder.
const FOLDER_TILES: usize = 4;

/// Nodes that respond to the pointer.
///
/// A theme's `when.state = hover` does nothing for anything missing here.
/// Slot for the cancel button; the same string is used to build it and to
/// match the press.
const CANCEL_COMPOSING: &str = "cancel_composing";

const INTERACTIVE: &[NodeId] = &[
    NodeId::NavGuildListHome,
    NodeId::NavGuildListItem,
    NodeId::NavGuildListFolder,
    NodeId::NavChannelListItem,
    NodeId::NavDmListItem,
    NodeId::NavMemberListItem,
    NodeId::ChatMessage,
    NodeId::ChromeTitlebarControl,
    NodeId::PrimitiveButton,
    NodeId::LayoutScrollbarThumb,
    NodeId::OverlayMenuItem,
    NodeId::OverlayModalAction,
];

/// How many panes to show.
///
/// Decided by width, not platform: a portrait tablet and a narrowed desktop
/// window want the same treatment, and asking the platform cannot answer
/// "Windows, but 500px wide". Themes do the same with `when.maxWidth`.
///
/// Panes are only hidden; there is no gesture to bring one back yet, so
/// widening the window is the only way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panes {
    /// Guilds, channels, chat, members.
    Four,
    /// Guilds, channels, chat.
    Three,
    /// Channels, chat.
    Two,
    /// Chat only.
    One,
}

impl Panes {
    /// Narrowest width that still fits the member list, which is the first
    /// thing to go: who is present matters less than what was said.
    const FOUR: f32 = 1140.0;
    /// Narrowest width for three panes: the two lists plus enough chat.
    const THREE: f32 = 900.0;
    /// Narrowest width for two panes.
    const TWO: f32 = 600.0;

    pub fn for_width(w: f32) -> Self {
        if w >= Self::FOUR {
            Panes::Four
        } else if w >= Self::THREE {
            Panes::Three
        } else if w >= Self::TWO {
            Panes::Two
        } else {
            Panes::One
        }
    }

    pub fn guilds(self) -> bool {
        matches!(self, Panes::Four | Panes::Three)
    }

    pub fn channels(self) -> bool {
        self != Panes::One
    }

    pub fn members(self) -> bool {
        self == Panes::Four
    }

    /// How to present a menu. By width, not device: a narrowed desktop window
    /// reads better with a sheet too.
    pub fn present(self) -> crate::menu::Present {
        match self {
            Panes::One => crate::menu::Present::Sheet,
            _ => crate::menu::Present::Popover,
        }
    }
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
    fn target(self) -> Option<u64> {
        match self {
            Composing::New => None,
            Composing::Reply(id) | Composing::Edit(id) => Some(id),
        }
    }
}

/// The app state, and building the UITree from it.
pub struct Gumicord {
    theme: Option<Theme>,
    /// Dropping this stops everything running on it.
    runtime: Option<tokio::runtime::Runtime>,
    /// Wakes the event loop; handed to the gateway after login.
    waker: Option<Waker>,
    /// Login progress; decides which screen is shown.
    login: Login,
    /// Real data. Whether this is empty is what separates demo from live.
    live: Live,
    /// Scale factor, needed to size CDN requests.
    scale: f32,
    /// The node under the pointer.
    hovered: Option<(NodeId, Option<Key>)>,
    /// The innermost scrollable under the pointer; only that list shows a
    /// scrollbar.
    hovered_scroll: Option<NodeId>,
    selected_guild: u64,
    /// Theme match context, captured before building.
    ///
    /// Inline decoration is spans rather than nodes, so it never reaches the
    /// resolver's walk; the theme has to be consulted while building.
    match_ctx: MatchContext,
    /// Messages whose spoilers are revealed. Per message, since per-span
    /// would require spans to be nodes.
    revealed: std::collections::HashSet<u64>,
    /// Whatever is floating; at most one.
    floating: Option<crate::menu::Floating>,
    /// What the composer is doing.
    composing: Composing,
    selected_channel: u64,
    /// Whether the composer has focus.
    input_focused: bool,
    /// The composer's contents.
    input: TextDocument,
    /// Messages sent in demo mode; unused when live.
    sent: Vec<demo::Message>,
    /// Fetches images.
    images: images::Images,
    /// The time read at the head of the frame. Never re-read while building,
    /// or adjacent relative timestamps disagree.
    now: i64,
    /// How long the built tree stays valid. `None` means nothing changes
    /// with time. A `Cell` because building takes `&self`.
    holds: std::cell::Cell<Option<i64>>,
}

impl Gumicord {
    pub fn new() -> Self {
        // Reads the cache here, so the first frame has something to draw.
        Gumicord::with(Login::new(), Live::new())
    }

    /// Skips login and builds from fixed demo data, as `GUMICORD_SKIP_LOGIN`
    /// does. Opens no cache: real data mixed in would break the premise.
    pub fn demo() -> Self {
        Gumicord::with(Login::skipped(), Live::without_cache())
    }

    fn with(login: Login, live: Live) -> Self {
        // Restore the last channel, and the guild it belongs to.
        let (guild, channel) = match live.last_channel() {
            Some(ch) => {
                let guild = live
                    .store()
                    .channel(ch)
                    .and_then(|c| c.guild_id)
                    .map(|g| g.get())
                    .unwrap_or(0);
                (guild, ch.get())
            }
            None => (demo::GUILDS[0].id, demo::CHANNELS[1].id),
        };

        Gumicord {
            theme: load_theme(),
            runtime: None,
            waker: None,
            login,
            live,
            scale: 1.0,
            hovered: None,
            hovered_scroll: None,
            selected_guild: guild,
            match_ctx: MatchContext::new(0.0),
            revealed: std::collections::HashSet::new(),
            floating: None,
            composing: Composing::New,
            selected_channel: channel,
            input_focused: false,
            input: TextDocument::new(),
            sent: Vec::new(),
            images: images::Images::new(),
            now: gumicord_platform::now_unix(),
            holds: std::cell::Cell::new(None),
        }
    }

    fn is_hovered(&self, id: NodeId, key: Option<&Key>) -> bool {
        match &self.hovered {
            Some((hid, hkey)) => *hid == id && hkey.as_ref() == key,
            None => false,
        }
    }

    /// A list's scrollbar, shown only while the pointer is inside it.
    ///
    /// "While scrolling" would need a timer to hide it again, and the event
    /// loop sleeps. Since scrolling requires the pointer to be there anyway,
    /// position gives the same result without one.
    ///
    /// Dragging the thumb keeps it visible outside the list, because hover is
    /// not updated while dragging.
    /// What pixel size to request for something drawn this large.
    ///
    /// Multiplied by the scale factor: asking for 40px on a 200% display
    /// gives a blurry upscale. Discord rounds up to a power of two.
    fn asset_px(&self, logical: f32) -> u16 {
        let px = (logical * self.scale.max(1.0)).ceil();
        px.clamp(16.0, 4096.0) as u16
    }

    fn scrollbar(&self, owner: NodeId) -> Option<UiNode> {
        (self.hovered_scroll == Some(owner)).then(scrollbar_node)
    }

    fn hovered_id(&self, node: NodeId, id: u64) -> bool {
        self.is_hovered(node, Some(&Key::Id(id)))
    }
}

impl Default for Gumicord {
    fn default() -> Self {
        Self::new()
    }
}

fn load_theme() -> Option<Theme> {
    let src = match std::env::var(THEME_ENV) {
        Ok(path) => match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(%path, %e, "テーマを読めなかった。同梱のものを使う");
                DEFAULT_THEME.to_owned()
            }
        },
        Err(_) => DEFAULT_THEME.to_owned(),
    };

    let result = Theme::parse(&src);
    // A rejected rule does not reject the theme, but is never dropped
    // silently.
    for d in &result.diagnostics {
        tracing::warn!("テーマ: {d}");
    }
    result.theme
}

impl Application for Gumicord {
    fn title(&self) -> String {
        "Gumicord".to_owned()
    }

    /// Starts login, before the window exists. Key generation takes about a
    /// second, so earlier means the QR appears sooner.
    fn start(&mut self, waker: Waker) {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(%e, "非同期処理を始められない");
                return;
            }
        };

        self.login.start(runtime.handle(), waker.clone());
        self.live.attach_waker(waker.clone());
        self.waker = Some(waker);
        self.runtime = Some(runtime);
    }

    /// Hands over fetched images, just before drawing.
    /// The atlas evicted images. They are still on disk, so this re-reads
    /// rather than refetching; avatars vanish for one frame.
    fn images_dropped(&mut self) {
        tracing::debug!("アトラスが絵を忘れた。読み直す");
        self.images.forget_requested();
    }

    /// Requests images that were about to draw and were missing.
    ///
    /// Not gathered by walking the tree: visibility is decided by layout and
    /// clipping. A 300-row member list shows about 15, and the rest are not
    /// wanted.
    fn request_images(&mut self, urls: &[String]) {
        for url in urls {
            self.images.request(url);
        }
    }

    fn take_images(&mut self) -> Vec<gumicord_render::ImageData> {
        self.images.take()
    }

    /// A list scrolled; fetches history when it nears the top.
    ///
    /// Not at the top exactly: asking on arrival means staring at nothing
    /// until it returns. Asking early usually has it there first.
    ///
    /// Never before anything is shown: a list that does not overflow is also
    /// "at the top", which would fetch on every open.
    fn scrolled(&mut self, id: NodeId, at: f32, max: f32) {
        /// Distance from the top that triggers the next page.
        const REACH: f32 = 400.0;

        if id != NodeId::ChatMessageList || max <= 0.0 || at > REACH {
            return;
        }
        let channel = ChannelId::from(self.selected_channel);
        self.live.load_older(channel);
    }

    /// How long the time-dependent parts of the tree stay valid. `None`
    /// means there is no reason to wake.
    fn next_frame_in(&self) -> Option<std::time::Duration> {
        self.holds
            .get()
            .map(|s| std::time::Duration::from_secs(s.max(1) as u64))
    }

    /// Something was prepended; hold the scroll position.
    fn keep_place(&mut self) -> Option<NodeId> {
        self.live
            .take_prepended()
            .then_some(NodeId::ChatMessageList)
    }

    /// Drains background events. The only entry point for them.
    fn wake(&mut self) -> bool {
        let mut changed = self.login.poll();
        changed |= self.live.poll();
        // An arrived image counts as a change, or it never gets drawn.
        changed |= self.images.poll();

        // `Live::start` is a no-op once started, so calling it every time is
        // fine.
        if let (Some(l), Some(rt), Some(waker)) =
            (self.login.session().logged_in(), &self.runtime, &self.waker)
        {
            // Set before READY, so our own typing is filtered from the start.
            let me = l.me.user.id;
            self.live.start(
                rt.handle(),
                l.client.clone(),
                l.token.clone(),
                waker.clone(),
            );
            self.live.set_me(me);
            self.images
                .start(rt.handle(), l.client.clone(), waker.clone());
        }

        // The gateway rejected the token. Same path as pressing "log out".
        if self.live.take_rejection() {
            tracing::warn!("the token is no longer valid; signing out");
            changed |= self.sign_out();
        }

        changed |= self.sync_selection();
        changed
    }

    fn hover_changed(&mut self, hits: &[Hit]) -> bool {
        let mut changed = false;

        // Hits come front to back, so the first scrollable is the innermost.
        let scroll = hits
            .iter()
            .find(|h| gumicord_render::intrinsic(h.id).scroll)
            .map(|h| h.id);
        if scroll != self.hovered_scroll {
            self.hovered_scroll = scroll;
            changed = true;
        }

        let next = hits
            .iter()
            .find(|h| INTERACTIVE.contains(&h.id))
            .map(|h| (h.id, h.key.clone()));
        if next != self.hovered {
            self.hovered = next;
            changed = true;
        }
        changed
    }

    fn pressed(&mut self, hits: &[Hit]) -> bool {
        // Never passed through: a press meant to dismiss the menu would
        // navigate to whatever is underneath.
        if self.floating.is_some() {
            let item = hits.iter().find_map(|h| match (h.id, &h.key) {
                (NodeId::OverlayMenuItem | NodeId::OverlayModalAction, Some(Key::Index(i))) => {
                    Some(*i as usize)
                }
                _ => None,
            });
            return match item {
                Some(i) => self.run_action(i),
                // A dialog does not close on an outside press: it represents
                // an unmade decision, and dismissing it silently leaves the
                // outcome ambiguous.
                None => match &self.floating {
                    Some(crate::menu::Floating::Confirm(_)) => false,
                    _ => self.close_menu(),
                },
            };
        }

        let mut changed = false;

        // Pressing outside the composer removes focus.
        let on_input = hits.iter().any(|h| h.id == NodeId::ChatInputField);
        if on_input != self.input_focused {
            self.input_focused = on_input;
            changed = true;
        }

        // Only the frontmost selectable hit.
        for h in hits {
            match (h.id, &h.key) {
                // Folders only fold; they do not change the selected guild.
                (NodeId::NavGuildListFolder, Some(Key::Id(id))) => {
                    self.live.toggle_folder(*id);
                    changed = true;
                }
                (NodeId::NavGuildListItem, Some(Key::Id(id))) => {
                    if self.selected_guild == *id {
                        break;
                    }
                    self.selected_guild = *id;
                    // Clear the channel, or the list and the body disagree.
                    self.selected_channel = 0;
                    changed = true;
                }
                (NodeId::NavChannelListItem, Some(Key::Id(id))) => {
                    changed |= self.selected_channel != *id;
                    self.selected_channel = *id;
                }
                (NodeId::PrimitiveButton, Some(Key::Slot(CANCEL_COMPOSING))) => {
                    changed |= self.stop_composing();
                }
                // Per message: per-span would require spans to be nodes.
                (NodeId::ChatMessage, Some(Key::Id(id))) => {
                    changed |= self.revealed.insert(*id);
                }
                _ => continue,
            }
            break;
        }

        // Fetch immediately, or the selection looks stuck on empty until
        // something else happens.
        changed |= self.sync_selection();
        changed
    }

    /// Secondary press; what was hit decides the menu.
    fn context_menu(&mut self, hits: &[Hit], at: (f32, f32)) -> bool {
        // Reopens rather than closing, or opening the next message's menu
        // would take two presses.
        let items = hits.iter().find_map(|h| match (h.id, &h.key) {
            // The composer first: it overlaps the message list.
            (NodeId::ChatInputField, _) => Some(self.field_menu()),
            (NodeId::ChatMessage, Some(Key::Id(id))) => Some(self.message_menu(*id)),
            (NodeId::NavChannelListItem, Some(Key::Id(id))) => Some(self.channel_menu(*id)),
            (NodeId::NavGuildListItem, Some(Key::Id(id))) => Some(self.guild_menu(*id)),
            (NodeId::NavUserPanel, _) => Some(self.user_menu()),
            _ => None,
        });
        match items {
            Some(items) => self.open_menu(at, items),
            // A press on nothing just closes whatever is open.
            None => self.close_menu(),
        }
    }

    /// Only a focused field receives input.
    fn focused_document(&mut self) -> Option<&mut TextDocument> {
        self.input_focused.then_some(&mut self.input)
    }

    /// Sends, edits or replies, depending on [`Composing`]. Missing that
    /// turns an intended edit into a new message.
    fn submit(&mut self) -> bool {
        let body = self.input.text().trim().to_owned();
        let mode = self.composing;

        // Emptying an edit is not a delete; Discord rejects it too. Clearing
        // the field and pressing enter must not destroy the message.
        if body.is_empty() {
            return false;
        }
        self.input.take();
        self.composing = Composing::New;

        if self.uses_live() {
            let channel = ChannelId::from(self.selected_channel);
            match mode {
                Composing::Edit(id) => {
                    self.live.edit_message(channel, MessageId::from(id), body);
                }
                // The gateway echoes it back; adding it here shows it twice.
                Composing::Reply(id) => {
                    self.live
                        .send_message(channel, body, Some(MessageId::from(id)));
                }
                Composing::New => self.live.send_message(channel, body, None),
            }
            return true;
        }

        // Demo mode just appends locally.
        let id = 1000 + self.sent.len() as u64;
        self.sent.push(demo::Message {
            id,
            author: Cow::Borrowed("ねんねこ"),
            time: Cow::Borrowed("たった今"),
            body: Cow::Owned(body),
            mentioned: false,
        });
        true
    }

    fn cancel_input(&mut self) -> bool {
        // The menu floats above the composer, so escape stops here.
        if self.close_menu() {
            return true;
        }
        // Cancel the reply or edit before discarding the draft; doing both at
        // once leaves it unclear which was lost.
        if self.stop_composing() {
            return true;
        }
        if !self.input_focused {
            return false;
        }
        self.input_focused = false;
        true
    }

    /// Pipeline stages [3] and [5]. The plugin passes will go between them.
    fn build(&mut self, cx: &FrameCx) -> UiNode {
        // Image sizes depend on it, so capture before building.
        self.scale = cx.scale;

        // Read once per frame; re-reading mid-build makes adjacent relative
        // timestamps disagree.
        self.now = gumicord_platform::now_unix();
        self.holds.set(None);

        // Inline decoration is spans, which stage [5] never walks, so the
        // theme is consulted while building.
        let ctx = MatchContext::new(cx.viewport.w);
        self.match_ctx = ctx;

        // [3] build the tree
        let mut tree = self.build_tree(Panes::for_width(cx.viewport.w));

        // [5] resolve the theme
        match &self.theme {
            Some(theme) => {
                gumicord_theme::resolve(theme, &mut tree, &ctx);
            }
            None => gumicord_theme::resolve::clear(&mut tree),
        }
        tree
    }
}

impl Gumicord {
    /// Rebuilds the whole tree every frame; diffing waits until the renderer's
    /// requirements settle.
    fn build_tree(&self, panes: Panes) -> UiNode {
        // The only place the two screens diverge.
        let screen = if self.shows_main() {
            UiNode::new(NodeId::AppScreenMain)
                .children(self.sidebar(panes))
                .child(self.chat_view())
                .child_if(panes.members(), || self.member_list())
        } else {
            self.login_screen()
        };

        UiNode::new(NodeId::AppRoot)
            .child(
                UiNode::new(NodeId::AppWindow)
                    .child(self.titlebar())
                    .child(UiNode::new(NodeId::AppScreen).child(screen)),
            )
            // Only while open: a full-window layer would absorb every press.
            .child_if(self.floating.is_some(), || {
                let f = self.floating.as_ref().expect("直前に確かめた");
                f.node(panes.present(), self.hovered_item())
            })
    }

    /// A message's menu. Only what can actually be done: a greyed row adds to
    /// the search for a usable one.
    fn message_menu(&self, id: u64) -> Vec<crate::menu::Item> {
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
    fn is_mine(&self, id: u64) -> bool {
        let Some(me) = self.login.session().logged_in().map(|l| l.me.user.id) else {
            return false;
        };
        self.live
            .store()
            .messages(ChannelId::from(self.selected_channel))
            .iter()
            .any(|m| m.id.get() == id && m.author.id == me)
    }

    /// A message's body as typed.
    ///
    /// Not taken from the built nodes: those hold parsed text, where `<@123>`
    /// has already become a display name.
    fn raw_body(&self, id: u64) -> Option<String> {
        if !self.uses_live() {
            return None;
        }
        self.live
            .store()
            .messages(ChannelId::from(self.selected_channel))
            .iter()
            .find(|m| m.id.get() == id)
            .map(|m| m.content.clone())
    }

    fn channel_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let mut items = Vec::new();
        if self.live.store().is_unread(ChannelId::from(id)) {
            items.push(Item::new(Action::MarkRead(id), "既読にする").icon("check"));
        }
        items.push(Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id"));
        items
    }

    fn guild_menu(&self, id: u64) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        vec![Item::new(Action::Copy(id.to_string()), "ID をコピー").icon("id")]
    }

    /// The menu on our own panel. Only offered while actually signed in.
    fn user_menu(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let Some(l) = self.login.session().logged_in() else {
            return Vec::new();
        };
        vec![
            Item::new(Action::Copy(l.me.user.id.to_string()), "ID をコピー").icon("id"),
            Item::new(Action::LogOut, "ログアウト")
                .icon("logout")
                .danger(),
        ]
    }

    /// The hovered menu item or dialog button.
    fn hovered_item(&self) -> Option<usize> {
        match &self.hovered {
            Some((NodeId::OverlayMenuItem | NodeId::OverlayModalAction, Some(Key::Index(i)))) => {
                Some(*i as usize)
            }
            _ => None,
        }
    }

    /// Opens a menu; an empty one closes instead.
    fn open_menu(&mut self, at: (f32, f32), items: Vec<crate::menu::Item>) -> bool {
        if items.is_empty() {
            return self.close_menu();
        }
        self.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu { at, items }));
        true
    }

    fn close_menu(&mut self) -> bool {
        self.floating.take().is_some()
    }

    /// An item was pressed; also reached by dialog buttons.
    fn run_action(&mut self, index: usize) -> bool {
        let Some(f) = self.floating.take() else {
            return false;
        };
        let action = match &f {
            crate::menu::Floating::Menu(m) => match m.items.get(index) {
                Some(item) => item.action.clone(),
                None => return true,
            },
            crate::menu::Floating::Confirm(c) => match index {
                crate::menu::button::CONFIRM => c.action.clone(),
                // Cancel does nothing; the dialog is already closed.
                _ => return true,
            },
        };

        // Anything needing confirmation turns back here and opens the dialog;
        // the next call comes from its buttons.
        if let Some(confirm) = self.needs_confirming(&f, &action) {
            self.floating = Some(crate::menu::Floating::Confirm(confirm));
            return true;
        }

        self.perform(action)
    }

    /// Whether an action needs confirming, and with what.
    ///
    /// Never for something already coming from a dialog, or it would reopen
    /// forever.
    fn needs_confirming(
        &self,
        from: &crate::menu::Floating,
        action: &crate::menu::Action,
    ) -> Option<crate::menu::Confirm> {
        if matches!(from, crate::menu::Floating::Confirm(_)) {
            return None;
        }
        match action {
            // A deleted message cannot be recovered, and this is one row
            // among others in a menu.
            crate::menu::Action::Delete(id) => Some(crate::menu::Confirm {
                title: "この発言を削除しますか".to_owned(),
                body: "削除した発言は元に戻せません。".to_owned(),
                // Show what goes: "are you sure" alone could delete the wrong
                // thing if the list changed.
                preview: self
                    .raw_body(*id)
                    .as_deref()
                    .and_then(crate::menu::preview_line),
                action: action.clone(),
                confirm: "削除する".to_owned(),
                danger: true,
            }),
            // Getting back in needs the phone: password login does not exist
            // yet, so say so before the token is gone.
            crate::menu::Action::LogOut => Some(crate::menu::Confirm {
                title: "ログアウトしますか".to_owned(),
                body: "この端末に保存したログイン情報と、読み込んだ内容をすべて消します。\
                       入り直すにはスマホの Discord で QR を読み取る必要があります。"
                    .to_owned(),
                preview: self
                    .login
                    .session()
                    .logged_in()
                    .map(|l| l.me.user.display_name().to_owned()),
                action: action.clone(),
                confirm: "ログアウトする".to_owned(),
                danger: true,
            }),
            _ => None,
        }
    }

    /// Performs an action directly.
    fn perform(&mut self, action: crate::menu::Action) -> bool {
        match &action {
            crate::menu::Action::Copy(text) => {
                // Never swallowed: pasting the previous contents while
                // believing the copy worked is the worst outcome.
                if let Err(e) = gumicord_platform::clipboard::set_text(text) {
                    tracing::warn!(%e, "クリップボードへ入れられなかった");
                }
            }
            crate::menu::Action::MarkRead(channel) => {
                self.live.mark_read(ChannelId::from(*channel));
            }
            crate::menu::Action::Reply(id) => {
                // Keeps the draft: the expectation is that it gains a
                // recipient.
                self.composing = Composing::Reply(*id);
                self.input_focused = true;
            }
            crate::menu::Action::Edit(id) => {
                // The raw body; the parsed one would silently drop the
                // markup.
                let Some(text) = self.raw_body(*id) else {
                    return true;
                };
                self.input.take();
                self.input.insert(&text);
                self.composing = Composing::Edit(*id);
                self.input_focused = true;
            }
            crate::menu::Action::Delete(id) => {
                // Only reached after the dialog confirmed.
                // ([`Self::needs_confirming`])
                self.live
                    .delete_message(ChannelId::from(self.selected_channel), MessageId::from(*id));
                // Deleting what is being edited also cancels the edit.
                if self.composing.target() == Some(*id) {
                    self.composing = Composing::New;
                    self.input.take();
                }
            }

            crate::menu::Action::LogOut => {
                self.sign_out();
            }

            crate::menu::Action::Cut => {
                if self.copy_selection() {
                    self.input.insert("");
                }
            }
            crate::menu::Action::CopySelection => {
                self.copy_selection();
            }
            crate::menu::Action::Paste => {
                match gumicord_platform::clipboard::text() {
                    // The field is one line, so newlines would hide text.
                    // Discord collapses them on paste too.
                    Ok(Some(text)) => self.input.insert(&text.replace(['\r', '\n'], " ")),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%e, "クリップボードを読めなかった"),
                }
            }
            crate::menu::Action::SelectAll => self.input.select_all(),
        }
        true
    }

    /// Signs out and returns to the login screen.
    ///
    /// Reached both by pressing "log out" and by the gateway rejecting the
    /// token; the two must leave the same state behind, or one of them strands
    /// the app on a screen it cannot get out of.
    ///
    /// Nothing is kept: leaving the cache behind lets the next person on this
    /// machine read the previous one's messages.
    fn sign_out(&mut self) -> bool {
        let (Some(rt), Some(waker)) = (&self.runtime, &self.waker) else {
            // No runtime means demo mode, where there is nothing to sign out of.
            return false;
        };
        self.login.forget(rt.handle(), waker.clone());
        self.live.forget_everything();
        self.images.forget_everything();

        // Anything still on screen belongs to the account that just left.
        self.floating = None;
        self.composing = Composing::New;
        self.input.take();
        self.input_focused = false;
        self.revealed.clear();
        true
    }

    /// Copies the selection, if any.
    fn copy_selection(&mut self) -> bool {
        let sel = self.input.selection();
        if sel.is_empty() {
            return false;
        }
        let text = self.input.text()[sel].to_owned();
        if let Err(e) = gumicord_platform::clipboard::set_text(&text) {
            tracing::warn!(%e, "クリップボードへ入れられなかった");
            return false;
        }
        true
    }

    /// The composer's menu, desktop only. Lists only what would do something.
    fn field_menu(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item};
        let mut items = Vec::new();

        if !self.input.selection().is_empty() {
            items.push(Item::new(Action::Cut, "切り取り").icon("cut"));
            items.push(Item::new(Action::CopySelection, "コピー").icon("copy"));
        }
        // Not read: opening the clipboard takes it from other programs, which
        // is not something to do every time a menu opens.
        items.push(Item::new(Action::Paste, "貼り付け").icon("paste"));

        if !self.input.is_empty() {
            items.push(Item::new(Action::SelectAll, "すべて選択").icon("select_all"));
        }
        items
    }

    /// The login screen: a QR and nothing else to press, since scanning it is
    /// the only thing to do here. Spacers above and below centre it.
    fn login_screen(&self) -> UiNode {
        let s = self.login.session();

        UiNode::new(NodeId::AppScreenLogin)
            .child(UiNode::new(NodeId::LayoutSpacer))
            .child(UiNode::text(
                NodeId::AppScreenLoginTitle,
                "QR コードでログイン",
            ))
            // Nothing while connecting: an empty frame is an unscannable QR.
            .child_if(s.qr().is_some(), || {
                UiNode::qr(NodeId::PrimitiveQr, s.qr().unwrap_or_default())
            })
            .child(UiNode::text(NodeId::AppScreenLoginHint, s.hint()))
            .child(UiNode::new(NodeId::LayoutSpacer))
    }

    /// The custom title bar. Buttons are told apart by their slot, which is
    /// all the platform layer reads.
    fn titlebar(&self) -> UiNode {
        // Icons, not glyphs: as text their weight and size follow the font
        // and the three never line up.
        let button = |slot: &'static str, icon: &str| {
            UiNode::icon(NodeId::ChromeTitlebarControl, icon)
                .with_key(Key::Slot(slot))
                .with_state_if(
                    self.is_hovered(NodeId::ChromeTitlebarControl, Some(&Key::Slot(slot))),
                    State::Hover,
                )
        };

        // Shows who is signed in, and is the only visible sign that real data
        // is flowing.
        let title = match self.login.session().logged_in() {
            Some(l) => format!("  Gumicord — {}", l.me.user.display_name()),
            None => "  Gumicord".to_owned(),
        };

        UiNode::new(NodeId::ChromeTitlebar)
            .child(UiNode::text(NodeId::ChromeTitlebarTitle, title))
            .child(
                UiNode::new(NodeId::ChromeTitlebarControls)
                    .child(button("minimize", "window.minimize"))
                    .child(button("maximize", "window.maximize"))
                    .child(button("close", "window.close")),
            )
    }

    /// The guild list.
    fn guild_list(&self) -> UiNode {
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
    fn guild_item(&self, g: &GuildRow) -> UiNode {
        let selected = g.id == self.selected_guild;
        let hovered = self.hovered_id(NodeId::NavGuildListItem, g.id);

        // The container is wider than the icon, leaving a lane at the left
        // for the pill.
        let icon = face(NodeId::NavGuildListItemIcon, g.icon.as_deref(), &g.name)
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
    fn guild_pill(&self, g: &GuildRow, selected: bool, hovered: bool) -> Option<UiNode> {
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
    fn folder_face(&self, row: &GuildRow) -> UiNode {
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
        for pair in row.members.chunks(2).take(FOLDER_TILES / 2) {
            let mut line = UiNode::new(NodeId::LayoutRow);
            for m in pair {
                line = line.child(
                    face(NodeId::NavGuildListItemIcon, m.icon.as_deref(), &m.name)
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

    fn channel_list(&self) -> UiNode {
        let title = self
            .guild_rows()
            .into_iter()
            .find(|g| g.id == self.selected_guild)
            .map(|g| g.name)
            .unwrap_or_else(|| "Gumicord".to_owned());

        // Only the list scrolls: one scroll region would carry the header and
        // the user panel off screen.
        let mut list = UiNode::new(NodeId::LayoutScroll);

        for c in self.channel_rows() {
            // Categories are headings; nothing opens, so no hit target.
            if c.category {
                list = list
                    .child(UiNode::text(NodeId::NavChannelListCategory, c.name).with_id_key(c.id));
                continue;
            }

            let mut item = UiNode::new(NodeId::NavChannelListItem)
                .with_id_key(c.id)
                .with_data(c.id)
                .with_state_if(c.id == self.selected_channel, State::Selected)
                .with_state_if(c.unread, State::Unread)
                .with_state_if(c.mentions > 0, State::Mentioned)
                .with_state_if(
                    self.hovered_id(NodeId::NavChannelListItem, c.id),
                    State::Hover,
                )
                .child(UiNode::icon(NodeId::NavChannelListItemIcon, c.icon).with_data(c.id))
                .child(UiNode::text(NodeId::NavChannelListItemName, c.name).with_data(c.id));

            if c.mentions > 0 {
                item = item.child(
                    UiNode::text(NodeId::NavChannelListItemBadge, c.mentions.to_string())
                        .with_data(c.id),
                );
            }
            list = list.child(item);
        }

        UiNode::new(NodeId::NavChannelList)
            .child(UiNode::text(NodeId::NavChannelListHeader, title))
            .child(list.children(self.scrollbar(NodeId::LayoutScroll)))
    }

    /// The whole left side, with the user panel pinned below the lists.
    ///
    /// ```text
    ///   ┌────┬──────────────┐
    ///   │ ◎ │ # general     │
    ///   │ ◎ │ # chat        │
    ///   ├────┴──────────────┤  the panel spans both
    ///   │ ◎ name            │
    ///   └───────────────────┘
    /// ```
    ///
    /// Not under the channel list alone: it spans the guild list too.
    fn sidebar(&self, panes: Panes) -> Option<UiNode> {
        if !panes.guilds() && !panes.channels() {
            return None;
        }
        let lists = UiNode::new(NodeId::NavSidebarLists)
            .child_if(panes.guilds(), || self.guild_list())
            .child_if(panes.channels(), || self.channel_list());

        Some(
            UiNode::new(NodeId::NavSidebar)
                .child(lists)
                .children(self.user_panel()),
        )
    }

    /// Who is signed in.
    ///
    /// ```text
    ///   ┌──────────────────────┐
    ///   │ ◎  name             │
    ///   │ ●  online           │
    ///   └──────────────────────┘
    /// ```
    ///
    /// Being connected is not a status: showing "online" to someone set to do
    /// not disturb would be a lie, so an unknown status shows neither a word
    /// nor a dot. There is no way to change it yet, and a control that does
    /// nothing is worse than none.
    fn user_panel(&self) -> Option<UiNode> {
        let me = &self.login.session().logged_in()?.me.user;
        let status = self.live.status();

        let mut avatar = UiNode::image(
            NodeId::NavUserPanelAvatar,
            me.display_avatar()
                .with_size(self.asset_px(SMALL_AVATAR_PX))
                .url(),
        );
        if let Some(s) = status {
            // Keyed by slot: a fixed set, so themes can style each one.
            avatar = avatar
                .child(UiNode::new(NodeId::NavUserPanelPresence).with_key(Key::Slot(s.as_wire())));
        }

        let mut lines =
            UiNode::new(NodeId::LayoutColumn).child(UiNode::text(NodeId::NavUserPanelName, {
                // Not guild-scoped: this is the global display name.
                me.display_name().to_owned()
            }));
        if let Some(s) = status {
            lines = lines.child(UiNode::text(NodeId::NavUserPanelStatus, s.label()));
        }

        Some(UiNode::new(NodeId::NavUserPanel).child(avatar).child(lines))
    }

    /// The member list, at the right edge.
    ///
    /// ```text
    ///   ┌──────────────────┐
    ///   │ Admins — 2        │  heading
    ///   │ ◎ someone         │
    ///   │ ◎ someone else    │
    ///   │ Online — 5        │
    ///   └──────────────────┘
    /// ```
    ///
    /// The column exists before the data arrives: growing it later would
    /// change the chat width and reflow the body under the reader, which is
    /// worse than an empty column for a moment. `Loading` distinguishes "not
    /// here yet" from "nobody here".
    ///
    /// Headings show names, never ids: an 18-digit number tells the reader
    /// nothing, so a role whose name is unknown is skipped.
    ///
    /// Stops at 100 people, which is what the subscription asks for; paging
    /// further is not implemented.
    fn member_list(&self) -> UiNode {
        use gumicord_gateway::MemberRow;

        let guild = GuildId::from(self.selected_guild);
        let Some(list) = self.live.members(guild) else {
            return UiNode::new(NodeId::NavMemberList).with_state(State::Loading);
        };

        let mut out = UiNode::new(NodeId::NavMemberList);
        for row in list.rows() {
            match row {
                MemberRow::Group { id, count } => {
                    let Some(name) = self.group_name(guild, id) else {
                        continue;
                    };
                    out = out.child(UiNode::text(
                        NodeId::NavMemberListGroup,
                        format!("{name} — {count}"),
                    ));
                }
                MemberRow::Member(m) => {
                    let Some(user) = m.member.user.as_ref() else {
                        continue;
                    };
                    let id = user.id.get();

                    // Per-guild name and avatar win; the global ones leave the
                    // reader unable to tell who this is here.
                    let avatar = UiNode::image(
                        NodeId::NavMemberListItemAvatar,
                        m.member
                            .display_avatar(guild, user)
                            .with_size(self.asset_px(SMALL_AVATAR_PX))
                            .url(),
                    )
                    .with_data(id)
                    // Keyed by slot: a fixed set, so themes can style each
                    // one.
                    .child(
                        UiNode::new(NodeId::NavMemberListItemPresence)
                            .with_key(Key::Slot(m.status.as_wire()))
                            .with_data(id),
                    );

                    // The topmost coloured role wins; where it lands is the
                    // theme's call.
                    let tint = self
                        .live
                        .store()
                        .member_tint(guild, &m.member.roles)
                        .map(Color::from_rgb);

                    out = out.child(
                        UiNode::new(NodeId::NavMemberListItem)
                            .with_id_key(id)
                            .with_data(id)
                            .with_state_if(
                                self.hovered_id(NodeId::NavMemberListItem, id),
                                State::Hover,
                            )
                            .child(avatar)
                            .child(
                                UiNode::text(
                                    NodeId::NavMemberListItemName,
                                    m.member.display_name(user).to_owned(),
                                )
                                .with_data(id)
                                .with_tint_opt(tint),
                            ),
                    );
                }
            }
        }

        // Nothing nameable yet.
        if out.children.is_empty() {
            return out.with_state(State::Loading);
        }
        out.children(self.scrollbar(NodeId::NavMemberList))
    }

    /// A heading's name, if it can be resolved.
    fn group_name(&self, guild: GuildId, id: &str) -> Option<Cow<'_, str>> {
        match id {
            "online" => Some(Cow::Borrowed("オンライン")),
            "offline" => Some(Cow::Borrowed("オフライン")),
            // An unresolved role id does not read as a heading.
            other => {
                let role = other.parse::<u64>().ok()?;
                self.live
                    .store()
                    .role_name(guild, RoleId::from(role))
                    .map(Cow::Borrowed)
            }
        }
    }

    fn chat_view(&self) -> UiNode {
        let channels = self.openable_rows();
        let channel = channels
            .iter()
            .find(|c| c.id == self.selected_channel)
            .or(channels.first());

        let (id, name, icon, topic) = match channel {
            Some(c) => (c.id, c.name.clone(), c.icon, c.topic.clone()),
            None => (0, String::new(), "channel.text", None),
        };

        let header = UiNode::new(NodeId::ChatHeader)
            .with_data(id)
            .child(UiNode::icon(NodeId::PrimitiveIcon, icon))
            .child(UiNode::text(NodeId::ChatHeaderTitle, &name).with_data(id))
            .child(UiNode::text(NodeId::ChatHeaderTopic, topic.unwrap_or_default()).with_data(id));

        // The same author in a row skips the header line; the indent is the
        // theme's.
        let rows = self.message_rows();
        let mut messages = UiNode::new(NodeId::ChatMessageList);
        let mut prev: Option<&str> = None;
        for m in &rows {
            messages = messages.child(self.message(m, prev == Some(&*m.author)));
            prev = Some(&m.author);
        }
        messages = messages.children(self.scrollbar(NodeId::ChatMessageList));

        UiNode::new(NodeId::ChatView)
            .child(header)
            .child(messages)
            .child(UiNode::text(
                NodeId::ChatTypingIndicator,
                self.status_line(),
            ))
            .child(
                UiNode::new(NodeId::ChatInput)
                    // What the composer is doing has to be visible: sending a
                    // new message while meaning to edit cannot be undone.
                    .child_if(self.composing != Composing::New, || self.composing_bar())
                    .child(
                        UiNode::editable(
                            NodeId::ChatInputField,
                            Editable {
                                text: self.input.text().to_owned(),
                                caret: self.input.caret(),
                                selection: self.input.selection(),
                                composing: self.input.composing(),
                                placeholder: if name.is_empty() {
                                    "メッセージを送信".to_owned()
                                } else {
                                    format!("#{name} へメッセージを送信")
                                },
                            },
                        )
                        .with_state_if(self.input_focused, State::Focus),
                    ),
            )
    }

    /// Cancels a reply or an edit.
    ///
    /// The draft survives cancelling a reply, which only removed a recipient
    /// from text that is still sendable. It does not survive cancelling an
    /// edit, where the field holds the original message rather than anything
    /// the user wrote.
    fn stop_composing(&mut self) -> bool {
        match self.composing {
            Composing::New => false,
            Composing::Reply(_) => {
                self.composing = Composing::New;
                true
            }
            Composing::Edit(_) => {
                self.composing = Composing::New;
                self.input.take();
                true
            }
        }
    }

    /// The line above the composer, naming who is being replied to. "Replying"
    /// alone stops meaning anything once the list has scrolled.
    fn composing_bar(&self) -> UiNode {
        let (verb, slot) = match self.composing {
            Composing::Reply(_) => ("返信", "reply"),
            Composing::Edit(_) => ("編集", "edit"),
            Composing::New => ("", "none"),
        };
        let who = self
            .composing
            .target()
            .and_then(|id| self.message_rows().into_iter().find(|m| m.id == id))
            .map(|m| m.author);

        let text = match (&self.composing, who) {
            (Composing::Reply(_), Some(a)) => format!("{a} に{verb}中"),
            // A scrolled-away target cannot be resolved; still show the state.
            (Composing::Reply(_), None) => format!("{verb}中"),
            _ => format!("{verb}中"),
        };
        UiNode::new(NodeId::ChatInputToolbar)
            .with_key(Key::Slot(slot))
            .child(UiNode::text(NodeId::PrimitiveText, text).with_key(Key::Slot(slot)))
            // A spacer rather than a written margin, which would stop
            // matching once the theme changes the bar's padding.
            .child(UiNode::new(NodeId::LayoutSpacer))
            // Escape works too, but without a visible way out this looks like
            // a state with no exit.
            .child(
                UiNode::new(NodeId::PrimitiveButton)
                    .with_key(Key::Slot(CANCEL_COMPOSING))
                    .with_state_if(
                        self.is_hovered(
                            NodeId::PrimitiveButton,
                            Some(&Key::Slot(CANCEL_COMPOSING)),
                        ),
                        State::Hover,
                    )
                    .child(UiNode::icon(NodeId::PrimitiveIcon, "close")),
            )
    }

    /// The line below the list. Silent while connected; announcing the normal
    /// case buries the abnormal one.
    fn status_line(&self) -> String {
        if let Some(hint) = self.live.link().hint() {
            return format!("  {hint}");
        }
        if self.uses_live() {
            let channel = ChannelId::from(self.selected_channel);
            if self.live.is_loading(channel) {
                return "  読み込んでいます…".to_owned();
            }
            return typing_line(&self.live.typing_in(channel));
        }
        "  みどり が入力中…".to_owned()
    }

    /// One message. `grouped` drops the avatar and author line; the indent is
    /// the theme's, since a spacer node would bake it in.
    fn message(&self, m: &MessageRow, grouped: bool) -> UiNode {
        let body = UiNode::new(NodeId::LayoutColumn)
            .child_if(!grouped, || {
                UiNode::new(NodeId::ChatMessageHeader)
                    .with_data(m.id)
                    .child(
                        UiNode::text(NodeId::ChatMessageHeaderAuthor, &m.author)
                            .with_data(m.id)
                            .with_tint_opt(m.tint.map(Color::from_rgb)),
                    )
                    .child(
                        UiNode::text(NodeId::ChatMessageHeaderTime, format!("  {}", m.time))
                            .with_data(m.id),
                    )
            })
            .child(self.content_of(m));

        UiNode::new(NodeId::ChatMessage)
            .with_id_key(m.id)
            .with_data(m.id)
            .with_state_if(grouped, State::Grouped)
            .with_state_if(m.mentioned, State::Mentioned)
            .with_state_if(self.hovered_id(NodeId::ChatMessage, m.id), State::Hover)
            .child_if(!grouped, || {
                face(NodeId::ChatMessageAvatar, m.avatar.as_deref(), &m.author).with_data(m.id)
            })
            // Author line and body stacked.
            .child(body)
    }

    /// The body.
    ///
    /// Parsed every frame. Bodies are a few hundred characters and parsing is
    /// linear, so it does not show up in measurements yet. Cache by message id
    /// when it does — but measure first.
    fn content_of(&self, m: &MessageRow) -> UiNode {
        let ink = crate::markdown::Ink::new(
            self.theme.as_ref(),
            self.match_ctx,
            self.revealed.contains(&m.id),
            self.now,
        );
        let names = StoreNames {
            store: self.live.store(),
            guild: GuildId::from(self.selected_guild),
        };
        let node = UiNode::new(NodeId::ChatMessageContent)
            .with_data(m.id)
            .children(ink.blocks(&m.blocks, &names));

        // Record whichever time-dependent part changes soonest.
        if let Some(secs) = ink.holds_for() {
            self.hold(secs);
        }
        node
    }

    /// Accumulates validity; the shortest wins. A `Cell` because building
    /// takes `&self`.
    fn hold(&self, secs: i64) {
        let next = match self.holds.get() {
            Some(cur) => cur.min(secs),
            None => secs,
        };
        self.holds.set(Some(next));
    }
}

/// Resolves ids to names, and says so when it cannot.
struct StoreNames<'a> {
    store: &'a gumicord_store::Store,
    guild: GuildId,
}

impl crate::markdown::Names for StoreNames<'_> {
    fn user(&self, id: u64) -> Option<String> {
        self.store
            .member(self.guild, UserId::from(id))
            // The per-guild name wins, or the reader cannot tell who was
            // mentioned.
            .and_then(|m| {
                m.nick
                    .clone()
                    .or_else(|| m.user.as_ref().map(|u| u.display_name().to_owned()))
            })
    }

    fn channel(&self, id: u64) -> Option<String> {
        self.store
            .channel(ChannelId::from(id))
            .and_then(|c| c.name.clone())
    }

    fn role(&self, id: u64) -> Option<String> {
        self.store
            .role_name(self.guild, RoleId::from(id))
            .map(str::to_owned)
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Display rows
//
//  Demo and live data meet here, so the tree builder never has to ask which
//  one it is holding.
// ═══════════════════════════════════════════════════════════════════════

struct GuildRow {
    id: u64,
    name: String,
    /// Icon URL; the initials stand in when absent.
    icon: Option<String>,
    unread: bool,
    mentions: u32,
    /// The folder id, when this row is a folder header.
    folder_of_own: Option<u64>,
    /// Whether it sits inside a folder; the indent is the theme's.
    in_folder: bool,
    /// Whether the folder is folded.
    collapsed: bool,
    /// The folder's colour; where it lands is the theme's call.
    tint: Option<u32>,
    /// What the folder holds: children when open, tiles when folded.
    members: Vec<GuildRow>,
}

struct ChannelRow {
    id: u64,
    name: String,
    icon: &'static str,
    topic: Option<String>,
    unread: bool,
    mentions: u32,
    /// A category heading; nothing opens.
    category: bool,
}

struct MessageRow {
    id: u64,
    author: String,
    /// Avatar URL; the initials stand in when absent.
    avatar: Option<String>,
    /// The role colour; where it lands is the theme's call.
    tint: Option<u32>,
    time: String,
    /// The parsed body. The raw string is deliberately absent: holding both
    /// invites drawing from the wrong one, and only the reader would notice.
    blocks: Vec<gumicord_markdown::Block>,
    mentioned: bool,
}

impl Gumicord {
    /// Whether real data is being shown. The only place demo and live differ.
    ///
    /// Does not wait for login: a non-empty cache means a previous session on
    /// this account, since signing out deletes it. Waiting would mean about a
    /// second of empty screen.
    fn uses_live(&self) -> bool {
        self.login.session().logged_in().is_some() || !self.live.is_empty()
    }

    /// Whether the main screen may be shown.
    fn shows_main(&self) -> bool {
        self.login.shows_main() || !self.live.is_empty()
    }

    /// 選んでいるものが実在するように直し、要るものを取りに行く。
    ///
    /// ⚠️ **起動直後の選択は demo の ID である。** READY が来た時点で
    /// そんなギルドは無いので、放っておくと一覧だけ出て中身が空になる。
    ///
    /// 戻り値は「画面が変わったか」。取りに行っただけでは変わらない —
    /// 届いたときに [`Live::poll`] が改めて真を返す
    fn sync_selection(&mut self) -> bool {
        if !self.uses_live() {
            return false;
        }
        let mut changed = false;

        let guilds = self.guild_rows();
        if !guilds.iter().any(|g| g.id == self.selected_guild) {
            let Some(first) = guilds.first() else {
                // READY has not arrived; select nothing.
                return false;
            };
            self.selected_guild = first.id;
            self.selected_channel = 0;
            changed = true;
        }

        let channels = self.openable_rows();
        if !channels.iter().any(|c| c.id == self.selected_channel)
            && let Some(first) = channels.first()
        {
            self.selected_channel = first.id;
            changed = true;
        }

        if self.selected_channel != 0 {
            // A no-op after the first call.
            self.live.open_channel(
                GuildId::from(self.selected_guild),
                ChannelId::from(self.selected_channel),
            );
        }
        changed
    }

    fn guild_rows(&self) -> Vec<GuildRow> {
        let bare = |id: u64, name: String, unread: bool, mentions: u32| GuildRow {
            id,
            name,
            icon: None,
            unread,
            mentions,
            folder_of_own: None,
            in_folder: false,
            collapsed: false,
            tint: None,
            members: Vec::new(),
        };

        if !self.uses_live() {
            return demo::GUILDS
                .iter()
                .map(|g| bare(g.id, g.name.to_owned(), g.unread, g.mentions))
                .collect();
        }

        // The store has already handled unavailable guilds and folder
        // nesting; this receives what to show, in order.
        self.live
            .store()
            .guild_entries()
            .into_iter()
            .map(|e| match e {
                GuildEntry::Folder { id, row } => {
                    // Folders roll up their contents, or folding one hides
                    // the unread inside it.
                    let (unread, mentions) = row.guilds.iter().fold((false, 0), |acc, g| {
                        let (u, m) = self.live.store().guild_unread(*g);
                        (acc.0 || u, acc.1 + m)
                    });
                    GuildRow {
                        id,
                        // Unnamed folders borrow their contents' names.
                        name: row.name.clone().unwrap_or_else(|| self.folder_label(row)),
                        // Folders have no icon; the contents are tiled
                        // instead.
                        icon: None,
                        unread,
                        mentions,
                        folder_of_own: Some(id),
                        in_folder: false,
                        collapsed: self.live.store().is_collapsed(id),
                        tint: row.color,
                        members: self.folder_members(row),
                    }
                }
                GuildEntry::Guild { row, folder } => {
                    // Rolled up from the channels inside.
                    let (unread, mentions) = self.live.store().guild_unread(row.id);
                    GuildRow {
                        id: row.id.get(),
                        name: row.name.clone(),
                        icon: self
                            .live
                            .store()
                            .guild_icon(row.id)
                            .map(|a| a.with_size(self.asset_px(GUILD_ICON_PX)).url()),
                        unread,
                        mentions,
                        folder_of_own: None,
                        in_folder: folder.is_some(),
                        collapsed: false,
                        tint: None,
                        members: Vec::new(),
                    }
                }
            })
            .collect()
    }

    /// The heading for an unnamed folder: the guild names inside, as Discord
    /// does, rather than a blank.
    fn folder_label(&self, folder: &gumicord_store::FolderRow) -> String {
        folder
            .guilds
            .iter()
            .filter_map(|id| self.live.store().guild(*id))
            .map(|g| &*g.name)
            .collect::<Vec<_>>()
            .join("、")
    }

    /// The guilds inside a folder, unfiltered: an open folder shows them all,
    /// and the caller decides how many to tile.
    fn folder_members(&self, folder: &gumicord_store::FolderRow) -> Vec<GuildRow> {
        folder
            .guilds
            .iter()
            .filter_map(|id| {
                let g = self.live.store().guild(*id)?;
                Some(GuildRow {
                    id: id.get(),
                    name: g.name.clone(),
                    // Tiles are small, but the request size is unchanged so a
                    // larger copy already fetched can be reused.
                    icon: self
                        .live
                        .store()
                        .guild_icon(*id)
                        .map(|a| a.with_size(self.asset_px(GUILD_ICON_PX)).url()),
                    unread: self.live.store().guild_unread(*id).0,
                    mentions: self.live.store().guild_unread(*id).1,
                    folder_of_own: None,
                    in_folder: true,
                    collapsed: false,
                    tint: None,
                    members: Vec::new(),
                })
            })
            .collect()
    }

    /// Only the rows that can be opened. Categories are headings; treating
    /// one as openable made the default selection open a category nobody
    /// pressed.
    fn openable_rows(&self) -> Vec<ChannelRow> {
        self.channel_rows()
            .into_iter()
            .filter(|c| !c.category)
            .collect()
    }

    fn channel_rows(&self) -> Vec<ChannelRow> {
        if !self.uses_live() {
            return demo::CHANNELS
                .iter()
                .map(|c| ChannelRow {
                    id: c.id,
                    name: c.name.to_owned(),
                    icon: c.icon,
                    topic: Some(
                        "自前レンダラの縦通し。テーマ JSON だけで見た目が決まる".to_owned(),
                    ),
                    unread: c.unread,
                    mentions: c.mentions,
                    category: false,
                })
                .collect();
        }

        // Filtering, ordering and nesting are the store's; reordering per
        // frame could make the order flicker.
        self.live
            .store()
            .entries_of(GuildId::from(self.selected_guild))
            .map(|e| match e {
                ChannelEntry::Category(c) => ChannelRow {
                    id: c.id.get(),
                    name: c.display_name(),
                    icon: "",
                    topic: None,
                    unread: false,
                    mentions: 0,
                    category: true,
                },
                ChannelEntry::Channel(c) => ChannelRow {
                    id: c.id.get(),
                    name: c.display_name(),
                    icon: c.kind.icon(),
                    topic: c.topic.clone(),
                    // No read state yet; do not fake one.
                    unread: self.live.store().is_unread(c.id),
                    mentions: self.live.store().mentions(c.id),
                    category: false,
                },
            })
            .collect()
    }

    fn message_rows(&self) -> Vec<MessageRow> {
        if !self.uses_live() {
            return demo::MESSAGES
                .iter()
                .chain(&self.sent)
                .map(|m| MessageRow {
                    id: m.id,
                    author: m.author.to_string(),
                    avatar: None,
                    tint: None,
                    time: m.time.to_string(),
                    blocks: gumicord_markdown::parse(&m.body),
                    mentioned: m.mentioned,
                })
                .collect();
        }

        let me = self.login.session().logged_in().map(|l| l.me.user.id);
        // Which guild is open is known here, not from the message: REST
        // messages carry no `guild_id`.
        let guild = GuildId::from(self.selected_guild);
        self.live
            .store()
            .messages(ChannelId::from(self.selected_channel))
            .iter()
            .map(|m| {
                // REST messages carry no `member`.
                //
                // Discord attaches it to gateway events only, so fall back to
                // whatever was seen and remembered.
                let member = m
                    .member
                    .as_ref()
                    .or_else(|| self.live.store().member(guild, m.author.id));

                let blocks = gumicord_markdown::parse(&m.content);
                MessageRow {
                    id: m.id.get(),
                    // The per-guild name wins.
                    author: match member {
                        Some(x) => x.display_name(&m.author).to_owned(),
                        None => m.author.display_name().to_owned(),
                    },
                    // Everyone has an avatar: Discord hands out a default, so
                    // showing initials instead would be our invention.
                    avatar: Some(
                        match member {
                            Some(x) => x.display_avatar(guild, &m.author),
                            None => m.author.display_avatar(),
                        }
                        .with_size(self.asset_px(MESSAGE_AVATAR_PX))
                        .url(),
                    ),
                    // The topmost coloured role wins; where it lands is the
                    // theme's call.
                    tint: member.and_then(|x| self.live.store().member_tint(guild, &x.roles)),
                    time: local_time(&m.timestamp),
                    mentioned: m
                        .referenced_message
                        .as_ref()
                        .is_some_and(|r| Some(r.author.id) == me)
                        || calls_me(&blocks, me, member.map(|x| x.roles.as_slice())),
                    blocks,
                }
            })
            .collect()
    }
}

/// Whether a body mentions us.
///
/// Reads the parse, not the raw string: a `<@1>` inside code is not a
/// mention, and matching on text would notify someone for writing about one.
/// Roles count too, or being called by role goes unnoticed.
fn calls_me(
    blocks: &[gumicord_markdown::Block],
    me: Option<UserId>,
    roles: Option<&[RoleId]>,
) -> bool {
    use gumicord_markdown::{Block, InlineKind, Mention};

    fn walk(blocks: &[Block], f: &mut impl FnMut(Mention) -> bool) -> bool {
        blocks.iter().any(|b| match b {
            Block::Paragraph(c) | Block::Heading { content: c, .. } | Block::Subtext(c) => {
                c.iter().any(|i| match &i.kind {
                    InlineKind::Mention(m) => f(*m),
                    _ => false,
                })
            }
            Block::Quote(inner) => walk(inner, f),
            Block::List(items) => items.iter().any(|it| {
                it.content.iter().any(|i| match &i.kind {
                    InlineKind::Mention(m) => f(*m),
                    _ => false,
                })
            }),
            // Not inside code.
            Block::Code { .. } => false,
        })
    }

    walk(blocks, &mut |m| match m {
        Mention::User(id) => Some(UserId::from(id)) == me,
        Mention::Role(id) => roles.is_some_and(|r| r.contains(&RoleId::from(id))),
        Mention::Everyone | Mention::Here => true,
        Mention::Channel(_) => false,
    })
}

/// The first character of a name, shown when there is no icon.
fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

/// Formats an ISO 8601 timestamp as local `HH:MM`.
///
/// Discord returns UTC; showing it unshifted is hours out for most readers.
/// An unparseable value is returned as-is rather than invented.
fn local_time(iso: &str) -> String {
    // "2026-08-22T12:34:56.789000+00:00"
    let Some((_, time)) = iso.split_once('T') else {
        return iso.to_owned();
    };
    let mut parts = time.split(':');
    let (Some(h), Some(m)) = (parts.next(), parts.next()) else {
        return iso.to_owned();
    };
    let (Ok(h), Ok(m)) = (h.parse::<i32>(), m.parse::<i32>()) else {
        return iso.to_owned();
    };

    let total = h * 60 + m + gumicord_platform::local_utc_offset_minutes();
    // `rem_euclid` so a day boundary does not produce a negative remainder.
    let total = total.rem_euclid(24 * 60);
    format!("{:02}:{:02}", total / 60, total % 60)
}

/// A scrollbar. The thumb's size and position come from the renderer, since
/// the overflow is only known after layout. This says only that the list has
/// one; the theme decides how it looks.
fn scrollbar_node() -> UiNode {
    UiNode::new(NodeId::LayoutScrollbar).child(UiNode::new(NodeId::LayoutScrollbarThumb))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> Gumicord {
        Gumicord::demo()
    }

    // ═══════════════════════════════════════════════════════════════
    //  Replying and editing

    /// Sending a new message while meaning to edit cannot be undone, so the
    /// mode has to be visible.
    #[test]
    fn replying_and_editing_are_visible_on_screen() {
        let bar = |c: Composing| {
            let mut a = app();
            a.composing = c;
            let mut out = None;
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                if n.id == NodeId::ChatInputToolbar {
                    out = n.key.clone();
                }
            });
            out
        };
        assert_eq!(bar(Composing::New), None, "何もしていないのに出ている");
        assert_eq!(bar(Composing::Reply(1)), Some(Key::Slot("reply")));
        assert_eq!(bar(Composing::Edit(1)), Some(Key::Slot("edit")));
    }

    /// This happened: a later `primitive.button` rule added horizontal
    /// padding, which pushed the icon outside its 20-square box and left an
    /// empty dark box beside it. Reading the theme's numbers does not catch
    /// it; the laid-out rectangles do.
    #[test]
    fn the_cancel_icon_stays_inside_its_box() {
        let mut a = app();
        a.composing = Composing::Reply(1);
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let placed = gumicord_render::layout_for_test(&tree, cx.viewport);

        let find = |id| {
            placed
                .iter()
                .rev()
                .find(|(i, _)| *i == id)
                .map(|(_, r)| *r)
                .unwrap_or_else(|| panic!("{id:?} が置かれていない"))
        };
        let button = find(NodeId::PrimitiveButton);
        let icon = find(NodeId::PrimitiveIcon);

        assert!(
            button.w > 0.0 && button.h > 0.0,
            "箱が潰れている {button:?}"
        );
        assert!(
            icon.x >= button.x
                && icon.y >= button.y
                && icon.x + icon.w <= button.x + button.w
                && icon.y + icon.h <= button.y + button.h,
            "絵 {icon:?} が箱 {button:?} からはみ出している"
        );
    }

    /// Escape works too, but without a visible way out this looks like a
    /// state with no exit.
    #[test]
    fn a_cancel_button_appears_while_replying() {
        let cancel = |c: Composing| {
            let mut a = app();
            a.composing = c;
            let mut found = false;
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                found |=
                    n.id == NodeId::PrimitiveButton && n.key == Some(Key::Slot(CANCEL_COMPOSING));
            });
            found
        };
        assert!(!cancel(Composing::New), "何もしていないのに出ている");
        assert!(cancel(Composing::Reply(1)));
        assert!(cancel(Composing::Edit(1)));
    }

    /// ⚠️ **返信と編集で、打った文字の扱いが違う。**
    ///
    /// 返信をやめたときに打った文字まで消すと、宛先を外しただけのつもりが
    /// 書いたものごと消える。編集の入力欄にあるのは元の発言の中身であって、
    /// 利用者が書いたものではない
    #[test]
    fn cancelling_a_reply_keeps_the_draft_but_cancelling_an_edit_clears_it() {
        let press = |c: Composing| {
            let mut a = app();
            a.composing = c;
            a.input.insert("書いた文");
            let hits = [hit_of(
                NodeId::PrimitiveButton,
                Some(Key::Slot(CANCEL_COMPOSING)),
            )];
            assert!(a.pressed(&hits), "何も起きなかった");
            assert_eq!(a.composing, Composing::New, "やめていない");
            a.input.text().to_owned()
        };
        assert_eq!(press(Composing::Reply(1)), "書いた文", "返信で消えた");
        assert_eq!(press(Composing::Edit(1)), "", "編集で残った");
    }

    /// ⚠️ **他のボタンで取り消してはいけない。**
    ///
    /// `Key::Slot(CANCEL_COMPOSING)` の定数がパターンではなく**束縛**として
    /// 読まれると、あらゆる `primitive.button` がここへ落ちる。
    /// 見た目は同じに動くので、押すまで気づけない
    #[test]
    fn another_button_does_not_cancel() {
        let mut a = app();
        a.composing = Composing::Reply(1);
        let hits = [hit_of(NodeId::PrimitiveButton, Some(Key::Slot("その他")))];

        a.pressed(&hits);
        assert_eq!(a.composing, Composing::Reply(1), "別のボタンで取り消された");
    }

    /// ⚠️ **Esc は書きかけを捨てる前に、返信・編集をやめる。**
    ///
    /// 一度に両方起きると、打った文字が消えたのか宛先が消えたのかが
    /// 分からない
    #[test]
    fn esc_は返信をやめてから閉じる() {
        let mut a = app();
        a.input_focused = true;
        a.composing = Composing::Reply(1);
        a.input.insert("書きかけ");

        assert!(a.cancel_input());
        assert_eq!(a.composing, Composing::New, "返信のままである");
        assert!(a.input_focused, "フォーカスまで外れた");
    }

    /// ⚠️ **書き換えで空にするのは「消す」ではない。**
    ///
    /// うっかり全部消して Enter を押したときに発言が消えるのは、
    /// 取り返しがつかない
    #[test]
    fn submitting_an_empty_field_does_nothing() {
        let mut a = app();
        a.composing = Composing::Edit(1);
        assert!(!a.submit());
        assert_eq!(a.composing, Composing::Edit(1), "編集をやめてしまった");
    }

    /// 送ったら新規に戻ること。**戻らないと次の発言まで返信になる**
    #[test]
    fn submitting_returns_to_composing_a_new_message() {
        let mut a = app();
        a.composing = Composing::Reply(1);
        a.input.insert("やあ");
        assert!(a.submit());
        assert_eq!(a.composing, Composing::New);
    }

    /// ⚠️ **他人の発言に編集と削除を出さない。**
    ///
    /// サーバが 403 を返すだけである。押せる場所に出さないのが先で、
    /// サーバの拒否はその後ろの守りである
    #[test]
    fn someone_elses_message_offers_neither_edit_nor_delete() {
        use crate::menu::Action;
        // demo ではログインしていないので、誰の発言も自分のものではない
        let a = app();
        let items = a.message_menu(1);
        assert!(
            !items
                .iter()
                .any(|i| matches!(i.action, Action::Edit(_) | Action::Delete(_))),
            "他人の発言に編集か削除が出ている"
        );
        // 返信は誰の発言にも出る
        assert!(items.iter().any(|i| matches!(i.action, Action::Reply(_))));
    }

    /// ⚠️ **入力欄を先に見る。** 発言の一覧の上に重なっているので、
    /// 後ろに置くと入力欄の上で押しても発言のメニューが出る
    #[test]
    fn the_input_field_gets_the_input_menu() {
        use crate::menu::Action;
        let mut a = app();
        let hits = [
            hit_of(NodeId::ChatInputField, None),
            hit_of(NodeId::ChatMessage, Some(Key::Id(1))),
        ];
        assert!(a.context_menu(&hits, (0.0, 0.0)));

        let items = a.floating.as_ref().expect("開いていない").items();
        assert!(
            items.iter().any(|i| i.action == Action::Paste),
            "発言のメニューが出ている"
        );
    }

    /// ⚠️ **できないものを並べない。** 何も選んでいないのに「コピー」を
    /// 出しても、押して何も起きないだけである
    #[test]
    fn cut_and_copy_are_absent_without_a_selection() {
        use crate::menu::Action;
        let mut a = app();
        let has = |a: &Gumicord, want: Action| a.field_menu().iter().any(|i| i.action == want);

        assert!(!has(&a, Action::CopySelection));
        assert!(!has(&a, Action::SelectAll), "空なのに全選択が出ている");
        assert!(has(&a, Action::Paste), "貼り付けはいつでも出る");

        a.input.insert("あいう");
        assert!(has(&a, Action::SelectAll));
        assert!(!has(&a, Action::CopySelection), "まだ選んでいない");

        a.input.select_all();
        assert!(has(&a, Action::CopySelection));
        assert!(has(&a, Action::Cut));
    }

    // ═══════════════════════════════════════════════════════════════
    //  コンテクストメニュー (`FR-024`, `FR-028` の受け皿)

    fn hit_of(id: NodeId, key: Option<Key>) -> Hit {
        Hit {
            id,
            key,
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        }
    }

    fn with_menu() -> Gumicord {
        let mut a = app();
        let msg = hit_of(NodeId::ChatMessage, Some(Key::Id(1)));
        assert!(
            a.context_menu(std::slice::from_ref(&msg), (10.0, 20.0)),
            "メニューが開かなかった"
        );
        a
    }

    #[test]
    fn right_clicking_a_message_opens_the_menu() {
        let a = with_menu();
        assert!(a.floating.is_some());
        assert_eq!(
            a.floating.as_ref().and_then(|f| match f {
                crate::menu::Floating::Menu(m) => Some(m.at),
                _ => None,
            }),
            Some((10.0, 20.0))
        );
    }

    /// ⚠️ **開いている間は下へ渡さない。**
    ///
    /// 渡すと、メニューを閉じるつもりで押した場所のチャンネルへ移動する。
    /// 押した場所は当たり判定としては両方に掛かっているので、こちらが
    /// 「上が開いているなら上だけ」と決めなければ素通りする
    #[test]
    fn nothing_underneath_is_reachable_while_the_menu_is_open() {
        let mut a = with_menu();
        let before = a.selected_channel;
        // 下にあるチャンネルへの当たりを一緒に渡す
        let hits = [hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))];

        assert!(a.pressed(&hits), "閉じるという変化はある");
        assert!(a.floating.is_none(), "閉じていない");
        assert_eq!(a.selected_channel, before, "下のチャンネルへ移動した");
    }

    /// 項目を押したら、その操作が走って閉じる。
    ///
    /// ⚠️ **試験でクリップボードへ書かない。** 走らせた人の手元の
    /// コピー内容が消える。ここで見たいのは「押したら閉じる」だけである
    #[test]
    fn choosing_an_item_closes_the_menu() {
        let mut a = with_menu();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(
                crate::menu::Action::MarkRead(1),
                "既読にする",
            )],
        }));
        let hits = [hit_of(NodeId::OverlayMenuItem, Some(Key::Index(0)))];
        assert!(a.pressed(&hits));
        assert!(a.floating.is_none());
    }

    /// ⚠️ **Esc はメニューが先である。** 入力欄のフォーカスより手前に
    /// 浮かんでいるので、そこまで届いてはいけない
    #[test]
    fn esc_はメニューを先に閉じる() {
        let mut a = with_menu();
        a.input_focused = true;

        assert!(a.cancel_input(), "何も起きなかった");
        assert!(a.floating.is_none(), "メニューが閉じていない");
        assert!(a.input_focused, "入力欄のフォーカスまで外れた");

        assert!(a.cancel_input(), "2 回目でフォーカスが外れていない");
        assert!(!a.input_focused);
    }

    // ═══════════════════════════════════════════════════════════════
    //  Signing out

    /// Signing out is destructive and hard to reverse without a phone, so it
    /// goes through the same dialog as deleting.
    #[test]
    fn logging_out_asks_first() {
        let mut a = app();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(
                crate::menu::Action::LogOut,
                "ログアウト",
            )],
        }));
        press_menu(&mut a, 0);
        assert!(is_confirm(&a), "no confirmation appeared");
    }

    /// The dialog has to say the phone is needed, since password login does
    /// not exist yet.
    #[test]
    fn the_logout_dialog_says_a_phone_is_needed() {
        let a = app();
        let c = a
            .needs_confirming(
                &crate::menu::Floating::Menu(crate::menu::Menu {
                    at: (0.0, 0.0),
                    items: Vec::new(),
                }),
                &crate::menu::Action::LogOut,
            )
            .expect("log out should be confirmed");
        assert!(c.danger);
        assert!(c.body.contains("QR"), "does not mention the QR: {}", c.body);
    }

    /// Only offered while signed in; there is nothing to sign out of otherwise.
    #[test]
    fn the_user_menu_is_empty_when_signed_out() {
        assert!(app().user_menu().is_empty());
    }

    /// Demo mode has no runtime and nothing to sign out of.
    #[test]
    fn signing_out_without_a_runtime_does_nothing() {
        let mut a = app();
        assert!(!a.sign_out());
    }

    // ═══════════════════════════════════════════════════════════════
    //  時間で変わる表示 (`<t:…:R>`, `NFR-005`)

    fn built(a: &mut Gumicord) {
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        a.build(&cx);
    }

    /// ⚠️ **相対表示が 1 つも無ければ寝たままでよい。**
    ///
    /// 起きる理由が無いのに `WaitUntil` を置くと、何も変わらないのに
    /// 回り続ける (`NFR-005`)
    #[test]
    fn nothing_relative_means_no_wake_up() {
        let mut a = app();
        built(&mut a);
        assert_eq!(a.next_frame_in(), None);
    }

    /// ⚠️ **時間で変わるものがあれば、変わる頃に起き直す。**
    ///
    /// 出したきり寝ると、開きっぱなしの画面で「たった今」が何時間も残る
    #[test]
    fn a_relative_timestamp_asks_for_a_later_frame() {
        let mut a = app();
        // ⚠️ **時計を読んだ結果に合わせる。** 決め打ちの時刻を書くと、
        // 走らせるたびに「何年前」の側へ落ちる
        let at = gumicord_platform::now_unix() - 90;
        a.sent.push(demo::Message {
            id: 9_999,
            author: Cow::Borrowed("ねんねこ"),
            time: Cow::Borrowed("たった今"),
            body: Cow::Owned(format!("<t:{at}:R>")),
            mentioned: false,
        });
        built(&mut a);

        let d = a.next_frame_in().expect("起き直しを頼んでいない");
        assert!(
            d.as_secs() >= 1 && d.as_secs() <= 60,
            "分の切れ目のはずが {d:?}"
        );
    }

    // ═══════════════════════════════════════════════════════════════
    //  削除の前に確かめる (`FR-024`)

    /// 「削除」1 項目だけのメニューを開いた状態。
    ///
    /// ⚠️ **`message_menu` を通さない。** あちらは自分の発言かどうかを
    /// 見るので、ログインしていない demo では削除が並ばない。ここで
    /// 見たいのは並び方ではなく、押した後に何が起きるかである
    fn with_delete_menu() -> Gumicord {
        let mut a = app();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(crate::menu::Action::Delete(1), "削除").danger()],
        }));
        // 消えたことを外から見るための印。**確かめた後にだけ消える**
        a.composing = Composing::Edit(1);
        a
    }

    fn press_menu(a: &mut Gumicord, index: u32) -> bool {
        a.pressed(&[hit_of(NodeId::OverlayMenuItem, Some(Key::Index(index)))])
    }

    fn press_button(a: &mut Gumicord, index: usize) -> bool {
        a.pressed(&[hit_of(
            NodeId::OverlayModalAction,
            Some(Key::Index(index as u32)),
        )])
    }

    fn is_confirm(a: &Gumicord) -> bool {
        matches!(a.floating, Some(crate::menu::Floating::Confirm(_)))
    }

    /// ⚠️ **メニューの「削除」を押しただけでは消えない。**
    ///
    /// メニューの中に埋もれた 1 行で、押した瞬間に消えるのは危うい。
    /// 隣の項目と 1 行しか離れておらず、消した発言は戻せない
    #[test]
    fn one_press_of_delete_does_not_delete() {
        let mut a = with_delete_menu();
        assert!(press_menu(&mut a, 0));
        assert!(is_confirm(&a), "確認の窓が出ていない");
        assert_eq!(a.composing, Composing::Edit(1), "確かめる前に消えている");
    }

    /// やめたら何も起きない。**窓も閉じる**
    #[test]
    fn cancelling_the_dialog_does_nothing() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(press_button(&mut a, crate::menu::button::CANCEL));
        assert!(a.floating.is_none(), "窓が閉じていない");
        assert_eq!(a.composing, Composing::Edit(1), "やめたのに消えている");
    }

    /// 確かめたら進む。**ここで初めて消える**
    #[test]
    fn confirming_the_dialog_deletes() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(press_button(&mut a, crate::menu::button::CONFIRM));
        assert!(a.floating.is_none(), "窓が閉じていない");
        // 書き換えている最中に消したら、書き換えもやめる
        assert_eq!(a.composing, Composing::New, "消えていない");
    }

    /// ⚠️ **窓の外を押しても閉じない。**
    ///
    /// メニューは押し間違えても閉じるだけだが、窓は「まだ決めていない」
    /// ことを示している。外を押して消えると、決めたのか消えたのかが
    /// 分からない
    #[test]
    fn clicking_outside_does_not_close_the_dialog() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(
            !a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]),
            "何かが変わってしまった"
        );
        assert!(is_confirm(&a), "外を押しただけで窓が消えた");
    }

    /// Esc なら閉じる。**閉じ方が 1 つも無いのは行き止まりである**
    #[test]
    fn escape_closes_the_dialog() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(a.cancel_input());
        assert!(a.floating.is_none(), "Esc で閉じない");
        assert_eq!(a.composing, Composing::Edit(1), "Esc で消えている");
    }

    /// ⚠️ **窓から来たものにもう一度窓を挟まない。** 挟むと窓が出続けて
    /// 永久に進めない
    #[test]
    fn the_dialog_does_not_reappear() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        press_button(&mut a, crate::menu::button::CONFIRM);
        assert!(a.floating.is_none(), "窓がもう一度出ている");
    }

    /// 窓が出ているあいだは、下のチャンネルへ届かない
    #[test]
    fn nothing_underneath_is_reachable_while_the_dialog_is_open() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        let before = a.selected_channel;

        a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]);
        assert_eq!(a.selected_channel, before, "下のチャンネルへ移動した");
    }

    /// ⚠️ **戻せない操作以外に窓を挟まない。** 何を押しても窓が出ると、
    /// 窓そのものが読まれなくなる
    #[test]
    fn a_reversible_action_gets_no_dialog() {
        let mut a = app();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(
                crate::menu::Action::MarkRead(1),
                "既読にする",
            )],
        }));
        press_menu(&mut a, 0);
        assert!(a.floating.is_none(), "既読にするだけで窓が出た");
    }

    /// 窓を置いた結果の矩形。
    ///
    /// ⚠️ **テーマの数値を読んでも、置いた結果は分からない。**
    /// `cd72d6f` の ✕ はこれで見つかった
    fn placed_confirm(w: f32, h: f32) -> Vec<(NodeId, gumicord_render::Rect)> {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        assert!(is_confirm(&a));

        // ⚠️ demo には本文が無いので、出す 1 行をここで入れる。
        // **無いまま測ると、一番幅を食う行を測り損ねる**
        if let Some(crate::menu::Floating::Confirm(c)) = &mut a.floating {
            c.preview = crate::menu::preview_line(
                "おはようございます。今日はよろしくお願いします。長めの本文です",
            );
        }

        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(w, h),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        gumicord_render::layout_for_test(&tree, cx.viewport)
    }

    fn all_of(
        placed: &[(NodeId, gumicord_render::Rect)],
        id: NodeId,
    ) -> Vec<gumicord_render::Rect> {
        placed
            .iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, r)| *r)
            .collect()
    }

    fn one_of(placed: &[(NodeId, gumicord_render::Rect)], id: NodeId) -> gumicord_render::Rect {
        let all = all_of(placed, id);
        assert_eq!(all.len(), 1, "{id:?} が {} 個ある", all.len());
        all[0]
    }

    fn contains(outer: gumicord_render::Rect, inner: gumicord_render::Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.w <= outer.x + outer.w
            && inner.y + inner.h <= outer.y + outer.h
    }

    /// ⚠️ **窓の中身が窓からはみ出さないこと。**
    ///
    /// はみ出したボタンは「出ているのに押せない」という壊れ方をする
    #[test]
    fn the_dialog_contents_stay_inside_it() {
        let placed = placed_confirm(1280.0, 800.0);
        let modal = one_of(&placed, NodeId::OverlayModal);
        assert!(modal.w > 0.0 && modal.h > 0.0, "窓が潰れている {modal:?}");

        for id in [
            NodeId::OverlayModalTitle,
            NodeId::OverlayModalBody,
            NodeId::OverlayModalPreview,
            NodeId::OverlayModalActions,
        ] {
            let r = one_of(&placed, id);
            assert!(contains(modal, r), "{id:?} {r:?} が窓 {modal:?} から出た");
        }

        let buttons = all_of(&placed, NodeId::OverlayModalAction);
        assert_eq!(buttons.len(), 2, "ボタンが 2 つ無い");
        for b in &buttons {
            assert!(b.w > 0.0 && b.h > 0.0, "ボタンが潰れている {b:?}");
            assert!(contains(modal, *b), "ボタン {b:?} が窓 {modal:?} から出た");
        }

        // ⚠️ **文字がボタンからはみ出さないこと。** はみ出すと、押せる
        // 場所と読める場所がずれる
        for (label, button) in all_of(&placed, NodeId::OverlayModalActionLabel)
            .iter()
            .zip(&buttons)
        {
            assert!(
                contains(*button, *label),
                "文字 {label:?} がボタン {button:?} から出た"
            );
        }
    }

    /// ⚠️ **2 つのボタンが重ならないこと。** 重なると、上のボタンしか
    /// 押せないのに下のボタンも見えている状態になる
    #[test]
    fn the_two_buttons_do_not_overlap() {
        let placed = placed_confirm(1280.0, 800.0);
        let b = all_of(&placed, NodeId::OverlayModalAction);
        assert_eq!(b.len(), 2);
        let (left, right) = (b[0], b[1]);
        assert!(
            left.x + left.w <= right.x + 0.01,
            "やめる {left:?} と 削除する {right:?} が重なっている"
        );
    }

    /// 窓は押した場所ではなく**画面の真ん中**に出る
    #[test]
    fn the_dialog_is_centred_on_screen() {
        let (w, h) = (1280.0, 800.0);
        let modal = one_of(&placed_confirm(w, h), NodeId::OverlayModal);
        let cx = modal.x + modal.w / 2.0;
        let cy = modal.y + modal.h / 2.0;
        assert!((cx - w / 2.0).abs() < 1.0, "横にずれている {modal:?}");
        assert!((cy - h / 2.0).abs() < 1.0, "縦にずれている {modal:?}");
    }

    /// ⚠️ **狭い窓でも入りきること。** 携帯の幅で画面からはみ出すと、
    /// 「やめる」に手が届かなくなる
    #[test]
    fn it_fits_in_a_narrow_window() {
        let (w, h) = (400.0, 700.0);
        let placed = placed_confirm(w, h);
        let modal = one_of(&placed, NodeId::OverlayModal);
        assert!(
            modal.x >= 0.0 && modal.x + modal.w <= w + 0.01,
            "画面から出た {modal:?} (幅 {w})"
        );
        assert!(
            modal.y >= 0.0 && modal.y + modal.h <= h + 0.01,
            "画面から出た {modal:?} (高さ {h})"
        );
        for b in all_of(&placed, NodeId::OverlayModalAction) {
            assert!(contains(modal, b), "ボタン {b:?} が窓 {modal:?} から出た");
        }
    }

    /// ⚠️ **開いていないときは層を載せない。**
    ///
    /// 常に載せると、窓いっぱいの層が当たりを受け止め続けて何も押せなくなる
    #[test]
    fn no_overlay_layer_is_built_while_nothing_is_open() {
        let has_layer = |a: &Gumicord| {
            let mut found = false;
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                found |= n.id == NodeId::OverlayLayer;
            });
            found
        };
        assert!(!has_layer(&app()));
        assert!(has_layer(&with_menu()));
    }

    /// 何も無いところで押したら、開いていたものを閉じるだけ
    #[test]
    fn right_clicking_empty_space_closes_the_menu() {
        let mut a = with_menu();
        assert!(a.context_menu(&[], (0.0, 0.0)));
        assert!(a.floating.is_none());
    }

    /// ⚠️ **端末の種類ではなく幅で決める。** 窓を狭くした机の上でも、
    /// 指の下にメニューが出るより下から出たほうが読める
    #[test]
    fn a_narrow_window_presents_the_menu_as_a_sheet() {
        use crate::menu::Present;
        assert_eq!(Panes::One.present(), Present::Sheet);
        assert_eq!(Panes::Two.present(), Present::Popover);
        assert_eq!(Panes::Four.present(), Present::Popover);
    }

    /// ⚠️ **コードの中の `<@1>` は呼びかけではない。**
    ///
    /// 素の文字列を `contains` で調べる実装だと、`` `<@1>` `` と書いた
    /// だけで相手に通知が飛ぶ。ここは解析した結果を見ている (`FR-022`)
    #[test]
    fn a_mention_inside_code_is_not_a_mention() {
        let me = Some(UserId::from(1));
        let call = |src: &str| calls_me(&gumicord_markdown::parse(src), me, None);

        assert!(call("やあ <@1>"));
        assert!(!call("`<@1>` と書くと呼べる"));
        assert!(!call(
            "```
<@1>
```"
        ));
        // 別人は呼ばない
        assert!(!call("やあ <@2>"));
    }

    /// ⚠️ **役職も見る。** `@everyone` だけを見ていると、自分の役職が
    /// 呼ばれたときに気づけない
    #[test]
    fn a_mention_of_our_own_role_counts() {
        let me = Some(UserId::from(1));
        let roles = [RoleId::from(9)];
        let call =
            |src: &str, r: Option<&[RoleId]>| calls_me(&gumicord_markdown::parse(src), me, r);

        assert!(call("<@&9> 集合", Some(&roles)));
        assert!(!call("<@&8> 集合", Some(&roles)));
        assert!(!call("<@&9> 集合", None));
        assert!(call("@everyone", None));
        assert!(call("@here", None));
        // チャンネルは呼びかけではない
        assert!(!call("<#9> を見て", Some(&roles)));
    }

    /// 引用と箇条書きの中の呼びかけも拾うこと
    #[test]
    fn a_nested_mention_is_found() {
        let me = Some(UserId::from(1));
        let call = |src: &str| calls_me(&gumicord_markdown::parse(src), me, None);
        assert!(call("> やあ <@1>"));
        assert!(call("- やあ <@1>"));
        assert!(call("# やあ <@1>"));
    }
    /// 同梱テーマが常に読める。ここが壊れると起動して真っ黒になる
    #[test]
    fn the_bundled_theme_parses() {
        let result = Theme::parse(DEFAULT_THEME);
        let errors: Vec<_> = result.errors().collect();
        assert!(errors.is_empty(), "同梱テーマに誤りがある: {errors:?}");
        assert!(result.is_applied());
    }

    /// 木が組め、選択状態が反映される
    #[test]
    fn the_tree_reflects_the_selection() {
        let mut a = app();
        a.selected_channel = demo::CHANNELS[2].id;
        let tree = a.build_tree(Panes::Three);

        let mut selected = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::NavChannelListItem && n.states.contains(State::Selected) {
                selected.push(n.key.clone());
            }
        });
        assert_eq!(selected, vec![Some(Key::Id(demo::CHANNELS[2].id))]);
    }

    /// テーマ解決まで通すと、`app.window` に背景が付く
    #[test]
    fn theme_reaches_the_tree() {
        let mut a = app();
        let cx = FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        let tree = a.build(&cx);
        let window = &tree.children[0];
        assert_eq!(window.id, NodeId::AppWindow);
        assert!(
            window.style.background.is_some(),
            "app.window に背景が解決されていない"
        );
        // 継承が末端まで届いている
        let title = &window.children[0].children[0];
        assert_eq!(title.id, NodeId::ChromeTitlebarTitle);
        assert!(title.style.color.is_some(), "文字色が継承されていない");
    }

    /// 押しても何も変わらないなら、再描画を要求しない
    #[test]
    fn pressing_the_current_channel_changes_nothing() {
        let mut a = app();
        let hit = Hit {
            id: NodeId::NavChannelListItem,
            key: Some(Key::Id(a.selected_channel)),
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        };
        assert!(!a.pressed(std::slice::from_ref(&hit)));
    }
}

#[cfg(test)]
mod responsive_tests {
    use super::*;

    fn panes_in(tree: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| {
            if matches!(
                n.id,
                NodeId::NavGuildList | NodeId::NavChannelList | NodeId::ChatView
            ) {
                out.push(n.id);
            }
        });
        out
    }

    /// 幅で段が切り替わること。境界のちょうどの値は広いほうに入る
    #[test]
    fn panes_are_chosen_by_width() {
        assert_eq!(Panes::for_width(1600.0), Panes::Four);
        assert_eq!(Panes::for_width(1140.0), Panes::Four);
        // ⚠️ **メンバー一覧が真っ先に消える。** 誰が居るかは、
        // 何が書いてあるかより後で構わない
        assert_eq!(Panes::for_width(1139.0), Panes::Three);
        assert_eq!(Panes::for_width(900.0), Panes::Three);
        assert_eq!(Panes::for_width(899.0), Panes::Two);
        assert_eq!(Panes::for_width(600.0), Panes::Two);
        assert_eq!(Panes::for_width(599.0), Panes::One);
        assert_eq!(Panes::for_width(320.0), Panes::One);
    }

    /// 狭くしても**チャットは必ず残る**。
    /// 何も出ない幅があってはいけない
    #[test]
    fn the_chat_view_never_disappears() {
        let a = Gumicord::demo();
        for w in [320.0, 599.0, 600.0, 899.0, 900.0, 1920.0] {
            let tree = a.build_tree(Panes::for_width(w));
            assert!(
                panes_in(&tree).contains(&NodeId::ChatView),
                "幅 {w} でチャットが消えた"
            );
        }
    }

    #[test]
    fn narrower_windows_drop_panes_from_the_left() {
        let a = Gumicord::demo();

        assert_eq!(
            panes_in(&a.build_tree(Panes::Three)),
            vec![
                NodeId::NavGuildList,
                NodeId::NavChannelList,
                NodeId::ChatView
            ]
        );
        assert_eq!(
            panes_in(&a.build_tree(Panes::Two)),
            vec![NodeId::NavChannelList, NodeId::ChatView]
        );
        assert_eq!(panes_in(&a.build_tree(Panes::One)), vec![NodeId::ChatView]);
    }
}

#[cfg(test)]
mod input_tests {
    use gumicord_uitree::Content;

    use super::*;

    fn cx() -> FrameCx {
        FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        }
    }

    fn field(tree: &UiNode) -> Editable {
        let mut found = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatInputField
                && let Content::Editable(e) = &n.content
            {
                found = Some(e.clone());
            }
        });
        found.expect("入力欄が見つからない")
    }

    /// 本文を読める文字列に戻す。
    ///
    /// ⚠️ **飾りは落ちる。** 本文は `chat.message.content` の下に積まれた
    /// 段落の並びであり、段落の中は走りの並びである (`FR-021`)。
    /// ここが見ているのは「何が書いてあるか」だけで、どう飾られているかは
    /// 見ていない
    fn bodies(tree: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id != NodeId::ChatMessageContent {
                return;
            }
            let mut text = String::new();
            n.walk(&mut |c, _| {
                if let Some(spans) = c.content.as_rich() {
                    for sp in spans {
                        text.push_str(&sp.text);
                    }
                } else if let Some(s) = c.content.as_text() {
                    text.push_str(s);
                }
            });
            out.push(text);
        });
        out
    }

    /// フォーカスが無いと入力は届かない (`PLT-001`)
    #[test]
    fn input_only_reaches_a_focused_field() {
        let mut a = Gumicord::demo();
        assert!(a.focused_document().is_none());

        a.input_focused = true;
        assert!(a.focused_document().is_some());
    }

    /// 変換中の範囲が UITree まで届く。**下線を描くのに要る**
    #[test]
    fn a_composition_reaches_the_tree() {
        let mut a = Gumicord::demo();
        a.input_focused = true;

        let doc = a.focused_document().unwrap();
        doc.insert("送信: ");
        doc.set_composition("にほんご", None);

        let f = field(&a.build(&cx()));
        assert_eq!(f.text, "送信: にほんご");
        assert_eq!(
            f.composing,
            Some("送信: ".len().."送信: にほんご".len()),
            "変換中の範囲が伝わっていない"
        );
        assert_eq!(f.caret, f.text.len());
    }

    /// 空なら placeholder が入り、**変換の印は出ない**
    #[test]
    fn an_empty_field_shows_only_its_placeholder() {
        let mut a = Gumicord::demo();
        let f = field(&a.build(&cx()));
        assert!(f.text.is_empty());
        assert!(f.placeholder.contains("メッセージを送信"));
        assert!(f.composing.is_none());
    }

    /// FR-024: Enter で送ると一覧に増え、入力欄は空になる
    #[test]
    fn submitting_appends_the_message_and_clears_the_field() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        a.focused_document().unwrap().insert("こんにちは");

        let before = bodies(&a.build(&cx())).len();
        assert!(a.submit());

        let tree = a.build(&cx());
        let after = bodies(&tree);
        assert_eq!(after.len(), before + 1);
        assert_eq!(after.last().map(String::as_str), Some("こんにちは"));
        assert!(field(&tree).text.is_empty(), "入力欄が空になっていない");
    }

    /// 空白だけのものは送らない
    #[test]
    fn whitespace_is_not_submitted() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        a.focused_document().unwrap().insert("   ");
        assert!(!a.submit());
    }

    /// Esc でフォーカスが外れる。**変換中の Esc は取り消しであって、
    /// フォーカス外しではない** — その分岐はプラットフォーム層が持つ
    #[test]
    fn escape_leaves_the_field() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        assert!(a.cancel_input());
        assert!(!a.input_focused);
        assert!(!a.cancel_input(), "既に外れていれば何も起きない");
    }
}

#[cfg(test)]
mod login_tests {
    use super::session::{Login, LoginEvent};
    use super::*;

    /// まだログインしていないアプリ。
    ///
    /// ⚠️ `Gumicord::new()` は `GUMICORD_SKIP_LOGIN` を読む。**開発機の
    /// 環境変数で試験の結果が変わってはいけない**ので、ここで潰す
    fn pending() -> Gumicord {
        Gumicord::with(Login::fresh_for_test(), Live::without_cache())
    }

    fn ids(tree: &UiNode) -> Vec<NodeId> {
        let mut out = Vec::new();
        tree.walk(&mut |n, _| out.push(n.id));
        out
    }

    /// **ログインしていなければメイン画面は組み立てもしない。**
    /// 中身が見えているのに触れない状態が一番たちが悪い
    #[test]
    fn the_main_screen_is_not_built_before_login() {
        let a = pending();
        let seen = ids(&a.build_tree(Panes::Three));

        assert!(seen.contains(&NodeId::AppScreenLogin));
        assert!(!seen.contains(&NodeId::AppScreenMain));
        assert!(!seen.contains(&NodeId::ChatMessageList), "本文が漏れている");
    }

    /// QR が来る前に QR ノードを出さない。
    /// **読めない QR を見せるのは、何も見せないより悪い**
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

    /// 進み具合が必ず文字で出ている。**黙って止まって見える状態を作らない**
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

    /// テーマがログイン画面まで届く。**QR の地は必ず明るい**
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

    /// `GUMICORD_SKIP_LOGIN` 相当ならメイン画面が出る
    #[test]
    fn skipping_shows_the_main_screen() {
        let a = Gumicord::demo();
        assert!(a.login.shows_main());
        assert!(ids(&a.build_tree(Panes::Three)).contains(&NodeId::AppScreenMain));
    }

    /// 未ログインでも `Login::new` が勝手に走り出さない (試験が網を叩かない)
    #[test]
    fn nothing_starts_until_start_is_called() {
        let login = Login::fresh_for_test();
        assert!(!login.shows_main());
        assert!(login.session().qr().is_none());
    }
}

/// 「入力中」の一行を組み立てる。
///
/// ⚠️ **名前を全部並べない。** 賑やかなサーバでは 10 人が同時に打つことが
/// あり、そのまま並べると一行に収まらず、他の表示を押し出す。
/// Discord も 3 人までで打ち切る。
fn typing_line(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [a] => format!("  {a} が入力中…"),
        [a, b] => format!("  {a} と {b} が入力中…"),
        [a, b, c] => format!("  {a}、{b}、{c} が入力中…"),
        [a, b, rest @ ..] => format!("  {a}、{b} ほか {} 人が入力中…", rest.len()),
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    /// 誰も打っていなければ**何も出さない**
    #[test]
    fn nobody_typing_says_nothing() {
        assert_eq!(typing_line(&[]), "");
    }

    #[test]
    fn one_and_two_and_three_are_named() {
        assert_eq!(typing_line(&["あ"]), "  あ が入力中…");
        assert_eq!(typing_line(&["あ", "い"]), "  あ と い が入力中…");
        assert_eq!(typing_line(&["あ", "い", "う"]), "  あ、い、う が入力中…");
    }

    /// ⚠️ **賑やかなサーバでも一行に収まる。**
    /// 全部並べると他の表示を押し出す
    #[test]
    fn a_crowd_is_summarised() {
        let many = ["あ", "い", "う", "え", "お", "か"];
        assert_eq!(typing_line(&many), "  あ、い ほか 4 人が入力中…");
    }
}

#[cfg(test)]
mod user_panel_tests {
    use super::*;

    fn names(node: &UiNode) -> Vec<NodeId> {
        fn walk(n: &UiNode, out: &mut Vec<NodeId>) {
            out.push(n.id);
            for c in &n.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        walk(node, &mut out);
        out
    }

    /// ⚠️ **ログインしていなければ出さない。**
    ///
    /// 誰でもない自分を出す意味がない
    #[test]
    fn there_is_no_panel_before_logging_in() {
        let a = Gumicord::demo();
        assert!(a.user_panel().is_none());
        let side = a.sidebar(Panes::Three).unwrap();
        assert!(!names(&side).contains(&NodeId::NavUserPanel));
    }

    /// ⚠️ **自分の欄はサーバ一覧の分までまたがる。**
    ///
    /// チャンネル一覧の中に置くと、そこだけの幅になって Discord と違う。
    /// 実機で見比べて報告を受けた
    #[test]
    fn the_panel_spans_both_lists() {
        let a = Gumicord::demo();
        let side = a.sidebar(Panes::Three).expect("3 ペインなら出る");

        assert_eq!(side.id, NodeId::NavSidebar);
        assert_eq!(side.children[0].id, NodeId::NavSidebarLists);
        // 一覧はまとめられ、自分はその**外**にいる
        let inside = names(&side.children[0]);
        assert!(inside.contains(&NodeId::NavGuildList));
        assert!(inside.contains(&NodeId::NavChannelList));
        assert!(!inside.contains(&NodeId::NavUserPanel));

        // ⚠️ 伸びると、チャットから幅を奪う
        assert_eq!(gumicord_render::intrinsic(NodeId::NavSidebar).grow, 0.0);
    }

    /// スクロールバーは**触っている一覧にしか出ない**。
    ///
    /// 出しっぱなしだと、丸が並ぶだけのサーバ一覧の右端にいつも線が入る
    #[test]
    fn only_the_list_under_the_pointer_has_a_scrollbar() {
        let mut a = Gumicord::demo();

        // どこにも居ないうちは、どの一覧にも出ない
        assert!(!names(&a.guild_list()).contains(&NodeId::LayoutScrollbar));
        assert!(!names(&a.channel_list()).contains(&NodeId::LayoutScrollbar));

        a.hovered_scroll = Some(NodeId::NavGuildList);
        assert!(names(&a.guild_list()).contains(&NodeId::LayoutScrollbar));
        // 隣の一覧には出ない
        assert!(!names(&a.channel_list()).contains(&NodeId::LayoutScrollbar));
    }

    /// ⚠️ **一番内側の巻ける領域を採る。** チャンネル一覧の中身は
    /// `layout.scroll` であって `nav.channel_list` ではない
    #[test]
    fn the_innermost_scroll_region_wins() {
        let mut a = Gumicord::demo();
        let hit = |id| Hit {
            id,
            key: None,
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        };

        // 手前から並ぶ。項目 → 内側の巻ける領域 → 外側の入れ物
        a.hover_changed(&[
            hit(NodeId::NavChannelListItem),
            hit(NodeId::LayoutScroll),
            hit(NodeId::NavChannelList),
        ]);
        assert_eq!(a.hovered_scroll, Some(NodeId::LayoutScroll));

        a.hover_changed(&[]);
        assert_eq!(a.hovered_scroll, None);
    }

    /// 一番狭いときは一覧ごと消える。**自分も一緒に消える**
    #[test]
    fn one_pane_has_no_sidebar_at_all() {
        let a = Gumicord::demo();
        assert!(a.sidebar(Panes::One).is_none());
    }

    /// ⚠️ **見出しと自分は巻かない。**
    ///
    /// 一覧ごと 1 つのスクロール領域にすると、下まで巻いたときに
    /// 自分が誰かもどのサーバを見ているかも見えなくなる
    #[test]
    fn only_the_list_scrolls() {
        let a = Gumicord::demo();
        let pane = a.channel_list();

        assert_eq!(pane.id, NodeId::NavChannelList);
        assert!(
            !gumicord_render::intrinsic(NodeId::NavChannelList).scroll,
            "外側が巻いてしまっている"
        );
        assert!(
            pane.children.iter().any(|c| c.id == NodeId::LayoutScroll),
            "巻く領域が中に無い"
        );
        assert_eq!(pane.children[0].id, NodeId::NavChannelListHeader);
    }
}

#[cfg(test)]
mod member_tests {
    use super::*;
    use gumicord_model::{Member, Message, MessageId, User, UserId};

    fn message(nick: Option<&str>, member_avatar: Option<&str>) -> Message {
        Message {
            id: MessageId::from(100u64),
            channel_id: ChannelId::from(10u64),
            guild_id: None,
            author: User {
                id: UserId::from(7u64),
                username: "nenneko".to_owned(),
                global_name: Some("ねんねこ".to_owned()),
                discriminator: "0".to_owned(),
                avatar_hash: None,
                bot: false,
            },
            content: "こんにちは".to_owned(),
            timestamp: "2026-08-22T12:34:56+00:00".to_owned(),
            edited_timestamp: None,
            pinned: false,
            attachments: Vec::new(),
            member: Some(Member {
                nick: nick.map(|s| s.to_owned()),
                avatar_hash: member_avatar.map(|s| s.to_owned()),
                roles: Vec::new(),
                joined_at: None,
                user: None,
            }),
            referenced_message: None,
            mentions: Vec::new(),
            mention_everyone: false,
        }
    }

    fn app(m: Message) -> Gumicord {
        let mut a = Gumicord::demo();
        // ⚠️ **ギルドを 1 つ置かないと demo のままである。**
        // 本物かどうかの分かれ目は `uses_live()` の 1 箇所しかない
        a.live
            .store_mut()
            .replace_guilds(vec![gumicord_model::Guild {
                id: 1u64.into(),
                name: "テスト".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: vec![gumicord_model::Channel {
                    id: 10u64.into(),
                    kind: gumicord_model::ChannelKind::GuildText,
                    name: Some("いっぱん".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: None,
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                }],
                roles: Vec::new(),
            }]);
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);
        a.selected_guild = 1;
        a.selected_channel = 10;
        a
    }

    /// ⚠️ **そのサーバでの呼び名が勝つ。**
    ///
    /// 全体の名前で出すと、「このサーバでは誰なのか」が分からない
    #[test]
    fn a_nickname_wins_over_the_global_name() {
        let a = app(message(Some("ねこ"), None));
        assert_eq!(a.message_rows()[0].author, "ねこ");

        let a = app(message(None, None));
        assert_eq!(a.message_rows()[0].author, "ねんねこ");
    }

    /// ⚠️ **顔もサーバごとに違う。** 見ているギルドが URL に入る
    #[test]
    fn a_guild_avatar_wins_over_the_global_one() {
        let a = app(message(None, Some("xyz")));
        let url = a.message_rows()[0].avatar.clone().unwrap();
        assert!(
            url.starts_with("https://cdn.discordapp.com/guilds/1/users/7/avatars/xyz.png"),
            "{url}"
        );

        // 何も上書きしていなければ、本人も設定していないので既定の絵
        let a = app(message(None, None));
        let url = a.message_rows()[0].avatar.clone().unwrap();
        assert!(url.contains("/embed/avatars/"), "{url}");
    }

    /// ⚠️ **本文の送信者名も役職の色で出す。**
    ///
    /// 一覧だけ色が付いていて本文が白いと、同じ人が別人に見える
    #[test]
    fn the_author_name_carries_the_role_colour() {
        let mut a = app(message(None, None));
        a.live.store_mut().upsert_guild(gumicord_model::Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: vec![gumicord_model::Role {
                id: 55u64.into(),
                name: "管理者".to_owned(),
                position: 3,
                hoist: true,
                color: Some(0x00e0_5260),
            }],
        });

        // その役職を持つ人の発言に差し替える
        let mut m = message(None, None);
        m.member.as_mut().expect("居る").roles = vec![55u64.into()];
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);

        assert_eq!(a.message_rows()[0].tint, Some(0x00e0_5260));

        // 木にも載る
        let tree = a.chat_view();
        let mut found = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageHeaderAuthor {
                found = n.tint;
            }
        });
        assert_eq!(found, Some(Color::from_rgb(0x00e0_5260)));
    }

    /// ⚠️ **REST で取った発言には `member` が付いていない。**
    ///
    /// Discord が添えるのは Gateway の `MESSAGE_CREATE` だけである。
    /// 発言だけを見ていると、**チャンネルを開いた直後は呼び名も顔も色も
    /// 出ず、新しい発言が 1 つ来たときだけそこに色が付く**。実際にそうなった
    #[test]
    fn a_message_without_a_member_falls_back_to_what_we_have_seen() {
        let mut a = app(message(None, None));
        a.live.store_mut().upsert_guild(gumicord_model::Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: Vec::new(),
            roles: vec![gumicord_model::Role {
                id: 55u64.into(),
                name: "管理者".to_owned(),
                position: 3,
                hoist: true,
                color: Some(0x00e0_5260),
            }],
        });
        // 一覧か過去の発言で見かけた姿
        a.live.store_mut().remember_member(
            1u64.into(),
            7u64.into(),
            Member {
                nick: Some("ねこ".to_owned()),
                avatar_hash: None,
                roles: vec![55u64.into()],
                joined_at: None,
                user: None,
            },
        );

        // REST から来た発言。**`member` が無い**
        let mut m = message(None, None);
        m.member = None;
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);

        let row = &a.message_rows()[0];
        assert_eq!(row.author, "ねこ", "呼び名も出る");
        assert_eq!(row.tint, Some(0x00e0_5260), "役職の色も出る");
    }

    /// 発言に付いていれば**そちらが勝つ**。より新しいので
    #[test]
    fn the_member_on_the_message_wins() {
        let mut a = app(message(Some("いまの呼び名"), None));
        a.live.store_mut().remember_member(
            1u64.into(),
            7u64.into(),
            Member {
                nick: Some("むかしの呼び名".to_owned()),
                avatar_hash: None,
                roles: Vec::new(),
                joined_at: None,
                user: None,
            },
        );
        assert_eq!(a.message_rows()[0].author, "いまの呼び名");
    }
}

#[cfg(test)]
mod member_list_tests {
    use super::*;
    use gumicord_gateway::member_list;
    use serde_json::json;

    /// 役職 1 つを持つギルドと、その中のチャンネルを開いた状態
    fn app() -> Gumicord {
        let mut a = Gumicord::demo();
        a.live
            .store_mut()
            .replace_guilds(vec![gumicord_model::Guild {
                id: 1u64.into(),
                name: "テスト".to_owned(),
                icon_hash: None,
                unavailable: false,
                channels: vec![gumicord_model::Channel {
                    id: 10u64.into(),
                    kind: gumicord_model::ChannelKind::GuildText,
                    name: Some("いっぱん".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: None,
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                }],
                roles: vec![gumicord_model::Role {
                    id: 55u64.into(),
                    name: "管理者".to_owned(),
                    position: 3,
                    hoist: true,
                    color: Some(0x00e0_5260),
                }],
            }]);
        a.selected_guild = 1;
        a.selected_channel = 10;
        a
    }

    fn sync(a: &mut Gumicord, items: Vec<serde_json::Value>) {
        let raw = json!({
            "guild_id": "1",
            "member_count": 9,
            "online_count": 2,
            "ops": [{ "op": "SYNC", "range": [0, 99], "items": items }],
        });
        let update = member_list::parse(&raw).expect("読める");
        a.live
            .apply_for_test(live::LiveEvent::Members(Box::new(update)));
    }

    fn person(id: &str, name: &str) -> serde_json::Value {
        json!({ "member": {
            "user": { "id": id, "username": name },
            "roles": [],
            "presence": { "status": "online" },
        }})
    }

    fn texts(node: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        node.walk(&mut |n, _| {
            if let Some(t) = n.content.as_text() {
                out.push(t.to_owned());
            }
        });
        out
    }

    /// ⚠️ **届く前から列は立てておく。**
    ///
    /// 届いてから生やすと、そのときチャットの幅が変わって本文が組み直される。
    /// 読んでいる最中に画面が動くのは、空の列が一瞬見えるより悪い
    #[test]
    fn the_column_stands_before_anything_arrives() {
        let a = app();

        let empty = a.member_list();
        assert_eq!(empty.id, NodeId::NavMemberList);
        assert!(empty.children.is_empty(), "中身はまだ無い");
        // 「まだ来ていない」と「誰も居ない」は別のことである
        assert!(empty.states.contains(State::Loading));

        let tree = a.build_tree(Panes::Four);
        let mut found = false;
        tree.walk(&mut |n, _| found |= n.id == NodeId::NavMemberList);
        assert!(found, "幅があるうちは列が立っている");
    }

    /// 届いたら [`State::Loading`] は下りる
    #[test]
    fn the_loading_state_goes_away_once_people_arrive() {
        let mut a = app();
        sync(&mut a, vec![person("7", "ねんねこ")]);
        assert!(!a.member_list().states.contains(State::Loading));
    }

    /// 見出しは**名前**で出る。役職の識別子は出さない
    #[test]
    fn headings_come_out_as_names() {
        let mut a = app();
        sync(
            &mut a,
            vec![
                json!({ "group": { "id": "55", "count": 1 } }),
                person("7", "ねんねこ"),
                json!({ "group": { "id": "online", "count": 1 } }),
                person("8", "すぴき"),
            ],
        );

        let list = a.member_list();
        assert_eq!(
            texts(&list),
            vec!["管理者 — 1", "ねんねこ", "オンライン — 1", "すぴき"]
        );
    }

    /// ⚠️ **知らない役職の見出しは飛ばす。**
    /// 18 桁の数字が並んでも、利用者にできることは増えない
    #[test]
    fn a_role_we_cannot_name_is_skipped() {
        let mut a = app();
        sync(
            &mut a,
            vec![
                json!({ "group": { "id": "999999999999999999", "count": 1 } }),
                person("7", "ねんねこ"),
            ],
        );

        assert_eq!(texts(&a.member_list()), vec!["ねんねこ"]);
    }

    /// 役職の色は**名前のノードに載る**。塗る場所を決めるのはテーマである
    #[test]
    fn a_role_colour_rides_on_the_name() {
        let mut a = app();
        sync(
            &mut a,
            vec![json!({ "member": {
                "user": { "id": "7", "username": "ねんねこ" },
                "roles": ["55"],
                "presence": { "status": "online" },
            }})],
        );

        let list = a.member_list();
        let name = list
            .children
            .iter()
            .flat_map(|c| c.children.iter())
            .find(|n| n.id == NodeId::NavMemberListItemName)
            .expect("名前がある");
        assert_eq!(name.tint, Some(Color::from_rgb(0x00e0_5260)));
    }

    /// ⚠️ **知らない役職しか持たない人には色を付けない。**
    /// 分からないことを既定の色で埋めない
    #[test]
    fn a_member_with_no_known_role_has_no_colour() {
        let mut a = app();
        sync(
            &mut a,
            vec![json!({ "member": {
                "user": { "id": "8", "username": "すぴき" },
                "roles": ["999999999999999999"],
            }})],
        );

        let list = a.member_list();
        let name = list
            .children
            .iter()
            .flat_map(|c| c.children.iter())
            .find(|n| n.id == NodeId::NavMemberListItemName)
            .expect("名前がある");
        assert_eq!(name.tint, None);
    }

    /// ⚠️ **狭いときはメンバー一覧から畳む。** チャットより後で構わない
    #[test]
    fn the_column_is_the_first_thing_to_go() {
        let mut a = app();
        sync(&mut a, vec![person("7", "ねんねこ")]);

        let has = |a: &Gumicord, panes| {
            let tree = a.build_tree(panes);
            let mut found = false;
            tree.walk(&mut |n, _| found |= n.id == NodeId::NavMemberList);
            found
        };
        assert!(has(&a, Panes::Four));
        assert!(!has(&a, Panes::Three));
        assert!(!has(&a, Panes::One));
    }
}

#[cfg(test)]
mod channel_selection_tests {
    use super::*;
    use gumicord_model::{Channel, ChannelKind, Guild};

    fn guild_with_category() -> Guild {
        Guild {
            id: 1u64.into(),
            name: "テスト".to_owned(),
            icon_hash: None,
            unavailable: false,
            channels: vec![
                // ⚠️ **カテゴリが先に来る。** 位置が 0 なので、
                // 素直に「最初のもの」を採るとこれが選ばれる
                Channel {
                    id: 10u64.into(),
                    kind: ChannelKind::GuildCategory,
                    name: Some("カテゴリ".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: None,
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                },
                Channel {
                    id: 11u64.into(),
                    kind: ChannelKind::GuildText,
                    name: Some("いっぱん".to_owned()),
                    guild_id: Some(1u64.into()),
                    parent_id: Some(10u64.into()),
                    position: 0,
                    topic: None,
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                },
            ],
            roles: Vec::new(),
        }
    }

    /// ⚠️ **カテゴリは見出しであって、開けるものではない。**
    ///
    /// 既定の選択がカテゴリを拾うと、押してもいないカテゴリが開かれた
    /// ことになる。実際にそうなった
    #[test]
    fn a_category_is_never_selected_by_default() {
        let mut a = Gumicord::demo();
        a.live
            .store_mut()
            .replace_guilds(vec![guild_with_category()]);
        a.selected_guild = 1;
        a.selected_channel = 0;

        a.sync_selection();

        assert_eq!(a.selected_channel, 11, "カテゴリを開こうとしている");
    }

    /// 一覧にはカテゴリも出る。**出さないのと開けないのは別である**
    #[test]
    fn the_category_still_appears_in_the_list() {
        let mut a = Gumicord::demo();
        a.live
            .store_mut()
            .replace_guilds(vec![guild_with_category()]);
        a.selected_guild = 1;

        let rows = a.channel_rows();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].category);
        assert_eq!(a.openable_rows().len(), 1);
    }
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use gumicord_model::Guild;
    use gumicord_store::FolderRow;

    fn guild(id: u64, name: &str, icon: Option<&str>) -> Guild {
        Guild {
            id: id.into(),
            name: name.to_owned(),
            icon_hash: icon.map(|s| s.to_owned()),
            unavailable: false,
            channels: Vec::new(),
            roles: Vec::new(),
        }
    }

    /// 3 つ入ったフォルダ 1 つと、その外のギルド 1 つ
    fn app_with_folder() -> Gumicord {
        let mut a = Gumicord::demo();
        a.live.store_mut().replace_guilds(vec![
            guild(1, "あ", Some("aaa")),
            guild(2, "い", Some("bbb")),
            guild(3, "う", None),
            guild(4, "え", Some("ddd")),
        ]);
        a.live.store_mut().set_sidebar(vec![
            FolderRow {
                id: Some(100),
                name: None,
                color: Some(0x007c_6cf0),
                guilds: vec![1u64.into(), 2u64.into(), 3u64.into()],
            },
            FolderRow {
                id: None,
                name: None,
                color: None,
                guilds: vec![4u64.into()],
            },
        ]);
        a
    }

    /// 閉じたフォルダの面に敷き詰められた絵の枚数。
    ///
    /// ⚠️ **`collapsed` で数える。** 普通のサーバも同じ安定 ID の絵を
    /// 持っているので、ID だけで数えると一覧じゅうの絵が混ざる
    fn tiles(node: &UiNode) -> usize {
        fn walk(n: &UiNode, found: &mut usize) {
            if n.id == NodeId::NavGuildListItemIcon && n.states.contains(State::Collapsed) {
                *found += 1;
            }
            for c in &n.children {
                walk(c, found);
            }
        }
        let mut found = 0;
        walk(node, &mut found);
        found
    }

    /// 左端の印は**選択中・未読・ホバーのときだけ**出る。
    ///
    /// ⚠️ 高さ 0 で隠すのではなく置かない。**出ている印は必ず何かを言う**
    #[test]
    fn the_pill_only_appears_when_it_means_something() {
        fn pills(n: &UiNode, out: &mut Vec<gumicord_uitree::StateSet>) {
            if n.id == NodeId::NavGuildListItemPill {
                out.push(n.states);
            }
            for c in &n.children {
                pills(c, out);
            }
        }

        let mut a = app_with_folder();
        a.selected_guild = 4;

        let mut found = Vec::new();
        pills(&a.guild_list(), &mut found);

        // 選んでいるサーバ 1 つにしか出ない
        assert_eq!(found.len(), 1);
        assert!(found[0].contains(State::Selected));

        // どれも選んでいなければ 1 つも出ない
        a.selected_guild = 0;
        let mut none = Vec::new();
        pills(&a.guild_list(), &mut none);
        assert!(none.is_empty());
    }

    /// ⚠️ **絵は入れ物の子である。** 入れ物のほうが広く、左端に印の
    /// 通り道がある。絵をそのまま項目にすると印を置く場所が無い
    #[test]
    fn the_item_holds_the_picture_rather_than_being_it() {
        let a = app_with_folder();
        let list = a.guild_list();

        let item = list
            .children
            .iter()
            .find(|n| n.id == NodeId::NavGuildListItem)
            .expect("サーバがある");
        assert!(item.content.as_image().is_none(), "入れ物は絵を持たない");
        assert!(
            item.children
                .iter()
                .any(|c| c.id == NodeId::NavGuildListItemIcon),
            "絵は子である"
        );
        assert!(
            gumicord_render::intrinsic(NodeId::NavGuildListItem).width
                > gumicord_render::intrinsic(NodeId::NavGuildListHome).width,
            "印の通り道のぶん広い"
        );
    }

    /// ⚠️ **閉じたフォルダは中身の絵を並べる。**
    ///
    /// 頭文字 1 つの箱では、どのフォルダを閉じているのか分からない。
    /// 実際にそうなっていて、直せと言われた
    #[test]
    fn a_closed_folder_shows_what_is_inside() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);

        assert_eq!(tiles(&a.guild_list()), 3);
    }

    /// ⚠️ **開いているときに敷き詰めない。** 中身はフォルダの子として
    /// 並んでいるので、同じ絵が上下に二重に出る
    #[test]
    fn an_open_folder_does_not_repeat_its_contents() {
        let a = app_with_folder();

        assert_eq!(tiles(&a.guild_list()), 0);
    }

    /// ⚠️ **開いたフォルダの中身は、フォルダの子である。**
    ///
    /// 兄弟として並べると背景がフォルダの分しか無くなり、どこまでが
    /// 1 つのフォルダなのか見て分からなくなる
    #[test]
    fn an_open_folder_holds_its_contents() {
        let a = app_with_folder();
        let list = a.guild_list();

        let folder = list
            .children
            .iter()
            .find(|n| n.id == NodeId::NavGuildListFolder)
            .expect("フォルダが無い");
        let inside = folder
            .children
            .iter()
            .filter(|n| n.id == NodeId::NavGuildListItem)
            .count();
        let outside = list
            .children
            .iter()
            .filter(|n| n.id == NodeId::NavGuildListItem)
            .count();

        assert_eq!(inside, 3, "中身がフォルダの中に無い");
        assert_eq!(outside, 1, "フォルダの外のサーバだけが兄弟であるはず");
    }

    /// 閉じたフォルダは中身を抱えない。**開くまで出さない**
    #[test]
    fn a_closed_folder_holds_nothing() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);
        let list = a.guild_list();

        let items = list
            .children
            .iter()
            .filter(|n| n.id == NodeId::NavGuildListItem)
            .count();
        assert_eq!(items, 1);
    }

    /// ⚠️ 2×2 に入りきらない分は出しても意味がない
    #[test]
    fn only_four_fit() {
        let mut a = Gumicord::demo();
        let many: Vec<_> = (1..=7).map(|i| guild(i, "さ", Some("hash"))).collect();
        a.live.store_mut().replace_guilds(many);
        a.live.store_mut().set_sidebar(vec![FolderRow {
            id: Some(100),
            name: None,
            color: None,
            guilds: (1..=7u64).map(Into::into).collect(),
        }]);
        a.live.store_mut().set_collapsed([100]);

        assert_eq!(tiles(&a.guild_list()), FOLDER_TILES);
    }

    /// 絵の無いサーバも 1 枚として数える。**穴が空くほうが分かりにくい**
    #[test]
    fn a_guild_without_an_icon_still_takes_a_tile() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);
        let rows = a.guild_rows();
        let folder = rows.iter().find(|r| r.folder_of_own == Some(100)).unwrap();

        assert_eq!(folder.members.len(), 3);
        assert!(folder.members[2].icon.is_none());
    }
}

/// アイコンかアバターを 1 つ作る。
///
/// # 絵が無いときは頭文字を出す
///
/// ⚠️ **絵が届くまでの間も同じノードである。** 届いてから別のノードに
/// 差し替えると、`key` が変わって差分更新がやり直しになる。
///
/// 絵が「まだ手元に無い」ことはレンダラしか知らないので、ここでは
/// **URL があるかどうかだけ**で決める。届いていなければレンダラが
/// 何も描かず、テーマの背景色がそのまま見える。
fn face(id: NodeId, url: Option<&str>, name: &str) -> UiNode {
    match url {
        Some(url) => UiNode::image(id, url),
        None => UiNode::text(id, initial(name)),
    }
}
