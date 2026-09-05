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
//! [4] runs on a worker thread with latest-only handoff, so a runaway plugin
//! lags effects instead of frames. [6] and [9] do not exist yet, and [3]
//! rebuilds the whole tree every frame rather than diffing.
//!
//! There are two screens. A cache, a login, or `GUMICORD_SKIP_LOGIN` all lead
//! to the main screen; nothing leads to the login screen. Having a cache
//! skips waiting for login, since READY takes closer to a second and would
//! blow the cold-start budget — and because signing out deletes the cache,
//! its presence is itself proof of a previous session on this account.
//!
//! `uses_live()` is the single place that distinguishes demo data from real
//! data. The row types absorb the difference so the tree builder never asks.

pub mod a11y;
pub mod account;
pub mod assets;
pub mod demo;
pub mod images;
pub mod live;
pub mod markdown;
pub mod menu;
pub mod session;
pub mod time;

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};

use gumicord_model::{ChannelId, GuildId, MessageId, RoleId, UserId};
use gumicord_platform::{Application, FrameCx, HiddenKey, TextDocument, Waker};
use gumicord_plugin::{ManagerEvent, PluginManager};
use gumicord_render::Hit;
use gumicord_store::{ChannelEntry, GuildEntry};
use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::value::Color;
use gumicord_uitree::{Content, DataKind, Editable, Key, NodeId, State, UiNode};
use live::Live;
use session::{Login, Session};

/// The default theme, embedded rather than loaded: the app has to run even
/// when no theme file can be read.
const DEFAULT_THEME: &str = include_str!("../../../examples/themes/midnight/theme.json");

/// Swaps the theme, for comparing themes while writing one. Goes away once
/// there is a settings screen and hot reload.
const THEME_ENV: &str = "GUMICORD_THEME";

/// How long a toast stays up, in seconds.
const TOAST_SECS: i64 = 4;
/// How many toasts stack; older ones drop off unread.
const TOAST_MAX: usize = 3;

/// Starts without loading any plugin. Plugin code runs on first sight, so
/// a broken one can take the session with it before anything is visible.
const SAFE_MODE_ENV: &str = "GUMICORD_SAFE_MODE";

/// Whether safe mode is on: any value but `0` counts, like the login skip.
fn safe_mode_enabled(var: Option<&str>) -> bool {
    matches!(var, Some(v) if v != "0")
}

/// A plugin asking for capabilities.
struct PendingApproval {
    id: String,
    name: String,
    capabilities: Vec<String>,
}

/// A dialog the plugin flow owns. Anything else showing means this one is
/// gone: with no settings screen to revisit it, dismissal denies.
enum Showing {
    Approval(String),
    ThemeHosts(Vec<String>),
    Notice,
}

/// Where the settings screen stands. Closed most of the time; while open it
/// owns every press, like a menu, and the tree carries it last so it draws
/// on top.
#[derive(Debug, Clone, Default)]
struct SettingsView {
    open: bool,
    category: crate::menu::SettingsCategory,
    /// The plugin whose page is showing, if drilled in.
    plugin: Option<String>,
    /// That plugin's settings tree, read once per selection. The call
    /// blocks on the plugin worker, so frames never make it.
    page: Option<(String, UiNode)>,
    /// The plugin rows, refreshed on open, on actions, and on plugin
    /// events. Cached for the same reason as the page.
    states: Vec<gumicord_plugin::PluginState>,
}

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
/// Slot for the settings gear in the user panel; same use.
const SETTINGS_OPEN: &str = "settings_open";

/// Leads the login screen after an involuntary sign-out. A silent kick back
/// to the QR reads as a crash.
const DEAD_SESSION_NOTICE: &str = "セッションが無効になったため、ログアウトしました";

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

/// Which login-form field, if any, has focus. Only one at a time, and only
/// while a form is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginField {
    Email,
    Password,
    Totp,
    /// The bot-token form's single field.
    Token,
}

/// The app state, and building the UITree from it.
pub struct Gumicord {
    theme: Option<Theme>,
    /// The theme file being watched, if one was configured. The bundled
    /// theme has no file, so there is nothing to watch for it.
    theme_path: Option<std::path::PathBuf>,
    /// What the watched file looked like when last loaded. Compared every
    /// frame; the file itself is only read when this moves.
    theme_mtime: Option<std::time::SystemTime>,
    /// Background images of the current theme, resolving.
    assets: crate::assets::ThemeAssets,
    /// Which theme the backgrounds currently drawing belong to. Read by the
    /// renderer every frame; the app sets it on every theme load.
    theme_namespace: Option<String>,
    /// Dropping this stops everything running on it.
    runtime: Option<tokio::runtime::Runtime>,
    /// Wakes the event loop; handed to the gateway after login.
    waker: Option<Waker>,
    /// Login progress; decides which screen is shown.
    login: Login,
    /// The captcha challenge awaiting a solution, kept on the app side so the
    /// platform's modal can hand back a bare token while the challenge's own
    /// `rqtoken`/`session_id` are still available to echo on the retry.
    pending: Option<gumicord_rest::CaptchaChallenge>,
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
    /// Which spoilers stand open, whole messages or single runs.
    reveals: crate::markdown::Reveals,
    /// Whatever is floating; at most one.
    floating: Option<crate::menu::Floating>,
    /// Transient notices; several share one node and none blocks input.
    toasts: VecDeque<crate::menu::Toast>,
    /// What the composer is doing.
    composing: Composing,
    selected_channel: u64,
    /// Whether the composer has focus.
    input_focused: bool,
    /// The composer's contents.
    input: TextDocument,
    /// Which login-form field has focus, if any.
    login_field: Option<LoginField>,
    /// The form the user is on: it stays put while login runs or fails, so an
    /// error lands back on the same form instead of bouncing to the QR.
    login_form: Option<LoginField>,
    /// The last login failure, shown on the form so the user knows why and can
    /// retry. Cleared by the next attempt or by leaving the form.
    login_error: Option<String>,
    /// The login form's email contents. Kept across password retries.
    login_email: TextDocument,
    /// The login form's password or TOTP code, whichever step is shown.
    login_input: TextDocument,
    /// The hidden code (konami) typed on the QR screen so far. Completed
    /// sequences open the bot-token form; anything else resets it.
    hidden_code: Vec<HiddenKey>,
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
    account_switch_rx: std::sync::mpsc::Receiver<
        Result<session::LoggedIn, (crate::account::AccountKey, String, bool)>,
    >,
    account_switch_tx: std::sync::mpsc::Sender<
        Result<session::LoggedIn, (crate::account::AccountKey, String, bool)>,
    >,
    /// Plugin hosts on their worker thread. Disabled in demo and safe mode.
    plugins: PluginManager,
    /// The latest plugin output; redrawn while the worker chews the next.
    last_patched: Option<UiNode>,
    /// Plugin approvals waiting for a dialog.
    approval_queue: VecDeque<PendingApproval>,
    /// Notices waiting for the same dialog.
    dialogs: VecDeque<PendingDialog>,
    /// The plugin dialog currently showing, if any.
    showing: Option<Showing>,
    /// The settings screen. Closed most of the time.
    settings: SettingsView,
}

impl Gumicord {
    pub fn new() -> Self {
        let mut live = Live::without_cache();
        if let Ok(store) = gumicord_platform::SecretStore::new()
            && let Ok(idx) = crate::account::AccountsIndex::load(&store)
            && let Some(active) = idx.active.or_else(|| idx.accounts.first().map(|a| a.key))
        {
            live.open_cache(active.is_bot, active.id);
        }
        Gumicord::with(Login::new(), live, Self::start_plugins())
    }

    /// Skips login and builds from fixed demo data, as `GUMICORD_SKIP_LOGIN`
    /// does. Opens no cache: real data mixed in would break the premise.
    pub fn demo() -> Self {
        Gumicord::with(
            Login::skipped(),
            Live::without_cache(),
            PluginManager::disabled(),
        )
    }

    /// Plugin hosts for this machine, unless safe mode says otherwise.
    fn start_plugins() -> PluginManager {
        if safe_mode_enabled(std::env::var(SAFE_MODE_ENV).as_deref().ok()) {
            tracing::warn!("safe mode: starting without plugins");
            return PluginManager::disabled();
        }
        match gumicord_platform::app_data_dir() {
            Some(dir) => PluginManager::start(dir.join("plugins")),
            None => {
                tracing::warn!("no home directory; starting without plugins");
                PluginManager::disabled()
            }
        }
    }

    fn with(login: Login, live: Live, plugins: PluginManager) -> Self {
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

        let (account_switch_tx, account_switch_rx) = std::sync::mpsc::channel();
        let theme_path = theme_file();
        let theme_mtime = theme_path.as_ref().and_then(|p| mtime_of(p));

        let mut app = Gumicord {
            theme: load_theme(),
            theme_path,
            theme_mtime,
            assets: crate::assets::ThemeAssets::new(),
            theme_namespace: None,
            runtime: None,
            waker: None,
            login,
            pending: None,
            live,
            scale: 1.0,
            hovered: None,
            hovered_scroll: None,
            selected_guild: guild,
            match_ctx: MatchContext::new(0.0),
            reveals: crate::markdown::Reveals::default(),
            floating: None,
            toasts: VecDeque::new(),
            composing: Composing::New,
            selected_channel: channel,
            input_focused: false,
            input: TextDocument::new(),
            login_field: None,
            login_form: None,
            login_error: None,
            login_email: TextDocument::new(),
            login_input: TextDocument::new(),
            hidden_code: Vec::new(),
            sent: Vec::new(),
            images: images::Images::new(),
            now: gumicord_platform::now_unix(),
            holds: std::cell::Cell::new(None),
            account_switch_rx,
            account_switch_tx,
            plugins,
            last_patched: None,
            approval_queue: VecDeque::new(),
            dialogs: VecDeque::new(),
            showing: None,
            settings: SettingsView::default(),
        };
        app.refresh_theme_assets();
        app
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
                tracing::warn!(%path, %e, "could not read the theme; using the bundled one");
                DEFAULT_THEME.to_owned()
            }
        },
        Err(_) => DEFAULT_THEME.to_owned(),
    };

    let result = Theme::parse(&src);
    // A rejected rule does not reject the theme, but is never dropped
    // silently.
    for d in &result.diagnostics {
        tracing::warn!("theme: {d}");
    }
    result.theme
}

/// The configured theme file, if any. An empty or missing variable both
/// mean the bundled theme.
fn theme_file() -> Option<std::path::PathBuf> {
    std::env::var(THEME_ENV).ok().map(std::path::PathBuf::from)
}

/// When a file was last written, if that is still known.
fn mtime_of(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

impl Gumicord {
    /// Re-points the background resolver at the current theme.
    fn refresh_theme_assets(&mut self) {
        let Some(theme) = &self.theme else {
            self.theme_namespace = None;
            self.assets
                .set_theme(String::new(), None, String::new(), Vec::new(), Vec::new());
            return;
        };
        let dir = self
            .theme_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf);
        let at = self
            .theme_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "bundled".to_owned());
        let namespace = format!("{}:{at}", theme.manifest.id);
        self.theme_namespace = Some(namespace.clone());
        self.assets.set_theme(
            namespace,
            dir,
            theme.manifest.name.clone(),
            theme.background_images(),
            theme.manifest.remote_assets.clone(),
        );
    }

    /// Re-reads the theme file when it changed. Runs on the frame boundary,
    /// never mid-build. A file that cannot be read or parsed leaves the
    /// last good theme up: editors write broken JSON halfway through a save.
    fn maybe_reload_theme(&mut self) -> bool {
        let path = match &self.theme_path {
            Some(path) => path.clone(),
            None => return false,
        };
        if mtime_of(&path) == self.theme_mtime {
            return false;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            return false;
        };
        let result = Theme::parse(&src);
        for d in &result.diagnostics {
            tracing::warn!("theme: {d}");
        }
        let Some(theme) = result.theme else {
            return false;
        };
        self.theme = Some(theme);
        self.theme_mtime = mtime_of(&path);
        self.refresh_theme_assets();
        self.notify_toast("テーマを再読み込みしました".to_owned());
        tracing::info!(?path, "reloaded the theme");
        true
    }

    /// Which node the screen reader follows. Dialogs and menus grab it;
    /// otherwise the focused field does.
    fn a11y_focus(&self) -> Option<&'static str> {
        if matches!(self.floating, Some(crate::menu::Floating::Confirm(_))) {
            Some("overlay.modal")
        } else if matches!(self.floating, Some(crate::menu::Floating::Menu(_))) {
            Some("overlay.menu")
        } else if self.input_focused {
            Some("chat.input.field")
        } else if self.login_field.is_some() {
            Some("app.screen.login.field")
        } else {
            None
        }
    }
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
                tracing::error!(%e, "could not start the async runtime");
                return;
            }
        };

        self.login.start(runtime.handle(), waker.clone());
        self.live.attach_waker(waker.clone());
        self.assets.start(runtime.handle(), waker.clone());
        self.waker = Some(waker);
        self.runtime = Some(runtime);
    }

    /// Hands over fetched images, just before drawing.
    /// The atlas evicted images. They are still on disk, so this re-reads
    /// rather than refetching; avatars vanish for one frame. Backgrounds own
    /// their textures and are untouched.
    fn images_dropped(&mut self) {
        tracing::debug!("the atlas evicted images; re-reading them");
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

    /// Takes arrived background images. Apart from avatars on purpose: those
    /// share the atlas and recycle, while backgrounds own their textures and
    /// outlive evictions.
    fn take_backgrounds(&mut self) -> Vec<gumicord_render::ImageData> {
        self.assets.take()
    }

    fn accesskit_update(
        &mut self,
        tree: &gumicord_uitree::UiNode,
    ) -> Option<accesskit::TreeUpdate> {
        Some(crate::a11y::tree_update(
            tree,
            self.a11y_focus(),
            &self.title(),
        ))
    }

    fn request_backgrounds(&mut self, keys: &[String]) {
        self.assets.request(keys);
    }

    fn theme_namespace(&self) -> Option<&str> {
        self.theme_namespace.as_deref()
    }

    /// A list scrolled; fetches more when it nears an end.
    ///
    /// Not at the edge exactly: asking on arrival means staring at nothing
    /// until it returns. Asking early usually has it there first.
    ///
    /// Never before anything is shown: a list that does not overflow is also
    /// "at the edge", which would fetch on every open.
    fn scrolled(&mut self, id: NodeId, at: f32, max: f32) {
        /// Distance from the edge that triggers the next page.
        const REACH: f32 = 400.0;

        match id {
            // History grows upward, toward where the reader already is.
            NodeId::ChatMessageList => {
                if max <= 0.0 || at > REACH {
                    return;
                }
                let channel = ChannelId::from(self.selected_channel);
                self.live.load_older(channel);
            }
            // Members grow downward, at the far end of the scroll.
            NodeId::NavMemberList => {
                if max <= 0.0 || at < max - REACH {
                    return;
                }
                let guild = GuildId::from(self.selected_guild);
                self.live.extend_members(guild);
            }
            _ => {}
        }
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
        if let Some(msg) = self.login.take_last_error() {
            self.login_error = Some(msg);
            changed = true;
        }
        changed |= self.live.poll();
        // An arrived image counts as a change, or it never gets drawn.
        changed |= self.images.poll();
        changed |= self.maybe_reload_theme();
        changed |= self.assets.poll();
        changed |= self.prune_toasts(gumicord_platform::now_unix());
        if let Some(ask) = self.assets.poll_ask() {
            changed = true;
            self.dialogs.push_back(PendingDialog::theme_hosts(ask));
        }

        while let Ok(res) = self.account_switch_rx.try_recv() {
            changed = true;
            match res {
                Ok(logged_in) => {
                    tracing::info!(user = %logged_in.me.user.display_name(), "account switched");
                    self.notify_toast(format!(
                        "{} に切り替えました",
                        logged_in.me.user.display_name()
                    ));
                    let key = crate::account::AccountKey::new(
                        logged_in.me.user.id,
                        logged_in.token.is_bot(),
                    );
                    self.live.disconnect();
                    self.live.open_cache(key.is_bot, key.id);
                    if let Ok(store) = gumicord_platform::SecretStore::new()
                        && let Ok(mut idx) = crate::account::AccountsIndex::load(&store)
                    {
                        idx.active = Some(key);
                        let _ = idx.save(&store);
                    }
                    if let Some(ch) = self.live.last_channel() {
                        self.selected_channel = ch.get();
                        if let Some(c) = self.live.store().channel(ch)
                            && let Some(g) = c.guild_id
                        {
                            self.selected_guild = g.get();
                        }
                    } else {
                        self.selected_guild = 0;
                        self.selected_channel = 0;
                    }
                    self.images.forget_everything();
                    self.floating = None;
                    self.composing = Composing::New;
                    self.input.take();
                    self.input_focused = false;
                    self.reveals = crate::markdown::Reveals::default();
                    self.login.set_logged_in(logged_in);
                }
                Err((key, err, unauthorized)) => {
                    tracing::warn!(%err, "account switch failed");
                    self.notify_toast("アカウントを切り替えられませんでした".to_owned());
                    if unauthorized
                        && let Ok(store) = gumicord_platform::SecretStore::new()
                        && let Ok(mut idx) = crate::account::AccountsIndex::load(&store)
                    {
                        let _ = idx.remove(&store, key);
                    }
                }
            }
        }

        // `Live::start` is a no-op once started, so calling it every time is
        // fine.
        if let (Some(l), Some(rt), Some(waker)) =
            (self.login.session().logged_in(), &self.runtime, &self.waker)
        {
            let key = crate::account::AccountKey::new(l.me.user.id, l.token.is_bot());
            self.live.open_cache(key.is_bot, key.id);

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

        // The gateway rejected the token mid-run. Same path as pressing
        // "log out".
        if self.live.take_rejection() {
            tracing::warn!("the token is no longer valid; signing out");
            changed |= self.sign_out();
            self.login.set_notice(DEAD_SESSION_NOTICE);
        } else if self.login.take_ended() {
            // The session was already over at startup: the stored token was
            // refused, or there was none. Cache-first would keep showing a
            // chat nothing can refresh, so the cache goes instead. Only it —
            // the login loop never stopped, and starting another one here
            // would race the QR already being fetched.
            tracing::warn!("no session could be restored; signing out");
            let had_cache = !self.live.is_empty();
            changed |= self.forget_account();
            if had_cache {
                self.login.set_notice(DEAD_SESSION_NOTICE);
            }
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

        // The settings screen owns every press while open: letting one
        // through would navigate the chat behind it.
        if self.settings.open {
            let item = hits.iter().find_map(|h| match (h.id, &h.key) {
                (NodeId::OverlayMenuItem, Some(Key::Index(i))) => Some(*i as usize),
                _ => None,
            });
            return match item {
                Some(i) => self.settings_action(i),
                // An inert control inside a plugin's page: swallowed, so the
                // screen does not close under a curious press.
                None if hits.iter().any(|h| h.id == NodeId::PrimitiveButton) => false,
                // An outside press closes; unlike a dialog there is no unmade
                // decision to protect.
                None => self.close_settings(),
            };
        }

        let mut changed = false;

        // Pressing outside the composer removes focus.
        let on_input = hits.iter().any(|h| h.id == NodeId::ChatInputField);
        if on_input != self.input_focused {
            self.input_focused = on_input;
            changed = true;
        }

        // A login-form field takes focus; the composer and the login form
        // never share it.
        if let Some(field) = hits.iter().find_map(|h| match (h.id, &h.key) {
            (
                NodeId::AppScreenLoginField,
                Some(Key::Slot(s @ ("email" | "password" | "totp" | "token"))),
            ) => Some(match *s {
                "email" => LoginField::Email,
                "password" => LoginField::Password,
                "token" => LoginField::Token,
                _ => LoginField::Totp,
            }),
            _ => None,
        }) && (self.login_field != Some(field) || self.input_focused)
        {
            self.login_field = Some(field);
            self.input_focused = false;
            changed = true;
        }

        // Only the frontmost selectable hit.
        for h in hits {
            match (h.id, &h.key) {
                // The way into the password form (or its submit / back).
                (
                    NodeId::PrimitiveButton,
                    Some(Key::Slot(slot @ ("login_submit" | "login_back" | "login_password"))),
                ) => {
                    changed |= self.login_button(slot);
                }
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
                // The gear sits on the user panel; its hit comes first, so
                // this arm wins over the panel's own menu.
                (NodeId::PrimitiveButton, Some(Key::Slot(SETTINGS_OPEN))) => {
                    changed |= self.open_settings();
                }
                // A press anywhere else on the message still opens all of it:
                // a single run is a small target.
                (NodeId::ChatMessage, Some(Key::Id(id))) => {
                    changed |= self.reveals.messages.insert(*id);
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

    /// A link run was pressed; opening it is the whole answer.
    ///
    /// While something floats, the press belongs to it: dismissing a menu or
    /// leaving a decision alone beats following what happened to sit
    /// underneath.
    fn link_pressed(&mut self, _url: &str) -> bool {
        self.floating.is_none()
    }

    /// A spoiler run was pressed; it opens alone, or closes again if it was
    /// already open.
    ///
    /// Same rule as links while something floats. The run's number is the
    /// renderer's own count of that message's spoiler runs, which is why the
    /// state can be a plain set.
    fn spoiler_pressed(&mut self, owner: u64, run: usize) -> bool {
        if self.floating.is_some() {
            return false;
        }
        if self.reveals.is_open(owner, run) {
            self.reveals.shut_run(owner, run);
        } else {
            self.reveals.open_run(owner, run);
        }
        true
    }

    /// Secondary press; what was hit decides the menu.
    fn context_menu(&mut self, hits: &[Hit], at: (f32, f32)) -> bool {
        // Reopens rather than closing, or opening the next message's menu
        // would take two presses.
        let items = hits.iter().find_map(|h| match (h.id, &h.key) {
            // The composer first: it overlaps the message list. Focusing it
            // makes the menu and its items act on the composer.
            (NodeId::ChatInputField, _) => {
                self.input_focused = true;
                self.login_field = None;
                Some(self.field_menu())
            }
            // A login-form field: focusing it makes the menu and its items
            // act on that field.
            (
                NodeId::AppScreenLoginField,
                Some(Key::Slot(s @ ("email" | "password" | "totp" | "token"))),
            ) => {
                self.login_field = Some(match *s {
                    "email" => LoginField::Email,
                    "password" => LoginField::Password,
                    "token" => LoginField::Token,
                    _ => LoginField::Totp,
                });
                self.input_focused = false;
                Some(self.field_menu())
            }
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

    /// Only a focused field receives input: a login-form field, or the
    /// composer. Never more than one holds focus at once.
    fn focused_document(&mut self) -> Option<&mut TextDocument> {
        match self.login_field {
            Some(LoginField::Email) => Some(&mut self.login_email),
            Some(LoginField::Password | LoginField::Totp | LoginField::Token) => {
                Some(&mut self.login_input)
            }
            None => self.input_focused.then_some(&mut self.input),
        }
    }

    /// Sends, edits or replies, depending on [`Composing`]. Missing that
    /// turns an intended edit into a new message.
    ///
    /// On the login form it instead submits that step: the whole form on the
    /// password screen, or the TOTP code. Enter means the same thing in both
    /// places.
    fn submit(&mut self) -> bool {
        if self.login_field.is_some() {
            return self.submit_login();
        }

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
        // The settings screen sits above everything but a menu.
        if self.close_settings() {
            return true;
        }
        // Escape on a login field abandons the whole password login.
        if self.login_field.is_some() {
            self.leave_login_form();
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

    /// The hidden login code on the QR screen: the konami sequence. Completed
    /// sequences open the bot-token form; any wrong key resets the buffer.
    fn hidden_key(&mut self, key: HiddenKey) -> bool {
        use HiddenKey::{A, B, Down, Left, Right, Up};

        const SEQUENCE: [HiddenKey; 10] = [Up, Up, Down, Down, Left, Right, Left, Right, B, A];

        // Only the QR screen listens; elsewhere the arrows and B/A mean
        // nothing to the app.
        if self.login.session().qr().is_none() {
            self.hidden_code.clear();
            return true;
        }

        self.hidden_code.push(key);
        let len = self.hidden_code.len();
        if self.hidden_code[..] != SEQUENCE[..len] {
            self.hidden_code.clear();
            return true;
        }
        if len == SEQUENCE.len() {
            self.hidden_code.clear();
            self.login.start_token();
            self.login_form = Some(LoginField::Token);
            self.login_field = Some(LoginField::Token);
        }
        true
    }

    /// A clipboard operation on the focused field, from a Ctrl shortcut or a
    /// field-menu item. Without a focused field every operation does nothing.
    fn clipboard(&mut self, op: gumicord_platform::ClipboardOp) -> bool {
        use gumicord_platform::ClipboardOp::{Copy, Cut, Paste};
        let Some(doc) = self.focused_document() else {
            return false;
        };
        match op {
            Copy | Cut => {
                let sel = doc.selection();
                if sel.is_empty() {
                    return false;
                }
                let text = doc.text()[sel].to_owned();
                if let Err(e) = gumicord_platform::clipboard::set_text(&text) {
                    tracing::warn!(%e, "could not write to the clipboard");
                    return false;
                }
                if op == Cut {
                    doc.insert("");
                }
                true
            }
            Paste => match gumicord_platform::clipboard::text() {
                // The field is one line, so newlines would hide text. Discord
                // collapses them on paste too.
                Ok(Some(text)) => {
                    doc.insert(&text.replace(['\r', '\n'], " "));
                    true
                }
                Ok(None) => false,
                Err(e) => {
                    tracing::warn!(%e, "could not read the clipboard");
                    false
                }
            },
        }
    }

    /// A captcha question came back from the API. Forward it to the platform,
    /// which shows the modal. The password form stays up underneath.
    fn pending_captcha(&mut self) -> Option<gumicord_platform::CaptchaChallenge> {
        let pending = self.login.take_pending()?;
        let platform = gumicord_platform::CaptchaChallenge {
            site_key: pending.sitekey.clone()?,
            rqdata: pending.rqdata.clone(),
        };
        // Remember the challenge for the retry: the token comes back alone, but
        // `rqtoken` and `session_id` must be echoed alongside it.
        self.pending = Some(pending);
        Some(platform)
    }

    /// The modal produced a token; retry the challenged login with it.
    fn captcha_solved(&mut self, solved: gumicord_platform::SolvedCaptcha) {
        let Some(pending) = self.pending.take() else {
            tracing::error!("a captcha was solved but no challenge is pending");
            return;
        };
        self.login.submit_captcha(gumicord_rest::SolvedCaptcha {
            key: solved.solution,
            rqtoken: pending.rqtoken,
            session_id: pending.session_id,
        });
    }

    /// The modal was cancelled; abandon the password login and go back.
    fn captcha_cancelled(&mut self) {
        self.pending = None;
        self.login.cancel_password();
        self.login_field = None;
        self.input_focused = false;
        self.login_input.take();
    }

    /// Pipeline stages [3] through [5]. The plugin pass runs between them.
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
        let panes = Panes::for_width(cx.viewport.w);
        // The member subscription follows the member pane: hidden narrows to
        // the minimum instead of unsubscribing, which never worked.
        self.live.set_members_visible(panes.members());
        let tree = self.build_tree(panes);

        // [4] run it through the plugins; the newest finished output wins,
        // or the raw tree when nothing finished yet.
        let mut tree = self.apply_plugins(tree);

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

/// A dialog waiting for room: plugin flows share one modal at a time.
struct PendingDialog {
    confirm: crate::menu::Confirm,
    showing: Showing,
}

impl PendingDialog {
    fn notice(title: &str, body: String) -> Self {
        PendingDialog {
            confirm: crate::menu::Confirm {
                title: title.to_owned(),
                body,
                preview: None,
                action: crate::menu::Action::Acknowledge,
                confirm: "わかった".to_owned(),
                danger: false,
            },
            showing: Showing::Notice,
        }
    }

    /// What a theme asking for remote hosts says. The hosts and the privacy
    /// cost are the whole question; the rest of the theme applies either way.
    fn theme_hosts(ask: crate::assets::HostAsk) -> Self {
        let mut body = ask
            .hosts
            .iter()
            .map(|h| format!("・{h}"))
            .collect::<Vec<_>>()
            .join("\n");
        body.push_str("\nこれらのサイトには、あなたが Gumicord を起動したことが伝わります。");
        PendingDialog {
            confirm: crate::menu::Confirm {
                title: format!("「{}」の画像取得", ask.theme),
                body,
                preview: None,
                action: crate::menu::Action::ApproveThemeHosts {
                    hosts: ask.hosts.clone(),
                },
                confirm: "許可する".to_owned(),
                danger: false,
            },
            showing: Showing::ThemeHosts(ask.hosts),
        }
    }
}

impl PendingApproval {
    /// What the approval asks, in the user's words.
    fn confirm(self) -> crate::menu::Confirm {
        let mut lines = capability_bullets(&self.capabilities);
        lines.push_str("\n「やめる」を押すと、このプラグインは読み込まれません。");
        crate::menu::Confirm {
            title: format!("「{}」の許可", self.name),
            body: lines,
            preview: Some(self.id.clone()),
            action: crate::menu::Action::ApprovePlugin {
                id: self.id,
                granted: self.capabilities,
            },
            confirm: "許可する".to_owned(),
            danger: false,
        }
    }
}

/// Capability ids in the user's words, one bullet per line. Shared by the
/// approval dialog and the settings screen, so a capability never has two
/// names.
fn capability_bullets(caps: &[String]) -> String {
    caps.iter()
        .map(|c| match c.as_str() {
            "log" => "・記録を残す",
            "storage" => "・設定などのデータを保存する",
            _ => "・（不明な権限）",
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A plugin's state in the user's words. One place, so the list and the
/// page never disagree.
fn plugin_state_label(state: gumicord_plugin::PluginStateKind) -> &'static str {
    use gumicord_plugin::PluginStateKind;
    match state {
        PluginStateKind::Loaded => "有効",
        PluginStateKind::Disabled => "無効",
        PluginStateKind::Denied => "拒否",
        PluginStateKind::NeedsApproval => "未許可",
        PluginStateKind::LoadFailed => "読込失敗",
    }
}

impl Gumicord {
    /// Submits the active login step. Runs the password flow, hands off a TOTP
    /// code, or logs in with a bot token; nothing to send stays put.
    fn submit_login(&mut self) -> bool {
        self.login_error = None;
        match self.login_field {
            Some(LoginField::Token) => {
                let token = self.login_input.text().trim().to_owned();
                if token.is_empty() {
                    return false;
                }
                self.login.submit_bot_token(token);
                self.login_input.take();
                self.login_field = None;
                true
            }
            Some(LoginField::Email) | Some(LoginField::Password) | None => {
                let email = self.login_email.text().trim().to_owned();
                let password = self.login_input.text().to_owned();

                if email.is_empty() || password.is_empty() {
                    return false;
                }
                self.login.submit_password(email, password);
                // Keep the email for a retry; the password is a secret that
                // has done its job.
                self.login_input.take();
                self.login_field = None;
                true
            }
            Some(LoginField::Totp) => {
                let code = self.login_input.text().trim().to_owned();
                if code.is_empty() {
                    return false;
                }
                self.login.submit_totp(code);
                self.login_input.take();
                self.login_field = None;
                true
            }
        }
    }

    /// Drops the login form and returns to the QR screen.
    fn leave_login_form(&mut self) {
        self.login.cancel_password();
        self.login_field = None;
        self.login_form = None;
        self.login_error = None;
        self.login_input.take();
    }

    /// Opens the settings screen. The drill-in is reset; the category stays,
    /// like Discord remembering the section.
    fn open_settings(&mut self) -> bool {
        self.settings.plugin = None;
        self.settings.page = None;
        self.refresh_settings_states();
        if self.settings.open {
            return false;
        }
        self.settings.open = true;
        true
    }

    fn close_settings(&mut self) -> bool {
        if !self.settings.open {
            return false;
        }
        self.settings.open = false;
        self.settings.plugin = None;
        self.settings.page = None;
        true
    }

    /// Re-reads the plugin rows. Blocking on the worker, so only on open,
    /// on actions, and on plugin events — never per frame.
    fn refresh_settings_states(&mut self) {
        self.settings.states = self.plugins.plugin_states();
    }

    fn select_settings_plugin(&mut self, id: String) {
        let page = self.plugins.settings_tree(&id);
        self.settings.plugin = Some(id.clone());
        self.settings.page = page.map(|tree| (id, tree));
    }

    /// A pressed settings row. The nav and the page share one index space,
    /// in tree order.
    fn settings_action(&mut self, index: usize) -> bool {
        let action = self
            .settings_nav_items()
            .into_iter()
            .chain(self.settings_page_items())
            .nth(index)
            .map(|item| item.action);
        match action {
            Some(action) => self.perform(action),
            // A stale index: the list changed under the press. Staying put
            // beats acting on the wrong row.
            None => true,
        }
    }

    fn settings_nav_items(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item, SettingsCategory};
        let mut items = vec![Item::new(Action::CloseSettings, "閉じる").icon("close")];
        for category in [SettingsCategory::Plugins, SettingsCategory::Theme] {
            items.push(
                Item::new(Action::SettingsCategory(category), category.label())
                    .selected(self.settings.category == category),
            );
        }
        items
    }

    fn settings_page_items(&self) -> Vec<crate::menu::Item> {
        use crate::menu::{Action, Item, SettingsCategory};
        use gumicord_plugin::PluginStateKind;
        match self.settings.category {
            SettingsCategory::Theme => Vec::new(),
            SettingsCategory::Plugins => match &self.settings.plugin {
                None => self
                    .settings
                    .states
                    .iter()
                    .map(|p| {
                        // One-line rows assemble name, version and state;
                        // split them when the message table arrives (ADR-0010).
                        Item::new(
                            Action::SelectSettingsPlugin(p.id.clone()),
                            format!(
                                "{} {}（{}）",
                                p.name,
                                p.version,
                                plugin_state_label(p.state)
                            ),
                        )
                    })
                    .collect(),
                Some(id) => {
                    let mut items = vec![Item::new(Action::SettingsPluginBack, "← プラグイン")];
                    if let Some(p) = self.settings.states.iter().find(|p| &p.id == id) {
                        // The approval dialog owns unapproved plugins; the
                        // re-approve row below is their way back to it.
                        match p.state {
                            PluginStateKind::Loaded => {
                                items.push(Item::new(
                                    Action::DisablePlugin(id.clone()),
                                    "無効にする",
                                ));
                            }
                            PluginStateKind::Disabled | PluginStateKind::LoadFailed => {
                                items.push(Item::new(
                                    Action::EnablePlugin(id.clone()),
                                    "有効にする",
                                ));
                            }
                            PluginStateKind::Denied | PluginStateKind::NeedsApproval => {}
                        }
                        items.push(Item::new(
                            Action::ReapprovePlugin(id.clone()),
                            "許可をやり直す",
                        ));
                    }
                    items
                }
            },
        }
    }

    /// The page's display-only lines: status, capabilities, warnings. Rows
    /// stay rows; these are what rows cannot say.
    fn settings_page_texts(&self) -> Vec<String> {
        use crate::menu::SettingsCategory;
        match self.settings.category {
            SettingsCategory::Theme => {
                let warnings = self.assets.warnings();
                if warnings.is_empty() {
                    vec!["画像の取得で問題は起きていません。".to_owned()]
                } else {
                    warnings
                }
            }
            SettingsCategory::Plugins => match &self.settings.plugin {
                None if self.settings.states.is_empty() => {
                    vec!["プラグインはありません。".to_owned()]
                }
                None => Vec::new(),
                Some(id) => match self.settings.states.iter().find(|p| &p.id == id) {
                    None => vec!["このプラグインはもうありません。".to_owned()],
                    Some(p) => {
                        // Name and state stay on separate lines: word order
                        // moves by language, so they must not share one
                        // sentence (ADR-0010).
                        let mut out = vec![
                            format!("{} {}", p.name, p.version),
                            plugin_state_label(p.state).to_owned(),
                        ];
                        if p.capabilities.is_empty() {
                            out.push("権限を求めません。".to_owned());
                        } else {
                            out.push(format!(
                                "求める権限:\n{}",
                                capability_bullets(&p.capabilities)
                            ));
                        }
                        let page_missing =
                            self.settings.page.as_ref().is_none_or(|(pid, _)| pid != id);
                        if p.has_settings && page_missing {
                            out.push("設定ページを読み込めませんでした。".to_owned());
                        }
                        out
                    }
                },
            },
        }
    }

    /// The selected plugin's own page, if it declared and delivered one.
    fn settings_page_embed(&self) -> Option<UiNode> {
        let id = self.settings.plugin.as_ref()?;
        let (pid, tree) = self.settings.page.as_ref()?;
        (pid == id).then(|| tree.clone())
    }

    /// The settings screen, Discord-style: categories left, page right. Rows
    /// are menu items, so the theme and the press routing already know them.
    fn settings_screen(&self) -> UiNode {
        let nav = self.settings_nav_items();
        let page_items = self.settings_page_items();
        let hovered = self.hovered_item();
        let hovered_nav = hovered.filter(|i| *i < nav.len());
        let hovered_page = hovered.and_then(|i| i.checked_sub(nav.len()));

        let mut page = UiNode::new(NodeId::SettingsPage);
        for text in self.settings_page_texts() {
            page = page.child(UiNode::text(NodeId::PrimitiveText, text));
        }
        if !page_items.is_empty() {
            page = page.child(crate::menu::rows(&page_items, hovered_page));
        }
        if let Some(tree) = self.settings_page_embed() {
            page = page.child(tree);
        }

        UiNode::new(NodeId::SettingsScreen)
            .child(UiNode::new(NodeId::SettingsNav).child(crate::menu::rows(&nav, hovered_nav)))
            .child(page)
    }

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
        let tooltip = self.tooltip();

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
            .child_if(!self.toasts.is_empty(), || {
                let texts: Vec<String> = self.toasts.iter().map(|t| t.text.clone()).collect();
                if let Some(until) = self.toasts.iter().map(|t| t.until).min() {
                    self.hold(until - self.now);
                }
                crate::menu::toast_node(&texts).expect("空でないと確かめた")
            })
            .child_if(tooltip.is_some(), || {
                tooltip.clone().expect("直前に確かめた")
            })
            // Last, so it draws above everything: while open it also owns
            // every press.
            .child_if(self.settings.open, || self.settings_screen())
    }

    /// Full date for a hovered timestamp. The header shows only the hour, and
    /// the whole date is what hovering asks for.
    fn tooltip(&self) -> Option<UiNode> {
        let (id, key) = self.hovered.as_ref()?;
        if *id != NodeId::ChatMessageHeaderTime {
            return None;
        }
        let Some(Key::Id(mid)) = key.as_ref() else {
            return None;
        };
        let row = self.message_rows().into_iter().find(|m| m.id == *mid)?;
        if row.day.is_empty() {
            return None;
        }
        Some(crate::menu::tooltip_node(&format!(
            "{} {}",
            row.day, row.time
        )))
    }

    /// Shows a transient notice. User-initiated outcomes only: anything else
    /// would chatter while the user reads.
    fn notify_toast(&mut self, text: String) {
        self.toasts.push_back(crate::menu::Toast {
            text,
            until: gumicord_platform::now_unix() + TOAST_SECS,
        });
        while self.toasts.len() > TOAST_MAX {
            self.toasts.pop_front();
        }
    }

    /// Drops expired notices. Split out for tests: the clock is real
    /// everywhere else.
    fn prune_toasts(&mut self, now: i64) -> bool {
        let shown = self.toasts.len();
        self.toasts.retain(|t| t.until > now);
        self.toasts.len() != shown
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
                    tracing::warn!(%e, "could not write to the clipboard");
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
            crate::menu::Action::SwitchAccount(key) => {
                self.switch_account(*key);
            }
            crate::menu::Action::AddAccount => {
                self.add_account();
            }

            crate::menu::Action::LogOut => {
                self.sign_out();
            }
            crate::menu::Action::ApprovePlugin { id, granted } => {
                self.showing = None;
                self.plugins.approve(id, granted);
            }
            crate::menu::Action::ApproveThemeHosts { hosts } => {
                self.showing = None;
                self.assets.approve_hosts(hosts);
            }
            crate::menu::Action::Acknowledge => {
                self.showing = None;
            }
            crate::menu::Action::OpenSettings => {
                self.open_settings();
            }
            crate::menu::Action::CloseSettings => {
                self.close_settings();
            }
            crate::menu::Action::SettingsCategory(category) => {
                self.settings.category = *category;
                self.settings.plugin = None;
                self.settings.page = None;
            }
            crate::menu::Action::SelectSettingsPlugin(id) => {
                self.select_settings_plugin(id.clone());
            }
            crate::menu::Action::SettingsPluginBack => {
                self.settings.plugin = None;
                self.settings.page = None;
            }
            crate::menu::Action::DisablePlugin(id) => {
                self.plugins.disable(id);
                self.refresh_settings_states();
            }
            crate::menu::Action::EnablePlugin(id) => {
                self.plugins.enable(id);
                self.refresh_settings_states();
            }
            crate::menu::Action::ReapprovePlugin(id) => {
                self.plugins.reapprove(id);
                self.refresh_settings_states();
            }

            crate::menu::Action::Cut => {
                self.clipboard(gumicord_platform::ClipboardOp::Cut);
            }
            crate::menu::Action::CopySelection => {
                self.clipboard(gumicord_platform::ClipboardOp::Copy);
            }
            crate::menu::Action::Paste => {
                self.clipboard(gumicord_platform::ClipboardOp::Paste);
            }
            crate::menu::Action::SelectAll => {
                if let Some(doc) = self.focused_document() {
                    doc.select_all();
                }
            }
        }
        true
    }

    fn switch_account(&mut self, target: crate::account::AccountKey) -> bool {
        let (Some(rt), Some(waker)) = (&self.runtime, &self.waker) else {
            return false;
        };
        if let Some(l) = self.login.session().logged_in()
            && l.me.user.id == target.id
            && l.token.is_bot() == target.is_bot
        {
            return false;
        }

        let store = match gumicord_platform::SecretStore::new() {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(%e, "cannot access secure store for account switch");
                return false;
            }
        };
        let index = match crate::account::AccountsIndex::load(&store) {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!(%e, "cannot read accounts index for switch");
                return false;
            }
        };
        let token = match index.load_token(&store, target) {
            Ok(Some(tok)) => tok,
            Ok(None) => {
                tracing::warn!("no token stored for target account");
                return false;
            }
            Err(e) => {
                tracing::warn!(%e, "cannot load token for target account");
                return false;
            }
        };

        let tx = self.account_switch_tx.clone();
        let waker = waker.clone();
        rt.spawn(async move {
            let res = match gumicord_rest::RestClient::anonymous() {
                Ok(rest) => match rest.authenticate(token.clone()).await {
                    Ok((client, me)) => Ok(session::LoggedIn { me, client, token }),
                    Err(e) => {
                        let unauthorized = e.is_unauthorized();
                        Err((target, e.to_string(), unauthorized))
                    }
                },
                Err(e) => {
                    let unauthorized = e.is_unauthorized();
                    Err((target, e.to_string(), unauthorized))
                }
            };
            let _ = tx.send(res);
            waker.wake();
        });
        true
    }

    fn add_account(&mut self) -> bool {
        let (Some(rt), Some(waker)) = (&self.runtime, &self.waker) else {
            return false;
        };
        self.live.disconnect();
        self.images.forget_everything();
        self.login.start_add_account(rt.handle(), waker.clone());
        self.login_form = None;
        self.login_error = None;
        self.floating = None;
        self.composing = Composing::New;
        self.input.take();
        self.input_focused = false;
        self.reveals = crate::markdown::Reveals::default();
        true
    }

    /// Signs out and returns to the login screen.
    ///
    /// Reached both by pressing "log out" and by the gateway rejecting the
    /// token; the two must leave the same state behind, or one of them strands
    /// the app on a screen it cannot get out of.
    fn sign_out(&mut self) -> bool {
        let (Some(rt), Some(waker)) = (&self.runtime, &self.waker) else {
            // No runtime means demo mode, where there is nothing to sign out of.
            return false;
        };
        self.login.forget(rt.handle(), waker.clone());
        self.login_form = None;
        self.login_error = None;
        self.forget_account()
    }

    /// Drops everything belonging to the account that is leaving: the caches,
    /// and whatever is still on screen.
    ///
    /// Nothing is kept: leaving the cache behind lets the next person on this
    /// machine read the previous one's messages. Split from [`Self::sign_out`]
    /// because a session already dead at startup reaches here while the login
    /// loop is still running and must not be started again.
    fn forget_account(&mut self) -> bool {
        self.live.forget_everything();
        self.images.forget_everything();

        // Anything still on screen belongs to the account that just left.
        self.floating = None;
        self.composing = Composing::New;
        self.input.take();
        self.input_focused = false;
        self.reveals = crate::markdown::Reveals::default();
        true
    }

    /// The document a field menu and its items act on: the focused login field
    /// if any, else the composer. Read-only; [`focused_document`] is the
    /// mutable twin used to actually edit it.
    ///
    /// [`focused_document`]: crate::Application::focused_document
    fn field_doc(&self) -> &TextDocument {
        match self.login_field {
            Some(LoginField::Email) => &self.login_email,
            Some(LoginField::Password | LoginField::Totp | LoginField::Token) => &self.login_input,
            None => &self.input,
        }
    }

    /// A field's menu, desktop only. Lists only what would do something.
    fn field_menu(&self) -> Vec<crate::menu::Item> {
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

    /// The login screen. Shows a QR by default, the password form when that
    /// was chosen, or the TOTP step when a second factor is needed.
    fn login_screen(&self) -> UiNode {
        let s = self.login.session();
        let mut screen = UiNode::new(NodeId::AppScreenLogin);

        // A form the user chose stays up while the login runs or even fails, so
        // its error is shown on the form, not bounced to the QR.
        let form = self.login_form.or(match s {
            Session::Password => Some(LoginField::Password),
            Session::PasswordTotp => Some(LoginField::Totp),
            Session::Token => Some(LoginField::Token),
            _ => None,
        });

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
                            &self.login_email,
                            false,
                        ))
                        .child(self.login_label("パスワード"))
                        .child(self.login_field("password", "パスワード", &self.login_input, true))
                        .child(self.login_forgot_password())
                        .child_if(self.login_error.is_some(), || self.login_error_node())
                        .child(self.login_submit("ログイン"))
                        .child(self.login_divider())
                        .child(self.login_qr_button())
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
                        .child(self.login_field("totp", "認証コード", &self.login_input, false))
                        .child_if(self.login_error.is_some(), || self.login_error_node())
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
                        .child(self.login_field("token", "トークン", &self.login_input, false))
                        .child_if(self.login_error.is_some(), || self.login_error_node())
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
            self.login_error.as_deref().unwrap_or_default(),
        )
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
            (self.login_field, slot),
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
    fn login_button(&mut self, slot: &str) -> bool {
        match slot {
            // From the QR screen into the password form.
            "login_password" => {
                self.login.start_password();
                self.login_field = None;
                self.login_form = Some(LoginField::Password);
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

        Some(
            UiNode::new(NodeId::NavUserPanel)
                .child(avatar)
                .child(lines)
                // Last, so it draws and hits above the panel: a press on the
                // gear must reach it, not the panel's own menu.
                .child(
                    UiNode::new(NodeId::PrimitiveButton)
                        .with_key(Key::Slot(SETTINGS_OPEN))
                        .with_state_if(
                            self.is_hovered(
                                NodeId::PrimitiveButton,
                                Some(&Key::Slot(SETTINGS_OPEN)),
                            ),
                            State::Hover,
                        )
                        .child(UiNode::icon(NodeId::PrimitiveIcon, "gear")),
                ),
        )
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

        // A day always starts labelled, and one header covers a run: same
        // author, same day, close together. Anything else starts over.
        let rows = self.message_rows();
        let mut messages = UiNode::new(NodeId::ChatMessageList);
        let mut prev: Option<(&str, &str, i64)> = None;
        for m in &rows {
            if !m.day.is_empty() && prev.is_none_or(|(_, day, _)| day != m.day) {
                messages = messages.child(Self::day_divider(&m.day));
            }
            let grouped = match prev {
                Some((author, day, unix)) => {
                    author == m.author && crate::time::continues(day, unix, &m.day, m.unix)
                }
                None => false,
            };
            messages = messages.child(self.message(m, grouped));
            prev = Some((&m.author, &m.day, m.unix));
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

    /// A day divider: the date centred with a line reaching both sides.
    /// The lines are spacers the theme paints; soaking the row's remainder
    /// keeps the label centred whatever the width.
    fn day_divider(day: &str) -> UiNode {
        UiNode::new(NodeId::LayoutRow)
            .with_key(Key::Slot("day_divider"))
            .child(UiNode::new(NodeId::LayoutSpacer).with_key(Key::Slot("day_divider_line")))
            .child(UiNode::text(NodeId::ChatMessageListDayDivider, day))
            .child(UiNode::new(NodeId::LayoutSpacer).with_key(Key::Slot("day_divider_line")))
    }

    /// Drains plugin events and hands the tree over. Returns the newest
    /// finished output, or the input when the worker has none yet.
    fn apply_plugins(&mut self, tree: UiNode) -> UiNode {
        let mut plugin_changed = false;
        for event in self.plugins.drain() {
            plugin_changed = true;
            match event {
                ManagerEvent::Patched(patched) => {
                    self.last_patched = Some(*patched);
                }
                ManagerEvent::Disabled { id, failures, .. } => {
                    tracing::error!(plugin = %id, failures, "plugin disabled");
                    self.dialogs.push_back(PendingDialog::notice(
                        "プラグインを無効化しました",
                        format!("「{id}」が繰り返し失敗したため、読み込みを止めました。"),
                    ));
                }
                ManagerEvent::NeedsApproval {
                    id,
                    name,
                    capabilities,
                } => {
                    self.approval_queue.push_back(PendingApproval {
                        id,
                        name,
                        capabilities,
                    });
                }
                ManagerEvent::Warned { message } => {
                    tracing::warn!("plugin: {message}");
                }
            }
        }
        // The settings rows cache the worker's answers; refresh them when
        // the worker did something, never per frame.
        if plugin_changed && self.settings.open {
            self.refresh_settings_states();
            if let Some(id) = self.settings.plugin.clone() {
                let page = self.plugins.settings_tree(&id);
                self.settings.page = page.map(|tree| (id, tree));
            }
        }
        self.settle_dialog();
        // Approvals can wait for login; a dialog over the QR screen invites
        // approving something unread.
        if self.login.session().logged_in().is_some() {
            self.pump_dialog();
        }
        self.plugins.submit(&tree, &self.plugin_data_context(&tree));
        self.last_patched.clone().unwrap_or(tree)
    }

    /// Domain facts for patches: what `ctx.data` carries.
    ///
    /// The tree hands over shape; snowflake IDs mean nothing to JS, so the
    /// readable side (bodies, names, counts) travels here instead, keyed by
    /// node identity. Only nodes carrying a `DataRef` the resolver knows
    /// appear; anything else reads `undefined`, exactly as the SDK types
    /// promise for nodes without data.
    fn plugin_data_context(&self, tree: &UiNode) -> gumicord_plugin::PatchContext {
        use gumicord_gateway::member_list::MemberRow;
        use gumicord_gateway::status::Status;

        let guild = GuildId::from(self.selected_guild);
        let channel = ChannelId::from(self.selected_channel);
        let store = self.live.store();
        let messages: HashMap<u64, &gumicord_model::Message> = store
            .messages(channel)
            .iter()
            .map(|m| (m.id.get(), m))
            .collect();
        let guilds: HashMap<u64, GuildRow> =
            self.guild_rows().into_iter().map(|g| (g.id, g)).collect();
        let channels: HashMap<u64, ChannelRow> = self
            .openable_rows()
            .into_iter()
            .map(|c| (c.id, c))
            .collect();
        let mut statuses: HashMap<u64, &'static str> = HashMap::new();
        if let Some(list) = self.live.members(guild) {
            for row in list.rows() {
                if let MemberRow::Member(entry) = row
                    && let Some(user) = entry.member.user.as_ref()
                {
                    // Invisible looks offline to everyone else.
                    let status = match entry.status {
                        Status::Invisible => "offline",
                        s => s.as_wire(),
                    };
                    statuses.insert(user.id.get(), status);
                }
            }
        }

        let mut table = serde_json::Map::new();
        tree.walk(&mut |node, _| {
            let Some(data) = &node.data else {
                return;
            };
            let key = gumicord_plugin::data_key(node.id.as_str(), &node.key);
            let value = match data.kind {
                DataKind::Message => messages
                    .get(&data.id)
                    .map(|m| message_data(m, &channel.get().to_string(), guild.get(), store)),
                DataKind::Guild => guilds.get(&data.id).map(guild_data),
                DataKind::Channel => channels
                    .get(&data.id)
                    .and_then(|c| store.channel(ChannelId::from(c.id)).map(|m| (c, m)))
                    .map(|(c, m)| channel_data(c, m)),
                DataKind::Member => member_data(&data.id, guild, store, &statuses),
                // No data-bearing nodes of the other kinds exist today;
                // their resolvers arrive with their nodes.
                _ => None,
            };
            if let Some(value) = value {
                table.insert(key, value);
            }
        });
        gumicord_plugin::PatchContext {
            data: Some(serde_json::Value::Object(table)),
        }
    }
}

/// `ctx.data` shapes, mirroring `sdk/src/data.ts` field for field: the
/// TypeScript types promise these exact names.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UserDataJson {
    id: String,
    username: String,
    display_name: String,
    bot: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    avatar_url: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MessageDataJson {
    id: String,
    channel_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    guild_id: Option<String>,
    created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    edited_at: Option<String>,
    content: String,
    author: UserDataJson,
    pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    referenced_message_id: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GuildDataJson {
    id: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    icon_url: Option<String>,
    unread: bool,
    mention_count: u32,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ChannelDataJson {
    id: String,
    name: String,
    /// The Discord channel type number, as a string.
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    topic: Option<String>,
    nsfw: bool,
    unread: bool,
    mention_count: u32,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MemberDataJson {
    user: UserDataJson,
    display_name: String,
    status: String,
    roles: Vec<String>,
}

fn user_data(user: &gumicord_model::User, avatar_url: Option<String>) -> UserDataJson {
    UserDataJson {
        id: user.id.to_string(),
        username: user.username.clone(),
        display_name: user.display_name().to_owned(),
        bot: user.bot,
        avatar_url,
    }
}

fn message_data(
    m: &gumicord_model::Message,
    channel_id: &str,
    guild: u64,
    store: &gumicord_store::Store,
) -> serde_json::Value {
    let guild_id = GuildId::from(guild);
    let avatar_url = store
        .member(guild_id, m.author.id)
        .map(|member| member.display_avatar(guild_id, &m.author).url())
        .unwrap_or_else(|| m.author.display_avatar().url());
    let data = MessageDataJson {
        id: m.id.to_string(),
        channel_id: channel_id.to_owned(),
        guild_id: (guild != 0).then(|| guild.to_string()),
        created_at: m.timestamp.clone(),
        edited_at: m.edited_timestamp.clone(),
        content: m.content.clone(),
        author: user_data(&m.author, Some(avatar_url)),
        pinned: m.pinned,
        referenced_message_id: m.referenced_message.as_ref().map(|r| r.id.to_string()),
    };
    serde_json::to_value(data).expect("plain data always serialises")
}

fn guild_data(g: &GuildRow) -> serde_json::Value {
    serde_json::to_value(GuildDataJson {
        id: g.id.to_string(),
        name: g.name.clone(),
        icon_url: g.icon.clone(),
        unread: g.unread,
        mention_count: g.mentions,
    })
    .expect("plain data always serialises")
}

fn channel_data(c: &ChannelRow, m: &gumicord_model::Channel) -> serde_json::Value {
    serde_json::to_value(ChannelDataJson {
        id: c.id.to_string(),
        name: c.name.clone(),
        kind: channel_kind_number(&m.kind),
        topic: m.topic.clone(),
        nsfw: m.nsfw,
        unread: c.unread,
        mention_count: c.mentions,
    })
    .expect("plain data always serialises")
}

/// The Discord channel type number, as a string. Names would be invented
/// ABI; the numbers are Discord's own.
fn channel_kind_number(kind: &gumicord_model::ChannelKind) -> String {
    use gumicord_model::ChannelKind::*;
    match kind {
        GuildText => "0",
        Dm => "1",
        GuildVoice => "2",
        GroupDm => "3",
        GuildCategory => "4",
        GuildAnnouncement => "5",
        AnnouncementThread => "10",
        PublicThread => "11",
        PrivateThread => "12",
        GuildStageVoice => "13",
        GuildForum => "15",
        Unknown(n) => return n.to_string(),
    }
    .to_owned()
}

fn member_data(
    id: &u64,
    guild: GuildId,
    store: &gumicord_store::Store,
    statuses: &std::collections::HashMap<u64, &'static str>,
) -> Option<serde_json::Value> {
    use gumicord_model::UserId;
    let member = store.member(guild, UserId::from(*id))?;
    let user = member.user.as_ref()?;
    let data = MemberDataJson {
        user: user_data(user, Some(member.display_avatar(guild, user).url())),
        display_name: member.display_name(user).to_owned(),
        status: statuses.get(id).copied().unwrap_or("offline").to_owned(),
        roles: member
            .roles
            .iter()
            .filter_map(|r| store.role_name(guild, *r))
            .map(str::to_owned)
            .collect(),
    };
    Some(serde_json::to_value(data).expect("plain data always serialises"))
}

impl Gumicord {
    /// A shown dialog that is gone was dismissed: without a settings screen
    /// to revisit it, that denies an approval and drops a notice.
    fn settle_dialog(&mut self) {
        let showing = self.showing.as_ref();
        let still_there = match (&self.floating, showing) {
            (Some(crate::menu::Floating::Confirm(c)), Some(Showing::Approval(id))) => {
                matches!(&c.action, crate::menu::Action::ApprovePlugin { id: aid, .. } if aid == id)
            }
            (Some(crate::menu::Floating::Confirm(c)), Some(Showing::ThemeHosts(_))) => {
                matches!(&c.action, crate::menu::Action::ApproveThemeHosts { .. })
            }
            (Some(crate::menu::Floating::Confirm(c)), Some(Showing::Notice)) => {
                matches!(&c.action, crate::menu::Action::Acknowledge)
            }
            _ => false,
        };
        if still_there {
            return;
        }
        // Notices and nothing showing need no farewell.
        match self.showing.take() {
            Some(Showing::Approval(id)) => {
                self.plugins.deny(&id);
            }
            Some(Showing::ThemeHosts(hosts)) => {
                self.assets.deny_hosts(&hosts);
            }
            _ => {}
        }
    }

    /// Shows the next queued dialog, if nothing is already showing.
    fn pump_dialog(&mut self) {
        if self.showing.is_some() || self.floating.is_some() {
            return;
        }
        if let Some(dialog) = self.dialogs.pop_front() {
            self.floating = Some(crate::menu::Floating::Confirm(dialog.confirm));
            self.showing = Some(dialog.showing);
            return;
        }
        if let Some(approval) = self.approval_queue.pop_front() {
            let id = approval.id.clone();
            self.floating = Some(crate::menu::Floating::Confirm(approval.confirm()));
            self.showing = Some(Showing::Approval(id));
        }
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
                            .with_data(m.id)
                            .with_id_key(m.id),
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
            &self.reveals,
            m.id,
            self.now,
        );

        // A single custom emoji is displayed larger, like Discord.
        if let Some(id) = crate::markdown::Ink::single_emoji_id(&m.blocks) {
            return UiNode::new(NodeId::ChatMessageContent)
                .with_data(m.id)
                .child(ink.large_emoji(id));
        }

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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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
    /// Local day label, also the grouping key: equal strings share a day.
    day: String,
    /// Whole seconds, to tell a live run from yesterday's tail.
    unix: i64,
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

    /// Repairs the selection so it points at something real, and fetches what
    /// that needs.
    ///
    /// The startup selection holds demo ids, which do not exist once READY
    /// arrives; leaving them would show the lists with nothing in them.
    ///
    /// Returns whether the screen changed. Starting a fetch does not count;
    /// arrival does.
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
                .enumerate()
                .map(|(i, m)| MessageRow {
                    id: m.id,
                    author: m.author.to_string(),
                    avatar: None,
                    tint: None,
                    time: m.time.to_string(),
                    // Demo shares one day so its runs still join; spacing
                    // stays inside the grouping window.
                    day: "今日".to_owned(),
                    unix: i as i64 * 60,
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
                let (time, day, unix) = row_time(&m.timestamp);
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
                    time,
                    day,
                    unix,
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

/// Splits an ISO 8601 timestamp into local `HH:MM`, day label and instant.
///
/// Discord returns UTC; the day label doubles as the grouping key. Anything
/// unparseable keeps the raw string for display and never groups.
fn row_time(iso: &str) -> (String, String, i64) {
    // "2026-08-22T12:34:56.789000+00:00"
    let Some(unix) = crate::time::parse_unix(iso) else {
        return (iso.to_owned(), String::new(), 0);
    };
    let (day, h, m) = crate::time::local_day_hm(unix);
    (format!("{h:02}:{m:02}"), day, unix)
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

    /// Cancelling a reply keeps the draft; cancelling an edit does not, since
    /// the field held the original message rather than anything typed.
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

    /// If the slot constant is read as a binding rather than a pattern, every
    /// button falls through here. It looks identical until one is pressed.
    #[test]
    fn another_button_does_not_cancel() {
        let mut a = app();
        a.composing = Composing::Reply(1);
        let hits = [hit_of(NodeId::PrimitiveButton, Some(Key::Slot("その他")))];

        a.pressed(&hits);
        assert_eq!(a.composing, Composing::Reply(1), "別のボタンで取り消された");
    }

    /// Escape cancels the reply or edit before discarding the draft; both at
    /// once leaves it unclear which was lost.
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

    /// Clearing the field and pressing enter must not destroy the message.
    #[test]
    fn submitting_an_empty_field_does_nothing() {
        let mut a = app();
        a.composing = Composing::Edit(1);
        assert!(!a.submit());
        assert_eq!(a.composing, Composing::Edit(1), "編集をやめてしまった");
    }

    /// Sending returns to composing a new message, or the next one is a reply
    /// too.
    #[test]
    fn submitting_returns_to_composing_a_new_message() {
        let mut a = app();
        a.composing = Composing::Reply(1);
        a.input.insert("やあ");
        assert!(a.submit());
        assert_eq!(a.composing, Composing::New);
    }

    /// The server would return 403 anyway, but not offering it comes first.
    #[test]
    fn someone_elses_message_offers_neither_edit_nor_delete() {
        use crate::menu::Action;
        // Demo mode is signed out, so nothing is ours.
        let a = app();
        let items = a.message_menu(1);
        assert!(
            !items
                .iter()
                .any(|i| matches!(i.action, Action::Edit(_) | Action::Delete(_))),
            "他人の発言に編集か削除が出ている"
        );
        // Reply is offered on anyone's message.
        assert!(items.iter().any(|i| matches!(i.action, Action::Reply(_))));
    }

    /// The composer overlaps the message list, so it is checked first.
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

    /// Only what would do something.
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
    //  Context menus

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

    /// A press hits both layers, so without a rule it passes through and
    /// navigates to whatever the user meant to dismiss the menu over.
    #[test]
    fn nothing_underneath_is_reachable_while_the_menu_is_open() {
        let mut a = with_menu();
        let before = a.selected_channel;
        // Include a hit on the channel underneath.
        let hits = [hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))];

        assert!(a.pressed(&hits), "閉じるという変化はある");
        assert!(a.floating.is_none(), "閉じていない");
        assert_eq!(a.selected_channel, before, "下のチャンネルへ移動した");
    }

    /// A link under an open menu is dismissed with the menu, not opened:
    /// declining hands the press back to the dismissal path.
    #[test]
    fn a_link_press_declines_while_something_floats() {
        let mut a = with_menu();
        assert!(!a.link_pressed("https://example.com/"));

        let mut b = app();
        assert!(b.link_pressed("https://example.com/"));
    }

    /// A covered run opens alone and closes again on the second press; under
    /// an open menu it declines exactly like a link does.
    #[test]
    fn a_spoiler_press_toggles_and_declines_while_something_floats() {
        let mut a = with_menu();
        assert!(!a.spoiler_pressed(1, 0));
        assert!(!a.reveals.is_open(1, 0), "断ったのに開いている");

        // Toggle: open once, then cover again.
        let mut b = app();
        assert!(b.spoiler_pressed(5, 2));
        assert!(b.reveals.is_open(5, 2));
        assert!(b.spoiler_pressed(5, 2));
        assert!(!b.reveals.is_open(5, 2), "もう一度押しても閉じない");

        // The message-level reveal still counts as open for every run.
        b.reveals.messages.insert(5);
        assert!(b.spoiler_pressed(5, 2));
        assert!(
            b.reveals.is_open(5, 2),
            "メッセージ全体が開いているのに閉じた"
        );
    }

    /// Pressing an item runs it and closes the menu.
    ///
    /// Never writes to the clipboard: that would destroy whatever the person
    /// running the tests had copied.
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

    /// The menu floats above the composer, so escape stops there.
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
    //  Time-dependent display

    fn built(a: &mut Gumicord) {
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        a.build(&cx);
    }

    /// With no relative timestamp there is nothing to wake for, and a
    /// deadline would spin for no change.
    #[test]
    fn nothing_relative_means_no_wake_up() {
        let mut a = app();
        built(&mut a);
        assert_eq!(a.next_frame_in(), None);
    }

    /// Otherwise "just now" stays on an open screen for hours.
    #[test]
    fn a_relative_timestamp_asks_for_a_later_frame() {
        let mut a = app();
        // Relative to the real clock; a fixed timestamp would drift into
        // "years ago".
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
    //  Confirming before deleting

    /// A menu holding only "delete".
    ///
    /// Built directly rather than through `message_menu`, which checks whether
    /// the message is ours and so offers nothing in demo mode. What matters
    /// here is what pressing it does.
    fn with_delete_menu() -> Gumicord {
        let mut a = app();
        a.floating = Some(crate::menu::Floating::Menu(crate::menu::Menu {
            at: (0.0, 0.0),
            items: vec![crate::menu::Item::new(crate::menu::Action::Delete(1), "削除").danger()],
        }));
        // An observable marker that only clears once confirmed.
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

    /// One row among others in a menu, one line from its neighbours, and a
    /// deleted message cannot be recovered.
    #[test]
    fn one_press_of_delete_does_not_delete() {
        let mut a = with_delete_menu();
        assert!(press_menu(&mut a, 0));
        assert!(is_confirm(&a), "確認の窓が出ていない");
        assert_eq!(a.composing, Composing::Edit(1), "確かめる前に消えている");
    }

    /// Cancelling does nothing and closes the dialog.
    #[test]
    fn cancelling_the_dialog_does_nothing() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(press_button(&mut a, crate::menu::button::CANCEL));
        assert!(a.floating.is_none(), "窓が閉じていない");
        assert_eq!(a.composing, Composing::Edit(1), "やめたのに消えている");
    }

    /// Confirming is what actually deletes.
    #[test]
    fn confirming_the_dialog_deletes() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(press_button(&mut a, crate::menu::button::CONFIRM));
        assert!(a.floating.is_none(), "窓が閉じていない");
        // Deleting what is being edited also cancels the edit.
        assert_eq!(a.composing, Composing::New, "消えていない");
    }

    /// A dialog represents an unmade decision; dismissing it on an outside
    /// press leaves the outcome ambiguous.
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

    /// Escape closes it; no way out at all would be a dead end.
    #[test]
    fn escape_closes_the_dialog() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);

        assert!(a.cancel_input());
        assert!(a.floating.is_none(), "Esc で閉じない");
        assert_eq!(a.composing, Composing::Edit(1), "Esc で消えている");
    }

    /// Confirming again would reopen the dialog forever.
    #[test]
    fn the_dialog_does_not_reappear() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        press_button(&mut a, crate::menu::button::CONFIRM);
        assert!(a.floating.is_none(), "窓がもう一度出ている");
    }

    /// Nothing underneath is reachable while it is open.
    #[test]
    fn nothing_underneath_is_reachable_while_the_dialog_is_open() {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        let before = a.selected_channel;

        a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]);
        assert_eq!(a.selected_channel, before, "下のチャンネルへ移動した");
    }

    /// Confirming everything would stop the dialog being read at all.
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

    // ═══════════════════════════════════════════════════════════════
    //  Settings screen

    fn with_settings() -> Gumicord {
        let mut a = app();
        assert!(a.open_settings(), "開かなかった");
        a
    }

    fn settings_ids(a: &Gumicord) -> Vec<NodeId> {
        let mut out = Vec::new();
        a.build_tree(Panes::Four).walk(&mut |n, _| out.push(n.id));
        out
    }

    fn settings_texts(a: &Gumicord) -> Vec<String> {
        let mut out = Vec::new();
        a.build_tree(Panes::Four).walk(&mut |n, _| {
            if n.id == NodeId::PrimitiveText {
                out.extend(n.content.as_text().map(str::to_owned));
            }
        });
        out
    }

    /// The screen carries the nav and the page, in that order.
    #[test]
    fn opening_settings_shows_screen_nav_and_page() {
        let a = with_settings();
        let ids = settings_ids(&a);
        let screen = ids
            .iter()
            .position(|i| *i == NodeId::SettingsScreen)
            .expect("画面がない");
        let nav = ids
            .iter()
            .position(|i| *i == NodeId::SettingsNav)
            .expect("分類がない");
        let page = ids
            .iter()
            .position(|i| *i == NodeId::SettingsPage)
            .expect("中身がない");
        assert!(screen < nav && nav < page, "並びが逆");

        let closed = app();
        assert!(
            !settings_ids(&closed).contains(&NodeId::SettingsScreen),
            "閉じているのに出ている"
        );
    }

    /// Demo loads no plugins, and nothing failed to fetch.
    #[test]
    fn empty_lists_say_so() {
        let a = with_settings();
        let texts = settings_texts(&a).join("\n");
        assert!(texts.contains("プラグインはありません"), "{texts}");
        let mut theme = with_settings();
        press_menu(&mut theme, 2);
        let texts = settings_texts(&theme).join("\n");
        assert!(texts.contains("問題は起きていません"), "{texts}");
    }

    /// The rows share the menu's index space: 0 closes, 1 and 2 switch.
    #[test]
    fn settings_rows_route_by_index() {
        let mut a = with_settings();
        press_menu(&mut a, 2);
        assert_eq!(
            a.settings.category,
            crate::menu::SettingsCategory::Theme,
            "分類が変わらない"
        );
        press_menu(&mut a, 1);
        assert_eq!(
            a.settings.category,
            crate::menu::SettingsCategory::Plugins,
            "戻れない"
        );
        press_menu(&mut a, 0);
        assert!(!a.settings.open, "閉じない");
    }

    /// A stale index stays put instead of acting on the wrong row.
    #[test]
    fn a_stale_settings_index_keeps_the_screen() {
        let mut a = with_settings();
        press_menu(&mut a, 99);
        assert!(a.settings.open, "画面が消えた");
    }

    /// Nothing underneath is reachable while it is open. Like a menu, an
    /// outside press closes the screen instead of navigating behind it.
    #[test]
    fn nothing_underneath_is_reachable_while_settings_are_open() {
        let mut a = with_settings();
        let before = a.selected_channel;
        assert!(a.pressed(&[hit_of(NodeId::NavChannelListItem, Some(Key::Id(999)))]));
        assert_eq!(a.selected_channel, before, "下のチャンネルへ移動した");
        assert!(!a.settings.open, "外側の押下で閉じない");
    }

    /// Escape closes it; an outside press does too, with nothing to decide.
    #[test]
    fn escape_and_outside_press_close_settings() {
        let mut a = with_settings();
        assert!(a.cancel_input());
        assert!(!a.settings.open, "Esc で閉じない");

        let mut b = with_settings();
        assert!(b.pressed(&[]), "閉じるという変化がない");
        assert!(!b.settings.open, "外側の押下で閉じない");
    }

    /// The dialog's laid-out rectangles. Reading the theme's numbers does not
    /// show where things land.
    fn placed_confirm(w: f32, h: f32) -> Vec<(NodeId, gumicord_render::Rect)> {
        let mut a = with_delete_menu();
        press_menu(&mut a, 0);
        assert!(is_confirm(&a));

        // Demo mode has no body, and without the preview the widest row is
        // never measured.
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

    /// A button outside the dialog is visible but unpressable.
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

        // Otherwise what can be pressed and what can be read drift apart.
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

    /// Overlapping leaves one visible but unreachable.
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

    /// Centred, not placed at the press.
    #[test]
    fn the_dialog_is_centred_on_screen() {
        let (w, h) = (1280.0, 800.0);
        let modal = one_of(&placed_confirm(w, h), NodeId::OverlayModal);
        let cx = modal.x + modal.w / 2.0;
        let cy = modal.y + modal.h / 2.0;
        assert!((cx - w / 2.0).abs() < 1.0, "横にずれている {modal:?}");
        assert!((cy - h / 2.0).abs() < 1.0, "縦にずれている {modal:?}");
    }

    /// Overflowing at phone widths puts cancel out of reach.
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

    /// A permanent full-window layer would absorb every press.
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

    /// A press on nothing just closes what is open.
    #[test]
    fn right_clicking_empty_space_closes_the_menu() {
        let mut a = with_menu();
        assert!(a.context_menu(&[], (0.0, 0.0)));
        assert!(a.floating.is_none());
    }

    /// By width, not device: a narrowed desktop window reads better with a
    /// sheet.
    #[test]
    fn a_narrow_window_presents_the_menu_as_a_sheet() {
        use crate::menu::Present;
        assert_eq!(Panes::One.present(), Present::Sheet);
        assert_eq!(Panes::Two.present(), Present::Popover);
        assert_eq!(Panes::Four.present(), Present::Popover);
    }

    /// Matching on the raw string would notify someone for writing about a
    /// mention inside code.
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
        // A different person is not us.
        assert!(!call("やあ <@2>"));
    }

    /// Watching only `@everyone` misses being called by role.
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
        // A channel reference is not a mention.
        assert!(!call("<#9> を見て", Some(&roles)));
    }

    /// Mentions inside quotes and lists count.
    #[test]
    fn a_nested_mention_is_found() {
        let me = Some(UserId::from(1));
        let call = |src: &str| calls_me(&gumicord_markdown::parse(src), me, None);
        assert!(call("> やあ <@1>"));
        assert!(call("- やあ <@1>"));
        assert!(call("# やあ <@1>"));
    }
    /// The bundled theme always parses; a broken one starts up black.
    #[test]
    fn the_bundled_theme_parses() {
        let result = Theme::parse(DEFAULT_THEME);
        let errors: Vec<_> = result.errors().collect();
        assert!(errors.is_empty(), "同梱テーマに誤りがある: {errors:?}");
        assert!(result.is_applied());
    }

    /// The tree builds and reflects the selection.
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

    /// Resolving the theme gives `app.window` a background.
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
        // Inheritance reaches the leaves.
        let title = &window.children[0].children[0];
        assert_eq!(title.id, NodeId::ChromeTitlebarTitle);
        assert!(title.style.color.is_some(), "文字色が継承されていない");
    }

    /// A press that changes nothing asks for no redraw.
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

    /// Width picks the tier; a boundary value belongs to the wider one.
    #[test]
    fn panes_are_chosen_by_width() {
        assert_eq!(Panes::for_width(1600.0), Panes::Four);
        assert_eq!(Panes::for_width(1140.0), Panes::Four);
        // The member list goes first: who is present matters less than what
        // was said.
        assert_eq!(Panes::for_width(1139.0), Panes::Three);
        assert_eq!(Panes::for_width(900.0), Panes::Three);
        assert_eq!(Panes::for_width(899.0), Panes::Two);
        assert_eq!(Panes::for_width(600.0), Panes::Two);
        assert_eq!(Panes::for_width(599.0), Panes::One);
        assert_eq!(Panes::for_width(320.0), Panes::One);
    }

    /// Chat survives every width; no width may show nothing.
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

    /// Flattens a body back to readable text, dropping decoration: this only
    /// checks what was written, not how it looks.
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

    /// Input needs focus.
    #[test]
    fn input_only_reaches_a_focused_field() {
        let mut a = Gumicord::demo();
        assert!(a.focused_document().is_none());

        a.input_focused = true;
        assert!(a.focused_document().is_some());
    }

    /// The preedit range reaches the tree, which is what draws the underline.
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

    /// Empty shows the placeholder and no preedit marks.
    #[test]
    fn an_empty_field_shows_only_its_placeholder() {
        let mut a = Gumicord::demo();
        let f = field(&a.build(&cx()));
        assert!(f.text.is_empty());
        assert!(f.placeholder.contains("メッセージを送信"));
        assert!(f.composing.is_none());
    }

    /// Enter appends and clears the field.
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

    /// Whitespace alone is not sent.
    #[test]
    fn whitespace_is_not_submitted() {
        let mut a = Gumicord::demo();
        a.input_focused = true;
        a.focused_document().unwrap().insert("   ");
        assert!(!a.submit());
    }

    /// Escape removes focus. During composition it cancels instead, and that
    /// branch belongs to the platform layer.
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

    /// A signed-out app. `Gumicord::new` reads the environment, and a
    /// developer's variables must not change test results.
    fn pending() -> Gumicord {
        Gumicord::with(
            Login::fresh_for_test(),
            Live::without_cache(),
            PluginManager::disabled(),
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
        assert!(matches!(a.login_field, Some(LoginField::Email)));
        a.focused_document().unwrap().insert("a@b.c");

        a.pressed(&[login_hit_of(
            NodeId::AppScreenLoginField,
            Key::Slot("password"),
        )]);
        assert!(matches!(a.login_field, Some(LoginField::Password)));
        a.focused_document().unwrap().insert("secret");

        assert_eq!(a.login_email.text(), "a@b.c", "email 欄の内容が消えた");
        assert_eq!(
            a.login_input.text(),
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
        assert_eq!(a.login_field, None, "送信後もフォーカスが残っている");
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
            a.login_field,
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
        assert_eq!(a.login_field, None);
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
        assert_eq!(a.login_field, None);
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
        assert_eq!(a.login_field, None, "送信後もフォーカスが残っている");
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
        assert!(matches!(a.login_field, Some(LoginField::Email)));

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
            a.login_input.has_selection(),
            "ログイン欄が選択されていない"
        );
        assert!(!a.input.has_selection(), "コンポーザーが触られた");
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

/// Builds the typing line.
///
/// Truncated after a few names: a busy server can have ten people typing at
/// once, which would push everything else off the line. Discord truncates
/// too.
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

    /// Nobody typing shows nothing.
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

    /// Fits on one line even on a busy server.
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

    /// Nothing to show while signed out.
    #[test]
    fn there_is_no_panel_before_logging_in() {
        let a = Gumicord::demo();
        assert!(a.user_panel().is_none());
        let side = a.sidebar(Panes::Three).unwrap();
        assert!(!names(&side).contains(&NodeId::NavUserPanel));
    }

    /// The user panel spans the guild list too; inside the channel list it
    /// would only be as wide as that.
    #[test]
    fn the_panel_spans_both_lists() {
        let a = Gumicord::demo();
        let side = a.sidebar(Panes::Three).expect("3 ペインなら出る");

        assert_eq!(side.id, NodeId::NavSidebar);
        assert_eq!(side.children[0].id, NodeId::NavSidebarLists);
        // The lists are grouped; the panel sits outside them.
        let inside = names(&side.children[0]);
        assert!(inside.contains(&NodeId::NavGuildList));
        assert!(inside.contains(&NodeId::NavChannelList));
        assert!(!inside.contains(&NodeId::NavUserPanel));

        // Growing here would take width from chat.
        assert_eq!(gumicord_render::intrinsic(NodeId::NavSidebar).grow, 0.0);
    }

    /// Only the list under the pointer gets a scrollbar.
    #[test]
    fn only_the_list_under_the_pointer_has_a_scrollbar() {
        let mut a = Gumicord::demo();

        // Nowhere means none of them.
        assert!(!names(&a.guild_list()).contains(&NodeId::LayoutScrollbar));
        assert!(!names(&a.channel_list()).contains(&NodeId::LayoutScrollbar));

        a.hovered_scroll = Some(NodeId::NavGuildList);
        assert!(names(&a.guild_list()).contains(&NodeId::LayoutScrollbar));
        // Not on the neighbouring list.
        assert!(!names(&a.channel_list()).contains(&NodeId::LayoutScrollbar));
    }

    /// The innermost scrollable wins.
    #[test]
    fn the_innermost_scroll_region_wins() {
        let mut a = Gumicord::demo();
        let hit = |id| Hit {
            id,
            key: None,
            rect: gumicord_render::Rect::ZERO,
            clip: None,
        };

        // Front to back: item, inner scrollable, outer container.
        a.hover_changed(&[
            hit(NodeId::NavChannelListItem),
            hit(NodeId::LayoutScroll),
            hit(NodeId::NavChannelList),
        ]);
        assert_eq!(a.hovered_scroll, Some(NodeId::LayoutScroll));

        a.hover_changed(&[]);
        assert_eq!(a.hovered_scroll, None);
    }

    /// At the narrowest width the lists go, and the panel with them.
    #[test]
    fn one_pane_has_no_sidebar_at_all() {
        let a = Gumicord::demo();
        assert!(a.sidebar(Panes::One).is_none());
    }

    /// One scroll region would carry the header and the panel off screen.
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

    #[test]
    fn user_menu_shows_account_options_and_masks_tokens() {
        let mut a = Gumicord::demo();
        let me = gumicord_model::CurrentUser {
            user: gumicord_model::User {
                id: UserId::from(1234567890u64),
                username: "Alice".to_owned(),
                discriminator: "0".to_owned(),
                global_name: Some("Alice".to_owned()),
                avatar_hash: None,
                bot: false,
            },
            email: None,
            verified: false,
            mfa_enabled: false,
        };
        let client = gumicord_rest::RestClient::anonymous().unwrap();
        let token = gumicord_model::Token::new("super_secret_token");
        a.login
            .set_logged_in(session::LoggedIn { me, client, token });

        let menu = a.user_menu();
        assert!(menu.iter().any(|it| it.label == "ID をコピー"));
        assert!(menu.iter().any(|it| it.label == "アカウントを追加"));
        assert!(menu.iter().any(|it| it.label == "ログアウト"));

        // Secret tokens must never appear in menu labels.
        for item in &menu {
            assert!(!item.label.contains("super_secret_token"));
        }
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
        // Without a guild this stays demo mode.
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

    /// The per-guild name wins.
    #[test]
    fn a_nickname_wins_over_the_global_name() {
        let a = app(message(Some("ねこ"), None));
        assert_eq!(a.message_rows()[0].author, "ねこ");

        let a = app(message(None, None));
        assert_eq!(a.message_rows()[0].author, "ねんねこ");
    }

    /// Avatars are per guild too; the guild appears in the URL.
    #[test]
    fn a_guild_avatar_wins_over_the_global_one() {
        let a = app(message(None, Some("xyz")));
        let url = a.message_rows()[0].avatar.clone().unwrap();
        assert!(
            url.starts_with("https://cdn.discordapp.com/guilds/1/users/7/avatars/xyz.png"),
            "{url}"
        );

        // No override and no avatar means the default one.
        let a = app(message(None, None));
        let url = a.message_rows()[0].avatar.clone().unwrap();
        assert!(url.contains("/embed/avatars/"), "{url}");
    }

    /// Colouring the member list but not the author line makes one person
    /// look like two.
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

        // Swap in a message from someone with that role.
        let mut m = message(None, None);
        m.member.as_mut().expect("居る").roles = vec![55u64.into()];
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);

        assert_eq!(a.message_rows()[0].tint, Some(0x00e0_5260));

        // And it reaches the tree.
        let tree = a.chat_view();
        let mut found = None;
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageHeaderAuthor {
                found = n.tint;
            }
        });
        assert_eq!(found, Some(Color::from_rgb(0x00e0_5260)));
    }

    /// REST messages carry no `member`. Reading only the message left a
    /// freshly opened channel with no nicknames, avatars or colours until one
    /// new message arrived and coloured just that row.
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
        // A member seen in the list or in earlier messages.
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

        // From REST, so no `member`.
        let mut m = message(None, None);
        m.member = None;
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), vec![m]);

        let row = &a.message_rows()[0];
        assert_eq!(row.author, "ねこ", "呼び名も出る");
        assert_eq!(row.tint, Some(0x00e0_5260), "役職の色も出る");
    }

    fn stamped(id: u64, user: u64, name: &str, timestamp: &str) -> Message {
        let mut m = message(None, None);
        m.id = MessageId::from(id);
        m.author.id = UserId::from(user);
        m.author.username = name.to_owned();
        m.author.global_name = None;
        m.timestamp = timestamp.to_owned();
        m.member = None;
        m
    }

    fn backlog(a: &mut Gumicord, messages: Vec<Message>) {
        a.live
            .store_mut()
            .set_backlog(ChannelId::from(10u64), messages);
    }

    /// Consecutive messages from one author share a header; the tree marks
    /// every message after the first grouped.
    #[test]
    fn close_messages_from_one_author_share_a_header() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-03T12:06:00+00:00"),
            ],
        );
        let rows = a.message_rows();
        assert_eq!(rows[0].day, rows[1].day);

        let tree = a.chat_view();
        let (mut dividers, mut lines, mut grouped) = (0, 0, Vec::new());
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageListDayDivider {
                dividers += 1;
            }
            if n.key == Some(Key::Slot("day_divider_line")) {
                lines += 1;
            }
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
        });
        assert_eq!(dividers, 1, "one day, one divider");
        assert_eq!(lines, 2, "a line reaches each side");
        assert_eq!(grouped, vec![false, true]);
    }

    /// A new day breaks the run and draws its divider, even minutes apart.
    /// A 26-hour gap always spans a local midnight, on any machine.
    #[test]
    fn a_new_day_breaks_the_run_and_draws_its_divider() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-04T14:00:00+00:00"),
            ],
        );
        let rows = a.message_rows();
        assert_ne!(rows[0].day, rows[1].day);

        let tree = a.chat_view();
        let (mut dividers, mut lines, mut grouped) = (0, 0, Vec::new());
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessageListDayDivider {
                dividers += 1;
            }
            if n.key == Some(Key::Slot("day_divider_line")) {
                lines += 1;
            }
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
        });
        assert_eq!(dividers, 2, "one divider per day, starting with the first");
        assert_eq!(lines, 4, "a line reaches each side");
        assert_eq!(grouped, vec![false, false]);
    }

    /// The date sits centred with a line reaching each side: the spacers
    /// either side of the label hold equal widths on the same height.
    #[test]
    fn the_day_divider_centres_its_label_between_two_lines() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00")],
        );
        let cx = gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        };
        let placed = gumicord_render::layout_for_test(&a.build(&cx), cx.viewport);

        let label = placed
            .iter()
            .find(|(id, _)| *id == NodeId::ChatMessageListDayDivider)
            .map(|(_, r)| *r)
            .expect("日付がない");
        let mut lines: Vec<_> = placed
            .iter()
            .filter(|(id, r)| *id == NodeId::LayoutSpacer && r.h > 0.0 && r.h <= 2.0)
            .map(|(_, r)| *r)
            .collect();
        assert_eq!(lines.len(), 2, "両側に線が1本ずつ");
        lines.sort_by(|a, b| a.x.total_cmp(&b.x));
        let (left, right) = (lines[0], lines[1]);
        assert!(
            left.x + left.w <= label.x + 1.0,
            "左の線がラベルに食い込んでいる"
        );
        assert!(
            label.x + label.w <= right.x + 1.0,
            "右の線がラベルに食い込んでいる"
        );
        assert!(
            (left.w - right.w).abs() < 2.0,
            "左右の線が均等でない {left:?} {right:?}"
        );
        assert!(
            (left.y - label.y).abs() < label.h,
            "線と文字が同じ高さにない"
        );
    }

    /// Seven minutes apart starts over, even on the same day.
    #[test]
    fn a_long_pause_starts_over() {
        let mut a = app(message(None, None));
        backlog(
            &mut a,
            vec![
                stamped(1, 7, "nenneko", "2026-09-03T12:00:00+00:00"),
                stamped(2, 7, "nenneko", "2026-09-03T12:07:00+00:00"),
            ],
        );
        let rows = a.message_rows();
        assert_eq!(rows[0].day, rows[1].day);

        let tree = a.chat_view();
        let mut grouped = Vec::new();
        tree.walk(&mut |n, _| {
            if n.id == NodeId::ChatMessage {
                grouped.push(n.states.contains(State::Grouped));
            }
        });
        assert_eq!(grouped, vec![false, false]);
    }

    /// The message's own member wins, being newer.
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

    /// A guild with one role, and a channel open in it.
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

    /// Growing the column later would reflow the body under the reader.
    #[test]
    fn the_column_stands_before_anything_arrives() {
        let a = app();

        let empty = a.member_list();
        assert_eq!(empty.id, NodeId::NavMemberList);
        assert!(empty.children.is_empty(), "中身はまだ無い");
        // "Not here yet" is not "nobody here".
        assert!(empty.states.contains(State::Loading));

        let tree = a.build_tree(Panes::Four);
        let mut found = false;
        tree.walk(&mut |n, _| found |= n.id == NodeId::NavMemberList);
        assert!(found, "幅があるうちは列が立っている");
    }

    /// Arrival clears `Loading`.
    #[test]
    fn the_loading_state_goes_away_once_people_arrive() {
        let mut a = app();
        sync(&mut a, vec![person("7", "ねんねこ")]);
        assert!(!a.member_list().states.contains(State::Loading));
    }

    /// Headings show names, never role ids.
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

    /// An unresolved role id tells the reader nothing.
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

    /// The role colour rides on the name node; the theme decides where it
    /// lands.
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

    /// Unknown roles do not get a default colour.
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

    /// The member list folds before chat does.
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
                // Categories sort first, so "the first row" picks one.
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

    /// Categories are headings; the default selection once picked one and
    /// opened a category nobody pressed.
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

    /// Categories still appear; not openable is not the same as not shown.
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

    /// One folder of three, and one guild outside it.
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

    /// Tiles on a folded folder. Counted by `collapsed`, since ordinary
    /// guilds use the same stable ID.
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

    /// The pill appears only when selected, unread or hovered — absent rather
    /// than zero-height, so a visible pill always means something.
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

        // Only on the selected guild.
        assert_eq!(found.len(), 1);
        assert!(found[0].contains(State::Selected));

        // None selected, none shown.
        a.selected_guild = 0;
        let mut none = Vec::new();
        pills(&a.guild_list(), &mut none);
        assert!(none.is_empty());
    }

    /// The icon is a child of the container, which is wider and leaves a lane
    /// for the pill.
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

    /// A folded folder tiles its contents; a box with one initial does not
    /// say which folder it is.
    #[test]
    fn a_closed_folder_shows_what_is_inside() {
        let mut a = app_with_folder();
        a.live.store_mut().set_collapsed([100]);

        assert_eq!(tiles(&a.guild_list()), 3);
    }

    /// Tiling an open folder would show the same icons twice.
    #[test]
    fn an_open_folder_does_not_repeat_its_contents() {
        let a = app_with_folder();

        assert_eq!(tiles(&a.guild_list()), 0);
    }

    /// As siblings the background would stop covering them and the folder's
    /// extent would be invisible.
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

    /// A folded folder holds no children.
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

    /// Anything beyond the 2x2 is pointless.
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

    /// A guild without an icon still takes a tile; a gap reads worse.
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

/// Builds an icon or avatar, falling back to initials.
///
/// The same node either way: swapping node types on arrival would change the
/// key and restart diffing. Only the renderer knows whether an image is in
/// hand, so this decides on the URL alone.
fn face(id: NodeId, url: Option<&str>, name: &str) -> UiNode {
    match url {
        Some(url) => UiNode::image(id, url),
        None => UiNode::text(id, initial(name)),
    }
}

#[cfg(test)]
mod plugin_tests {
    use super::*;

    fn plugins_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-app-plugin-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_plugin(root: &std::path::Path, id: &str, capabilities: &str, source: &str) {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("manifest.json"),
            format!(
                r#"{{"id":"{id}","name":"Hi","version":"1.0.0","capabilities":[{capabilities}]}}"#
            ),
        )
        .unwrap();
        std::fs::write(dir.join("plugin.js"), source).unwrap();
    }

    fn frame() -> gumicord_platform::FrameCx {
        gumicord_platform::FrameCx {
            viewport: gumicord_render::Size::new(1280.0, 800.0),
            scale: 1.0,
        }
    }

    /// A real plugin worker on a scratch directory. Demo data, not live:
    /// faking a login would show an empty live store with nothing to patch.
    fn app_with_plugins(dir: &std::path::Path) -> Gumicord {
        Gumicord::with(
            Login::skipped(),
            Live::without_cache(),
            PluginManager::start(dir.to_owned()),
        )
    }

    /// Approval dialogs only show once signed in.
    fn sign_in(a: &mut Gumicord) {
        a.login.set_logged_in(session::LoggedIn {
            me: gumicord_model::CurrentUser {
                user: gumicord_model::User {
                    id: UserId::from(1u64),
                    username: "nenneko".to_owned(),
                    discriminator: "0".to_owned(),
                    global_name: None,
                    avatar_hash: None,
                    bot: false,
                },
                email: None,
                verified: false,
                mfa_enabled: false,
            },
            client: gumicord_rest::RestClient::anonymous().unwrap(),
            token: gumicord_model::Token::new("t"),
        });
    }

    fn confirm_action(a: &Gumicord) -> Option<crate::menu::Action> {
        match &a.floating {
            Some(crate::menu::Floating::Confirm(c)) => Some(c.action.clone()),
            _ => None,
        }
    }

    #[test]
    fn safe_mode_flag() {
        assert!(!safe_mode_enabled(None));
        assert!(!safe_mode_enabled(Some("0")));
        // Same rule as the login skip: anything but "0" counts.
        assert!(safe_mode_enabled(Some("")));
        assert!(safe_mode_enabled(Some("1")));
    }

    /// Approval appears as a dialog naming the plugin and its capabilities;
    /// confirming records the grant.
    #[test]
    fn approving_loads_with_the_granted_capabilities() {
        let root = plugins_dir("approve");
        write_plugin(
            &root,
            "com.example.hi",
            r#""log""#,
            "globalThis.__gumicord_apply = (n) => n;",
        );
        let mut a = app_with_plugins(&root);
        sign_in(&mut a);
        let cx = frame();
        for _ in 0..2000 {
            a.build(&cx);
            if confirm_action(&a).is_some() {
                break;
            }
            // The worker scans on its own thread; spin without yielding
            // and it never gets scheduled.
            std::thread::yield_now();
        }
        let action = confirm_action(&a).expect("no approval dialog appeared");
        let (id, granted) = match action {
            crate::menu::Action::ApprovePlugin { id, granted } => (id, granted),
            other => panic!("not an approval dialog: {other:?}"),
        };
        assert_eq!(id, "com.example.hi");
        assert_eq!(granted, ["log"]);

        assert!(a.run_action(crate::menu::button::CONFIRM));
        for _ in 0..5000 {
            if root.join("grants.json").is_file() {
                break;
            }
            std::thread::yield_now();
        }
        let grants = std::fs::read_to_string(root.join("grants.json")).expect("no grants file");
        assert!(
            grants.contains("com.example.hi") && grants.contains("log"),
            "{grants}"
        );
    }

    /// Dismissing the dialog denies: the grant is recorded empty and the
    /// dialog does not come back.
    #[test]
    fn dismissing_denies_and_does_not_ask_again() {
        let root = plugins_dir("deny");
        write_plugin(
            &root,
            "com.example.hi",
            r#""storage""#,
            "globalThis.__gumicord_apply = (n) => n;",
        );
        let mut a = app_with_plugins(&root);
        sign_in(&mut a);
        let cx = frame();
        for _ in 0..2000 {
            a.build(&cx);
            if confirm_action(&a).is_some() {
                break;
            }
            std::thread::yield_now();
        }
        assert!(confirm_action(&a).is_some(), "no approval dialog appeared");

        assert!(a.run_action(crate::menu::button::CANCEL));
        for _ in 0..5000 {
            if root.join("grants.json").is_file() {
                break;
            }
            std::thread::yield_now();
        }
        // One more frame lets the dismissal settle into a denial.
        for _ in 0..5000 {
            a.build(&cx);
            let grants = std::fs::read_to_string(root.join("grants.json")).unwrap_or_default();
            if grants.contains("com.example.hi") {
                assert!(!grants.contains("storage"), "{grants}");
                break;
            }
            std::thread::yield_now();
        }
        // Settled and gone: rebuilding asks nothing more.
        for _ in 0..50 {
            a.build(&cx);
        }
        assert!(
            confirm_action(&a).is_none() && a.approval_queue.is_empty(),
            "denial did not stick"
        );
    }

    /// End to end: a capability-free plugin loads by itself and its patch
    /// shows up in the built tree, all off the main thread. The walk below
    /// mirrors the SDK runtime (bottom-up, original IDs, no output recursion).
    #[test]
    fn a_sample_patch_reaches_the_tree() {
        let root = plugins_dir("e2e");
        write_plugin(
            &root,
            "com.example.hi",
            "",
            r#"globalThis.__gumicord_apply = (n) => {
                const walk = (x) => {
                    const kids = (x.children ?? []).map(walk);
                    const cur = kids.length ? { ...x, children: kids } : x;
                    if (cur.id !== "chat.message.content") return cur;
                    return { ...cur, children: [...(cur.children ?? []), { id: "primitive.badge", props: { text: "hi" } }] };
                };
                return walk(n);
            };"#,
        );
        let mut a = app_with_plugins(&root);
        let cx = frame();
        let mut found = false;
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..2000 {
            let tree = a.build(&cx);
            // Mirror production: Patched goes back where it belongs, the
            // rest is only recorded.
            for e in a.plugins.drain() {
                match e {
                    ManagerEvent::Patched(t) => a.last_patched = Some(*t),
                    other => {
                        let s = format!("{other:?}");
                        if !seen.contains(&s) {
                            seen.push(s);
                        }
                    }
                }
            }
            let mut badges = 0;
            tree.walk(&mut |n, _| {
                if n.id == NodeId::PrimitiveBadge {
                    badges += 1;
                }
            });
            if badges > 0 {
                found = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(found, "the sample patch never reached the tree; {seen:?}");
    }
}

#[cfg(test)]
mod theme_hot_reload_tests {
    use super::*;

    fn theme_file(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-theme-reload-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("theme.json")
    }

    fn watching(path: std::path::PathBuf) -> Gumicord {
        let mut a = Gumicord::demo();
        a.theme_path = Some(path);
        a.theme_mtime = None;
        a
    }

    #[test]
    fn editing_the_theme_file_reapplies_it() {
        let path = theme_file("reapply");
        std::fs::write(&path, DEFAULT_THEME).unwrap();
        let mut a = watching(path);

        assert!(a.maybe_reload_theme());
        assert!(a.theme.is_some());
        assert!(!a.maybe_reload_theme(), "same file, no change");
    }

    #[test]
    fn a_broken_edit_keeps_the_last_good_theme() {
        let path = theme_file("broken");
        std::fs::write(&path, DEFAULT_THEME).unwrap();
        let mut a = watching(path);
        assert!(a.maybe_reload_theme());

        std::fs::write(a.theme_path.as_ref().unwrap(), "{broken").unwrap();
        a.theme_mtime = None;
        assert!(!a.maybe_reload_theme());
        assert!(a.theme.is_some(), "the broken edit took the theme down");
    }

    #[test]
    fn without_a_theme_file_there_is_nothing_to_watch() {
        let mut a = Gumicord::demo();
        a.theme_path = None;
        assert!(!a.maybe_reload_theme());
    }
}

#[cfg(test)]
mod toast_tests {
    use super::*;
    use gumicord_model::{Message, MessageId, User, UserId};

    fn live_app(timestamp: &str) -> Gumicord {
        let mut a = Gumicord::demo();
        a.selected_guild = 1;
        a.selected_channel = 10;
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
        a.live.store_mut().set_backlog(
            ChannelId::from(10u64),
            vec![Message {
                id: MessageId::from(1u64),
                channel_id: ChannelId::from(10u64),
                guild_id: None,
                author: User {
                    id: UserId::from(7u64),
                    username: "nenneko".to_owned(),
                    global_name: None,
                    discriminator: "0".to_owned(),
                    avatar_hash: None,
                    bot: false,
                },
                content: "hi".to_owned(),
                timestamp: timestamp.to_owned(),
                edited_timestamp: None,
                pinned: false,
                attachments: Vec::new(),
                member: None,
                referenced_message: None,
                mentions: Vec::new(),
                mention_everyone: false,
            }],
        );
        a
    }

    fn tip_text(tip: &UiNode) -> Vec<String> {
        let mut out = Vec::new();
        tip.walk(&mut |n, _| {
            if n.id == NodeId::OverlayTooltip
                && let Some(s) = n.content.as_text()
            {
                out.push(s.to_owned());
            }
        });
        out
    }

    /// Hovering a timestamp shows the whole date, not the short hour.
    #[test]
    fn hovering_a_timestamp_shows_the_whole_date() {
        let mut a = live_app("2026-09-03T12:00:00+00:00");
        let rows = a.message_rows();
        assert!(!rows.is_empty(), "backlog did not take");
        a.hovered = Some((NodeId::ChatMessageHeaderTime, Some(Key::Id(1))));
        let tip = a.tooltip().expect("no tip");
        assert_eq!(
            tip_text(&tip),
            vec![format!("{} {}", rows[0].day, rows[0].time)]
        );
    }

    /// Anywhere else, and for dateless rows, there is nothing to add.
    #[test]
    fn hovering_anywhere_else_shows_nothing() {
        let mut a = live_app("2026-09-03T12:00:00+00:00");
        assert!(!a.message_rows().is_empty(), "backlog did not take");
        a.hovered = Some((NodeId::ChatMessageContent, Some(Key::Id(1))));
        assert!(a.tooltip().is_none());
        a.hovered = None;
        assert!(a.tooltip().is_none());

        let mut b = live_app("あとで");
        assert!(!b.message_rows().is_empty(), "backlog did not take");
        b.hovered = Some((NodeId::ChatMessageHeaderTime, Some(Key::Id(1))));
        assert!(b.tooltip().is_none(), "dateless rows add nothing");
    }

    /// Notices stack three deep, then expire.
    #[test]
    fn toasts_cap_and_expire() {
        let mut a = Gumicord::demo();
        for n in ["one", "two", "three", "four"] {
            a.notify_toast(n.to_owned());
        }
        assert_eq!(a.toasts.len(), 3);
        assert_eq!(a.toasts[0].text, "two");

        let now = gumicord_platform::now_unix();
        a.toasts.push_front(crate::menu::Toast {
            text: "old".to_owned(),
            until: now - 1,
        });
        assert!(a.prune_toasts(now));
        assert!(a.toasts.iter().all(|t| t.until > now));
        assert!(!a.prune_toasts(now), "nothing left to drop");
    }

    /// Toasts ride the tree only while shown.
    #[test]
    fn toasts_reach_the_tree_while_shown() {
        let mut a = Gumicord::demo();
        let shown = |a: &Gumicord| {
            let mut found = false;
            a.build_tree(Panes::Four).walk(&mut |n, _| {
                found = found || n.id == NodeId::OverlayToast;
            });
            found
        };
        assert!(!shown(&a));
        a.notify_toast("hi".to_owned());
        assert!(shown(&a));
    }
}

#[cfg(test)]
mod plugin_data_tests {
    use super::*;
    use gumicord_model::{Member, Message, MessageId, RoleId, User, UserId};

    fn live_app() -> Gumicord {
        let mut a = Gumicord::demo();
        a.selected_guild = 1;
        a.selected_channel = 10;
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
                    topic: Some("ようこそ".to_owned()),
                    nsfw: false,
                    recipients: Vec::new(),
                    last_message_id: None,
                }],
                roles: vec![gumicord_model::Role {
                    id: 55u64.into(),
                    name: "管理者".to_owned(),
                    position: 3,
                    hoist: true,
                    color: None,
                }],
            }]);
        a.live.store_mut().set_backlog(
            ChannelId::from(10u64),
            vec![Message {
                id: MessageId::from(1u64),
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
                content: "hi".to_owned(),
                timestamp: "2026-09-03T12:00:00+00:00".to_owned(),
                edited_timestamp: None,
                pinned: true,
                attachments: Vec::new(),
                member: None,
                referenced_message: None,
                mentions: Vec::new(),
                mention_everyone: false,
            }],
        );
        a.live.store_mut().remember_member(
            GuildId::from(1u64),
            UserId::from(7u64),
            Member {
                nick: Some("ねこ".to_owned()),
                avatar_hash: None,
                roles: vec![RoleId::from(55u64)],
                joined_at: None,
                user: Some(User {
                    id: UserId::from(7u64),
                    username: "nenneko".to_owned(),
                    global_name: Some("ねんねこ".to_owned()),
                    discriminator: "0".to_owned(),
                    avatar_hash: None,
                    bot: false,
                }),
            },
        );
        a
    }

    fn table(a: &Gumicord, tree: &UiNode) -> serde_json::Value {
        let ctx = a.plugin_data_context(tree);
        ctx.data.expect("empty table")
    }

    /// A message node reads its body, author and ids.
    #[test]
    fn messages_carry_their_facts() {
        let a = live_app();
        let tree = UiNode::new(NodeId::ChatMessage).with_id_key(1).with_data(1);
        let data = table(&a, &tree);
        let m = &data["chat.message\n1"];
        assert_eq!(m["content"], "hi");
        assert_eq!(m["id"], "1");
        assert_eq!(m["channelId"], "10");
        assert_eq!(m["guildId"], "1");
        assert_eq!(m["author"]["username"], "nenneko");
        assert_eq!(m["author"]["displayName"], "ねんねこ");
        assert!(m.get("editedAt").is_none(), "absent, not null");
    }

    /// Keyless nodes key on the stable ID alone; unknown ids stay out.
    #[test]
    fn keys_follow_the_nodes() {
        let a = live_app();
        let tree = UiNode::new(NodeId::ChatMessage).with_data(1);
        let data = table(&a, &tree);
        assert!(data.get("chat.message\n").is_some());
        assert!(data.get("chat.message\n1").is_none());

        let tree = UiNode::new(NodeId::ChatMessage)
            .with_id_key(999)
            .with_data(999);
        assert!(table(&a, &tree).as_object().unwrap().is_empty());
    }

    /// Guilds, channels and members resolve with names and counts.
    #[test]
    fn guilds_channels_and_members_resolve() {
        let a = live_app();
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(UiNode::new(NodeId::NavGuildListItem).with_data(1))
            .child(UiNode::new(NodeId::NavChannelListItem).with_data(10))
            .child(
                UiNode::new(NodeId::NavMemberListItem)
                    .with_id_key(7)
                    .with_data(7),
            );
        let data = table(&a, &tree);
        let g = &data["nav.guild_list.item\n"];
        assert_eq!(g["name"], "テスト");
        let c = &data["nav.channel_list.item\n"];
        assert_eq!(c["name"], "いっぱん");
        assert_eq!(c["type"], "0");
        let m = &data["nav.member_list.item\n7"];
        assert_eq!(m["displayName"], "ねこ");
        assert_eq!(m["status"], "offline");
        assert_eq!(m["roles"], serde_json::json!(["管理者"]));
    }
}
