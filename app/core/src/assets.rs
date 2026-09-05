//! Theme background assets: resolve, fetch, decode, hand to the renderer.
//!
//! Resolved pixels travel to their own textures; what differs from avatars is
//! where the bytes come from. Bundled files stay local, data URIs decode in
//! place, and remote hosts need a declaration plus the user's approval before
//! anything leaves the machine.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use gumicord_platform::Waker;
use gumicord_render::ImageData;
use gumicord_uitree::value::{AssetRef, Fit};

use crate::images::{box_blur, decode_image_capped};

/// Longest side kept for a background, in pixels. Own textures lift the
/// atlas page limit; past this the memory stops matching a background.
const MAX_SIDE: u32 = 4096;
/// One remote file may use this much memory; themes are someone else's.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// How long one fetch may take before it counts as failed.
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Redirects followed per fetch; themes should link straight at the file.
const MAX_HOPS: usize = 5;

/// Asks the user before an unapproved host is touched.
#[derive(Debug, Clone)]
pub struct HostAsk {
    pub theme: String,
    pub hosts: Vec<String>,
}

pub struct ThemeAssets {
    tx: Sender<ImageData>,
    rx: Receiver<ImageData>,
    rt: Option<tokio::runtime::Handle>,
    waker: Option<Waker>,
    http: Option<reqwest::Client>,
    namespace: String,
    dir: Option<PathBuf>,
    declared: Vec<String>,
    theme_name: String,
    known: HashMap<String, (AssetRef, f32)>,
    requested: HashSet<String>,
    ask: Option<HostAsk>,
    granted: HashSet<String>,
    denied: HashSet<String>,
    grants_file: Option<PathBuf>,
    ready: Vec<ImageData>,
}

impl ThemeAssets {
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let grants_file = default_grants_file();
        let (granted, denied) = load_grants(grants_file.as_deref());
        ThemeAssets {
            tx,
            rx,
            rt: None,
            waker: None,
            http: None,
            namespace: String::new(),
            dir: None,
            declared: Vec::new(),
            theme_name: String::new(),
            known: HashMap::new(),
            requested: HashSet::new(),
            ask: None,
            granted,
            denied,
            grants_file,
            ready: Vec::new(),
        }
    }

    pub fn start(&mut self, rt: &tokio::runtime::Handle, waker: Waker) {
        self.rt = Some(rt.clone());
        self.waker = Some(waker);
        // A bare client sends no cookies, no auth, and no referrer: it never
        // learned any. The user agent names us honestly; this is not Discord.
        self.http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(FETCH_TIMEOUT)
            .user_agent("Gumicord")
            .build()
            .ok();
        self.kick();
    }

    /// Points the resolver at a new theme. Previously asked marks go, so a
    /// changed file is re-read; grants and denials stay, so nobody is asked
    /// twice for the same host.
    pub fn set_theme(
        &mut self,
        namespace: String,
        dir: Option<PathBuf>,
        name: String,
        refs: Vec<(AssetRef, Fit, f32)>,
        declared: Vec<String>,
    ) {
        self.namespace = namespace;
        self.dir = dir;
        self.declared = declared;
        self.theme_name = name;
        self.known.clear();
        self.requested.clear();
        self.ask = None;
        for (r, _, blur) in refs {
            let key = r.cache_key(&self.namespace);
            self.known.entry(key).or_insert((r, blur));
        }
        self.kick();
    }

    /// Re-resolves keys the renderer dropped, as after an atlas recycle.
    pub fn request(&mut self, keys: &[String]) {
        for key in keys {
            if self.requested.contains(key) {
                continue;
            }
            if let Some((r, blur)) = self.known.get(key).cloned() {
                self.resolve(key, &r, blur);
            }
        }
        self.collect_ask();
    }

    /// An approval dialog the app should show, if any.
    pub fn poll_ask(&mut self) -> Option<HostAsk> {
        self.ask.take()
    }

    /// Records approved hosts and fetches what waited on them.
    pub fn approve_hosts(&mut self, hosts: &[String]) {
        for h in hosts {
            self.granted.insert(h.to_lowercase());
        }
        self.save_grants();
        self.kick();
    }

    /// Records refused hosts; their images stay fallback colours.
    pub fn deny_hosts(&mut self, hosts: &[String]) {
        for h in hosts {
            self.denied.insert(h.to_lowercase());
        }
        self.save_grants();
        self.kick();
    }

    /// Collects what arrived, reporting whether anything did.
    pub fn poll(&mut self) -> bool {
        let before = self.ready.len();
        while let Ok(image) = self.rx.try_recv() {
            self.ready.push(image);
        }
        self.ready.len() != before
    }

    /// Takes the arrived images; the caller passes them to the renderer.
    pub fn take(&mut self) -> Vec<ImageData> {
        self.poll();
        std::mem::take(&mut self.ready)
    }

    /// Starts every known asset that has neither an answer nor a fetch.
    fn kick(&mut self) {
        let keys: Vec<String> = self.known.keys().cloned().collect();
        for key in &keys {
            if self.requested.contains(key) {
                continue;
            }
            if let Some((r, blur)) = self.known.get(key).cloned() {
                self.resolve(key, &r, blur);
            }
        }
        self.collect_ask();
    }

    /// Gathers hosts that are declared but have no answer yet.
    fn collect_ask(&mut self) {
        if self.ask.is_some() {
            return;
        }
        let mut hosts: Vec<String> = self
            .known
            .values()
            .filter_map(|(r, _)| match r {
                AssetRef::Remote { host, .. } => {
                    let h = host.to_lowercase();
                    (!self.granted.contains(&h) && !self.denied.contains(&h)).then_some(h)
                }
                _ => None,
            })
            .collect();
        hosts.sort();
        hosts.dedup();
        if !hosts.is_empty() {
            self.ask = Some(HostAsk {
                theme: self.theme_name.clone(),
                hosts,
            });
        }
    }

    /// Starts one fetch unless it needs an answer first. Decoding and the
    /// one-time blur run off the async runtime; the pixels arrive ready to
    /// upload.
    fn resolve(&mut self, key: &str, r: &AssetRef, blur: f32) {
        let (Some(rt), Some(waker)) = (self.rt.clone(), self.waker.clone()) else {
            return;
        };
        let fetchable = match r {
            AssetRef::Bundled(_) | AssetRef::Data { .. } => true,
            AssetRef::Remote { host, .. } => self.granted.contains(&host.to_lowercase()),
        };
        if !fetchable {
            return;
        }
        self.requested.insert(key.to_owned());

        let (tx, waker) = (self.tx.clone(), waker.clone());
        let (key, r) = (key.to_owned(), r.clone());
        let (dir, http, cache) = (
            self.dir.clone(),
            self.http.clone(),
            cache_dir(self.grants_file.as_deref()),
        );
        let declared = self.declared.clone();
        rt.spawn(async move {
            let bytes = match &r {
                AssetRef::Bundled(rel) => read_bundled(dir.as_deref(), rel),
                AssetRef::Data { mime, base64 } => decode_data_uri(mime, base64),
                AssetRef::Remote { url, .. } => {
                    fetch_remote(http.as_ref(), cache.as_deref(), url, &declared).await
                }
            };
            let Some(bytes) = bytes else { return };
            let owned = key.clone();
            if let Ok(Some(image)) = tokio::task::spawn_blocking(move || {
                decode_image_capped(&owned, &bytes, MAX_SIDE).map(|image| box_blur(image, blur))
            })
            .await
            {
                let _ = tx.send(image);
                waker.wake();
            } else {
                tracing::warn!(
                    key,
                    "could not decode the image; keeping the fallback colour"
                );
            }
        });
    }

    fn save_grants(&self) {
        save_grants(self.grants_file.as_deref(), &self.granted, &self.denied);
    }
}

impl Default for ThemeAssets {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads a bundled file without leaving the theme directory. The parser
/// already rejected `..` lexically; resolving symlinks here catches links
/// planted inside the directory pointing out.
fn read_bundled(dir: Option<&Path>, rel: &str) -> Option<Vec<u8>> {
    let base = dir?.canonicalize().ok()?;
    let path = base.join(rel).canonicalize().ok()?;
    if !path.starts_with(&base) {
        tracing::warn!(rel, "bundled asset escapes the theme directory");
        return None;
    }
    std::fs::read(path).ok()
}

/// Decodes a data: URI body. Only images ride here; fonts come later.
fn decode_data_uri(mime: &str, base64_body: &str) -> Option<Vec<u8>> {
    if !mime.starts_with("image/") {
        return None;
    }
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(base64_body.trim())
        .ok()
}

/// Fetches an approved host's file, honouring the local cache first so a
/// start never refetches. Redirects stay on declared hosts or stop.
async fn fetch_remote(
    http: Option<&reqwest::Client>,
    cache: Option<&Path>,
    url: &str,
    declared: &[String],
) -> Option<Vec<u8>> {
    if let Some(bytes) = read_cache(cache, url) {
        return Some(bytes);
    }
    let http = http?;
    let mut current = url.to_owned();
    for _ in 0..=MAX_HOPS {
        let response = http.get(&current).send().await.ok()?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)?
                .to_str()
                .ok()?;
            let next = reqwest::Url::parse(&current).ok()?.join(location).ok()?;
            let host = next.host_str()?.to_lowercase();
            if next.scheme() != "https" || !declared.iter().any(|d| d == &host) {
                tracing::warn!(%current, host, "redirect leaves the declared hosts");
                return None;
            }
            current = next.to_string();
            continue;
        }
        if !status.is_success() {
            return None;
        }
        let bytes = response.bytes().await.ok()?.to_vec();
        if bytes.len() > MAX_BYTES {
            tracing::warn!(url, len = bytes.len(), "theme asset too large; dropping it");
            return None;
        }
        write_cache(cache, url, &bytes);
        return Some(bytes);
    }
    None
}

/// Names a cache file after a URL, which holds characters a filename cannot.
fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    format!("{:016x}.bin", h.finish())
}

fn read_cache(cache: Option<&Path>, url: &str) -> Option<Vec<u8>> {
    std::fs::read(cache?.join(cache_name(url))).ok()
}

fn write_cache(cache: Option<&Path>, url: &str, bytes: &[u8]) {
    let Some(dir) = cache else { return };
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(cache_name(url)), bytes);
}

/// Derives the remote cache from wherever the grants live.
fn cache_dir(grants_file: Option<&Path>) -> Option<PathBuf> {
    grants_file?.parent().map(|d| d.join("cache"))
}

fn default_grants_file() -> Option<PathBuf> {
    let dir = gumicord_platform::app_data_dir()?.join("themes");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("hosts.json"))
}

fn load_grants(path: Option<&Path>) -> (HashSet<String>, HashSet<String>) {
    let empty = (HashSet::new(), HashSet::new());
    let Some(path) = path else { return empty };
    let Ok(text) = std::fs::read_to_string(path) else {
        return empty;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return empty;
    };
    let take = |key: &str| {
        json.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_lowercase))
                    .collect()
            })
            .unwrap_or_default()
    };
    (take("granted"), take("denied"))
}

fn save_grants(path: Option<&Path>, granted: &HashSet<String>, denied: &HashSet<String>) {
    let Some(path) = path else { return };
    let mut granted: Vec<&String> = granted.iter().collect();
    let mut denied: Vec<&String> = denied.iter().collect();
    granted.sort();
    denied.sort();
    let Ok(text) = serde_json::to_string_pretty(&serde_json::json!({
        "granted": granted,
        "denied": denied,
    })) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_uitree::value::AssetRef;

    #[test]
    fn keys_are_stable_and_namespaced() {
        let a = AssetRef::Bundled("assets/bg.png".to_owned());
        assert_eq!(a.cache_key("one"), "one/assets/bg.png");
        assert_ne!(a.cache_key("one"), a.cache_key("two"));
        let r = AssetRef::Remote {
            url: "https://cdn.example.com/bg.png".to_owned(),
            host: "cdn.example.com".to_owned(),
        };
        assert_eq!(r.cache_key("one"), "https://cdn.example.com/bg.png");
        let d = AssetRef::Data {
            mime: "image/png".to_owned(),
            base64: "aGk=".to_owned(),
        };
        assert!(d.cache_key("one").starts_with("one/data#"));
        assert_ne!(d.cache_key("one"), d.cache_key("two"));
    }

    #[test]
    fn data_uris_decode_or_refuse() {
        assert_eq!(
            decode_data_uri("image/png", "aGk=").as_deref(),
            Some(b"hi".as_slice())
        );
        assert!(decode_data_uri("font/woff2", "aGk=").is_none());
        assert!(decode_data_uri("image/png", "not base64!!").is_none());
    }

    #[test]
    fn bundled_files_stay_inside_the_theme() {
        let dir = std::env::temp_dir().join("gumicord-theme-contained");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets").join("bg.png"), b"fake").unwrap();

        assert_eq!(
            read_bundled(Some(&dir), "assets/bg.png").as_deref(),
            Some(b"fake".as_slice())
        );
        assert!(read_bundled(Some(&dir), "../outside.png").is_none());
        assert!(read_bundled(Some(&dir), "missing.png").is_none());
        assert!(read_bundled(None, "assets/bg.png").is_none());
    }

    #[test]
    fn unknown_hosts_are_asked_once_then_remembered() {
        let dir = std::env::temp_dir().join("gumicord-theme-grants");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let mut assets = ThemeAssets::new();
        assets.grants_file = Some(dir.join("hosts.json"));
        assets.set_theme(
            "ns".to_owned(),
            None,
            "Wall".to_owned(),
            vec![(
                AssetRef::Remote {
                    url: "https://a.example.com/bg.png".to_owned(),
                    host: "a.example.com".to_owned(),
                },
                Fit::Cover,
                0.0,
            )],
            vec!["a.example.com".to_owned()],
        );

        let ask = assets.poll_ask().expect("no ask");
        assert_eq!(ask.hosts, ["a.example.com"]);
        assert!(assets.poll_ask().is_none(), "asked twice");

        assets.approve_hosts(&["a.example.com".to_owned()]);
        let (granted, denied) = load_grants(Some(&dir.join("hosts.json")));
        assert!(granted.contains("a.example.com"));
        assert!(denied.is_empty());

        assets.deny_hosts(&["b.example.com".to_owned()]);
        let (_, denied) = load_grants(Some(&dir.join("hosts.json")));
        assert!(denied.contains("b.example.com"));
    }
}
