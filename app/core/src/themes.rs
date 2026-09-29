//! Which theme is showing: selection, watching, namespace.
//!
//! The settings screen lists and picks; this owns what was picked, watches
//! the file, and points the background resolver at it.
use gumicord_theme::Theme;

use crate::assets::ThemeAssets;

/// Which theme is showing. One at a time: composing themes is M2
/// (`EXT-019`), so the settings screen picks a single one or the bundled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThemeSource {
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
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) path: std::path::PathBuf,
}

/// The default theme, embedded rather than loaded: the app has to run even
/// when no theme file can be read.
pub(crate) const DEFAULT_THEME: &str = include_str!("../../../examples/themes/midnight/theme.json");

/// Swaps the theme file, for authors and CI. Wins over the saved selection;
/// the settings screen manages everything else.
pub(crate) const THEME_ENV: &str = "GUMICORD_THEME";

/// The selected theme and its watch state. The settings screen picks;
/// this remembers, re-reads, and namespaces.
#[derive(Debug)]
pub(crate) struct ThemeState {
    pub(crate) theme: Option<Theme>,
    pub(crate) path: Option<std::path::PathBuf>,
    pub(crate) source: ThemeSource,
    pub(crate) dir: Option<std::path::PathBuf>,
    pub(crate) mtime: Option<std::time::SystemTime>,
    pub(crate) namespace: Option<String>,
}

impl ThemeState {
    /// The theme to start with: the environment's file, the saved
    /// selection, then the bundled theme. A saved theme that no longer
    /// reads falls back to bundled.
    pub(crate) fn initial(dir: Option<std::path::PathBuf>) -> Self {
        let (theme, path, source) = match &dir {
            Some(dir) => initial_theme_in(dir),
            None => (parse_theme_file(DEFAULT_THEME), None, ThemeSource::Bundled),
        };
        let mtime = path.as_ref().and_then(|p| mtime_of(p));
        ThemeState {
            theme,
            path,
            source,
            dir,
            mtime,
            namespace: None,
        }
    }

    /// Re-points the background resolver at the current theme.
    pub(crate) fn refresh_assets(&mut self, assets: &mut ThemeAssets) {
        let Some(theme) = &self.theme else {
            self.namespace = None;
            assets.set_theme(String::new(), None, String::new(), Vec::new(), Vec::new());
            return;
        };
        let dir = self
            .path
            .as_ref()
            .and_then(|p| p.parent())
            .map(std::path::Path::to_path_buf);
        let at = self
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "bundled".to_owned());
        let namespace = format!("{}:{at}", theme.manifest.id);
        self.namespace = Some(namespace.clone());
        assets.set_theme(
            namespace,
            dir,
            theme.manifest.name.clone(),
            theme.background_images(),
            theme.manifest.remote_assets.clone(),
        );
    }

    /// Applies a theme file, watching it from now on. A file that cannot
    /// be read or parsed leaves the current theme up.
    pub(crate) fn apply_file(&mut self, assets: &mut ThemeAssets, path: &std::path::Path) -> bool {
        let Ok(src) = std::fs::read_to_string(path) else {
            return false;
        };
        let Some(theme) = parse_theme_file(&src) else {
            return false;
        };
        self.theme = Some(theme);
        self.path = Some(path.to_owned());
        self.mtime = mtime_of(path);
        self.refresh_assets(assets);
        true
    }

    /// Re-reads the watched file when it changed. Runs on the frame
    /// boundary, never mid-build. The caller announces it; editors write
    /// broken JSON halfway through a save, and those stay silent here.
    pub(crate) fn maybe_reload(&mut self, assets: &mut ThemeAssets) -> bool {
        let path = match &self.path {
            Some(path) => path.clone(),
            None => return false,
        };
        if mtime_of(&path) == self.mtime {
            return false;
        }
        if !self.apply_file(assets, &path) {
            return false;
        }
        tracing::info!(?path, "reloaded the theme");
        true
    }

    /// Back to the embedded theme. The caller clears the saved selection.
    pub(crate) fn use_bundled(&mut self, assets: &mut ThemeAssets) {
        self.theme = parse_theme_file(DEFAULT_THEME);
        self.path = None;
        self.mtime = None;
        self.source = ThemeSource::Bundled;
        self.refresh_assets(assets);
    }

    /// The namespace the renderer reads every frame.
    pub(crate) fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }
}

/// Parses a theme file, warning about rejected rules like startup does. A
/// rejected rule never rejects the theme, but is never dropped silently.
pub(crate) fn parse_theme_file(src: &str) -> Option<Theme> {
    let result = Theme::parse(src);
    for d in &result.diagnostics {
        tracing::warn!("theme: {d}");
    }
    result.theme
}

/// Where installed themes live: one subdirectory per theme, each holding a
/// `theme.json` next to its assets.
pub(crate) fn themes_dir() -> Option<std::path::PathBuf> {
    gumicord_platform::app_data_dir().map(|d| d.join("themes"))
}

/// The saved selection: `{"theme": "<manifest id>"}`. Missing or broken
/// means the bundled theme.
fn active_path_in(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("active.json")
}

pub(crate) fn load_active_id_in(dir: &std::path::Path) -> Option<String> {
    let src = std::fs::read_to_string(active_path_in(dir)).ok()?;
    serde_json::from_str::<serde_json::Value>(&src)
        .ok()?
        .get("theme")?
        .as_str()
        .map(str::to_owned)
}

pub(crate) fn save_active_id_in(dir: &std::path::Path, id: Option<&str>) {
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
pub(crate) fn scan_themes_in(dir: &std::path::Path) -> Vec<InstalledTheme> {
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
pub(crate) fn initial_theme_in(
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
