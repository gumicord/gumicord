//! テーマの値の型。
//!
//! 長さは**常に論理ピクセル**である。物理ピクセルへの変換はレンダラが行う
//! (`PLT-009`)。ここで DPI を掛けてはならない。
//!
//! 仕様: [`spec/04-theme.md`] 3.3, 6, 6.1

/// `#RGB` / `#RRGGBB` / `#RRGGBBAA`
///
/// 内部表現はストレートアルファ (乗算済みではない)。レンダラ側の
/// `BlendState::ALPHA_BLENDING` と対応する (`EXT-024`)。
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

    /// 16 進表記から解析する。受け付けない書式では `None`。
    /// `0xRRGGBB` から。**Discord が色を渡してくる形である**
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
            // #RGB は各桁を 2 回繰り返す (#abc → #aabbcc)
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

    /// 完全に不透明か。半透明合成の要否判定に使う (`EXT-024`)。
    pub const fn is_opaque(self) -> bool {
        self.a == 255
    }
}

/// 書体。
///
/// **すべての項目が省略可能である。** 省略された項目は継承元、
/// 最終的にはクライアント同梱の既定フォントで埋まる。`family` の省略が
/// `EXT-020` の観点では望ましい ([`spec/04-theme.md`] 9 章)。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Font {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub line_height: Option<f32>,
    pub weight: Option<u16>,
    pub italic: Option<bool>,
    pub letter_spacing: Option<f32>,
}

/// 影。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

/// 内余白 / 外余白。単一値と `[上, 右, 下, 左]` の両方を受ける。
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

/// 背景画像の領域への合わせ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fit {
    /// 領域を覆う。はみ出す分は切る (既定)
    #[default]
    Cover,
    /// 領域に収める。余る分は透明
    Contain,
    /// 縦横比を無視して引き伸ばす
    Stretch,
    /// 原寸で敷き詰める
    Tile,
    /// 原寸のまま 1 枚だけ置く
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

    /// `position` を見るのはこの 3 つのときだけである。
    pub const fn uses_position(self) -> bool {
        matches!(self, Fit::Cover | Fit::Contain | Fit::None)
    }
}

/// アセットの参照先 (`EXT-017`)。
///
/// **相対パスがテーマディレクトリの外へ出られないこと**と、
/// **外部 URL のホストが `manifest.remoteAssets` に宣言されていること**は、
/// この型を作る時点で検査済みである ([`AssetRef::parse`])。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetRef {
    /// theme.json からの相対パス。`..` も絶対パスも含まないことは検証済み
    Bundled(String),
    /// data: URI
    Data { mime: String, base64: String },
    /// https の外部 URL。`host` は宣言済みであることを検証済み (`SEC-022`)
    Remote { url: String, host: String },
}

/// [`AssetRef::parse`] が受け付けなかった理由。
///
/// 呼び出し側はこれを診断に変換する。**どれもテーマ全体を捨てる理由には
/// ならない** (`EXT-027`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetError {
    /// `../` や絶対パスでテーマディレクトリの外を指している
    EscapesThemeDirectory,
    /// 拡張子が対応形式でない
    UnsupportedFormat,
    /// http など https 以外のスキーム (`SEC-024`)
    InsecureScheme,
    /// manifest.remoteAssets に宣言されていないホスト (`SEC-022`)
    UndeclaredHost(String),
    /// 書式として解釈できない
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

/// 画像として使える拡張子
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "avif"];
/// フォントとして使える拡張子 (`font.family` からの参照。M2)
const FONT_EXTENSIONS: &[&str] = &["woff2", "ttf", "otf"];

/// アセット参照として何を期待しているか。
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
    /// アセット参照を解析し、同時に安全性を検証する。
    ///
    /// `declared_hosts` は `manifest.remoteAssets`。空なら外部 URL は
    /// すべて拒否される。**拒否は「拒む」のではなく「到達しない」**という
    /// 意味である ([`spec/04-theme.md`] 6.4.1)。
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
        // https 以外のスキームは、それが何であれ外部参照の意図である
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
        // ポート番号を落とす。宣言はホスト名で行う
        let host = authority.split(':').next().unwrap_or(authority);
        // 大文字小文字を区別しない。宣言側はスキーマが小文字を強制している
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
        // Windows のパス区切りを混ぜさせない。テーマは全プラットフォームで
        // 同一に振る舞う必要がある (EXT-043)
        if path.contains('\\') {
            return Err(AssetError::EscapesThemeDirectory);
        }
        if path.starts_with('/') {
            return Err(AssetError::EscapesThemeDirectory);
        }
        // "C:/..." のようなドライブ文字つき絶対パス
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

/// 背景 (`EXT-021`〜`EXT-024`, `EXT-027`)。
///
/// 合成順序は下から `color` → `image` (`opacity` と `blur` 適用済み) → `tint`。
///
/// `blur` は**読み込み時に一度だけ**適用する (`EXT-023`)。毎フレームの
/// 畳み込みは S1 の下限基準 (Intel HD 520) では割に合わない。
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    /// 画像の下に敷く色。画像が読めなかったときのフォールバックでもある
    pub color: Option<Color>,
    pub image: Option<AssetRef>,
    pub fit: Fit,
    /// `[x, y]`、各 0.0〜1.0
    pub position: [f32; 2],
    pub opacity: f32,
    pub blur: f32,
    /// 画像の上に重ねる色。可読性の確保に使う
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
    /// 色だけの背景。`"background": "#111"` の短縮記法に対応する。
    pub fn solid(color: Color) -> Self {
        Background {
            color: Some(color),
            ..Default::default()
        }
    }

    /// 画像を伴うか。レンダラのテクスチャ経路が要るかの判定に使う。
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
        // 大文字も受ける
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
            assert_eq!(Color::parse(s), None, "{s} を受け付けてはならない");
        }
    }

    /// 相対パスはテーマディレクトリの外へ出られない (spec/04-theme.md 6.3)
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
                "{s} を通してはならない"
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
            "アニメーション形式は EXT-026 で M2"
        );
        assert_eq!(
            AssetRef::parse("assets/bg", AssetKind::Image, &[]),
            Err(AssetError::UnsupportedFormat)
        );
        // 大文字の拡張子は受ける
        assert!(AssetRef::parse("assets/BG.PNG", AssetKind::Image, &[]).is_ok());
        // 画像の場所にフォントは置けない
        assert_eq!(
            AssetRef::parse("assets/Inter.woff2", AssetKind::Image, &[]),
            Err(AssetError::UnsupportedFormat)
        );
    }

    /// SEC-022: 宣言されていないホストへは到達しない
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
        // 宣言が空なら外部は一切通らない
        assert!(matches!(
            AssetRef::parse("https://cdn.example.com/bg.png", AssetKind::Image, &[]),
            Err(AssetError::UndeclaredHost(_))
        ));
    }

    /// ポート番号つきでもホスト名で照合する
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

    /// SEC-024: https のみ
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
                "{s} を通してはならない"
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
