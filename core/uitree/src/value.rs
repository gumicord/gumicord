//! Theme value types.
//!
//! Lengths are always logical pixels; converting to physical pixels is the
//! renderer's job, so never multiply by DPI here.

/// `#RGB` / `#RRGGBB` / `#RRGGBBAA`
///
/// Straight alpha, not premultiplied, matching the renderer's
/// `BlendState::ALPHA_BLENDING`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };

    /// Parses hex notation, returning `None` for anything else.
    /// From `0xRRGGBB`, which is how Discord sends colours.
    pub const fn from_rgb(rgb: u32) -> Color {
        Color {
            r: ((rgb >> 16) & 0xff) as u8,
            g: ((rgb >> 8) & 0xff) as u8,
            b: (rgb & 0xff) as u8,
            a: 0xff,
        }
    }

    pub fn parse(s: &str) -> Option<Color> {
        let hex = s.strip_prefix('#')?;
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let nibble = |i: usize| -> Option<u8> {
            let c = hex.as_bytes().get(i)?;
            (*c as char).to_digit(16).map(|v| v as u8)
        };
        let byte = |i: usize| -> Option<u8> {
            let hi = nibble(i)?;
            let lo = nibble(i + 1)?;
            Some(hi << 4 | lo)
        };
        match hex.len() {
            // #RGB doubles each digit: #abc becomes #aabbcc.
            3 => Some(Color {
                r: nibble(0)? * 17,
                g: nibble(1)? * 17,
                b: nibble(2)? * 17,
                a: 255,
            }),
            6 => Some(Color {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: 255,
            }),
            8 => Some(Color {
                r: byte(0)?,
                g: byte(2)?,
                b: byte(4)?,
                a: byte(6)?,
            }),
            _ => None,
        }
    }

    /// Whether blending can be skipped.
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }
}

/// A font.
///
/// Every field is optional; omitted ones come from the inherited style and
/// finally from the bundled default. Omitting `family` is preferred, so a
/// theme renders identically everywhere.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Font {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub line_height: Option<f32>,
    pub weight: Option<u16>,
    pub italic: Option<bool>,
    pub letter_spacing: Option<f32>,
}

/// A shadow.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

/// Padding or margin. Accepts one value or `[top, right, bottom, left]`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub const fn all(v: f32) -> Self {
        Edges {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

/// How a background image fits its area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// Covers the area, cropping the overflow. The default.
    #[default]
    Cover,
    /// Fits inside the area; the remainder is transparent.
    Contain,
    /// Stretches, ignoring the aspect ratio.
    Stretch,
    /// Tiles at native size.
    Tile,
    /// Placed once at native size.
    None,
}

impl Fit {
    pub fn parse(s: &str) -> Option<Fit> {
        Some(match s {
            "cover" => Fit::Cover,
            "contain" => Fit::Contain,
            "stretch" => Fit::Stretch,
            "tile" => Fit::Tile,
            "none" => Fit::None,
            _ => return None,
        })
    }

    /// Only these three consult `position`.
    pub const fn uses_position(self) -> bool {
        matches!(self, Fit::Cover | Fit::Contain | Fit::None)
    }
}

/// Where an asset comes from.
///
/// [`AssetRef::parse`] has already checked that a relative path cannot leave
/// the theme directory and that an external host is declared in
/// `manifest.remoteAssets`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRef {
    /// Relative to theme.json, with no `..` and no absolute path.
    Bundled(String),
    /// data: URI
    Data { mime: String, base64: String },
    /// An https URL whose host is already known to be declared.
    Remote { url: String, host: String },
}

/// Why [`AssetRef::parse`] refused a reference.
///
/// Callers turn these into diagnostics. None is a reason to discard the whole
/// theme.
///
/// The messages stay Japanese: they are shown to the theme author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// Escapes the theme directory via `../` or an absolute path.
    EscapesThemeDirectory,
    /// Unsupported file extension.
    UnsupportedFormat,
    /// A scheme other than https.
    InsecureScheme,
    /// A host not declared in `manifest.remoteAssets`.
    UndeclaredHost(String),
    /// Not parseable at all.
    Malformed,
}

impl AssetError {
    pub fn message(&self) -> String {
        match self {
            Self::EscapesThemeDirectory => {
                "テーマディレクトリの外を参照している。相対パスは外へ出られない".into()
            }
            Self::UnsupportedFormat => {
                "対応していない形式。画像は png / jpg / jpeg / webp / avif のみ".into()
            }
            Self::InsecureScheme => "外部アセットは https のみ許可される (SEC-024)".into(),
            Self::UndeclaredHost(h) => format!(
                "ホスト {h} が manifest.remoteAssets に宣言されていない。\
                 宣言されていないホストへは到達しない (SEC-022)"
            ),
            Self::Malformed => "アセット参照として解釈できない".into(),
        }
    }
}

/// Extensions usable as images.
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "avif"];
/// Extensions usable as fonts.
const FONT_EXTENSIONS: &[&str] = &["woff2", "ttf", "otf"];

/// What kind of asset a reference is expected to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Image,
    Font,
}

impl AssetKind {
    const fn extensions(self) -> &'static [&'static str] {
        match self {
            Self::Image => IMAGE_EXTENSIONS,
            Self::Font => FONT_EXTENSIONS,
        }
    }

    const fn data_mimes(self) -> &'static [&'static str] {
        match self {
            Self::Image => &["image/png", "image/jpeg", "image/webp", "image/avif"],
            Self::Font => &["font/woff2"],
        }
    }
}

impl AssetRef {
    /// Parses an asset reference and validates it at the same time.
    ///
    /// `declared_hosts` is `manifest.remoteAssets`; empty rejects every
    /// external URL. Rejection means the host is never contacted, not that a
    /// request is refused.
    pub fn parse(s: &str, kind: AssetKind, declared_hosts: &[String]) -> Result<Self, AssetError> {
        if s.is_empty() {
            return Err(AssetError::Malformed);
        }
        if let Some(rest) = s.strip_prefix("data:") {
            return Self::parse_data(rest, kind);
        }
        if let Some(rest) = s.strip_prefix("https://") {
            return Self::parse_remote(s, rest, declared_hosts);
        }
        // Any non-https scheme is an attempt at an external reference.
        if s.contains("://") || s.starts_with("file:") {
            return Err(AssetError::InsecureScheme);
        }
        Self::parse_bundled(s, kind)
    }

    fn parse_data(rest: &str, kind: AssetKind) -> Result<Self, AssetError> {
        let (meta, base64) = rest.split_once(',').ok_or(AssetError::Malformed)?;
        let mime = meta.strip_suffix(";base64").ok_or(AssetError::Malformed)?;
        if !kind.data_mimes().contains(&mime) {
            return Err(AssetError::UnsupportedFormat);
        }
        if base64.is_empty() {
            return Err(AssetError::Malformed);
        }
        Ok(AssetRef::Data {
            mime: mime.to_owned(),
            base64: base64.to_owned(),
        })
    }

    fn parse_remote(url: &str, rest: &str, declared: &[String]) -> Result<Self, AssetError> {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if authority.is_empty() {
            return Err(AssetError::Malformed);
        }
        // Declarations are by host name, so drop the port.
        let host = authority.split(':').next().unwrap_or(authority);
        // Case-insensitive; the schema forces declarations to lower case.
        let host = host.to_ascii_lowercase();
        if host.is_empty() {
            return Err(AssetError::Malformed);
        }
        if !declared.iter().any(|h| h.eq_ignore_ascii_case(&host)) {
            return Err(AssetError::UndeclaredHost(host));
        }
        Ok(AssetRef::Remote {
            url: url.to_owned(),
            host,
        })
    }

    fn parse_bundled(path: &str, kind: AssetKind) -> Result<Self, AssetError> {
        // No Windows separators: a theme must behave identically on every
        // platform.
        if path.contains('\\') {
            return Err(AssetError::EscapesThemeDirectory);
        }
        if path.starts_with('/') {
            return Err(AssetError::EscapesThemeDirectory);
        }
        // An absolute path with a drive letter, such as "C:/…".
        let bytes = path.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            return Err(AssetError::EscapesThemeDirectory);
        }
        for component in path.split('/') {
            if component.is_empty() || component == "." || component == ".." {
                return Err(AssetError::EscapesThemeDirectory);
            }
        }
        let ext = path
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .ok_or(AssetError::UnsupportedFormat)?;
        if !kind.extensions().contains(&ext.as_str()) {
            return Err(AssetError::UnsupportedFormat);
        }
        Ok(AssetRef::Bundled(path.to_owned()))
    }
}

/// A background.
///
/// Composited bottom to top: `color`, then `image` with `opacity` and `blur`
/// already applied, then `tint`.
///
/// `blur` is applied once at load time; convolving every frame does not pay
/// for itself on the lowest GPU this targets.
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    /// Painted under the image, and the fallback if the image fails to load.
    pub color: Option<Color>,
    pub image: Option<AssetRef>,
    pub fit: Fit,
    /// `[x, y]`, each 0.0 to 1.0.
    pub position: [f32; 2],
    pub opacity: f32,
    pub blur: f32,
    /// Painted over the image, to keep text legible.
    pub tint: Option<Color>,
}

impl Default for Background {
    fn default() -> Self {
        Background {
            color: None,
            image: None,
            fit: Fit::Cover,
            position: [0.5, 0.5],
            opacity: 1.0,
            blur: 0.0,
            tint: None,
        }
    }
}

impl Background {
    /// A colour-only background, for the `"background": "#111"` shorthand.
    pub fn solid(color: Color) -> Self {
        Background {
            color: Some(color),
            ..Default::default()
        }
    }

    /// Whether the renderer's texture path is needed.
    pub const fn has_image(&self) -> bool {
        self.image.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_formats() {
        assert_eq!(
            Color::parse("#abc"),
            Some(Color {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
                a: 255
            })
        );
        assert_eq!(
            Color::parse("#7c6cf0"),
            Some(Color {
                r: 0x7c,
                g: 0x6c,
                b: 0xf0,
                a: 255
            })
        );
        assert_eq!(
            Color::parse("#ffffff14"),
            Some(Color {
                r: 255,
                g: 255,
                b: 255,
                a: 0x14
            })
        );
        // Upper case is accepted.
        assert_eq!(Color::parse("#FFFFFF"), Color::parse("#ffffff"));
    }

    #[test]
    fn color_rejects_bad_formats() {
        for s in [
            "",
            "#",
            "fff",
            "#ff",
            "#fffff",
            "#fffffff",
            "#gggggg",
            "#ffffff1",
            "rgb(1,2,3)",
        ] {
            assert_eq!(Color::parse(s), None, "{s} must not be accepted");
        }
    }

    /// A relative path cannot escape the theme directory.
    #[test]
    fn bundled_asset_cannot_escape() {
        let cases = [
            "../secret.png",
            "assets/../../secret.png",
            "/etc/passwd.png",
            "C:/Windows/win.png",
            "assets\\wallpaper.png",
            "./wallpaper.png",
            "assets//wallpaper.png",
        ];
        for s in cases {
            let got = AssetRef::parse(s, AssetKind::Image, &[]);
            assert_eq!(
                got,
                Err(AssetError::EscapesThemeDirectory),
                "{s} must not be allowed"
            );
        }
    }

    #[test]
    fn bundled_asset_accepts_nested_path() {
        let got = AssetRef::parse("assets/bg/wall.png", AssetKind::Image, &[]);
        assert_eq!(got, Ok(AssetRef::Bundled("assets/bg/wall.png".into())));
    }

    #[test]
    fn bundled_asset_checks_extension() {
        assert_eq!(
            AssetRef::parse("assets/bg.gif", AssetKind::Image, &[]),
            Err(AssetError::UnsupportedFormat),
            "animated formats are not supported yet"
        );
        assert_eq!(
            AssetRef::parse("assets/bg", AssetKind::Image, &[]),
            Err(AssetError::UnsupportedFormat)
        );
        // Upper-case extensions are accepted.
        assert!(AssetRef::parse("assets/BG.PNG", AssetKind::Image, &[]).is_ok());
        // A font cannot stand in for an image.
        assert_eq!(
            AssetRef::parse("assets/Inter.woff2", AssetKind::Image, &[]),
            Err(AssetError::UnsupportedFormat)
        );
    }

    /// An undeclared host is never contacted.
    #[test]
    fn remote_asset_requires_declaration() {
        let declared = vec!["cdn.example.com".to_string()];
        assert!(
            AssetRef::parse(
                "https://cdn.example.com/bg.png",
                AssetKind::Image,
                &declared
            )
            .is_ok()
        );
        assert_eq!(
            AssetRef::parse("https://evil.example/bg.png", AssetKind::Image, &declared),
            Err(AssetError::UndeclaredHost("evil.example".into()))
        );
        // With no declarations, nothing external passes.
        assert!(matches!(
            AssetRef::parse("https://cdn.example.com/bg.png", AssetKind::Image, &[]),
            Err(AssetError::UndeclaredHost(_))
        ));
    }

    /// Matching is by host name even when a port is present.
    #[test]
    fn remote_asset_ignores_port() {
        let declared = vec!["cdn.example.com".to_string()];
        assert!(
            AssetRef::parse(
                "https://cdn.example.com:8443/bg.png",
                AssetKind::Image,
                &declared
            )
            .is_ok()
        );
    }

    /// https only.
    #[test]
    fn remote_asset_rejects_insecure_schemes() {
        let declared = vec!["cdn.example.com".to_string()];
        for s in [
            "http://cdn.example.com/bg.png",
            "file:///etc/passwd",
            "ftp://cdn.example.com/bg.png",
        ] {
            assert_eq!(
                AssetRef::parse(s, AssetKind::Image, &declared),
                Err(AssetError::InsecureScheme),
                "{s} must not be allowed"
            );
        }
    }

    #[test]
    fn data_uri() {
        let ok = AssetRef::parse("data:image/png;base64,iVBORw0KGgo=", AssetKind::Image, &[]);
        assert_eq!(
            ok,
            Ok(AssetRef::Data {
                mime: "image/png".into(),
                base64: "iVBORw0KGgo=".into()
            })
        );
        assert_eq!(
            AssetRef::parse("data:image/gif;base64,AAA=", AssetKind::Image, &[]),
            Err(AssetError::UnsupportedFormat)
        );
        assert_eq!(
            AssetRef::parse("data:image/png,AAA=", AssetKind::Image, &[]),
            Err(AssetError::Malformed)
        );
    }

    #[test]
    fn background_defaults_match_spec() {
        let bg = Background::default();
        assert_eq!(bg.fit, Fit::Cover);
        assert_eq!(bg.position, [0.5, 0.5]);
        assert_eq!(bg.opacity, 1.0);
        assert_eq!(bg.blur, 0.0);
        assert_eq!(bg.color, None);
        assert_eq!(bg.tint, None);
    }
}
