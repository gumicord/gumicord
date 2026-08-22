//! テキストの整形とグリフアトラス。
//!
//! ```text
//! cosmic-text で整形 (shaping / bidi / フォールバック)
//!     │
//! swash でラスタライズ
//!     │
//! RGBA8 アトラス (2048², 棚詰め)
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

use cosmic_text::{
    Attrs, Buffer, CacheKey, Family, FontSystem, Metrics, Shaping, Style as FontStyle, SwashCache,
    SwashContent, Weight, Wrap,
};
use gumicord_uitree::Style;
use gumicord_uitree::value::Font;

use crate::geom::Size;

/// アトラス 1 ページの一辺 (物理 px)
pub const ATLAS_SIZE: u32 = 2048;

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
    text: String,
    font: ResolvedFont,
    /// 折り返し幅 (物理 px を量子化)。`u32::MAX` は折り返さない
    max_w_q: u32,
    /// DPI スケール。変わると物理ピクセルでの整形結果が変わる
    scale_q: u32,
}

/// 原点 (0,0) に置いたときのグリフ 1 個。位置は**物理 px**。
#[derive(Debug, Clone, Copy)]
pub struct PlacedGlyph {
    pub cache_key: CacheKey,
    pub x: i32,
    pub y: i32,
}

/// 整形済みのテキスト。
#[derive(Debug, Clone)]
pub struct Shaped {
    /// 論理 px での大きさ
    pub size: Size,
    /// 原点 (0,0) を左上としたときのグリフ列。位置は物理 px
    pub glyphs: Vec<PlacedGlyph>,
}

/// アトラスに載ったグリフ。
#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
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
    /// 致命的であり、同梱フォント先行 + 背景列挙へ移す必要がある
    /// (ロードマップ R4)。まだやっていない。
    pub fn new(scale: f32) -> Self {
        Shaper {
            font_system: FontSystem::new(),
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
        ShapeKey {
            text: text.to_owned(),
            font: font.clone(),
            max_w_q: max_w.map(quantize).unwrap_or(u32::MAX),
            scale_q: quantize(self.scale),
        }
    }

    fn ensure(&mut self, key: &ShapeKey, text: &str, font: &ResolvedFont, max_w: Option<f32>) {
        // `entry` を使うと `text` を必ず複製することになる。
        // 当たる回のほうが圧倒的に多いので、当たりを安く済ませる
        if !self.shaped.contains_key(key) {
            let shaped = self.shape_uncached(text, font, max_w);
            self.shaped.insert(key.clone(), shaped);
        }
    }

    /// 文字列を整形する。`max_w` は論理 px、`None` なら折り返さない。
    pub fn shape(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> &Shaped {
        let key = self.key(text, font, max_w);
        self.ensure(&key, text, font, max_w);
        &self.shaped[&key]
    }

    /// 整形して大きさだけを返す。レイアウトの計測で使う。
    pub fn measure(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> Size {
        self.shape(text, font, max_w).size
    }

    fn shape_uncached(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> Shaped {
        let scale = self.scale;
        let metrics = Metrics::new(font.size() * scale, font.line_height() * scale);
        let mut buf = Buffer::new(&mut self.font_system, metrics);

        buf.set_wrap(Wrap::WordOrGlyph);
        buf.set_size(max_w.map(|w| w * scale), None);

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

        buf.set_text(text, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.font_system, false);

        let mut w = 0.0f32;
        let mut h = 0.0f32;
        let mut glyphs = Vec::new();
        for run in buf.layout_runs() {
            w = w.max(run.line_w);
            h = h.max(run.line_top + run.line_height);
            for g in run.glyphs {
                let p = g.physical((0.0, run.line_y), 1.0);
                glyphs.push(PlacedGlyph {
                    cache_key: p.cache_key,
                    x: p.x,
                    y: p.y,
                });
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
        }
    }
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

    pub fn atlas_view(&self) -> &wgpu::TextureView {
        &self.atlas.view
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
        queue: &wgpu::Queue,
        text: &str,
        font: &ResolvedFont,
        max_w: Option<f32>,
        mut f: impl FnMut(&GlyphEntry, i32, i32),
    ) -> Size {
        let key = self.shaper.key(text, font, max_w);
        self.shaper.ensure(&key, text, font, max_w);

        let Shaper {
            shaped,
            font_system,
            swash,
            ..
        } = &mut self.shaper;
        let atlas = &mut self.atlas;

        let s = &shaped[&key];
        for g in &s.glyphs {
            if let Some(e) = atlas.get(queue, font_system, swash, g.cache_key)
                && e.w != 0
            {
                f(&e, g.x, g.y);
            }
        }
        s.size
    }

    /// アトラスに載っているグリフの数。性能の目安に使う
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
struct Atlas {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    entries: HashMap<CacheKey, Option<GlyphEntry>>,
    cursor_x: u32,
    cursor_y: u32,
    shelf_h: u32,
    full: bool,
    uploaded: usize,
}

impl Atlas {
    fn new(device: &wgpu::Device) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("gumicord-glyph-atlas"),
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
        Atlas {
            texture,
            view,
            entries: HashMap::new(),
            cursor_x: 0,
            cursor_y: 0,
            shelf_h: 0,
            full: false,
            uploaded: 0,
        }
    }

    fn get(
        &mut self,
        queue: &wgpu::Queue,
        font_system: &mut FontSystem,
        swash: &mut SwashCache,
        key: CacheKey,
    ) -> Option<GlyphEntry> {
        if let Some(e) = self.entries.get(&key) {
            return *e;
        }
        let entry = self.rasterize(queue, font_system, swash, key);
        self.entries.insert(key, entry);
        entry
    }

    fn rasterize(
        &mut self,
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
                uv: [0.0; 4],
                left: p.left,
                top: p.top,
                w: 0,
                h: 0,
                is_color: false,
            });
        }
        if self.full {
            return None;
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

        // 棚詰め。1px 空けて隣のグリフのにじみを防ぐ
        const PAD: u32 = 1;
        if self.cursor_x + w + PAD > ATLAS_SIZE {
            self.cursor_x = 0;
            self.cursor_y += self.shelf_h + PAD;
            self.shelf_h = 0;
        }
        if self.cursor_y + h + PAD > ATLAS_SIZE {
            tracing::warn!("グリフアトラスが溢れた。R3 (複数ページ化) が要る");
            self.full = true;
            return None;
        }

        let (x, y) = (self.cursor_x, self.cursor_y);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            &rgba,
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

        self.cursor_x += w + PAD;
        self.shelf_h = self.shelf_h.max(h);
        self.uploaded += 1;

        let s = ATLAS_SIZE as f32;
        Some(GlyphEntry {
            uv: [
                x as f32 / s,
                y as f32 / s,
                (x + w) as f32 / s,
                (y + h) as f32 / s,
            ],
            left: p.left,
            top: p.top,
            w,
            h,
            is_color,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
