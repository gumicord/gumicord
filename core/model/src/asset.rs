//! A single image on the CDN.
//!
//! Where an image lives is Discord's decision; what size and format to ask for
//! is ours. A function returning a URL string mixes the two at every call
//! site, so [`Asset`] carries only the location and the rest is layered on.
//!
//! ```
//! # use gumicord_model::User;
//! # let user: User = serde_json::from_str(r#"{"id":"7","username":"x"}"#).unwrap();
//! let url = user.display_avatar().with_size(128).url();
//! assert!(url.contains("/embed/avatars/"));
//! ```
//!
//! The default format is always PNG. An `a_` prefix marks an animated image,
//! but the decoder only handles PNG, so asking for the animated form would
//! render nothing at all rather than merely rendering it static.

use std::fmt;

use crate::{GuildId, UserId};

const BASE: &str = "https://cdn.discordapp.com";

const MIN_SIZE: u16 = 16;
const MAX_SIZE: u16 = 4096;

/// Image format.
///
/// `Gif` is selectable but not yet decodable; the variant exists so that
/// adding animation later does not touch [`Asset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    #[default]
    Png,
    Jpeg,
    Webp,
    Gif,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
            Format::Webp => "webp",
            Format::Gif => "gif",
        }
    }
}

/// A single image on the CDN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Path below [`BASE`], without extension or query.
    path: String,
    /// Discord's hash. Empty for built-in images.
    key: String,
    /// Whether a size can be requested.
    ///
    /// Default avatars cannot: everyone sees the same image, so a fixed size
    /// lets one copy be shared across users.
    resizable: bool,
    format: Format,
    size: Option<u16>,
}

/// CDN paths are built only here, so a change of layout has one place to fix.
impl Asset {
    pub fn user_avatar(user: UserId, hash: &str) -> Self {
        Asset::from_key(format!("avatars/{user}/{hash}"), hash)
    }

    /// An avatar set only within one guild.
    pub fn member_avatar(guild: GuildId, user: UserId, hash: &str) -> Self {
        Asset::from_key(format!("guilds/{guild}/users/{user}/avatars/{hash}"), hash)
    }

    /// `index` comes from [`crate::User::default_avatar_index`].
    pub fn default_avatar(index: u64) -> Self {
        Asset::fixed(format!("embed/avatars/{index}"))
    }

    pub fn guild_icon(guild: GuildId, hash: &str) -> Self {
        Asset::from_key(format!("icons/{guild}/{hash}"), hash)
    }

    fn from_key(path: impl Into<String>, key: &str) -> Self {
        Asset {
            path: path.into(),
            key: key.to_owned(),
            resizable: true,
            format: Format::default(),
            size: None,
        }
    }

    fn fixed(path: impl Into<String>) -> Self {
        Asset {
            path: path.into(),
            key: String::new(),
            resizable: false,
            format: Format::default(),
            size: None,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether animated source material exists. [`Asset::url`] still requests
    /// a still image.
    pub fn is_animated(&self) -> bool {
        self.key.starts_with("a_")
    }

    /// Sets the edge length.
    ///
    /// Discord rounds non-powers-of-two by rules of its own, so round up here
    /// instead. No-op for images that cannot be resized.
    pub fn with_size(mut self, size: u16) -> Self {
        if self.resizable {
            self.size = Some(round_up_pow2(size));
        }
        self
    }

    /// Sets the format. Only `Png` is decodable so far; the rest exist so
    /// that adding decoders later does not touch this type.
    pub fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    pub fn url(&self) -> String {
        let mut url = format!("{BASE}/{}.{}", self.path, self.format.extension());
        if let Some(size) = self.size {
            url.push_str(&format!("?size={size}"));
        }
        url
    }
}

impl fmt::Display for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.url())
    }
}

/// Smallest power of two at least `size`, clamped to the requestable range.
fn round_up_pow2(size: u16) -> u16 {
    let size = size.clamp(MIN_SIZE, MAX_SIZE);
    if size.is_power_of_two() {
        return size;
    }
    // MAX_SIZE is itself a power of two, so this cannot exceed the range.
    size.next_power_of_two().min(MAX_SIZE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_size_is_rounded_up_to_a_power_of_two() {
        assert_eq!(round_up_pow2(128), 128);
        assert_eq!(round_up_pow2(100), 128);
        assert_eq!(round_up_pow2(1), MIN_SIZE);
        assert_eq!(round_up_pow2(u16::MAX), MAX_SIZE);
    }

    #[test]
    fn an_animated_key_is_still_requested_as_png() {
        let a = Asset::user_avatar(UserId::from(7u64), "a_abc");
        assert!(a.is_animated());
        assert!(a.url().ends_with("a_abc.png"));
    }

    #[test]
    fn a_fixed_asset_ignores_the_size() {
        let a = Asset::default_avatar(3).with_size(128);
        assert_eq!(a.url(), format!("{BASE}/embed/avatars/3.png"));
        assert_eq!(a.key(), "");
        assert!(!a.is_animated());
    }

    #[test]
    fn size_and_format_stack_onto_the_place() {
        let a = Asset::guild_icon(GuildId::from(1u64), "abc")
            .with_size(100)
            .with_format(Format::Webp);
        assert_eq!(a.url(), format!("{BASE}/icons/1/abc.webp?size=128"));
        assert_eq!(a.to_string(), a.url());
    }
}
