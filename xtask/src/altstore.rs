//! AltStore source generation, one file per channel.
//!
//! The JSON is built in CI after the installable is published: it points
//! at the release asset by its final name, so generation runs after the
//! rename step. Nothing here is committed; the file rides to users as a
//! release asset (stable) or to the sources repository (nightly, served
//! over Pages).
//!
//!     cargo xtask altstore --channel nightly --tag nightly-20260926-abcdef1 \
//!       --ipa dist/gumicord-nightly-20260926-abcdef1.ipa --commit abcdef1 \
//!       --commit-message "..." --date 2026-09-26T12:00:00+00:00 \
//!       --out dist/apps-nightly.json

use std::path::{Path, PathBuf};

pub fn altstore(root: &Path, args: &[String]) -> Result<(), String> {
    let channel = value(args, "--channel")?;
    if channel != "nightly" && channel != "stable" {
        return Err("--channel must be nightly or stable".to_owned());
    }
    let tag = value(args, "--tag")?;
    let ipa = PathBuf::from(value(args, "--ipa")?);
    let commit = value(args, "--commit")?;
    let commit_message = value(args, "--commit-message")?;
    let date = value(args, "--date")?;
    let out = PathBuf::from(value(args, "--out")?);

    let workspace_version = workspace_version(root)?;
    let size = std::fs::metadata(&ipa)
        .map_err(|e| format!("cannot stat {}: {e}", ipa.display()))?
        .len();
    let asset = ipa
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("odd ipa file name: {}", ipa.display()))?;

    let source = source_json(
        &channel,
        &workspace_version,
        &tag,
        &commit,
        &commit_message,
        &date,
        asset,
        size,
    )?;
    validate(&source)?;
    let mut text = serde_json::to_string_pretty(&source)
        .map_err(|e| format!("cannot render source json: {e}"))?;
    text.push('\n');
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&out, text).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}

fn value(args: &[String], name: &str) -> Result<String, String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"));
        }
    }
    Err(format!("missing required {name}"))
}

/// The workspace version: the single source of truth both channels pin.
fn workspace_version(root: &Path) -> Result<String, String> {
    let manifest = std::fs::read_to_string(root.join("Cargo.toml"))
        .map_err(|e| format!("cannot read Cargo.toml: {e}"))?;
    for line in manifest.lines() {
        // First `version = "..."` at the top level is the workspace one.
        if let Some(rest) = line.strip_prefix("version = \"")
            && let Some(version) = rest.strip_suffix('"')
        {
            return Ok(version.to_owned());
        }
    }
    Err("no version in Cargo.toml".to_owned())
}

/// Builds the source document. Pure, so tests need no files.
#[allow(clippy::too_many_arguments)]
fn source_json(
    channel: &str,
    workspace_version: &str,
    tag: &str,
    commit: &str,
    commit_message: &str,
    date: &str,
    asset: &str,
    size: u64,
) -> Result<serde_json::Value, String> {
    let nightly = channel == "nightly";
    if nightly && !is_nightly_tag(tag) {
        return Err(format!("nightly tag looks wrong: {tag}"));
    }
    if !nightly && tag != format!("v{workspace_version}") {
        return Err(format!(
            "stable tag {tag} does not match workspace version {workspace_version}"
        ));
    }

    let version = if nightly {
        let (date8, sha) = nightly_tag_parts(tag);
        format!("{workspace_version}-nightly-{date8}-{sha}")
    } else {
        workspace_version.to_owned()
    };
    let version_description = if nightly {
        // First line only: the rest is not a changelog.
        let first = commit_message.lines().next().unwrap_or("").trim();
        if first.is_empty() {
            format!("Nightly build {commit}. Unsigned build.")
        } else {
            format!("{first}\nUnsigned nightly build.")
        }
    } else {
        format!("Stable release {tag} (unsigned build).")
    };
    let repo = if nightly {
        "gumicord/gumicord-nightly"
    } else {
        "gumicord/gumicord"
    };
    let app_name = if nightly {
        "Gumicord Nightly"
    } else {
        "Gumicord"
    };
    let icon = if nightly {
        "gumicord-nightly.png"
    } else {
        "gumicord.png"
    };

    Ok(serde_json::json!({
        "name": app_name,
        "identifier": if nightly { "dev.gumicord.altstore.nightly" } else { "dev.gumicord.altstore" },
        "website": "https://github.com/gumicord/gumicord",
        "iconURL": format!("https://raw.githubusercontent.com/gumicord/gumicord/main/packaging/icons/{icon}"),
        "tintColor": if nightly { "000000" } else { "5865F2" },
        "apps": [{
            "name": app_name,
            "bundleIdentifier": if nightly { "dev.gumicord.nightly" } else { "dev.gumicord.app" },
            "developerName": "Gumicord",
            "localizedDescription": "A third-party Discord client. WARNING: connecting to Discord with a third-party client breaks their terms of service and can cost you your account.",
            "iconURL": format!("https://raw.githubusercontent.com/gumicord/gumicord/main/packaging/icons/{icon}"),
            "tintColor": if nightly { "000000" } else { "5865F2" },
            "version": version,
            "versionDate": date,
            "versionDescription": version_description,
            "downloadURL": format!("https://github.com/{repo}/releases/download/{tag}/{asset}"),
            "size": size,
        }],
    }))
}

/// Every required field present and shaped: AltStore skips what it
/// cannot read, so an empty string ships as a broken source.
fn validate(source: &serde_json::Value) -> Result<(), String> {
    let app = source.pointer("/apps/0").ok_or("source has no apps")?;
    for key in [
        "name",
        "bundleIdentifier",
        "developerName",
        "localizedDescription",
        "iconURL",
        "tintColor",
        "version",
        "versionDate",
        "versionDescription",
        "downloadURL",
    ] {
        let text = app
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("{key} is missing"))?;
        if text.trim().is_empty() {
            return Err(format!("{key} is empty"));
        }
    }
    for key in ["name", "identifier", "iconURL"] {
        let text = source.get(key).and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            return Err(format!("source {key} is empty"));
        }
    }
    let url = app
        .get("downloadURL")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !url.starts_with("https://github.com/") {
        return Err(format!("downloadURL leaves github.com: {url}"));
    }
    if app.get("size").and_then(|v| v.as_u64()).unwrap_or(0) == 0 {
        return Err("size is zero".to_owned());
    }
    Ok(())
}

fn is_nightly_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix("nightly-") else {
        return false;
    };
    let mut parts = rest.splitn(2, '-');
    let (date, sha) = (parts.next(), parts.next());
    matches!((date, sha), (Some(d), Some(s)) if d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()) && s.len() == 7)
}

/// Splits `nightly-YYYYMMDD-sha7`. Checked by [`is_nightly_tag`] first.
fn nightly_tag_parts(tag: &str) -> (&str, &str) {
    let rest = tag.strip_prefix("nightly-").unwrap_or(tag);
    match rest.split_once('-') {
        Some((date, sha)) => (date, sha),
        None => (rest, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nightly() -> serde_json::Value {
        source_json(
            "nightly",
            "0.0.3",
            "nightly-20260926-abcdef1",
            "abcdef1",
            "Fix something\n\nBody here.",
            "2026-09-26T12:00:00+00:00",
            "gumicord-nightly-20260926-abcdef1.ipa",
            42000000,
        )
        .expect("fixture must build")
    }

    /// Nightly versions carry the date and the commit, so one build never
    /// hides behind another.
    #[test]
    fn nightly_versions_carry_the_date_and_the_commit() {
        let app = nightly().pointer("/apps/0").unwrap().clone();
        assert_eq!(app["version"], "0.0.3-nightly-20260926-abcdef1");
        assert_eq!(app["bundleIdentifier"], "dev.gumicord.nightly");
        assert_eq!(
            app["downloadURL"],
            "https://github.com/gumicord/gumicord-nightly/releases/download/nightly-20260926-abcdef1/gumicord-nightly-20260926-abcdef1.ipa"
        );
        assert_eq!(app["size"], 42000000);
        assert!(validate(&nightly()).is_ok());
    }

    /// Stable pins the workspace version; a mismatched tag refuses.
    #[test]
    fn stable_pins_the_workspace_version() {
        let source = source_json(
            "stable",
            "0.0.3",
            "v0.0.3",
            "abc1234",
            "",
            "2026-09-26T12:00:00+00:00",
            "gumicord-v0.0.3.ipa",
            41000000,
        )
        .expect("fixture must build");
        assert_eq!(
            source.pointer("/apps/0/version"),
            Some(&serde_json::json!("0.0.3"))
        );
        assert_eq!(
            source.pointer("/apps/0/bundleIdentifier"),
            Some(&serde_json::json!("dev.gumicord.app"))
        );
        assert!(
            source_json(
                "stable",
                "0.0.3",
                "v0.0.4",
                "abc1234",
                "",
                "2026-09-26T12:00:00+00:00",
                "gumicord-v0.0.4.ipa",
                1
            )
            .is_err()
        );
    }

    /// Malformed tags refuse instead of shipping a confusing version.
    #[test]
    fn malformed_tags_refuse() {
        for tag in ["v0.0.3", "nightly-2026092-abcdef1", "nightly-20260926-x"] {
            assert!(
                source_json(
                    "nightly",
                    "0.0.3",
                    tag,
                    "abcdef1",
                    "msg",
                    "2026-09-26T12:00:00+00:00",
                    "a.ipa",
                    1
                )
                .is_err(),
                "{tag} passed"
            );
        }
    }

    /// Empty fields never ship: AltStore skips what it cannot read.
    #[test]
    fn empty_fields_never_ship() {
        let mut bad = nightly();
        bad["apps"][0]["version"] = serde_json::json!("");
        assert!(validate(&bad).is_err());
        let mut zero = nightly();
        zero["apps"][0]["size"] = serde_json::json!(0);
        assert!(validate(&zero).is_err());
    }
}
