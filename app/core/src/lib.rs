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
//! There are two screens. A cache or a login leads
//! to the main screen; nothing leads to the login screen. Having a cache
//! skips waiting for login, since READY takes closer to a second and would
//! blow the cold-start budget — and because signing out deletes the cache,
//! its presence is itself proof of a previous session on this account.
//!
//! The rows show live data only. Without it the lists stay empty;
//! nothing falls back to bundled data.

pub mod a11y;
pub mod account;
pub mod assets;
pub mod images;
pub mod live;
pub mod markdown;
pub mod menu;
pub mod pages;
pub mod session;
pub mod time;

use std::collections::{HashMap, VecDeque};

use gumicord_model::{ChannelId, GuildId, MessageId, RoleId, UserId};
use gumicord_platform::{
    Application, FrameCx, HiddenKey, RevealRequest, Swipe, SwipeDir, TextDocument, Waker,
};
use gumicord_plugin::{ManagerEvent, PluginManager};
use gumicord_render::Hit;

use gumicord_theme::{MatchContext, Theme};
use gumicord_uitree::value::Color;
use gumicord_uitree::{Anchor, DataKind, Key, NodeId, State, UiNode};
use live::Live;
use pages::chat::Composing;
use pages::chat::{ChannelRow, GuildRow, MessageRow};
use pages::login::LoginField;
use session::Login;

/// The default theme, embedded rather than loaded: the app has to run even
/// when no theme file can be read.
const DEFAULT_THEME: &str = include_str!("../../../examples/themes/midnight/theme.json");

/// Swaps the theme file, for authors and CI. Wins over the saved selection;
/// the settings screen manages everything else.
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

/// Image sizes requested from the CDN, in logical px, matching what is drawn.
///
/// Never ask for more than is drawn: the atlas is one 2048-square page, and
/// requesting 128px for something drawn at 40px costs ten times the area. It
/// has overflowed in practice.
const GUILD_ICON_PX: f32 = 48.0;
const MESSAGE_AVATAR_PX: f32 = 40.0;
/// The user panel and the member list.
const SMALL_AVATAR_PX: f32 = 32.0;
/// The reply reference avatar, like Discord's.
const REPLY_AVATAR_PX: f32 = 16.0;

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
/// Slot for the member-list button in the chat header; same use. Only
/// built while the member pane is hidden.
const MEMBERS_OPEN: &str = "members_open";
/// Slot for the back button in the chat header; same use. Only built
/// while the guild list is hidden.
const BACK_OPEN: &str = "back_open";
/// The member-list button's icon. Unknown names draw nothing, so the
/// registry and this string are pinned together by a test below.
const MEMBERS_ICON: &str = "members";
/// The back button's icon; same guarantee.
const BACK_ICON: &str = "back";
/// A swipe starting this close to the left edge opens the drawer.
const DRAWER_EDGE: f32 = 24.0;
/// The gear's icon. Unknown names draw nothing, so the registry and this
/// string are pinned together by a test below.
const SETTINGS_GEAR: &str = "gear";

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

/// Phones have no window to drag or minimize; the OS owns the chrome.
/// By target, not width: a narrowed desktop window keeps its controls.
const fn is_mobile() -> bool {
    cfg!(target_os = "ios") || cfg!(target_os = "android")
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

/// The app state, and building the UITree from it.
pub struct Gumicord {
    theme: Option<Theme>,
    /// The theme file being watched, if one was configured. The bundled
    /// theme has no file, so there is nothing to watch for it.
    theme_path: Option<std::path::PathBuf>,
    /// Where the current theme came from. Decides which settings row shows
    /// as active.
    theme_source: ThemeSource,
    /// Where installed themes live and the selection is saved. `None`
    /// means nowhere: the bundled theme, always.
    themes_dir: Option<std::path::PathBuf>,
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
/// The main chat screen: lists, messages, composer. Owned outright by
/// [`pages::chat`](crate::pages::chat).
    chat: crate::pages::chat::ChatView,
    /// Theme match context, captured before building.
    ///
    /// Inline decoration is spans rather than nodes, so it never reaches the
    /// resolver's walk; the theme has to be consulted while building.
    match_ctx: MatchContext,
    /// Whatever is floating; at most one.
    floating: Option<crate::menu::Floating>,
    /// Transient notices; several share one node and none blocks input.
    toasts: VecDeque<crate::menu::Toast>,
    /// The login screens: fields, forms, errors. Owned outright by
    /// [`pages::login`](crate::pages::login).
    login_view: crate::pages::login::LoginView,
    /// Fetches images.
    images: images::Images,
    /// The time read at the head of the frame. Never re-read while building,
    /// or adjacent relative timestamps disagree.
    now: i64,
    /// How long the built tree stays valid. `None` means nothing changes
    /// with time. A `Cell` because building takes `&self`.
    holds: std::cell::Cell<Option<i64>>,
    /// First-frame diagnostics, written once for field debugging.
    startup_diag: std::cell::Cell<bool>,
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
    settings: crate::pages::settings::SettingsView,
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
        let login = Login::new();
        let mut app = Gumicord::with(login, live, Self::start_plugins());
        // Phones scan QR codes; they are not scanned. Start where typing
        // starts instead of on a code nobody can read.
        if is_mobile() {
            app.login.start_password();
            app.login_view.form = Some(LoginField::Password);
        }
        app
    }

    /// Skips login with an empty store. Opens no cache: real data mixed
    /// in would break the premise.
    pub fn demo() -> Self {
        Gumicord::with(
            Login::skipped(),
            Live::without_cache(),
            PluginManager::disabled(),
        )
    }

    /// Empty store with no theme folder: settings tests must neither read
    /// the machine's selection nor write it.
    #[cfg(test)]
    fn demo_unthemed() -> Self {
        Self::with_themes(
            Login::skipped(),
            Live::without_cache(),
            PluginManager::disabled(),
            None,
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
        Self::with_themes(login, live, plugins, themes_dir())
    }

    fn with_themes(
        login: Login,
        live: Live,
        plugins: PluginManager,
        themes_dir: Option<std::path::PathBuf>,
    ) -> Self {
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
            // Nothing yet; sync_selection picks something real once data arrives.
            None => (0, 0),
        };

        let (account_switch_tx, account_switch_rx) = std::sync::mpsc::channel();
        let (theme, theme_path, theme_source) = match &themes_dir {
            Some(dir) => initial_theme_in(dir),
            // Nowhere to install themes; the bundled one it is.
            None => (parse_theme_file(DEFAULT_THEME), None, ThemeSource::Bundled),
        };
        let theme_mtime = theme_path.as_ref().and_then(|p| mtime_of(p));

        let mut app = Gumicord {
            theme,
            theme_path,
            theme_source,
            themes_dir,
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
            chat: crate::pages::chat::ChatView::new(guild, channel),
            match_ctx: MatchContext::new(0.0),
            floating: None,
            toasts: VecDeque::new(),
            login_view: crate::pages::login::LoginView::new(),
            images: images::Images::new(),
            now: gumicord_platform::now_unix(),
            holds: std::cell::Cell::new(None),
            startup_diag: std::cell::Cell::new(false),
            account_switch_rx,
            account_switch_tx,
            plugins,
            last_patched: None,
            approval_queue: VecDeque::new(),
            dialogs: VecDeque::new(),
            showing: None,
            settings: crate::pages::settings::SettingsView::default(),
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

/// Which theme is showing. One at a time: composing themes is M2
/// (`EXT-019`), so the settings screen picks a single one or the bundled.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ThemeSource {
    /// The embedded theme.
    Bundled,
    /// An installed theme, by manifest id.
    Saved(String),
    /// The file the environment pointed at. Not a listed row.
    EnvFile,
}

/// An installed theme: one subdirectory of the themes folder holding a
/// `theme.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledTheme {
    id: String,
    name: String,
    version: String,
    path: std::path::PathBuf,
}

/// Parses a theme file, warning about rejected rules like startup does. A
/// rejected rule never rejects the theme, but is never dropped silently.
fn parse_theme_file(src: &str) -> Option<Theme> {
    let result = Theme::parse(src);
    for d in &result.diagnostics {
        tracing::warn!("theme: {d}");
    }
    result.theme
}

/// Where installed themes live: one subdirectory per theme, each holding a
/// `theme.json` next to its assets.
fn themes_dir() -> Option<std::path::PathBuf> {
    gumicord_platform::app_data_dir().map(|d| d.join("themes"))
}

/// The saved selection: `{"theme": "<manifest id>"}`. Missing or broken
/// means the bundled theme.
fn active_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("active.json")
}

fn load_active_id_in(dir: &std::path::Path) -> Option<String> {
    let src = std::fs::read_to_string(active_path_in(dir)).ok()?;
    serde_json::from_str::<serde_json::Value>(&src)
        .ok()?
        .get("theme")?
        .as_str()
        .map(str::to_owned)
}

fn save_active_id_in(dir: &std::path::Path, id: Option<&str>) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(raw) = serde_json::to_string_pretty(&serde_json::json!({ "theme": id })) else {
        return;
    };
    if let Err(e) = std::fs::write(active_path_in(dir), raw) {
        tracing::warn!(%e, "could not save the theme selection");
    }
}

/// Lists installed themes by manifest id. Broken ones are skipped with a
/// warning: a half-written theme must not hide the working ones.
fn scan_themes_in(dir: &std::path::Path) -> Vec<InstalledTheme> {
    let mut dirs: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.is_dir())
            .collect(),
        Err(_) => return Vec::new(),
    };
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        let path = dir.join("theme.json");
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(theme) = parse_theme_file(&src) else {
            tracing::warn!(?path, "skipping a theme that does not parse");
            continue;
        };
        let manifest = &theme.manifest;
        if out.iter().any(|t: &InstalledTheme| t.id == manifest.id) {
            tracing::warn!(id = %manifest.id, ?path, "duplicate theme id; keeping the first");
            continue;
        }
        out.push(InstalledTheme {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            version: manifest.version.clone(),
            path,
        });
    }
    out
}

/// The theme to start with: the environment's file, the saved selection,
/// then the bundled theme. A saved theme that no longer reads falls back
/// to bundled.
fn initial_theme_in(
    dir: &std::path::Path,
) -> (Option<Theme>, Option<std::path::PathBuf>, ThemeSource) {
    if let Some(path) = theme_file() {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(?path, %e, "could not read the theme; using the bundled one");
                DEFAULT_THEME.to_owned()
            }
        };
        return (parse_theme_file(&src), Some(path), ThemeSource::EnvFile);
    }
    if let Some(id) = load_active_id_in(dir) {
        match scan_themes_in(dir).into_iter().find(|t| t.id == id) {
            Some(t) => match std::fs::read_to_string(&t.path) {
                Ok(src) => match parse_theme_file(&src) {
                    Some(theme) => return (Some(theme), Some(t.path), ThemeSource::Saved(id)),
                    None => {
                        tracing::warn!(id = %id, "saved theme no longer parses; using the bundled one");
                    }
                },
                Err(e) => {
                    tracing::warn!(id = %id, %e, "saved theme unreadable; using the bundled one");
                }
            },
            None => {
                tracing::warn!(id = %id, "saved theme not installed; using the bundled one");
            }
        }
    }
    (parse_theme_file(DEFAULT_THEME), None, ThemeSource::Bundled)
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
        if !self.apply_theme_file(&path) {
            return false;
        }
        self.notify_toast("テーマを再読み込みしました".to_owned());
        tracing::info!(?path, "reloaded the theme");
        true
    }

    /// Applies a theme file, watching it from now on. A file that cannot
    /// be read or parsed leaves the current theme up.
    fn apply_theme_file(&mut self, path: &std::path::Path) -> bool {
        let Ok(src) = std::fs::read_to_string(path) else {
            return false;
        };
        let Some(theme) = parse_theme_file(&src) else {
            return false;
        };
        self.theme = Some(theme);
        self.theme_path = Some(path.to_owned());
        self.theme_mtime = mtime_of(path);
        self.refresh_theme_assets();
        true
    }

    fn select_theme(&mut self, id: String) {
        self.select_theme_in(id, self.themes_dir.clone());
    }

    /// Applies an installed theme and remembers it. Tests pass their own
    /// folder so the machine's selection stays untouched.
    fn select_theme_in(&mut self, id: String, dir: Option<std::path::PathBuf>) {
        let Some(t) = self.settings.themes.iter().find(|t| t.id == id).cloned() else {
            return;
        };
        if !self.apply_theme_file(&t.path) {
            self.notify_toast(format!("「{}」は読み込めませんでした", t.name));
            return;
        }
        self.theme_source = ThemeSource::Saved(id.clone());
        if let Some(dir) = &dir {
            save_active_id_in(dir, Some(&id));
        }
        self.refresh_theme_list();
        self.notify_toast(format!("テーマを「{}」にしました", t.name));
    }

    fn use_bundled_theme(&mut self) {
        self.use_bundled_theme_in(self.themes_dir.clone());
    }

    fn use_bundled_theme_in(&mut self, dir: Option<std::path::PathBuf>) {
        self.theme = parse_theme_file(DEFAULT_THEME);
        self.theme_path = None;
        self.theme_mtime = None;
        self.theme_source = ThemeSource::Bundled;
        self.refresh_theme_assets();
        if let Some(dir) = &dir {
            save_active_id_in(dir, None);
        }
        self.notify_toast("標準のテーマに戻しました".to_owned());
    }

    /// Which node the screen reader follows. Dialogs and menus grab it;
    /// otherwise the focused field does, then the last pressed message.
    /// Pressing a message is the only way to point the reader at the chat:
    /// hovering moves nothing.
    /// Which node the screen reader follows. Dialogs and menus grab it;
    /// otherwise the focused field does, then the drawer or the sheet,
    /// then the last pressed message.
    fn a11y_focus(&self) -> Option<(&'static str, Option<Key>)> {
        if matches!(self.floating, Some(crate::menu::Floating::Confirm(_))) {
            Some(("overlay.modal", None))
        } else if matches!(self.floating, Some(crate::menu::Floating::Menu(_))) {
            Some(("overlay.menu", None))
        } else if self.chat.input_focused {
            Some(("chat.input.field", None))
        } else if self.login_view.field.is_some() {
            Some(("app.screen.login.field", None))
        } else if self.chat.drawer_open {
            Some(("overlay.drawer", None))
        } else if self.chat.member_sheet_open {
            Some(("overlay.sheet", None))
        } else {
            self.chat.a11y_message
                .map(|id| ("chat.message", Some(Key::Id(id))))
        }
    }
}

impl Gumicord {
    /// One press against the hit arms. Returns what changed, if anything.
    fn press_loop(&mut self, hits: &[Hit]) -> bool {
        let mut changed = false;
        // Only the frontmost selectable hit.
        for h in hits {
            match (h.id, &h.key) {
                // The way into the password form (or its submit / back).
                // Every login_* slot routes here; the handler ignores what
                // it does not know, but the press must not fall through.
                (
                    NodeId::PrimitiveButton,
                    Some(Key::Slot(
                        slot @ ("login_submit"
                        | "login_back"
                        | "login_password"
                        | "login_forgot_password"
                        | "login_qr"
                        | "login_register"),
                    )),
                ) => {
                    changed |= self.login_button(slot);
                }
                // Folders only fold; they do not change the selected guild.
                (NodeId::NavGuildListFolder, Some(Key::Id(id))) => {
                    self.live.toggle_folder(*id);
                    changed = true;
                }
                (NodeId::NavGuildListItem, Some(Key::Id(id))) => {
                    if self.chat.selected_guild == *id {
                        break;
                    }
                    self.chat.selected_guild = *id;
                    // Clear the channel, or the list and the body disagree.
                    self.chat.selected_channel = 0;
                    changed = true;
                }
                (NodeId::NavChannelListItem, Some(Key::Id(id))) => {
                    changed |= self.chat.selected_channel != *id;
                    self.chat.selected_channel = *id;
                }
                (NodeId::PrimitiveButton, Some(Key::Slot(CANCEL_COMPOSING))) => {
                    changed |= self.stop_composing();
                }
                // The gear sits on the user panel; its hit comes first, so
                // this arm wins over the panel's own menu.
                (NodeId::PrimitiveButton, Some(Key::Slot(SETTINGS_OPEN))) => {
                    changed |= self.open_settings();
                }
                // The member-list button in the chat header.
                (NodeId::PrimitiveButton, Some(Key::Slot(MEMBERS_OPEN))) => {
                    changed |= self.open_member_sheet();
                }
                // The back button in the chat header.
                (NodeId::PrimitiveButton, Some(Key::Slot(BACK_OPEN))) => {
                    changed |= self.open_drawer();
                }
                // A press anywhere else on the message still opens all of it:
                // a single run is a small target.
                (NodeId::ChatMessage, Some(Key::Id(id))) => {
                    changed |= self.chat.reveals.messages.insert(*id);
                    self.chat.a11y_message = Some(*id);
                }
                // A reply reference jumps to the answered message.
                (NodeId::ChatMessageReplyRef, Some(Key::Id(target))) => {
                    changed |= self.jump_to_message(*target);
                }
                _ => continue,
            }
            break;
        }
        changed
    }

    /// A press while the drawer or the member sheet is open. Content acts
    /// through the normal arms; anything else dismisses.
    fn overlay_press(&mut self, hits: &[Hit]) -> bool {
        // Member rows have no profile view yet; tapping one closes the
        // sheet instead of stranding the press.
        if self.chat.member_sheet_open && hits.iter().any(|h| h.id == NodeId::NavMemberListItem) {
            return self.close_member_sheet();
        }
        let content = hits.iter().any(|h| {
            matches!(
                h.id,
                NodeId::NavSidebar
                    | NodeId::NavSidebarLists
                    | NodeId::NavGuildList
                    | NodeId::NavGuildListHome
                    | NodeId::NavGuildListItem
                    | NodeId::NavGuildListFolder
                    | NodeId::NavChannelList
                    | NodeId::NavChannelListItem
                    | NodeId::NavDmList
                    | NodeId::NavDmListItem
                    | NodeId::NavUserPanel
                    | NodeId::NavMemberList
                    | NodeId::NavMemberListGroup
                    | NodeId::NavMemberListSheet
                    | NodeId::NavMemberListItem
                    | NodeId::PrimitiveButton
                    | NodeId::LayoutScrollbarThumb
            )
        });
        if !content {
            let drawer = self.close_drawer();
            let sheet = self.close_member_sheet();
            return drawer || sheet;
        }
        let (guild, channel, settings_was, floating_was) = (
            self.chat.selected_guild,
            self.chat.selected_channel,
            self.settings.open,
            self.floating.is_some(),
        );
        let changed = self.press_loop(hits);
        if self.chat.selected_guild != guild
            || self.chat.selected_channel != channel
            || self.settings.open && !settings_was
            || self.floating.is_some() && !floating_was
        {
            self.close_drawer();
            self.close_member_sheet();
        }
        changed
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
                let channel = ChannelId::from(self.chat.selected_channel);
                self.live.load_older(channel);
            }
            // Members grow downward, at the far end of the scroll.
            NodeId::NavMemberList => {
                if max <= 0.0 || at < max - REACH {
                    return;
                }
                let guild = GuildId::from(self.chat.selected_guild);
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

    fn take_reveal(&mut self) -> Option<RevealRequest> {
        self.chat.pending_reveal.take().map(|target| RevealRequest {
            region: NodeId::ChatMessageList,
            id: NodeId::ChatMessage,
            key: Some(Key::Id(target)),
        })
    }

    /// Drains background events. The only entry point for them.
    fn wake(&mut self) -> bool {
        let mut changed = self.login.poll();
        if let Some(msg) = self.login.take_last_error() {
            self.login_view.error = Some(msg);
            changed = true;
        }
        let fields = self.login.take_field_errors();
        if !fields.is_empty() {
            self.login_view.field_errors = fields;
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
                        self.chat.selected_channel = ch.get();
                        if let Some(c) = self.live.store().channel(ch)
                            && let Some(g) = c.guild_id
                        {
                            self.chat.selected_guild = g.get();
                        }
                    } else {
                        self.chat.selected_guild = 0;
                        self.chat.selected_channel = 0;
                    }
                    self.images.forget_everything();
                    self.floating = None;
                    self.chat.composing = Composing::New;
                    self.chat.input.take();
                    self.chat.input_focused = false;
                    self.chat.reveals = crate::markdown::Reveals::default();
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

        // The drawer and the member sheet own their presses while open:
        // letting one through would navigate the chat behind them.
        if self.chat.drawer_open || self.chat.member_sheet_open {
            return self.overlay_press(hits);
        }

        let mut changed = false;

        // Pressing outside the composer removes focus.
        let on_input = hits.iter().any(|h| h.id == NodeId::ChatInputField);
        if on_input != self.chat.input_focused {
            self.chat.input_focused = on_input;
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
        }) && (self.login_view.field != Some(field) || self.chat.input_focused)
        {
            self.login_view.field = Some(field);
            self.chat.input_focused = false;
            changed = true;
        }

        // A press outside every field releases focus; otherwise the keyboard
        // stays up on phones with no other way to dismiss it.
        if self.login_view.field.is_some() && !hits.iter().any(|h| h.id == NodeId::AppScreenLoginField) {
            self.login_view.field = None;
            changed = true;
        }

        // Only the frontmost selectable hit.
        changed |= self.press_loop(hits);

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
        if self.chat.reveals.is_open(owner, run) {
            self.chat.reveals.shut_run(owner, run);
        } else {
            self.chat.reveals.open_run(owner, run);
        }
        true
    }

    fn swiped(&mut self, hits: &[Hit], swipe: Swipe) -> bool {
        // Overlays own every gesture while open, like they own presses.
        if self.floating.is_some() || self.settings.open {
            return false;
        }
        let Swipe::Point { dir, x, .. } = swipe;
        match dir {
            SwipeDir::Left => {
                // A message swiped left starts a reply, like the menu does.
                if let Some(id) = hits.iter().find_map(|h| match (h.id, &h.key) {
                    (NodeId::ChatMessage, Some(Key::Id(id))) => Some(*id),
                    _ => None,
                }) {
                    self.chat.composing = Composing::Reply(id);
                    self.chat.input_focused = true;
                    self.chat.a11y_message = Some(id);
                    return true;
                }
                // Anywhere else closes the drawer.
                self.close_drawer()
            }
            SwipeDir::Right => {
                // From the screen edge with the lists hidden: the drawer.
                if x <= DRAWER_EDGE && !self.chat.drawer_open {
                    return self.open_drawer();
                }
                false
            }
            // Vertical moves scroll; they never act.
            SwipeDir::Up | SwipeDir::Down => false,
        }
    }

    /// Secondary press; what was hit decides the menu.
    fn context_menu(&mut self, hits: &[Hit], at: (f32, f32)) -> bool {
        // Reopens rather than closing, or opening the next message's menu
        // would take two presses.
        let items = hits.iter().find_map(|h| match (h.id, &h.key) {
            // The composer first: it overlaps the message list. Focusing it
            // makes the menu and its items act on the composer.
            (NodeId::ChatInputField, _) => {
                self.chat.input_focused = true;
                self.login_view.field = None;
                Some(self.field_menu())
            }
            // A login-form field: focusing it makes the menu and its items
            // act on that field.
            (
                NodeId::AppScreenLoginField,
                Some(Key::Slot(s @ ("email" | "password" | "totp" | "token"))),
            ) => {
                self.login_view.field = Some(match *s {
                    "email" => LoginField::Email,
                    "password" => LoginField::Password,
                    "token" => LoginField::Token,
                    _ => LoginField::Totp,
                });
                self.chat.input_focused = false;
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
        match self.login_view.field {
            Some(LoginField::Email) => Some(&mut self.login_view.email),
            Some(LoginField::Password | LoginField::Totp | LoginField::Token) => {
                Some(&mut self.login_view.input)
            }
            None => self.chat.input_focused.then_some(&mut self.chat.input),
        }
    }

    /// What the focused field wants from the soft keyboard (mobile only).
    fn ime_field(&self) -> Option<gumicord_platform::ImeField> {
        use gumicord_platform::{ImeField, ImeKind};
        match self.login_view.field {
            Some(LoginField::Email) => Some(ImeField {
                kind: ImeKind::Email,
                multiline: false,
            }),
            Some(LoginField::Password) => Some(ImeField {
                kind: ImeKind::Password,
                multiline: false,
            }),
            Some(LoginField::Totp) => Some(ImeField {
                kind: ImeKind::Number,
                multiline: false,
            }),
            Some(LoginField::Token) => Some(ImeField {
                kind: ImeKind::Text,
                multiline: false,
            }),
            None => self.chat.input_focused.then_some(ImeField {
                kind: ImeKind::Text,
                multiline: true,
            }),
        }
    }

    /// The IME committed a newline in a single-line field (mobile only):
    /// advance through the login form, or submit.
    fn ime_newline(&mut self) -> bool {
        match self.login_view.field {
            Some(LoginField::Email) => self.focus_neighbor(true),
            _ => self.submit(),
        }
    }

    /// Sends, edits or replies, depending on [`Composing`]. Missing that
    /// turns an intended edit into a new message.
    ///
    /// On the login form it instead submits that step: the whole form on the
    /// password screen, or the TOTP code. Enter means the same thing in both
    /// places.
    fn submit(&mut self) -> bool {
        if self.login_view.field.is_some() {
            return self.submit_login();
        }

        let body = self.chat.input.text().trim().to_owned();
        let mode = self.chat.composing;

        // Emptying an edit is not a delete; Discord rejects it too. Clearing
        // the field and pressing enter must not destroy the message.
        if body.is_empty() {
            return false;
        }
        self.chat.input.take();
        self.chat.composing = Composing::New;

        if self.uses_live() {
            let channel = ChannelId::from(self.chat.selected_channel);
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

        // Without live data there is nowhere to send; the draft is still
        // cleared above so typing never sticks.
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
        // The drawer and the member sheet sit below dialogs and settings.
        if self.close_drawer() {
            return true;
        }
        if self.close_member_sheet() {
            return true;
        }
        // Escape on a login field abandons the whole password login.
        if self.login_view.field.is_some() {
            self.leave_login_form();
            return true;
        }
        // Cancel the reply or edit before discarding the draft; doing both at
        // once leaves it unclear which was lost.
        if self.stop_composing() {
            return true;
        }
        if !self.chat.input_focused {
            return false;
        }
        self.chat.input_focused = false;
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
            self.login_view.hidden_code.clear();
            return true;
        }

        self.login_view.hidden_code.push(key);
        let len = self.login_view.hidden_code.len();
        if self.login_view.hidden_code[..] != SEQUENCE[..len] {
            self.login_view.hidden_code.clear();
            return true;
        }
        if len == SEQUENCE.len() {
            self.login_view.hidden_code.clear();
            self.login.start_token();
            self.login_view.form = Some(LoginField::Token);
            self.login_view.field = Some(LoginField::Token);
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
        self.login_view.field = None;
        self.chat.input_focused = false;
        self.login_view.input.take();
    }

    /// Pipeline stages [3] through [5]. The plugin pass runs between them.
    fn build(&mut self, cx: &FrameCx) -> UiNode {
        // Image sizes depend on it, so capture before building.
        self.scale = cx.scale;

        // Read once per frame; re-reading mid-build makes adjacent relative
        // timestamps disagree.
        self.now = gumicord_platform::now_unix();
        self.holds.set(None);
        self.settle_jump();

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
        write_startup_diag(self, &tree, cx, panes);
        tree
    }
}

/// First-frame facts for field debugging, where no debugger reaches.
fn write_startup_diag(app: &Gumicord, tree: &UiNode, cx: &FrameCx, panes: Panes) {
    if app.startup_diag.get() {
        return;
    }
    app.startup_diag.set(true);
    let screen = if app.settings.open {
        "settings"
    } else if app.shows_main() {
        "main"
    } else {
        "login"
    };
    gumicord_platform::write_diag_file(
        "diag.log",
        &format!(
            "viewport={}x{} scale={} panes={panes:?} screen={screen} theme={} nodes={}\n",
            cx.viewport.w,
            cx.viewport.h,
            cx.scale,
            if app.theme.is_some() { "yes" } else { "none" },
            tree.count(),
        ),
    );
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

impl Gumicord {
    /// The current width class. Read from the last built frame; events
    /// never precede it meaningfully.
    fn panes(&self) -> Panes {
        Panes::for_width(self.match_ctx.window_width)
    }

    /// Rebuilds the whole tree every frame; diffing waits until the renderer's
    /// requirements settle.
    fn build_tree(&self, panes: Panes) -> UiNode {
        // The settings screen takes the screen's place, under the title bar:
        // covering the window controls would strand the window.
        let screen = if self.settings.open {
            self.settings_screen()
        } else if self.shows_main() {
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
                    .child_if(!is_mobile(), || self.titlebar())
                    .child(UiNode::new(NodeId::AppScreen).child(screen)),
            )
            // Only while open: a full-window layer would absorb every press.
            .child_if(self.floating.is_some(), || {
                let f = self.floating.as_ref().expect("直前に確かめた");
                f.node(panes.present(), self.hovered_item())
            })
            // The drawer and the member sheet sit above the chat but below
            // dialogs: a decision interrupts navigation, not the reverse.
            .child_if(self.chat.drawer_open, || {
                UiNode::new(NodeId::OverlayDrawer)
                    .with_anchor(Anchor::at(0.0, 0.0))
                    .children(self.sidebar(Panes::Four))
            })
            .child_if(self.chat.member_sheet_open, || {
                let sheet = UiNode::new(NodeId::OverlaySheet)
                    .child(UiNode::new(NodeId::OverlaySheetHandle));
                // The sheet's own container fills the width; the side
                // pane's fixed-width one would leave a narrow rail.
                let mut list = UiNode::new(NodeId::NavMemberListSheet);
                match self.member_list_rows() {
                    None => {
                        list = list.with_state(State::Loading);
                    }
                    Some(rows) => {
                        list = list
                            .children(rows)
                            .children(self.scrollbar(NodeId::NavMemberListSheet));
                    }
                }
                sheet.child(list)
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
                self.chat.composing = Composing::Reply(*id);
                self.chat.input_focused = true;
            }
            crate::menu::Action::Edit(id) => {
                // The raw body; the parsed one would silently drop the
                // markup.
                let Some(text) = self.raw_body(*id) else {
                    return true;
                };
                self.chat.input.take();
                self.chat.input.insert(&text);
                self.chat.composing = Composing::Edit(*id);
                self.chat.input_focused = true;
            }
            crate::menu::Action::Delete(id) => {
                // Only reached after the dialog confirmed.
                // ([`Self::needs_confirming`])
                self.live
                    .delete_message(ChannelId::from(self.chat.selected_channel), MessageId::from(*id));
                // Deleting what is being edited also cancels the edit.
                if self.chat.composing.target() == Some(*id) {
                    self.chat.composing = Composing::New;
                    self.chat.input.take();
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
            crate::menu::Action::ReloadPlugin(id) => {
                self.plugins.reload(id);
                self.refresh_settings_states();
            }
            crate::menu::Action::SelectTheme(id) => {
                self.select_theme(id.clone());
            }
            crate::menu::Action::UseBundledTheme => {
                self.use_bundled_theme();
            }
            crate::menu::Action::ShareLog => {
                // The toast is the whole result surface: success names
                // what opened, failure says so in one line.
                match gumicord_platform::share_log() {
                    Ok(note) => self.notify_toast(note),
                    Err(e) => self.notify_toast(e.to_string()),
                }
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
        self.login_view.form = None;
        self.login_view.error = None;
        self.login_view.field_errors.clear();
        self.floating = None;
        self.chat.composing = Composing::New;
        self.chat.input.take();
        self.chat.input_focused = false;
        self.chat.reveals = crate::markdown::Reveals::default();
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
        self.login_view.form = None;
        self.login_view.error = None;
        self.login_view.field_errors.clear();
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
        self.chat.composing = Composing::New;
        self.chat.input.take();
        self.chat.input_focused = false;
        self.chat.reveals = crate::markdown::Reveals::default();
        true
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

        let guild = GuildId::from(self.chat.selected_guild);
        let channel = ChannelId::from(self.chat.selected_channel);
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
            .child_if(m.reply.is_some(), || {
                let reply = m.reply.as_ref().expect("直前に確かめた");
                // Author and snippet share one line; splitting them for a
                // future language happens with the message table (ADR-0010).
                let text = if reply.snippet.is_empty() {
                    reply.author.clone()
                } else {
                    format!("{}: {}", reply.author, reply.snippet)
                };
                UiNode::new(NodeId::ChatMessageReplyRef)
                    .with_data(m.id)
                    .with_id_key(reply.target)
                    .child_if(reply.avatar.is_some(), || {
                        UiNode::image(
                            NodeId::ChatMessageReplyRefAvatar,
                            reply.avatar.clone().unwrap_or_default(),
                        )
                        .with_data(m.id)
                    })
                    .child(UiNode::text(NodeId::PrimitiveText, text).with_data(m.id))
            })
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

    /// Jumps the chat to a message. Loaded targets move at once; anything
    /// else is fetched around first, like the official client. A target
    /// that cannot be fetched says so instead of stranding the reader.
    fn jump_to_message(&mut self, target: u64) -> bool {
        if self.message_rows().iter().any(|m| m.id == target) {
            self.chat.pending_reveal = Some(target);
            self.chat.a11y_message = Some(target);
            self.chat.pending_jump = None;
            return true;
        }
        let channel = ChannelId::from(self.chat.selected_channel);
        if !self.live.fetch_around(channel, target) {
            self.notify_toast("そのメッセージは読み込めませんでした".to_owned());
            return true;
        }
        self.chat.pending_jump = Some((channel, target));
        true
    }

    /// Settles a waiting jump once its messages arrive. Runs on the frame
    /// after the fetch lands: the rows have to exist before revealing.
    fn settle_jump(&mut self) {
        let Some((channel, target)) = self.chat.pending_jump else {
            return;
        };
        if ChannelId::from(self.chat.selected_channel) != channel {
            self.chat.pending_jump = None;
            return;
        }
        if self.live.take_around_failed(channel, target) {
            self.chat.pending_jump = None;
            self.notify_toast("そのメッセージは読み込めませんでした".to_owned());
            return;
        }
        if self.message_rows().iter().any(|m| m.id == target) {
            self.chat.pending_reveal = Some(target);
            self.chat.a11y_message = Some(target);
            self.chat.pending_jump = None;
        }
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
            &self.chat.reveals,
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
            guild: GuildId::from(self.chat.selected_guild),
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

impl Gumicord {
    /// Whether real data is being shown.
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
    /// The startup selection holds ids that may not exist once READY
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
        if !guilds.iter().any(|g| g.id == self.chat.selected_guild) {
            let Some(first) = guilds.first() else {
                // READY has not arrived; select nothing.
                return false;
            };
            self.chat.selected_guild = first.id;
            self.chat.selected_channel = 0;
            changed = true;
        }

        let channels = self.openable_rows();
        if !channels.iter().any(|c| c.id == self.chat.selected_channel)
            && let Some(first) = channels.first()
        {
            self.chat.selected_channel = first.id;
            changed = true;
        }

        if self.chat.selected_channel != 0 {
            // A no-op after the first call.
            self.live.open_channel(
                GuildId::from(self.chat.selected_guild),
                ChannelId::from(self.chat.selected_channel),
            );
        }
        changed
    }
}

/// The first character of a name, shown when there is no icon.
fn initial(name: &str) -> String {
    name.chars().next().map(String::from).unwrap_or_default()
}

/// A scrollbar. The thumb's size and position come from the renderer, since
/// the overflow is only known after layout. This says only that the list has
/// one; the theme decides how it looks.
fn scrollbar_node() -> UiNode {
    UiNode::new(NodeId::LayoutScrollbar).child(UiNode::new(NodeId::LayoutScrollbarThumb))
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
        // Demo rows are gone; seed one live message so the patched
        // content nodes exist.
        a.live.store_mut().replace_guilds(vec![gumicord_model::Guild {
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
            vec![gumicord_model::Message {
                id: MessageId::from(1u64),
                channel_id: ChannelId::from(10u64),
                guild_id: None,
                author: gumicord_model::User {
                    id: UserId::from(7u64),
                    username: "nenneko".to_owned(),
                    global_name: None,
                    discriminator: "0".to_owned(),
                    avatar_hash: None,
                    bot: false,
                },
                content: "こんにちは".to_owned(),
                timestamp: "2026-08-22T12:34:56+00:00".to_owned(),
                edited_timestamp: None,
                pinned: false,
                attachments: Vec::new(),
                member: None,
                referenced_message: None,
                mentions: Vec::new(),
                mention_everyone: false,
            }],
        );
        a.chat.selected_guild = 1;
        a.chat.selected_channel = 10;
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
mod plugin_data_tests {
    use super::*;
    use gumicord_model::{Member, Message, MessageId, RoleId, User, UserId};

    fn live_app() -> Gumicord {
        let mut a = Gumicord::demo();
        a.chat.selected_guild = 1;
        a.chat.selected_channel = 10;
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
