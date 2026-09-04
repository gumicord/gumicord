//! Disk cache for system font enumeration.
//!
//! Scanning every font file takes hundreds of milliseconds cold; the parse
//! results barely change between starts. The cache stores per-face metadata
//! keyed by file identity, so an unchanged file is never parsed again.
//!
//! Anything the cache cannot prove — a new file, a changed file, a layout
//! that drifted under us — falls back to parsing that file. A cache that
//! cannot prove anything falls back to a full scan.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cosmic_text::fontdb::{self, Database};

/// Bumped whenever the stored shape changes; anything else is rescanned.
const FORMAT_VERSION: u32 = 1;
const CACHE_FILE: &str = "enumeration.json";

/// Fills the database, from cache where the files did not move.
pub fn populate(db: &mut Database, dir: Option<&Path>) {
    let Some(dir) = dir else {
        db.load_system_fonts();
        return;
    };
    let walked = walk_fonts();
    match load(dir) {
        Some(cache) if totals_match(&walked, &cache) => {
            let verified = verify_and_push(db, &cache);
            fill_unverified(db, &walked.files, &verified);
            store(db, dir, &walked);
        }
        _ => {
            db.load_system_fonts();
            store(db, dir, &walked);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    size: u64,
    secs: u64,
    nanos: u32,
}

struct FoundFile {
    path: PathBuf,
    identity: Identity,
}

struct Walked {
    files: Vec<FoundFile>,
    total_files: usize,
    total_bytes: u64,
}

fn file_identity(path: &Path) -> Option<Identity> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(Identity {
        size: meta.len(),
        secs: modified.as_secs(),
        nanos: modified.subsec_nanos(),
    })
}

/// Font file or nothing. Directories change shape too often to trust, so
/// only files are listed, and only by extension like the enumerator itself.
fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("ttf")
            | Some("ttc")
            | Some("TTF")
            | Some("TTC")
            | Some("otf")
            | Some("otc")
            | Some("OTF")
            | Some("OTC")
    )
}

fn walk_dir(dir: &Path, seen: &mut HashSet<PathBuf>, out: &mut Vec<FoundFile>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, seen, out);
            continue;
        }
        if !is_font_file(&path) {
            continue;
        }
        // Canonical paths, like the enumerator stores: the same file under
        // two names must not count twice or prove itself twice.
        let Ok(canon) = path.canonicalize() else {
            continue;
        };
        if !seen.insert(canon.clone()) {
            continue;
        }
        if let Some(identity) = file_identity(&canon) {
            out.push(FoundFile {
                path: canon,
                identity,
            });
        }
    }
}

/// The same roots the enumerator scans. A directory the list misses still
/// resolves correctly: the totals stop matching and the next start rescans
/// fully instead of trusting the cache.
fn system_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(target_os = "windows")]
    {
        if let Some(root) = std::env::var_os("SYSTEMROOT") {
            dirs.push(PathBuf::from(root).join("Fonts"));
        } else {
            dirs.push(PathBuf::from("C:\\Windows\\Fonts"));
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            let home = PathBuf::from(home);
            dirs.push(home.join("AppData\\Local\\Microsoft\\Windows\\Fonts"));
            dirs.push(home.join("AppData\\Roaming\\Microsoft\\Windows\\Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push(PathBuf::from("/Library/Fonts"));
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        if let Ok(entries) = std::fs::read_dir("/System/Library/AssetsV2") {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("com_apple_MobileAsset_Font")
                {
                    dirs.push(entry.path());
                }
            }
        }
        dirs.push(PathBuf::from("/Network/Library/Fonts"));
        if let Ok(home) = std::env::var("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
    }
    #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            dirs.push(home.join(".fonts"));
            dirs.push(home.join(".local/share/fonts"));
        }
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(xdg).join("fonts"));
        }
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
        dirs.push(PathBuf::from("/usr/share/fonts"));
    }
    dirs
}

fn walk_fonts() -> Walked {
    walk_dirs(&system_dirs())
}

fn walk_dirs(dirs: &[PathBuf]) -> Walked {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    for dir in dirs {
        walk_dir(dir, &mut seen, &mut files);
    }
    let total_bytes = files.iter().map(|f| f.identity.size).sum();
    let total_files = files.len();
    Walked {
        files,
        total_files,
        total_bytes,
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Cache {
    format: u32,
    total_files: usize,
    total_bytes: u64,
    files: Vec<CachedFile>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedFile {
    path: PathBuf,
    size: u64,
    secs: u64,
    nanos: u32,
    faces: Vec<CachedFace>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct CachedFace {
    index: u32,
    families: Vec<(String, String)>,
    post_script_name: String,
    style: u8,
    weight: u16,
    stretch: u16,
    monospaced: bool,
}

fn cache_path(dir: &Path) -> PathBuf {
    dir.join(CACHE_FILE)
}

fn load(dir: &Path) -> Option<Cache> {
    let text = std::fs::read_to_string(cache_path(dir)).ok()?;
    let cache: Cache = serde_json::from_str(&text).ok()?;
    (cache.format == FORMAT_VERSION).then_some(cache)
}

fn totals_match(walked: &Walked, cache: &Cache) -> bool {
    walked.total_files == cache.total_files && walked.total_bytes == cache.total_bytes
}

/// Pushes faces whose files did not move, returning the proven paths.
fn verify_and_push(db: &mut Database, cache: &Cache) -> HashSet<PathBuf> {
    let mut verified = HashSet::new();
    for file in &cache.files {
        let Some(identity) = file_identity(&file.path) else {
            continue;
        };
        if identity.size != file.size || identity.secs != file.secs || identity.nanos != file.nanos
        {
            continue;
        }
        let mut ok = true;
        for face in &file.faces {
            if !restore_face(db, &file.path, face) {
                ok = false;
                break;
            }
        }
        if ok {
            verified.insert(file.path.clone());
        }
    }
    verified
}

/// Parses what the cache could not prove. New and changed files land here;
/// anything else was already pushed.
fn fill_unverified(db: &mut Database, files: &[FoundFile], verified: &HashSet<PathBuf>) {
    for file in files {
        if verified.contains(&file.path) {
            continue;
        }
        let _ = db.load_font_file(&file.path);
    }
}

fn restore_face(db: &mut Database, path: &Path, face: &CachedFace) -> bool {
    let Some(cached) = cached_face(
        face.index,
        &face.families,
        &face.post_script_name,
        face.style,
        face.weight,
        face.stretch,
        face.monospaced,
    ) else {
        return false;
    };
    db.push_face_info(fontdb::FaceInfo {
        id: fontdb::ID::dummy(),
        source: fontdb::Source::File(path.to_owned()),
        index: cached.index,
        families: cached.families,
        post_script_name: cached.post_script_name,
        style: cached.style,
        weight: cached.weight,
        stretch: cached.stretch,
        monospaced: cached.monospaced,
    });
    true
}

struct RestoredFace {
    index: u32,
    families: Vec<(String, fontdb::Language)>,
    post_script_name: String,
    style: fontdb::Style,
    weight: fontdb::Weight,
    stretch: fontdb::Stretch,
    monospaced: bool,
}

#[allow(clippy::too_many_arguments)]
fn cached_face(
    index: u32,
    families: &[(String, String)],
    post_script_name: &str,
    style: u8,
    weight: u16,
    stretch: u16,
    monospaced: bool,
) -> Option<RestoredFace> {
    let mut restored = Vec::with_capacity(families.len());
    for (name, debug) in families {
        restored.push((name.clone(), restore_language(debug)?));
    }
    Some(RestoredFace {
        index,
        families: restored,
        post_script_name: post_script_name.to_owned(),
        style: restore_style(style)?,
        weight: fontdb::Weight(weight),
        stretch: restore_stretch(stretch)?,
        monospaced,
    })
}

fn restore_style(code: u8) -> Option<fontdb::Style> {
    match code {
        0 => Some(fontdb::Style::Normal),
        1 => Some(fontdb::Style::Italic),
        2 => Some(fontdb::Style::Oblique),
        _ => None,
    }
}

fn restore_stretch(code: u16) -> Option<fontdb::Stretch> {
    match code {
        1 => Some(fontdb::Stretch::UltraCondensed),
        2 => Some(fontdb::Stretch::ExtraCondensed),
        3 => Some(fontdb::Stretch::Condensed),
        4 => Some(fontdb::Stretch::SemiCondensed),
        5 => Some(fontdb::Stretch::Normal),
        6 => Some(fontdb::Stretch::SemiExpanded),
        7 => Some(fontdb::Stretch::Expanded),
        8 => Some(fontdb::Stretch::ExtraExpanded),
        9 => Some(fontdb::Stretch::UltraExpanded),
        _ => None,
    }
}

/// Restores a language tag by its debug name. Only the tags system fonts
/// actually carry are listed; anything else parses its file instead of
/// guessing. A name that changed meaning upstream falls back the same way.
fn restore_language(debug: &str) -> Option<fontdb::Language> {
    use fontdb::Language as L;
    Some(match debug {
        "Unknown" => L::Unknown,
        "Danish_Denmark" => L::Danish_Denmark,
        "Dutch_Netherlands" => L::Dutch_Netherlands,
        "English_Australia" => L::English_Australia,
        "English_Canada" => L::English_Canada,
        "English_NewZealand" => L::English_NewZealand,
        "English_UnitedKingdom" => L::English_UnitedKingdom,
        "English_UnitedStates" => L::English_UnitedStates,
        "Finnish_Finland" => L::Finnish_Finland,
        "French_France" => L::French_France,
        "German_Germany" => L::German_Germany,
        "Greek_Greece" => L::Greek_Greece,
        "Hebrew_Israel" => L::Hebrew_Israel,
        "Indonesian_Indonesia" => L::Indonesian_Indonesia,
        "Italian_Italy" => L::Italian_Italy,
        "Japanese_Japan" => L::Japanese_Japan,
        "Korean_Korea" => L::Korean_Korea,
        "Norwegian_Bokmal_Norway" => L::Norwegian_Bokmal_Norway,
        "Polish_Poland" => L::Polish_Poland,
        "Portuguese_Brazil" => L::Portuguese_Brazil,
        "Portuguese_Portugal" => L::Portuguese_Portugal,
        "Russian_Russia" => L::Russian_Russia,
        "Spanish_Mexico" => L::Spanish_Mexico,
        "Swedish_Sweden" => L::Swedish_Sweden,
        "Thai_Thailand" => L::Thai_Thailand,
        "Turkish_Turkey" => L::Turkish_Turkey,
        "Ukrainian_Ukraine" => L::Ukrainian_Ukraine,
        "Vietnamese_Vietnam" => L::Vietnamese_Vietnam,
        "Chinese_HongKongSAR" => L::Chinese_HongKongSAR,
        "Chinese_PeoplesRepublicOfChina" => L::Chinese_PeoplesRepublicOfChina,
        "Chinese_Singapore" => L::Chinese_Singapore,
        "Chinese_Taiwan" => L::Chinese_Taiwan,
        _ => return None,
    })
}

fn store(db: &Database, dir: &Path, walked: &Walked) {
    let by_path: HashMap<&Path, Identity> = walked
        .files
        .iter()
        .map(|f| (f.path.as_path(), f.identity))
        .collect();
    let mut grouped: HashMap<PathBuf, Vec<CachedFace>> = HashMap::new();
    for face in db.faces() {
        // File paths only, canonicalised like the walk: the enumerator may
        // keep them as walked, so both sides normalise before comparing.
        // Shaping can later remap these to memory, but the cache is always
        // written before that, straight after enumerating.
        let fontdb::Source::File(path) = &face.source else {
            continue;
        };
        let Ok(path) = path.canonicalize() else {
            continue;
        };
        if !by_path.contains_key(path.as_path()) {
            continue;
        };
        let style = match face.style {
            fontdb::Style::Normal => 0,
            fontdb::Style::Italic => 1,
            fontdb::Style::Oblique => 2,
        };
        grouped.entry(path).or_default().push(CachedFace {
            index: face.index,
            families: face
                .families
                .iter()
                .map(|(name, lang)| (name.clone(), format!("{lang:?}")))
                .collect(),
            post_script_name: face.post_script_name.clone(),
            style,
            weight: face.weight.0,
            stretch: face.stretch.to_number(),
            monospaced: face.monospaced,
        });
    }
    let mut files: Vec<CachedFile> = grouped
        .into_iter()
        .filter_map(|(path, faces)| {
            let identity = by_path.get(path.as_path())?;
            Some(CachedFile {
                path: path.to_owned(),
                size: identity.size,
                secs: identity.secs,
                nanos: identity.nanos,
                faces,
            })
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let cache = Cache {
        format: FORMAT_VERSION,
        total_files: walked.total_files,
        total_bytes: walked.total_bytes,
        files,
    };
    let Ok(text) = serde_json::to_string(&cache) else {
        return;
    };
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(cache_path(dir), text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font_file(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, include_bytes!("../../../assets/fonts/Inter.ttf")).unwrap();
        path
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gumicord-font-cache-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn families_of(db: &Database) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = db
            .faces()
            .map(|f| f.families.iter().map(|(n, _)| n.clone()).collect())
            .collect();
        out.sort();
        out
    }

    fn walked(dir: &Path) -> Walked {
        walk_dirs(std::slice::from_ref(&dir.to_path_buf()))
    }

    #[test]
    fn an_unchanged_file_is_never_parsed_again() {
        let dir = scratch("roundtrip");
        let fonts = dir.join("fonts");
        std::fs::create_dir_all(&fonts).unwrap();
        font_file(&fonts, "inter.ttf");

        let mut first = Database::new();
        first.load_fonts_dir(&fonts);
        assert!(first.faces().next().is_some());
        let cache = dir.join("cache");
        store(&first, &cache, &walked(&fonts));

        let loaded = load(&cache).expect("cache unreadable");
        let mut second = Database::new();
        let verified = verify_and_push(&mut second, &loaded);
        assert_eq!(verified.len(), 1);
        assert_eq!(families_of(&second), families_of(&first));
    }

    #[test]
    fn a_changed_file_falls_back_to_parsing() {
        let dir = scratch("changed");
        let fonts = dir.join("fonts");
        std::fs::create_dir_all(&fonts).unwrap();
        let path = font_file(&fonts, "inter.ttf");

        let mut db = Database::new();
        db.load_fonts_dir(&fonts);
        let cache = dir.join("cache");
        store(&db, &cache, &walked(&fonts));

        // Same name, different bytes: the identity no longer matches.
        std::fs::write(&path, b"not a font at all").unwrap();
        let loaded = load(&cache).expect("cache unreadable");
        let mut second = Database::new();
        let verified = verify_and_push(&mut second, &loaded);
        assert!(verified.is_empty());
        assert!(second.faces().next().is_none());
    }

    #[test]
    fn unknown_codes_and_layout_drift_fall_back() {
        assert!(restore_language("Bogus").is_none());
        assert!(restore_style(9).is_none());
        assert!(restore_stretch(0).is_none());
        assert!(restore_stretch(99).is_none());
    }

    #[test]
    fn a_broken_cache_file_scans_fully() {
        let dir = scratch("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(CACHE_FILE), "{oops").unwrap();
        assert!(load(&dir).is_none());
        let dir2 = scratch("wrong-version");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(
            dir2.join(CACHE_FILE),
            r#"{"format":999,"total_files":0,"total_bytes":0,"files":[]}"#,
        )
        .unwrap();
        assert!(load(&dir2).is_none());
    }

    /// A language round-trips through its debug name.
    #[test]
    fn languages_come_back_identical() {
        let dir = scratch("lang");
        let fonts = dir.join("fonts");
        std::fs::create_dir_all(&fonts).unwrap();
        font_file(&fonts, "inter.ttf");
        let mut db = Database::new();
        db.load_fonts_dir(&fonts);
        let face = db.faces().next().expect("no faces parsed");
        for (name, lang) in &face.families {
            let back = restore_language(&format!("{lang:?}")).expect("lost in transit");
            assert_eq!(&back, lang, "{name}");
        }
    }
}
