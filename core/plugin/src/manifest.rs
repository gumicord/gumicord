//! A plugin's manifest: identity and declared capabilities.
//!
//! The schema lives in `spec/schema/plugin-manifest.schema.json`; this
//! enforces the same rules at load time so a hand-edited file cannot widen
//! what the plugin may touch.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::PluginError;

/// Capabilities the host knows how to grant.
pub const KNOWN_CAPABILITIES: &[&str] = &["log", "storage"];

/// A parsed `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Entry file name; flat, never outside the plugin directory.
    #[serde(default = "default_entry")]
    pub entry: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Settings entry file name, when the plugin provides its own settings
    /// page. Same flat-file rules as `entry`; absent means no settings page.
    #[serde(default)]
    pub settings: Option<String>,
}

fn default_entry() -> String {
    "plugin.js".to_owned()
}

impl Manifest {
    /// Reads and checks `manifest.json` inside `dir`.
    pub fn load(dir: &Path) -> Result<Self, PluginError> {
        let path = dir.join("manifest.json");
        let raw = std::fs::read_to_string(&path).map_err(|e| PluginError::ManifestUnreadable {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        let manifest: Manifest =
            serde_json::from_str(&raw).map_err(|e| PluginError::ManifestInvalid {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
        manifest.check()?;
        Ok(manifest)
    }

    fn check(&self) -> Result<(), PluginError> {
        if !is_plugin_id(&self.id) {
            return Err(PluginError::BadManifestId(self.id.clone()));
        }
        if !is_semver(&self.version) {
            return Err(PluginError::BadVersion(self.version.clone()));
        }
        if !is_entry_name(&self.entry) {
            return Err(PluginError::BadEntry(self.entry.clone()));
        }
        if let Some(settings) = &self.settings
            && !is_entry_name(settings)
        {
            return Err(PluginError::BadEntry(settings.clone()));
        }
        let mut seen = HashSet::new();
        for cap in &self.capabilities {
            if !KNOWN_CAPABILITIES.contains(&cap.as_str()) {
                return Err(PluginError::UnknownCapability(cap.clone()));
            }
            if !seen.insert(cap) {
                return Err(PluginError::DuplicateCapability(cap.clone()));
            }
        }
        Ok(())
    }

    /// The entry file inside `dir`. Checked to stay flat at load.
    pub fn entry_path(&self, dir: &Path) -> PathBuf {
        dir.join(&self.entry)
    }
}

/// Reverse-domain shape, mirroring the theme manifest rule.
fn is_plugin_id(id: &str) -> bool {
    let mut parts = id.split('.');
    match (parts.next(), parts.next()) {
        (Some(first), Some(_)) if !first.is_empty() => {}
        _ => return false,
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !id.split('.').any(str::is_empty)
}

/// `1.2.3` with an optional pre-release tail, mirroring the schema.
fn is_semver(version: &str) -> bool {
    let core = version.split_once('-').map_or(version, |(c, _)| c);
    let mut parts = core.split('.');
    let triple = [parts.next(), parts.next(), parts.next()];
    if parts.next().is_some() {
        return false;
    }
    triple
        .into_iter()
        .all(|p| p.is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())))
}

fn is_entry_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains(['/', '\\'])
        && name != "."
        && name != ".."
        && (name.ends_with(".js") || name.ends_with(".qjsc"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, manifest_json: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("manifest.json"), manifest_json).unwrap();
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-plugin-manifest-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_full_manifest_loads() {
        let dir = scratch("full");
        write(
            &dir,
            r#"{"id":"com.example.hello","name":"Hello","version":"1.2.3",
                "entry":"plugin.qjsc","description":"Hi","capabilities":["log","storage"]}"#,
        );
        let m = Manifest::load(&dir).unwrap();
        assert_eq!(m.entry, "plugin.qjsc");
        assert_eq!(m.capabilities, ["log", "storage"]);
        assert!(m.entry_path(&dir).starts_with(&dir));
    }

    #[test]
    fn entry_defaults_to_plugin_js() {
        let dir = scratch("default-entry");
        write(
            &dir,
            r#"{"id":"com.example.h","name":"H","version":"1.0.0"}"#,
        );
        assert_eq!(Manifest::load(&dir).unwrap().entry, "plugin.js");
    }

    #[test]
    fn bad_manifests_are_refused() {
        for (tag, json) in [
            ("flat-id", r#"{"id":"hello","name":"H","version":"1.0.0"}"#),
            (
                "empty-part",
                r#"{"id":"com..hello","name":"H","version":"1.0.0"}"#,
            ),
            (
                "bad-version",
                r#"{"id":"com.example.h","name":"H","version":"1.0"}"#,
            ),
            (
                "escape",
                r#"{"id":"com.example.h","name":"H","version":"1.0.0","entry":"../evil.js"}"#,
            ),
            (
                "subdir",
                r#"{"id":"com.example.h","name":"H","version":"1.0.0","entry":"s/p.js"}"#,
            ),
            (
                "bad-ext",
                r#"{"id":"com.example.h","name":"H","version":"1.0.0","entry":"p.ts"}"#,
            ),
            (
                "unknown-cap",
                r#"{"id":"com.example.h","name":"H","version":"1.0.0","capabilities":["network"]}"#,
            ),
            (
                "dup-cap",
                r#"{"id":"com.example.h","name":"H","version":"1.0.0","capabilities":["log","log"]}"#,
            ),
        ] {
            let dir = scratch(tag);
            write(&dir, json);
            assert!(Manifest::load(&dir).is_err(), "{tag} loaded");
        }
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let dir = scratch("missing");
        assert!(Manifest::load(&dir.join("nope")).is_err());
    }

    #[test]
    fn settings_entry_follows_the_same_flat_rules() {
        let dir = scratch("settings-ok");
        write(
            &dir,
            r#"{"id":"com.example.h","name":"H","version":"1.0.0","settings":"settings.js"}"#,
        );
        assert_eq!(
            Manifest::load(&dir).unwrap().settings.as_deref(),
            Some("settings.js")
        );

        let dir = scratch("settings-absent");
        write(
            &dir,
            r#"{"id":"com.example.h","name":"H","version":"1.0.0"}"#,
        );
        assert!(Manifest::load(&dir).unwrap().settings.is_none());

        for (tag, settings) in [
            ("settings-escape", "../evil.js"),
            ("settings-subdir", "sub/settings.js"),
            ("settings-bad-ext", "settings.ts"),
        ] {
            let dir = scratch(tag);
            write(
                &dir,
                &format!(
                    r#"{{"id":"com.example.h","name":"H","version":"1.0.0","settings":"{settings}"}}"#
                ),
            );
            assert!(Manifest::load(&dir).is_err(), "{tag} loaded");
        }
    }
}
