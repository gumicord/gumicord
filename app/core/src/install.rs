//! Installing themes and plugins from archive files.
//!
//! Packages are zip or tarballs (`.tar.gz`, `.tgz`, `.tar`); 7z stays out
//! (mobile binary size). The layout inside mirrors an installed directory:
//! either the manifest sits at the root, or a single top folder holds it.
//! Everything lands validated: a broken manifest never reaches the
//! themes/plugins folders, and nothing installs twice.
//!
//! Entry point takes bytes, not a path, so desktop pickers and mobile
//! document providers share it: whoever holds the bytes calls in.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// What is being installed: decides the manifest file and the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    Theme,
    Plugin,
}

impl InstallKind {
    fn manifest_name(self) -> &'static str {
        match self {
            InstallKind::Theme => "theme.json",
            InstallKind::Plugin => "manifest.json",
        }
    }
}

/// What landed, for the confirmation toast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub id: String,
    pub name: String,
    pub version: String,
}

/// Failures are Japanese: they reach the user through toasts.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("この形式には対応していない（zip・tar.gz・tgz・tarのみ）")]
    UnsupportedFormat,
    #[error("展開できなかった：{0}")]
    Corrupt(String),
    #[error("危険なパスが含まれているため止めた：{0}")]
    UnsafePath(String),
    #[error("大きすぎるため止めた（上限 {0}）")]
    TooLarge(String),
    #[error("説明書（{0}）が見つからない")]
    NoManifest(&'static str),
    #[error("説明書が壊れている：{0}")]
    InvalidManifest(String),
    #[error("宣言された本体ファイルがない：{0}")]
    MissingEntry(String),
    #[error("同じIDがすでに入っている：{0}")]
    AlreadyInstalled(String),
    #[error("書き込めなかった：{0}")]
    Io(String),
}

fn io(e: std::io::Error) -> InstallError {
    InstallError::Io(e.to_string())
}

/// Budget against archive bombs: counted before anything lands.
struct Budget {
    files: usize,
    bytes: u64,
}

/// Entries past this stop the install.
const MAX_FILES: usize = 5000;
/// Unpacked bytes past this stop the install.
const MAX_TOTAL_BYTES: u64 = 256 << 20;
/// One entry past this stops the install.
const MAX_ONE_FILE: u64 = 64 << 20;

impl Budget {
    fn add(&mut self, size: u64) -> Result<(), InstallError> {
        self.files += 1;
        self.bytes = self.bytes.saturating_add(size);
        if self.files > MAX_FILES {
            return Err(InstallError::TooLarge(format!("ファイル数{MAX_FILES}件超")));
        }
        if size > MAX_ONE_FILE {
            return Err(InstallError::TooLarge(format!(
                "1ファイル{}MB超",
                MAX_ONE_FILE >> 20
            )));
        }
        if self.bytes > MAX_TOTAL_BYTES {
            return Err(InstallError::TooLarge(format!(
                "合計{}MB超",
                MAX_TOTAL_BYTES >> 20
            )));
        }
        Ok(())
    }
}

/// A safe relative path inside the staging directory, or the reason it is
/// not one. Absolute paths, parents and Windows prefixes never pass.
fn safe_join(staging: &Path, name: &str) -> Result<PathBuf, InstallError> {
    let rel = Path::new(name);
    if rel.is_absolute() {
        return Err(InstallError::UnsafePath(name.to_owned()));
    }
    let mut out = staging.to_owned();
    for part in rel.components() {
        match part {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(InstallError::UnsafePath(name.to_owned()));
            }
        }
    }
    Ok(out)
}

/// Unpacks a zip into the staging directory.
fn unpack_zip(data: &[u8], staging: &Path, budget: &mut Budget) -> Result<(), InstallError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data))
        .map_err(|e| InstallError::Corrupt(e.to_string()))?;
    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| InstallError::Corrupt(e.to_string()))?;
        let Some(path) = file.enclosed_name() else {
            return Err(InstallError::UnsafePath(file.name().to_owned()));
        };
        if file.is_dir() {
            continue;
        }
        match file.compression() {
            zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated => {}
            method => {
                return Err(InstallError::Corrupt(format!(
                    "未対応の圧縮方式：{method:?}"
                )));
            }
        }
        budget.add(file.size())?;
        let dest = safe_join(staging, &path.to_string_lossy())?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut out = std::fs::File::create(&dest).map_err(io)?;
        std::io::copy(&mut file.take(MAX_ONE_FILE + 1), &mut out).map_err(io)?;
        if out.metadata().map_err(io)?.len() > MAX_ONE_FILE {
            return Err(InstallError::TooLarge(format!(
                "1ファイル{}MB超",
                MAX_ONE_FILE >> 20
            )));
        }
    }
    Ok(())
}

/// Unpacks a tarball (optionally gzipped) into the staging directory.
/// Links of any kind are refused: a theme has no business with them.
fn unpack_tar(
    data: &[u8],
    gzip: bool,
    staging: &Path,
    budget: &mut Budget,
) -> Result<(), InstallError> {
    if gzip {
        let decoder = flate2::read::GzDecoder::new(data);
        unpack_tar_entries(tar::Archive::new(decoder), staging, budget)
    } else {
        unpack_tar_entries(tar::Archive::new(data), staging, budget)
    }
}

fn unpack_tar_entries<R: Read>(
    mut archive: tar::Archive<R>,
    staging: &Path,
    budget: &mut Budget,
) -> Result<(), InstallError> {
    let entries = archive
        .entries()
        .map_err(|e| InstallError::Corrupt(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| InstallError::Corrupt(e.to_string()))?;
        let kind = entry.header().entry_type();
        match kind {
            tar::EntryType::Regular | tar::EntryType::Directory | tar::EntryType::Continuous => {}
            kind => {
                return Err(InstallError::UnsafePath(format!(
                    "リンク類は入れられない：{kind:?}"
                )));
            }
        }
        let path = entry
            .path()
            .map_err(|e| InstallError::Corrupt(e.to_string()))?;
        // Validated before writing; unpack_in guards the rest.
        safe_join(staging, &path.to_string_lossy())?;
        if kind.is_dir() {
            continue;
        }
        budget.add(entry.size())?;
        entry.unpack_in(staging).map_err(io)?;
    }
    Ok(())
}

/// Finds the installed directory: a lone top folder holding the manifest
/// wins, otherwise the staging root must hold it.
fn find_root(staging: &Path, manifest: &'static str) -> Result<PathBuf, InstallError> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(staging)
        .map_err(io)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    if entries.len() == 1 && entries[0].is_dir() && entries[0].join(manifest).is_file() {
        return Ok(entries.remove(0));
    }
    if staging.join(manifest).is_file() {
        return Ok(staging.to_owned());
    }
    Err(InstallError::NoManifest(manifest))
}

/// A directory name from a manifest id. Reverse-domain ids pass through;
/// anything else becomes underscores rather than escaping the folder.
fn dir_name(id: &str) -> Result<String, InstallError> {
    let name: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() {
        return Err(InstallError::InvalidManifest("IDが空".to_owned()));
    }
    Ok(name)
}

/// Installs one archive. `filename` only picks the format; the bytes are
/// what gets unpacked. Validates before anything reaches `dest_root`.
pub fn install_archive(
    data: &[u8],
    filename: &str,
    kind: InstallKind,
    dest_root: &Path,
) -> Result<Installed, InstallError> {
    let lower = filename.to_ascii_lowercase();
    let tar_kind = if lower.ends_with(".zip") {
        None
    } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        Some(true)
    } else if lower.ends_with(".tar") {
        Some(false)
    } else {
        return Err(InstallError::UnsupportedFormat);
    };

    let staging = dest_root.join(".incoming");
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(io)?;
    }
    std::fs::create_dir_all(&staging).map_err(io)?;
    let done = |r: Result<Installed, InstallError>| {
        if r.is_err() {
            let _ = std::fs::remove_dir_all(&staging);
        }
        r
    };

    let mut budget = Budget { files: 0, bytes: 0 };
    let unpacked = match tar_kind {
        None => unpack_zip(data, &staging, &mut budget),
        Some(gzip) => unpack_tar(data, gzip, &staging, &mut budget),
    };
    if let Err(e) = unpacked {
        return done(Err(e));
    }

    let root = match find_root(&staging, kind.manifest_name()) {
        Ok(root) => root,
        Err(e) => return done(Err(e)),
    };

    let installed = match kind {
        InstallKind::Theme => {
            let src = std::fs::read_to_string(root.join("theme.json")).map_err(io)?;
            let result = gumicord_theme::Theme::parse(&src);
            let Some(theme) = result.theme else {
                let detail = result
                    .diagnostics
                    .first()
                    .map(|d| d.to_string())
                    .unwrap_or_else(|| "読めない".to_owned());
                return done(Err(InstallError::InvalidManifest(detail)));
            };
            Installed {
                id: theme.manifest.id.clone(),
                name: theme.manifest.name.clone(),
                version: theme.manifest.version.clone(),
            }
        }
        InstallKind::Plugin => {
            let manifest = gumicord_plugin::Manifest::load(&root)
                .map_err(|e| InstallError::InvalidManifest(e.to_string()))?;
            if !manifest.entry_path(&root).is_file() {
                return done(Err(InstallError::MissingEntry(manifest.entry.clone())));
            }
            Installed {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
            }
        }
    };

    let dir = match dir_name(&installed.id) {
        Ok(dir) => dir,
        Err(e) => return done(Err(e)),
    };
    let dest = dest_root.join(&dir);
    if dest.exists() {
        return done(Err(InstallError::AlreadyInstalled(installed.id.clone())));
    }
    if let Err(e) = std::fs::rename(&root, &dest) {
        return done(Err(io(e)));
    }
    if root != staging {
        let _ = std::fs::remove_dir_all(&staging);
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-install-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A zip holding one theme at its root.
    fn theme_zip(top: Option<&str>) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        let prefix = top.map(|t| format!("{t}/")).unwrap_or_default();
        zip.start_file(
            format!("{prefix}theme.json"),
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(
            br#"{"manifest": {"id": "dev.example.pack", "name": "Pack", "version": "1.0.0", "abi": 1}, "rules": []}"#,
        )
        .unwrap();
        zip.finish().unwrap();
        out.into_inner()
    }

    /// A theme installs from a zip and reports its manifest.
    #[test]
    fn a_theme_installs_from_a_zip() {
        let root = dir("theme-zip");
        let data = theme_zip(None);
        let got = install_archive(&data, "pack.zip", InstallKind::Theme, &root).unwrap();
        assert_eq!(got.id, "dev.example.pack");
        assert!(root.join("dev.example.pack").join("theme.json").is_file());
        assert!(!root.join(".incoming").exists(), "作業場が残っている");
    }

    /// A lone top folder holding the manifest installs from inside it.
    #[test]
    fn a_single_top_folder_installs_from_inside_it() {
        let root = dir("theme-top");
        let data = theme_zip(Some("pack-1.0"));
        let got = install_archive(&data, "pack.zip", InstallKind::Theme, &root).unwrap();
        assert_eq!(got.id, "dev.example.pack");
        assert!(root.join("dev.example.pack").join("theme.json").is_file());
        assert!(!root.join(".incoming").exists(), "作業場が残っている");
    }

    /// A zip-slip entry never leaves the staging directory.
    #[test]
    fn a_zip_slip_entry_stops_the_install() {
        let root = dir("slip");
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        zip.start_file("../evil.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"{}").unwrap();
        zip.finish().unwrap();
        let err = install_archive(&out.into_inner(), "evil.zip", InstallKind::Theme, &root)
            .expect_err("通り抜けた");
        assert!(
            matches!(err, InstallError::UnsafePath(_) | InstallError::Corrupt(_)),
            "{err:?}"
        );
        assert!(!root.join("evil.json").exists(), "外に書けた");
    }

    /// A tarball holding a plugin installs it.
    #[test]
    fn a_plugin_installs_from_a_tarball() {
        let root = dir("plugin-tgz");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut enc);
            let manifest = br#"{"id": "dev.example.side", "name": "Side", "version": "1.0.0"}"#;
            let mut header = tar::Header::new_gnu();
            header.set_size(manifest.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "manifest.json", &manifest[..])
                .unwrap();
            let entry = b"globalThis.__gumicord_apply = (n) => n;";
            let mut header = tar::Header::new_gnu();
            header.set_size(entry.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "plugin.js", &entry[..])
                .unwrap();
            tar.into_inner().unwrap();
        }
        let data = enc.finish().unwrap();
        let got = install_archive(&data, "side.tar.gz", InstallKind::Plugin, &root).unwrap();
        assert_eq!(got.id, "dev.example.side");
        assert!(
            root.join("dev.example.side")
                .join("manifest.json")
                .is_file()
        );
    }

    /// A tarball symlink never lands.
    #[test]
    fn a_tarball_symlink_stops_the_install() {
        let root = dir("link");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut enc);
            let manifest = br#"{"id": "dev.example.side", "name": "Side", "version": "1.0.0"}"#;
            let mut header = tar::Header::new_gnu();
            header.set_size(manifest.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "manifest.json", &manifest[..])
                .unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_cksum();
            tar.append_link(&mut header, "plugin.js", "/etc/passwd")
                .unwrap();
            tar.into_inner().unwrap();
        }
        let data = enc.finish().unwrap();
        let err =
            install_archive(&data, "side.tgz", InstallKind::Plugin, &root).expect_err("通り抜けた");
        assert!(matches!(err, InstallError::UnsafePath(_)), "{err:?}");
    }

    /// A broken theme never reaches the folder.
    #[test]
    fn a_broken_theme_never_reaches_the_folder() {
        let root = dir("broken");
        let mut out = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut out);
        zip.start_file("theme.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"{ broken").unwrap();
        zip.finish().unwrap();
        let err = install_archive(&out.into_inner(), "wall.zip", InstallKind::Theme, &root)
            .expect_err("通り抜けた");
        assert!(matches!(err, InstallError::InvalidManifest(_)), "{err:?}");
        assert!(root.read_dir().unwrap().next().is_none(), "残骸がある");
    }

    /// Installing twice refuses instead of overwriting behind the back.
    #[test]
    fn installing_twice_refuses() {
        let root = dir("twice");
        let data = theme_zip(None);
        install_archive(&data, "pack.zip", InstallKind::Theme, &root).unwrap();
        let err =
            install_archive(&data, "pack.zip", InstallKind::Theme, &root).expect_err("上書きした");
        assert!(matches!(err, InstallError::AlreadyInstalled(_)), "{err:?}");
    }

    /// Unknown extensions are refused up front.
    #[test]
    fn unknown_extensions_are_refused() {
        let root = dir("ext");
        let err = install_archive(b"junk", "pack.rar", InstallKind::Theme, &root)
            .expect_err("通り抜けた");
        assert!(matches!(err, InstallError::UnsupportedFormat), "{err:?}");
    }

    /// A huge entry trips the budget without writing gigabytes: the tar
    /// header claims past the per-file cap while the body stays empty.
    #[test]
    fn a_huge_entry_trips_the_budget() {
        let root = dir("huge");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            header.set_size(MAX_ONE_FILE + 1);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "theme.json", [].as_slice())
                .unwrap();
            tar.into_inner().unwrap();
        }
        let data = enc.finish().unwrap();
        let err = install_archive(&data, "big.tar.gz", InstallKind::Theme, &root)
            .expect_err("通り抜けた");
        assert!(matches!(err, InstallError::TooLarge(_)), "{err:?}");
    }
}
