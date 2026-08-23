//! テキストの整形とグリフアトラス。
//!
//! ```text
//! cosmic-text で整形 (shaping / bidi / フォールバック)
//!     │
//! swash でラスタライズ
//!     │
//! RGBA8 アトラス (2048²、棚詰め。**文字は上から、絵は下から**)
//!     │
//! テクスチャ付きクアッド
//! ```
//!
//! # アトラスが RGBA8 なのはカラー絵文字のため
//!
//! マスクグリフは `(255,255,255,alpha)` として格納し、シェーダ側で色を掛ける。
//! カラーグリフはテクスチャの色をそのまま使う ([`spec/06-renderer.md`] 6.1)。
//!
//! # 整形結果はキャッシュする
//!
//! S2 のスパイクは毎フレーム整形し直していた。同じ文字列・同じ書体・同じ
//! 折り返し幅なら結果は変わらないので、鍵にして持つ。
//!
//! 整形は**物理ピクセルで行う**。ラスタライズがそうである以上、そこで
//! 丸めるしかない。呼び出し側へ返す寸法だけを論理ピクセルに戻す。

use std::collections::HashMap;

use cosmic_text::fontdb;
use cosmic_text::{
    Attrs, Buffer, CacheKey, Fallback, Family, FontSystem, Metrics, PlatformFallback, Shaping,
    Style as FontStyle, SwashCache, SwashContent, Weight, Wrap,
};
use gumicord_uitree::Style;
use gumicord_uitree::value::Font;
use unicode_script::Script;

use crate::geom::Size;
use crate::icon;

/// アトラス 1 ページの一辺 (物理 px)
pub const ATLAS_SIZE: u32 = 2048;

/// クライアント同梱の本文フォント。**同梱する理由は 3 つある。**
///
/// | | |
/// |---|---|
/// | `EXT-020` | システムフォント任せだと環境ごとに書体が変わる |
/// | `NFR-001` | いずれ起動時のシステムフォント列挙 (S2 実測 360ms) を外すため |
/// | 見た目 | OS の既定 sans-serif は UI 用に設計されていない |
///
/// Inter (SIL OFL 1.1、[`assets/fonts/README.md`] に出処)。**可変フォント
/// 1 本で Thin〜Black を賄う。** cosmic-text 0.19 が `wght` 軸に対応して
/// いるので、静的インスタンスを重さのぶんだけ持つ必要がない。
///
/// ⚠️ **CJK は同梱していない。** Noto Sans JP を足すとバイナリが倍以上に
/// なるため、判断を分けた ([`spec/06-renderer.md`] 6.4)。日本語はいまも
/// システムフォントへのフォールバックで描いている。
const BUNDLED_SANS: &[u8] = include_bytes!("../../../assets/fonts/Inter.ttf");

/// 同梱フォントのファミリ名。`Family::SansSerif` の解決先にする
const BUNDLED_SANS_FAMILY: &str = "Inter";

/// 日本語のフォールバック先。**優先順**に並べる。
///
/// cosmic-text の Windows 用の表は `"Yu Gothic"` の 1 つしか持たない。
/// UI 用に調整された `Yu Gothic UI` を先に試し、古い環境のために `Meiryo` も
/// 見る。macOS / Linux / Android の名前を続けてあるのは、**同じ順序で同じ
/// 結果になってほしい**からである (`EXT-020`)。
///
/// ⚠️ これはあくまで「システムにあれば使う」一覧である。同じ書体が全環境に
/// あるわけではないので、`EXT-020` を厳密に満たすには日本語フォントの同梱が
/// 要る ([`assets/fonts/README.md`])。
const JAPANESE_FALLBACK: &[&str] = &[
    // Windows
    "Yu Gothic UI",
    "Yu Gothic",
    "Meiryo",
    // macOS / iOS
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    // Linux / Android
    "Noto Sans CJK JP",
    "Noto Sans JP",
];

/// フォントのフォールバック順を決める。
///
/// # なぜ自前で持つのか
///
/// **漢字は Han 統合により、同じ符号位置でも言語によって字形が違う。**
/// cosmic-text の判定は locale の**完全一致**で、`"ja"` / `"ko"` /
/// `"zh-HK"` / `"zh-TW"` / それ以外 の 5 択しかない。Windows が返す
/// `"ja-JP"` はどれにも当たらず、既定の `Microsoft YaHei UI`
/// (簡体字中国語) に落ちる。結果、日本語の文章が中国語の書体で描かれる。
///
/// locale の正規化 ([`normalize_locale`]) だけでも直るが、それでも
/// 頼れるのは `Yu Gothic` 1 つだけになる。ここで一覧ごと持ち替える。
#[derive(Debug)]
struct GumicordFallback;

impl Fallback for GumicordFallback {
    fn common_fallback(&self) -> &[&'static str] {
        PlatformFallback.common_fallback()
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        PlatformFallback.forbidden_fallback()
    }

    fn script_fallback(&self, script: Script, locale: &str) -> &[&'static str] {
        let han_unified = matches!(script, Script::Han | Script::Hiragana | Script::Katakana);
        // 中国語・韓国語の利用者にまで日本語の字形を出すのは誤りなので、
        // そこはプラットフォームの判断に任せる
        if han_unified && !locale.starts_with("zh") && !locale.starts_with("ko") {
            return JAPANESE_FALLBACK;
        }
        PlatformFallback.script_fallback(script, locale)
    }
}

/// cosmic-text の Han 統合の判定に通る形へ locale を整える。
///
/// 判定が完全一致なので、`"ja-JP"` や `"ja_JP.UTF-8"` はそのままでは
/// 当たらない。**地域まで見る必要があるのは中国語だけ**なので、それ以外は
/// 主言語タグへ切り詰める。
fn normalize_locale(locale: &str) -> String {
    let tag = locale.replace('_', "-");
    // "ja-JP.UTF-8" のような形も来る
    let tag = tag.split('.').next().unwrap_or("");
    let mut parts = tag.split('-');
    let lang = parts.next().unwrap_or("").to_ascii_lowercase();

    if lang != "zh" {
        return lang;
    }

    // 繁体か簡体かは文字体系か地域で決まる
    for p in parts {
        match p.to_ascii_uppercase().as_str() {
            "HANT" | "TW" => return "zh-TW".to_owned(),
            "HK" | "MO" => return "zh-HK".to_owned(),
            _ => {}
        }
    }
    "zh-CN".to_owned()
}

/// テーマが何も言わなかったときの本文の大きさ (論理 px)
pub const DEFAULT_FONT_SIZE: f32 = 15.0;
/// 同上、行の高さ
pub const DEFAULT_LINE_HEIGHT: f32 = 22.0;

/// 確定した書体。[`Style::font`] の未指定を既定で埋めたもの。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedFont {
    pub family: Option<String>,
    /// 論理 px を 1/64 単位で丸めた値。浮動小数を鍵にしないため
    size_q: u32,
    line_height_q: u32,
    /// 字送りの追加分 (論理 px を量子化)。0 なら指定なし
    letter_spacing_q: u32,
    pub weight: u16,
    pub italic: bool,
}

const Q: f32 = 64.0;

fn quantize(v: f32) -> u32 {
    (v.max(0.0) * Q).round() as u32
}

fn dequantize(q: u32) -> f32 {
    q as f32 / Q
}

impl ResolvedFont {
    pub fn from_style(style: &Style) -> Self {
        let f = style.font.clone().unwrap_or_default();
        Self::from_font(&f)
    }

    pub fn from_font(f: &Font) -> Self {
        let size = f.size.unwrap_or(DEFAULT_FONT_SIZE);
        ResolvedFont {
            family: f.family.clone(),
            size_q: quantize(size),
            // 行の高さの指定がなければ、大きさに比例させる。
            // 既定 (15 / 22) と同じ比率にしておくと、大きさだけ変えたときに
            // 行間が詰まって見えない
            line_height_q: quantize(
                f.line_height
                    .unwrap_or(size * DEFAULT_LINE_HEIGHT / DEFAULT_FONT_SIZE),
            ),
            letter_spacing_q: quantize(f.letter_spacing.unwrap_or(0.0)),
            weight: f.weight.unwrap_or(400),
            italic: f.italic.unwrap_or(false),
        }
    }

    pub fn size(&self) -> f32 {
        dequantize(self.size_q)
    }

    pub fn line_height(&self) -> f32 {
        dequantize(self.line_height_q)
    }
}

/// 整形結果の鍵。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    /// 整形する一続きの並び。飾りの無い文字列でも要素 1 つの並びである。
    ///
    /// ⚠️ **飾りごとに分けて整形しない。** 分けると折り返しがそれぞれ
    /// 独立して決まり、`これは **とても長い** 文章` の行末が合わなくなる
    runs: Vec<(String, ResolvedFont)>,
    /// 折り返し幅 (物理 px を量子化)。`u32::MAX` は折り返さない
    max_w_q: u32,
    /// DPI スケール。変わると物理ピクセルでの整形結果が変わる
    scale_q: u32,
}

/// 原点 (0,0) に置いたときのグリフ 1 個。位置は**物理 px**。
#[derive(Debug, Clone, Copy)]
pub struct PlacedGlyph {
    pub cache_key: CacheKey,
    /// ラスタライズ用に丸めた位置
    pub x: i32,
    pub y: i32,
    /// 元の文字列でのバイト範囲。キャレットと選択の位置決めに使う
    pub start: usize,
    pub end: usize,
    /// 丸める前の位置と送り幅。**丸めた `x` を使うと選択範囲に隙間が出る**
    pub left: f32,
    pub advance: f32,
    /// この字が乗っている行
    pub line_top: f32,
    pub line_height: f32,
    /// 何番目の走りから来た字か。**色を変えるのに要る**
    pub run: u32,
}

/// 整形済みのテキスト。
#[derive(Debug, Clone)]
pub struct Shaped {
    /// 論理 px での大きさ
    pub size: Size,
    /// 原点 (0,0) を左上としたときのグリフ列。位置は物理 px
    pub glyphs: Vec<PlacedGlyph>,
    /// 走りごとの矩形。下線・打ち消し・スポイラーの塗りに使う。
    ///
    /// ⚠️ **行ごとに切れている。** 折り返した走りを 1 つの矩形で塗ると、
    /// 行間まで塗って本文が読めなくなる ([`Shaped::range_rects`] と同じ理由)
    pub runs: Vec<RunRect>,
    /// 行の高さ (物理 px)。字が 1 つもないときのキャレットの高さに使う
    pub line_height: f32,
}

/// テキストの上の矩形 1 つ (物理 px、原点からの相対)。
///
/// キャレットも選択も下線も、結局は矩形である。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// 走り 1 つが 1 行の中で占める矩形 (物理 px、原点からの相対)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunRect {
    pub run: u32,
    pub rect: TextRect,
}

impl Shaped {
    /// バイト位置にキャレットを置いたときの矩形。
    ///
    /// **その位置から始まる字の左端**に置く。該当する字がなければ、
    /// 直前の字の右端 (= 行末) に置く。
    pub fn caret(&self, at: usize, width: f32) -> TextRect {
        // その位置から始まる字
        if let Some(g) = self.glyphs.iter().find(|g| g.start >= at) {
            return TextRect {
                x: g.left,
                y: g.line_top,
                w: width,
                h: g.line_height,
            };
        }
        // 行末。最後の字の右
        match self.glyphs.last() {
            Some(g) => TextRect {
                x: g.left + g.advance,
                y: g.line_top,
                w: width,
                h: g.line_height,
            },
            // 何も入っていない
            None => TextRect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: self.line_height,
            },
        }
    }

    /// バイト範囲を覆う矩形。**行ごとに 1 つずつ**返す。
    ///
    /// 折り返した選択範囲を 1 つの矩形で塗ると、行間まで塗ってしまう。
    pub fn range_rects(&self, range: &core::ops::Range<usize>) -> Vec<TextRect> {
        let mut out: Vec<TextRect> = Vec::new();
        if range.is_empty() {
            return out;
        }

        for g in &self.glyphs {
            // 範囲に少しでも掛かる字を拾う
            if g.end <= range.start || g.start >= range.end {
                continue;
            }
            // 同じ行の続きなら伸ばす
            match out.last_mut() {
                Some(last) if (last.y - g.line_top).abs() < f32::EPSILON => {
                    last.w = (g.left + g.advance) - last.x;
                }
                _ => out.push(TextRect {
                    x: g.left,
                    y: g.line_top,
                    w: g.advance,
                    h: g.line_height,
                }),
            }
        }
        out
    }
}

/// アトラスに載ったグリフ。
#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
    /// 何ページ目に載っているか。
    ///
    /// ⚠️ **ページごとにテクスチャが違う。** 描くときに束ね直す必要が
    /// あるので、これを持たないと 1 枚目のページから読んでしまう
    pub page: u32,
    /// アトラス内の UV (0..1)
    pub uv: [f32; 4],
    /// ペン位置からのずれ (物理 px)
    pub left: i32,
    pub top: i32,
    pub w: u32,
    pub h: u32,
    /// カラー絵文字なら真。シェーダで色を掛けない
    pub is_color: bool,
}

/// 文字列を整形するところ。**GPU を持たない。**
///
/// レイアウトに要るのは「この文字列はこの幅で何ピクセルになるか」だけであり、
/// テクスチャは要らない。ここを [`TextEngine`] から切り離してあるので、
/// **レイアウトは GPU なしで試験できる**。`NFR-015` (全プラットフォームでの
/// スクリーンショット比較) の足場にもなる。
pub struct Shaper {
    font_system: FontSystem,
    swash: SwashCache,
    shaped: HashMap<ShapeKey, Shaped>,
    scale: f32,
}

impl Shaper {
    /// ⚠️ `FontSystem::new()` はシステムフォントを列挙する。S2 の実測で
    /// **初回 360ms** かかった。`NFR-001` (コールドスタート 500ms) に対して
    /// 致命的である。
    ///
    /// R4 の残りはここである。同梱フォントだけで整形を始め、列挙は
    /// 背景スレッドへ回して、終わったらフォールバックの要るテキストだけを
    /// 整形し直す ([`spec/06-renderer.md`] 6.3)。**いまは列挙を待っている。**
    /// CJK のフォールバックがシステムフォント頼みで、待たないと日本語が
    /// 出ないためである。
    pub fn new(scale: f32) -> Self {
        let raw = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_owned());
        let locale = normalize_locale(&raw);
        tracing::debug!(%raw, %locale, "フォントの locale");

        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        db.load_font_data(BUNDLED_SANS.to_vec());
        // テーマが `family` を書かなければ `Family::SansSerif` になる。
        // その解決先を同梱フォントへ向けておくと、テーマ側は何も書かなくてよい
        db.set_sans_serif_family(BUNDLED_SANS_FAMILY);

        let font_system =
            FontSystem::new_with_locale_and_db_and_fallback(locale, db, GumicordFallback);

        Shaper {
            font_system,
            swash: SwashCache::new(),
            shaped: HashMap::new(),
            scale,
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// DPI が変わったら、物理ピクセルで作った整形結果をすべて捨てる。
    ///
    /// 戻り値が真なら、呼び出し側はグリフアトラスも捨てる必要がある
    /// ([`spec/06-renderer.md`] 3 章)。
    pub fn set_scale(&mut self, scale: f32) -> bool {
        if (scale - self.scale).abs() < f32::EPSILON {
            return false;
        }
        self.scale = scale;
        self.shaped.clear();
        true
    }

    fn key(&self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> ShapeKey {
        self.key_rich(&[(text.to_owned(), font.clone())], max_w)
    }

    fn key_rich(&self, runs: &[(String, ResolvedFont)], max_w: Option<f32>) -> ShapeKey {
        ShapeKey {
            runs: runs.to_vec(),
            max_w_q: max_w.map(quantize).unwrap_or(u32::MAX),
            scale_q: quantize(self.scale),
        }
    }

    fn ensure(&mut self, key: &ShapeKey, max_w: Option<f32>) {
        // `entry` を使うと鍵を必ず複製することになる。
        // 当たる回のほうが圧倒的に多いので、当たりを安く済ませる
        if !self.shaped.contains_key(key) {
            let shaped = self.shape_uncached(&key.runs, max_w);
            self.shaped.insert(key.clone(), shaped);
        }
    }

    /// 文字列を整形する。`max_w` は論理 px、`None` なら折り返さない。
    pub fn shape(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> &Shaped {
        let key = self.key(text, font, max_w);
        self.ensure(&key, max_w);
        &self.shaped[&key]
    }

    /// 飾りの混じった文字列を**一度に**整形する (`FR-021`)。
    ///
    /// ⚠️ **走りごとに [`Shaper::shape`] を呼んで横に並べてはいけない。**
    /// 折り返しがそれぞれ独立して決まるので、行末が合わなくなる。
    /// 混じったまま 1 つの整形にかけて、はじめて正しい行になる
    pub fn shape_rich(&mut self, runs: &[(String, ResolvedFont)], max_w: Option<f32>) -> &Shaped {
        let key = self.key_rich(runs, max_w);
        self.ensure(&key, max_w);
        &self.shaped[&key]
    }

    /// 整形して大きさだけを返す。レイアウトの計測で使う。
    pub fn measure(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> Size {
        self.shape(text, font, max_w).size
    }

    pub fn measure_rich(&mut self, runs: &[(String, ResolvedFont)], max_w: Option<f32>) -> Size {
        self.shape_rich(runs, max_w).size
    }

    /// 走りの並びを**1 つのバッファ**に流して整形する。
    ///
    /// ⚠️ 行の高さは全体で 1 つである。走りごとに大きさを変えたいときは
    /// ここでは足りない。M1 の Markdown では見出しだけが大きさを変え、
    /// 見出しは別のノードなので足りている
    fn shape_uncached(&mut self, runs: &[(String, ResolvedFont)], max_w: Option<f32>) -> Shaped {
        let scale = self.scale;
        let base = match runs.first() {
            Some((_, f)) => f.clone(),
            None => ResolvedFont::from_style(&Style::default()),
        };
        let metrics = Metrics::new(base.size() * scale, base.line_height() * scale);
        let mut buf = Buffer::new(&mut self.font_system, metrics);

        buf.set_wrap(Wrap::WordOrGlyph);
        buf.set_size(max_w.map(|w| w * scale), None);

        let spans: Vec<(&str, Attrs)> = runs
            .iter()
            .enumerate()
            // ⚠️ **番号を持たせる。** これが無いと、整形が済んだあとで
            // 「この字はどの走りから来たか」を byte 位置で数え直すことに
            // なる。走りが空文字列を含むと数え直しは合わない
            .map(|(i, (t, f))| (t.as_str(), attrs_of(f).metadata(i)))
            .collect();
        buf.set_rich_text(spans, &attrs_of(&base), Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.font_system, false);

        let mut w = 0.0f32;
        let mut h = 0.0f32;
        let mut glyphs = Vec::new();
        let mut rects: Vec<RunRect> = Vec::new();
        for run in buf.layout_runs() {
            w = w.max(run.line_w);
            h = h.max(run.line_top + run.line_height);
            for g in run.glyphs {
                let p = g.physical((0.0, run.line_y), 1.0);
                let which = g.metadata as u32;
                glyphs.push(PlacedGlyph {
                    cache_key: p.cache_key,
                    x: p.x,
                    y: p.y,
                    start: g.start,
                    end: g.end,
                    left: g.x,
                    advance: g.w,
                    line_top: run.line_top,
                    line_height: run.line_height,
                    run: which,
                });
                // 同じ走りが同じ行で続いている間は 1 つの矩形に伸ばす
                match rects.last_mut() {
                    Some(last)
                        if last.run == which
                            && (last.rect.y - run.line_top).abs() < f32::EPSILON =>
                    {
                        last.rect.w = (g.x + g.w) - last.rect.x;
                    }
                    _ => rects.push(RunRect {
                        run: which,
                        rect: TextRect {
                            x: g.x,
                            y: run.line_top,
                            w: g.w,
                            h: run.line_height,
                        },
                    }),
                }
            }
        }

        Shaped {
            // 物理 px で整形したので論理へ戻す。
            //
            // ⚠️ **切り上げる。** 内容ぴったりの幅になるノード (見出しなど) は、
            // ここで返した幅がそのまま矩形の幅になり、描画時にはその幅で
            // もう一度折り返し判定が走る。丸め誤差で 1ulp でも狭くなると、
            // 収まっていたはずの最後の 1 文字が次の行へ落ちる
            size: Size::new((w / scale).ceil(), (h / scale).ceil()),
            glyphs,
            runs: rects,
            line_height: metrics.line_height,
        }
    }
}

/// 書体を `cosmic-text` の言い方へ直す。
fn attrs_of(font: &ResolvedFont) -> Attrs<'_> {
    let mut attrs = Attrs::new()
        .weight(Weight(font.weight))
        .style(if font.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        });
    if let Some(family) = &font.family {
        attrs = attrs.family(Family::Name(family));
    }
    if font.letter_spacing_q != 0 {
        // テーマは論理 px で書く。cosmic-text は EM で受ける
        attrs = attrs.letter_spacing(dequantize(font.letter_spacing_q) / font.size());
    }
    attrs
}

/// 整形 ([`Shaper`]) に、GPU 上のグリフアトラスを足したもの。描画で使う。
pub struct TextEngine {
    shaper: Shaper,
    atlas: Atlas,
}

impl TextEngine {
    pub fn new(device: &wgpu::Device, scale: f32) -> Self {
        TextEngine {
            shaper: Shaper::new(scale),
            atlas: Atlas::new(device),
        }
    }

    /// 整形だけが要る呼び出し側 (レイアウト) へ渡す。
    pub fn shaper(&mut self) -> &mut Shaper {
        &mut self.shaper
    }

    /// ページごとのテクスチャ。**束ね直すのに要る**
    pub fn atlas_views(&self) -> Vec<&wgpu::TextureView> {
        self.atlas.pages.iter().map(|p| &p.view).collect()
    }

    /// ページが増えたか。**1 回だけ真を返す**
    pub fn took_atlas_growth(&mut self) -> bool {
        self.atlas.took_growth()
    }

    /// DPI が変わったら、整形結果もグリフも作り直す。
    pub fn set_scale(&mut self, device: &wgpu::Device, scale: f32) {
        if self.shaper.set_scale(scale) {
            self.atlas = Atlas::new(device);
        }
    }

    /// 整形してアトラスへ載せ、グリフを 1 個ずつ渡す。
    ///
    /// `f` は `(アトラス上の位置, テキスト原点からのずれ)` を受け取る。
    /// ずれは**物理 px** である。戻り値は整形結果の大きさ (論理 px)。
    ///
    /// 整形結果の借用 (不変) とアトラスへの追記 (可変) が衝突するので、
    /// `self` をフィールドごとに分解して両立させている。
    pub fn draw_glyphs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        text: &str,
        font: &ResolvedFont,
        max_w: Option<f32>,
        mut f: impl FnMut(&GlyphEntry, i32, i32),
    ) -> Size {
        let key = self.shaper.key(text, font, max_w);
        self.shaper.ensure(&key, max_w);

        let Shaper {
            shaped,
            font_system,
            swash,
            ..
        } = &mut self.shaper;
        let atlas = &mut self.atlas;

        let s = &shaped[&key];
        for g in &s.glyphs {
            if let Some(e) = atlas.glyph(device, queue, font_system, swash, g.cache_key)
                && e.w != 0
            {
                f(&e, g.x, g.y);
            }
        }
        s.size
    }

    /// 飾りの混じった文字を積む。
    ///
    /// `f` は `(アトラス上の位置, ずれ, 何番目の走りか)` を受け取る。
    /// **走りの番号が要る** — 走りごとに色が違うためである。
    ///
    /// 走りごとの矩形も返す。下線・打ち消し・スポイラーはそれで塗る
    pub fn draw_rich_glyphs(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        runs: &[(String, ResolvedFont)],
        max_w: Option<f32>,
        mut f: impl FnMut(&GlyphEntry, i32, i32, u32),
    ) -> Vec<RunRect> {
        let key = self.shaper.key_rich(runs, max_w);
        self.shaper.ensure(&key, max_w);

        let Shaper {
            shaped,
            font_system,
            swash,
            ..
        } = &mut self.shaper;
        let atlas = &mut self.atlas;

        let s = &shaped[&key];
        for g in &s.glyphs {
            if let Some(e) = atlas.glyph(device, queue, font_system, swash, g.cache_key)
                && e.w != 0
            {
                f(&e, g.x, g.y, g.run);
            }
        }
        // ⚠️ 借用の都合で複製する。走りの数は本文の飾りの数であって、
        // 字の数ではない。一続きの文で数個である
        s.runs.clone()
    }

    /// アイコンをアトラスへ載せて位置を返す。`size_px` は物理ピクセル。
    ///
    /// 知らない名前には `None` を返す。**誤りではない** ([`crate::icon`])。
    pub fn icon(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &str,
        size_px: u32,
    ) -> Option<GlyphEntry> {
        let (name, def) = icon::lookup(name)?;
        self.atlas.icon(device, queue, name, def, size_px)
    }

    /// アトラスに載っているものの数。性能の目安に使う
    pub fn glyph_count(&self) -> usize {
        self.atlas.uploaded
    }
}

// ─────────────────────────────────────────────────────────────── アトラス

/// 棚詰めのグリフアトラス。
///
/// ⚠️ **1 ページしかない。** 溢れたらそのグリフを描かない。
/// 複数ページ化と LRU 回収はロードマップ R3 で、まだやっていない。
/// 2048² に 20px 級のグリフなら 1 万個ほど入るので、M1.1 の範囲では溢れない。
/// アトラスに載るものの鍵。
///
/// **グリフとアイコンで texture を分けない。** 分けるとパイプラインの切り替えが
/// 増え、描画順を保ったまま束ねられる範囲が狭くなる。どちらも
/// 「RGBA8 のマスクをテクスチャ付きクアッドで描く」ものなので、同じ 1 枚に
/// 詰めればよい。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AtlasKey {
    Glyph(CacheKey),
    /// アイコン名と、描く物理ピクセルの大きさ
    Icon(&'static str, u32),
    /// 取ってきた画像。**URL の指紋で引く**
    Image(u64),
}

/// アトラスへ詰めるときの、絵そのもの以外の情報。
#[derive(Debug, Clone, Copy)]
struct Placement {
    /// ペン位置からのずれ (物理 px)
    left: i32,
    top: i32,
    /// カラー絵文字なら真
    is_color: bool,
}

impl Placement {
    /// アイコンは正方形で、ペン位置からのずれを持たない
    const ICON: Placement = Placement {
        left: 0,
        top: 0,
        is_color: false,
    };
}

/// アトラスの詰め方。**大きさの桁が違うものを混ぜない。**
///
/// # ⚠️ 棚は一番背の高いものに合わせて厚くなる
///
/// 20px のグリフが並ぶ棚に 128px のアバターが 1 枚落ちると、**その棚は
/// 128px 厚になる**。残りの 108px × 2048 は誰も使わない。それが数回
/// 起きただけで 2048×2048 が埋まり、以降の**グリフが 1 つも入らなく
/// なる**。実際に、日本語の本文が虫食いで出た。
///
/// そこで、グリフは上から下へ、絵は下から上へ詰める。互いの棚に
/// 混ざらないので、厚みの無駄が出ない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// 文字。小さくて数が多い
    Glyph,
    /// 絵。大きくて数が少ない
    Image,
}

/// 棚の状態だけ。**GPU に触らない**ので、そのまま試験できる
#[derive(Debug, Default)]
struct Shelves {
    /// 上から下へ伸びる、文字の棚
    cursor_x: u32,
    cursor_y: u32,
    shelf_h: u32,
    /// 下から上へ伸びる、絵の棚。`image_top` は**いまの棚の上端**
    image_x: u32,
    image_top: u32,
    image_shelf_h: u32,
    /// ⚠️ **側ごとに持つ。** 絵で埋まったからといって文字まで
    /// 諦めると、本文が虫食いになる
    glyphs_full: bool,
    images_full: bool,
}

/// アトラス 1 ページ。**1 枚のテクスチャと、その詰め具合**
struct Page {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    shelves: Shelves,
}

impl Page {
    fn new(device: &wgpu::Device, n: usize) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("gumicord-atlas-{n}")),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Page {
            texture,
            view,
            shelves: Shelves::new(),
        }
    }
}

/// ページを何枚まで増やすか。
///
/// ⚠️ **1 枚 16MB である** (2048² × RGBA)。増やすほど常駐メモリが増える
/// ので、際限なくは持てない。4 枚で 64MB — 公式 Electron 版より小さい
/// という主張を保てる上限として置いてある。
///
/// **足りるかどうかは実測で決める。** ここに達するのは、日本語と絵文字と
/// 顔を大量に見た後である
const MAX_PAGES: usize = 4;

struct Atlas {
    /// ⚠️ **最後のページが「いま詰めているページ」である。**
    /// 前のページに戻って隙間を探したりはしない — 探す価値のある隙間は
    /// 棚詰めの性質上ほとんど残らない
    pages: Vec<Page>,
    entries: HashMap<AtlasKey, Option<GlyphEntry>>,
    uploaded: usize,
    /// 前回作り直してから何枚入れたか。**空回りを見分けるために要る**
    images_since_recycle: usize,
    /// 絵を作り直した。**呼び出し側が取りに来る** ([`Atlas::took_recycle`])
    recycled: bool,
    /// ページが増えた。**束ね直しが要る** ([`Atlas::took_growth`])
    grew: bool,
}

impl Atlas {
    fn new(device: &wgpu::Device) -> Self {
        Atlas {
            pages: vec![Page::new(device, 0)],
            entries: HashMap::new(),
            uploaded: 0,
            images_since_recycle: 0,
            recycled: false,
            grew: true,
        }
    }

    /// ページが増えたか。**1 回だけ真を返す**
    fn took_growth(&mut self) -> bool {
        std::mem::take(&mut self.grew)
    }

    /// いま詰めているページで場所を取る。入らなければ**ページを足す**。
    ///
    /// ⚠️ **足せなくなったら諦める。** そのときだけ虫食いになる
    fn alloc(&mut self, device: &wgpu::Device, w: u32, h: u32, side: Side) -> Option<(u32, u32)> {
        let last = self.pages.len() - 1;
        if let Some(at) = self.pages[last].shelves.alloc(w, h, side) {
            return Some(at);
        }
        if self.pages.len() >= MAX_PAGES {
            tracing::warn!(
                pages = MAX_PAGES,
                "アトラスが上限まで埋まった。ここから先は描けない"
            );
            return None;
        }
        tracing::info!(pages = self.pages.len() + 1, "アトラスのページを足す");
        self.pages.push(Page::new(device, self.pages.len()));
        self.grew = true;
        let last = self.pages.len() - 1;
        self.pages[last].shelves.alloc(w, h, side)
    }

    /// 絵の側を丸ごと空けて、詰め直せるようにする。
    ///
    /// # ⚠️ 追い出すのではなく、忘れる
    ///
    /// 棚詰めは途中の 1 枚を空けられない。**要らなくなった 1 枚だけを
    /// 抜くことができない**以上、まとめて忘れるしかない。
    ///
    /// 忘れた絵は取ってきた側が持っている (円盤にも残っている) ので、
    /// **次のフレームで入れ直される**。1 フレームだけ顔が消える。
    ///
    /// ⚠️ **文字の側は触らない。** 本文が虫食いになるのは、顔が 1 枚
    /// 消えるよりずっと悪い。
    ///
    /// ⚠️ **入れた枚数が少ないうちは作り直さない。** 入りきらない大きさの
    /// ものを頼まれているなら、何度作り直しても入らない。**作り直しては
    /// 溢れ、溢れては作り直す**空回りになる
    fn recycle_images(&mut self) -> bool {
        /// これだけ入ってから溢れたなら、詰め直す価値がある
        const WORTH_IT: usize = 16;

        if self.images_since_recycle < WORTH_IT {
            return false;
        }
        tracing::info!(images = self.images_since_recycle, "アトラスの絵を詰め直す");
        self.entries.retain(|k, _| !matches!(k, AtlasKey::Image(_)));
        for p in &mut self.pages {
            p.shelves.reset_images();
        }
        self.images_since_recycle = 0;
        self.recycled = true;
        true
    }

    /// 絵を忘れたか。**1 回だけ真を返す**
    fn took_recycle(&mut self) -> bool {
        std::mem::take(&mut self.recycled)
    }
}

impl Shelves {
    fn new() -> Self {
        Shelves {
            cursor_x: 0,
            cursor_y: 0,
            shelf_h: 0,
            // 幅いっぱいから始めることで、最初の 1 枚が棚を作る
            image_x: ATLAS_SIZE,
            image_top: ATLAS_SIZE,
            image_shelf_h: 0,
            glyphs_full: false,
            images_full: false,
        }
    }

    /// 絵の棚を空にする。**文字の棚は触らない**
    fn reset_images(&mut self) {
        self.image_x = ATLAS_SIZE;
        self.image_top = ATLAS_SIZE;
        self.image_shelf_h = 0;
        self.images_full = false;
    }

    /// 空き場所を 1 つ取る。**取れなければ `None`**
    fn alloc(&mut self, w: u32, h: u32, side: Side) -> Option<(u32, u32)> {
        /// 棚詰め。1px 空けて隣のにじみを防ぐ
        const PAD: u32 = 1;

        match side {
            Side::Glyph => {
                if self.glyphs_full {
                    return None;
                }
                if self.cursor_x + w + PAD > ATLAS_SIZE {
                    self.cursor_x = 0;
                    self.cursor_y += self.shelf_h + PAD;
                    self.shelf_h = 0;
                }
                // 絵の側へ食い込まない
                if self.cursor_y + h + PAD > self.image_top {
                    tracing::debug!(
                        y = self.cursor_y,
                        "このページの文字側が埋まった。次のページへ"
                    );
                    self.glyphs_full = true;
                    return None;
                }
                let at = (self.cursor_x, self.cursor_y);
                self.cursor_x += w + PAD;
                self.shelf_h = self.shelf_h.max(h);
                Some(at)
            }
            Side::Image => {
                if self.images_full {
                    return None;
                }
                // 横が足りない、または棚より背が高いなら棚を作り直す
                if self.image_x + w + PAD > ATLAS_SIZE || h > self.image_shelf_h {
                    let need = h + PAD;
                    if self.image_top < need {
                        self.images_full = true;
                        return None;
                    }
                    let top = self.image_top - need;
                    // 文字の側へ食い込まない
                    if top < self.cursor_y + self.shelf_h + PAD {
                        tracing::debug!(
                            top = self.image_top,
                            "このページの絵の側が埋まった。次のページへ"
                        );
                        self.images_full = true;
                        return None;
                    }
                    self.image_top = top;
                    self.image_x = 0;
                    self.image_shelf_h = h;
                }
                let at = (self.image_x, self.image_top);
                self.image_x += w + PAD;
                Some(at)
            }
        }
    }
}

impl Atlas {
    fn glyph(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_system: &mut FontSystem,
        swash: &mut SwashCache,
        key: CacheKey,
    ) -> Option<GlyphEntry> {
        let k = AtlasKey::Glyph(key);
        if let Some(e) = self.entries.get(&k) {
            return *e;
        }
        let entry = self.rasterize_glyph(device, queue, font_system, swash, key);
        self.entries.insert(k, entry);
        entry
    }

    /// アイコンを載せる。`size` は物理ピクセルでの一辺。
    fn icon(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &'static str,
        def: &icon::IconDef,
        size: u32,
    ) -> Option<GlyphEntry> {
        let k = AtlasKey::Icon(name, size);
        if let Some(e) = self.entries.get(&k) {
            return *e;
        }
        // アイコンは正方形で、ペン位置からのずれを持たない
        let entry = self.insert(
            device,
            queue,
            size,
            size,
            &def.rasterize(size),
            Placement::ICON,
            Side::Glyph,
        );
        self.entries.insert(k, entry);
        entry
    }

    fn rasterize_glyph(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        font_system: &mut FontSystem,
        swash: &mut SwashCache,
        key: CacheKey,
    ) -> Option<GlyphEntry> {
        let image = swash.get_image(font_system, key).as_ref()?;
        let p = image.placement;
        if p.width == 0 || p.height == 0 {
            // 空白など。描くものはないが、位置は返す必要がある
            return Some(GlyphEntry {
                page: 0,
                uv: [0.0; 4],
                left: p.left,
                top: p.top,
                w: 0,
                h: 0,
                is_color: false,
            });
        }
        let (w, h) = (p.width, p.height);
        let is_color = matches!(image.content, SwashContent::Color);

        let mut rgba = vec![0u8; (w * h * 4) as usize];
        match image.content {
            SwashContent::Mask => {
                // マスクは (255,255,255,a)。色はシェーダが掛ける
                for (i, a) in image.data.iter().enumerate() {
                    let o = i * 4;
                    if o + 3 >= rgba.len() {
                        break;
                    }
                    rgba[o] = 255;
                    rgba[o + 1] = 255;
                    rgba[o + 2] = 255;
                    rgba[o + 3] = *a;
                }
            }
            SwashContent::Color | SwashContent::SubpixelMask => {
                let n = rgba.len().min(image.data.len());
                rgba[..n].copy_from_slice(&image.data[..n]);
            }
        }

        self.insert(
            device,
            queue,
            w,
            h,
            &rgba,
            Placement {
                left: p.left,
                top: p.top,
                is_color,
            },
            Side::Glyph,
        )
    }

    #[allow(clippy::too_many_arguments)]
    /// RGBA8 を 1 枚アトラスへ詰める。**グリフも絵もここを通る。**
    ///
    /// `side` は詰める向きを決める ([`Side`])
    fn insert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        w: u32,
        h: u32,
        rgba: &[u8],
        p: Placement,
        side: Side,
    ) -> Option<GlyphEntry> {
        let Placement {
            left,
            top,
            is_color,
        } = p;
        if w == 0 || h == 0 {
            return None;
        }
        let (x, y) = self.alloc(device, w, h, side)?;
        let page = self.pages.len() - 1;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.pages[page].texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );

        self.uploaded += 1;

        let s = ATLAS_SIZE as f32;
        Some(GlyphEntry {
            page: page as u32,
            uv: [
                x as f32 / s,
                y as f32 / s,
                (x + w) as f32 / s,
                (y + h) as f32 / s,
            ],
            left,
            top,
            w,
            h,
            is_color,
        })
    }
}

#[cfg(test)]
mod shelf_tests {
    use super::*;

    /// ⚠️ **絵は文字の棚を厚くしない。**
    ///
    /// 20px の字が並ぶ棚に 128px のアバターが 1 枚落ちると、その棚は
    /// 128px 厚になり、残りの 108px × 2048 は誰も使わない。それが何度か
    /// 起きただけでアトラスが埋まり、**以降の字が 1 つも入らなくなる**。
    /// 実際に、日本語の本文が虫食いで出た
    #[test]
    fn a_picture_does_not_thicken_the_glyph_shelf() {
        let mut s = Shelves::new();

        // 字を 1 つ置いてから、大きな絵を置く
        s.alloc(20, 20, Side::Glyph).expect("入る");
        let before = s.shelf_h;
        s.alloc(128, 128, Side::Image).expect("入る");

        assert_eq!(s.shelf_h, before, "字の棚は厚くならない");
    }

    /// 字は上から、絵は下から。**互いの領分へ食い込まない**
    #[test]
    fn glyphs_grow_down_and_pictures_grow_up() {
        let mut s = Shelves::new();

        let (_, gy) = s.alloc(20, 20, Side::Glyph).expect("入る");
        let (_, iy) = s.alloc(128, 128, Side::Image).expect("入る");

        assert_eq!(gy, 0, "字は上端から");
        assert!(iy > gy, "絵は下のほう");
        assert!(iy + 128 <= ATLAS_SIZE);
    }

    /// 同じ棚に並び、幅が尽きたら次の棚へ移る
    #[test]
    fn pictures_share_a_shelf_until_the_width_runs_out() {
        let mut s = Shelves::new();

        let (_, first) = s.alloc(128, 128, Side::Image).expect("入る");
        let (x, same) = s.alloc(128, 128, Side::Image).expect("入る");
        assert_eq!(same, first, "同じ棚");
        assert!(x > 0, "横に並ぶ");

        // 幅を使い切らせる
        for _ in 0..20 {
            s.alloc(128, 128, Side::Image);
        }
        let (_, next) = s.alloc(128, 128, Side::Image).expect("入る");
        assert!(next < first, "次の棚は上へ");
    }

    /// ⚠️ **絵で埋まっても字は入り続ける。**
    /// 諦めると本文が虫食いになる
    #[test]
    fn a_full_picture_side_does_not_stop_the_glyphs() {
        let mut s = Shelves::new();

        // 絵で埋め尽くす
        while s.alloc(256, 256, Side::Image).is_some() {}
        assert!(s.images_full);

        assert!(s.alloc(20, 20, Side::Glyph).is_some(), "字はまだ入る");
        assert!(!s.glyphs_full);
    }

    /// ⚠️ **詰め直せば、また入る。** 棚詰めは途中の 1 枚を空けられない
    /// ので、まとめて忘れるしかない
    #[test]
    fn resetting_the_picture_side_makes_room_again() {
        let mut s = Shelves::new();
        while s.alloc(256, 256, Side::Image).is_some() {}
        assert!(s.images_full);

        s.reset_images();
        assert!(!s.images_full);
        assert!(s.alloc(256, 256, Side::Image).is_some());
    }

    /// ⚠️ **詰め直しても文字の側は動かない。**
    /// 本文が虫食いになるのは、顔が 1 枚消えるよりずっと悪い
    #[test]
    fn resetting_the_picture_side_leaves_the_glyphs_alone() {
        let mut s = Shelves::new();
        s.alloc(20, 20, Side::Glyph).expect("入る");
        let (before_x, before_y) = (s.cursor_x, s.cursor_y);

        s.alloc(128, 128, Side::Image).expect("入る");
        s.reset_images();

        assert_eq!((s.cursor_x, s.cursor_y), (before_x, before_y));
        assert!(!s.glyphs_full);
    }

    /// ⚠️ **重ならない。** 両側から詰めても、同じ場所を 2 度渡さない
    #[test]
    fn the_two_sides_never_overlap() {
        let mut s = Shelves::new();

        let mut lowest_glyph = 0;
        while let Some((_, y)) = s.alloc(64, 64, Side::Glyph) {
            lowest_glyph = lowest_glyph.max(y + 64);
        }
        // 字で埋めた後は、絵はもう入らない
        assert!(s.alloc(128, 128, Side::Image).is_none());
        assert!(lowest_glyph <= ATLAS_SIZE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(size: f32) -> ResolvedFont {
        ResolvedFont::from_font(&Font {
            size: Some(size),
            ..Font::default()
        })
    }

    fn bold(size: f32) -> ResolvedFont {
        ResolvedFont::from_font(&Font {
            size: Some(size),
            weight: Some(700),
            ..Font::default()
        })
    }

    /// ⚠️ **飾りごとに整形して横に並べてはいけない。**
    ///
    /// 並べると折り返しがそれぞれ独立して決まり、行末が合わなくなる。
    /// ここでは「まとめて整形すると 2 行に折り返す幅」を与えて、
    /// **一続きの文として折り返していること**を見ている。走りごとに
    /// 整形していたら、どちらの走りも 1 行に収まって 1 行のままになる
    #[test]
    fn 混じった飾りは一続きの文として折り返す() {
        let mut sh = Shaper::new(1.0);
        let runs = [
            ("aaaa ".to_owned(), plain(16.0)),
            ("bbbb".to_owned(), bold(16.0)),
        ];

        let one = sh.measure_rich(&runs, None);
        // どちらの走りも単独なら収まる幅で、まとめると収まらない幅
        let narrow = one.w * 0.7;
        let two = sh.measure_rich(&runs, Some(narrow));

        assert!(
            two.h > one.h,
            "まとめて折り返していない。1 行 {:?} / 折り返し {:?}",
            one,
            two
        );
        assert!(two.w <= narrow.ceil(), "折り返し幅を超えている {two:?}");
    }

    /// 走りの番号が字まで届いていること。**届かないと色が塗り分けられない**
    #[test]
    fn 字はどの走りから来たかを覚えている() {
        let mut sh = Shaper::new(1.0);
        let runs = [
            ("ab".to_owned(), plain(16.0)),
            ("cd".to_owned(), bold(16.0)),
        ];
        let shaped = sh.shape_rich(&runs, None);

        let which: Vec<u32> = shaped.glyphs.iter().map(|g| g.run).collect();
        assert_eq!(which, vec![0, 0, 1, 1], "走りの番号が字に付いていない");
    }

    /// ⚠️ 折り返した走りを 1 つの矩形で塗ると、行間まで塗って読めなくなる
    #[test]
    fn 走りの矩形は行ごとに切れる() {
        let mut sh = Shaper::new(1.0);
        let runs = [("aaaa bbbb".to_owned(), plain(16.0))];
        let wide = sh.measure_rich(&runs, None).w;
        let shaped = sh.shape_rich(&runs, Some(wide * 0.6));

        assert!(
            shaped.runs.len() >= 2,
            "折り返したのに矩形が 1 つしかない {:?}",
            shaped.runs
        );
        let ys: Vec<f32> = shaped.runs.iter().map(|r| r.rect.y).collect();
        assert!(ys[0] != ys[1], "行が違うのに同じ高さにある {ys:?}");
    }

    /// 走りが 1 つのときは、飾りの無い文字列と同じ結果になること。
    /// **同じ道を通っている**ことの確認である
    #[test]
    fn 走りが一つなら素の文字列と同じ() {
        let mut sh = Shaper::new(1.0);
        let f = plain(16.0);
        let a = sh.measure("こんにちは world", &f, None);
        let b = sh.measure_rich(&[("こんにちは world".to_owned(), f)], None);
        assert_eq!(a, b);
    }

    /// ⚠️ **走りの切れ目が行に影響してはいけない。**
    ///
    /// 同じ書体で 2 つに割った走りは、割らずに 1 つで書いたものと
    /// **1 ピクセルも違わない**はずである。ここが違うということは、
    /// 走りごとに独立して置いているということで、飾りを付けた途端に
    /// 行末がずれる
    #[test]
    fn 走りの切れ目は行に影響しない() {
        let mut sh = Shaper::new(1.0);
        let f = plain(16.0);
        let whole = "the quick brown fox jumps over the lazy dog";
        let split = [
            ("the quick brown ".to_owned(), f.clone()),
            ("fox jumps over ".to_owned(), f.clone()),
            ("the lazy dog".to_owned(), f.clone()),
        ];
        for w in [40.0, 80.0, 160.0, 320.0] {
            assert_eq!(
                sh.measure(whole, &f, Some(w)),
                sh.measure_rich(&split, Some(w)),
                "折り返し幅 {w} で結果が違う"
            );
        }
    }

    /// cosmic-text の Han 統合の判定は完全一致なので、そこへ通る形に
    /// なっていること。**ここが崩れると日本語が中国語の書体で描かれる。**
    #[test]
    fn locale_is_normalised_for_han_unification() {
        assert_eq!(normalize_locale("ja-JP"), "ja");
        assert_eq!(normalize_locale("ja_JP.UTF-8"), "ja");
        assert_eq!(normalize_locale("ja"), "ja");
        assert_eq!(normalize_locale("en-US"), "en");
        assert_eq!(normalize_locale("ko-KR"), "ko");

        // 中国語だけは地域・文字体系まで見ないと繁体/簡体が決まらない
        assert_eq!(normalize_locale("zh-CN"), "zh-CN");
        assert_eq!(normalize_locale("zh-TW"), "zh-TW");
        assert_eq!(normalize_locale("zh-Hant-TW"), "zh-TW");
        assert_eq!(normalize_locale("zh-Hans-CN"), "zh-CN");
        assert_eq!(normalize_locale("zh-HK"), "zh-HK");
        assert_eq!(normalize_locale("zh-MO"), "zh-HK");
    }

    /// 日本語の利用者に中国語の字形を出さない。
    /// 逆に、中国語・韓国語の利用者から字形を横取りもしない
    #[test]
    fn han_scripts_fall_back_by_locale() {
        let f = GumicordFallback;
        assert_eq!(f.script_fallback(Script::Han, "ja"), JAPANESE_FALLBACK);
        assert_eq!(f.script_fallback(Script::Hiragana, "ja"), JAPANESE_FALLBACK);
        assert_eq!(f.script_fallback(Script::Katakana, "en"), JAPANESE_FALLBACK);

        assert_ne!(f.script_fallback(Script::Han, "zh-CN"), JAPANESE_FALLBACK);
        assert_ne!(f.script_fallback(Script::Han, "zh-TW"), JAPANESE_FALLBACK);
        assert_ne!(f.script_fallback(Script::Han, "ko"), JAPANESE_FALLBACK);

        // 漢字圏以外は横取りしない
        assert_ne!(f.script_fallback(Script::Arabic, "ja"), JAPANESE_FALLBACK);
    }

    #[test]
    fn font_defaults_come_from_the_body_style() {
        let f = ResolvedFont::from_style(&Style::default());
        assert_eq!(f.size(), DEFAULT_FONT_SIZE);
        assert_eq!(f.line_height(), DEFAULT_LINE_HEIGHT);
        assert_eq!(f.weight, 400);
        assert!(!f.italic);
    }

    /// 行の高さの指定がなければ、大きさに比例させる
    #[test]
    fn line_height_scales_with_size() {
        let f = ResolvedFont::from_font(&Font {
            size: Some(30.0),
            ..Default::default()
        });
        assert_eq!(f.size(), 30.0);
        assert_eq!(f.line_height(), 44.0, "15/22 と同じ比率");
    }

    /// 鍵に f32 を直接使うと NaN と -0.0 で壊れる。量子化して避けている
    #[test]
    fn fonts_with_equal_metrics_share_a_key() {
        let a = ResolvedFont::from_font(&Font {
            size: Some(15.0),
            line_height: Some(22.0),
            ..Default::default()
        });
        let b = ResolvedFont::from_style(&Style::default());
        assert_eq!(a, b);
    }
}

/// 1 行に収まるように末尾を「…」で詰めた文字列。
///
/// # なぜ折り返さずに切るのか
///
/// チャンネル名やサーバ名は**一覧の 1 行**である。折り返すと行の高さが
/// 揃わなくなり、一覧が読めなくなる。Discord も切って「…」を出す。
///
/// ⚠️ **切る位置は文字の境界である。** バイトで切ると、日本語も絵文字も
/// 途中で割れて化ける。整形した結果の字の位置を見て決める。
impl Shaper {
    pub fn fit_single_line(&mut self, text: &str, font: &ResolvedFont, max_w: f32) -> String {
        /// 詰めたことを示す字。**1 文字で済ませる**
        const ELLIPSIS: &str = "…";

        if text.is_empty() || !max_w.is_finite() || max_w <= 0.0 {
            return text.to_owned();
        }

        // ⚠️ 折り返さずに測る。折り返して測ると「収まっている」ことになる
        if self.measure(text, font, None).w <= max_w {
            return text.to_owned();
        }

        let room = max_w - self.measure(ELLIPSIS, font, None).w;
        if room <= 0.0 {
            return ELLIPSIS.to_owned();
        }

        // 収まる最後の字の**手前**で切る
        let limit = room * self.scale;
        let mut cut = 0;
        for g in &self.shape(text, font, None).glyphs {
            if g.left + g.advance > limit {
                break;
            }
            cut = g.end;
        }
        if cut == 0 {
            return ELLIPSIS.to_owned();
        }

        // ⚠️ 整形は書記素をまとめるので `end` は境界のはずだが、
        // **信じずに確かめる**。ここで割れると画面に化けた字が出る
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        format!("{}{ELLIPSIS}", &text[..cut])
    }
}

#[cfg(test)]
mod fit_tests {
    use super::*;

    fn shaper() -> Shaper {
        Shaper::new(1.0)
    }

    /// 収まるものはそのまま
    #[test]
    fn text_that_fits_is_untouched() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        let wide = s.measure("あい", &font, None).w + 10.0;
        assert_eq!(s.fit_single_line("あい", &font, wide), "あい");
    }

    /// はみ出したら「…」が付き、**元より短くなる**
    #[test]
    fn overflowing_text_is_cut_with_an_ellipsis() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        let long = "とてもながいチャンネルめい";
        let narrow = s.measure(long, &font, None).w * 0.4;

        let cut = s.fit_single_line(long, &font, narrow);
        assert!(cut.ends_with('…'), "「…」で終わっていない: {cut}");
        assert!(cut.chars().count() < long.chars().count());
        // ⚠️ **収まっている**こと。切ったのに溢れていては意味がない
        assert!(s.measure(&cut, &font, None).w <= narrow + 0.5);
    }

    /// ⚠️ **文字の途中で割らない。** バイトで切ると日本語も絵文字も化ける
    #[test]
    fn multibyte_text_is_never_split_mid_character() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        let text = "🍣🍣🍣🍣🍣🍣🍣🍣";

        // どの幅で切っても、正しい文字列であること
        for n in 1..40 {
            let cut = s.fit_single_line(text, &font, n as f32 * 3.0);
            assert!(cut.chars().all(|c| c == '🍣' || c == '…'), "化けた: {cut}");
        }
    }

    /// 幅が無いに等しくても落ちない
    #[test]
    fn an_impossible_width_does_not_panic() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        assert_eq!(s.fit_single_line("あ", &font, 0.0), "あ");
        assert_eq!(s.fit_single_line("あいうえお", &font, 1.0), "…");
        assert_eq!(s.fit_single_line("", &font, 100.0), "");
    }
}

/// 取ってきた画像 1 枚。**画素はアプリが用意する。**
///
/// ⚠️ レンダラは網に触らない ([`spec/02-architecture.md`])。ここへ来るのは
/// 既に取得も復号も済んだ RGBA である。
#[derive(Debug, Clone)]
pub struct ImageData {
    /// 取り出し元。**同じ URL なら同じ絵である**
    pub url: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8。長さは `width * height * 4`
    pub rgba: Vec<u8>,
}

/// URL の指紋。**アトラスの鍵にする。**
///
/// ⚠️ 文字列そのものを鍵にすると、フレームごとに複製することになる。
/// URL は 100 文字を超えることがあり、1 フレームに何十個も引く
pub fn image_key(url: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    h.finish()
}

impl TextEngine {
    /// 取ってきた画像をアトラスへ入れる。**同じ URL は 1 度だけ入る。**
    ///
    /// 入らなければ (アトラスが溢れていれば) `false`。呼び出し側は
    /// **諦めてよい** — 絵が出ないだけで、他は何も壊れない
    pub fn put_image(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        image: &ImageData,
    ) -> bool {
        let key = AtlasKey::Image(image_key(&image.url));
        if self.atlas.entries.contains_key(&key) {
            return true;
        }
        if image.rgba.len() != (image.width as usize) * (image.height as usize) * 4 {
            tracing::warn!(url = %image.url, "画素の数が大きさと合わない");
            return false;
        }

        let place = |atlas: &mut Atlas| {
            atlas.insert(
                device,
                queue,
                image.width,
                image.height,
                &image.rgba,
                Placement {
                    left: 0,
                    top: 0,
                    // 画像は色そのものを使う。文字のように色を掛けない
                    is_color: true,
                },
                Side::Image,
            )
        };

        // 入らなければ絵の側を詰め直して、1 度だけやり直す。
        //
        // ⚠️ **やり直すのは 1 度きり。** 空いた場所にも入らないなら、
        // 入りきらない大きさのものを頼まれている
        let mut entry = place(&mut self.atlas);
        if entry.is_none() && self.atlas.recycle_images() {
            entry = place(&mut self.atlas);
        }
        if entry.is_some() {
            self.atlas.images_since_recycle += 1;
        }
        self.atlas.entries.insert(key, entry);
        entry.is_some()
    }

    /// 絵を忘れたか。**1 回だけ真を返す。**
    ///
    /// 真のとき、取ってきた側は**入れ直しを頼み直す**必要がある。
    /// 頼まないと、忘れられた顔は二度と出てこない
    pub fn took_image_recycle(&mut self) -> bool {
        self.atlas.took_recycle()
    }

    /// 既にアトラスに入っている画像。**無ければ `None`** で、
    /// 呼び出し側は何も描かない
    pub fn image(&self, url: &str) -> Option<GlyphEntry> {
        self.atlas
            .entries
            .get(&AtlasKey::Image(image_key(url)))
            .copied()
            .flatten()
    }

    /// その画像を持っているか。**取りに行くかどうかの判断に使う**
    pub fn has_image(&self, url: &str) -> bool {
        self.atlas
            .entries
            .contains_key(&AtlasKey::Image(image_key(url)))
    }
}
