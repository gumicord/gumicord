//! Text shaping and the glyph atlas.
//!
//! ```text
//! shape (shaping, bidi, fallback)
//!     │
//! rasterise
//!     │
//! RGBA8 atlas, shelf-packed: glyphs from the top, images from the bottom
//!     │
//! textured quads
//! ```
//!
//! The atlas is RGBA8 for colour emoji: mask glyphs are stored as white with
//! alpha and tinted in the shader, while colour glyphs keep their own texels.
//!
//! Shaping is cached by string, font and wrap width, since the result cannot
//! change. It happens in physical pixels, because rasterisation does; only the
//! sizes handed back are converted to logical.

use std::collections::HashMap;

use cosmic_text::fontdb;
use cosmic_text::{
    Attrs, Buffer, CacheKey, Fallback, Family, FontSystem, Metrics, PlatformFallback, Shaping,
    Style as FontStyle, SwashCache, SwashContent, Weight, Wrap,
};
use std::sync::atomic::AtomicBool;

use gumicord_uitree::Style;
use gumicord_uitree::value::Font;
use unicode_script::Script;

use crate::geom::Size;
use crate::icon;

/// Edge length of one atlas page.
pub const ATLAS_SIZE: u32 = 2048;

/// The bundled body font.
///
/// | | |
/// |---|---|
/// Bundled so the typeface does not change per machine, so system font
/// enumeration can eventually leave the startup path, and because the default
/// sans-serif on each OS is not designed for UI.
///
/// One variable font covers every weight, since the `wght` axis is set at
/// rasterisation time.
///
/// No CJK is bundled: adding it would more than double the binary, so that is
/// a separate decision. Japanese still falls back to a system font.
const BUNDLED_SANS: &[u8] = include_bytes!("../../../assets/fonts/Inter.ttf");

/// The bundled font's family name, which sans-serif resolves to.
const BUNDLED_SANS_FAMILY: &str = "Inter";

/// Japanese fallbacks, in order.
///
/// The library's Windows table holds one entry; the UI-tuned variant goes
/// first and an older one follows. Names for the other platforms come after,
/// so the same order produces the same result everywhere.
///
/// This only picks from what the system has. Identical rendering everywhere
/// needs a bundled Japanese font.
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

/// Decides the font fallback order.
///
/// Han unification means the same code point has different shapes per
/// language. The library matches the locale exactly against five cases, and
/// the `ja-JP` Windows reports matches none of them, falling through to a
/// Simplified Chinese default — Japanese text rendered with Chinese shapes.
///
/// Normalising the locale alone fixes it, but leaves exactly one usable font,
/// so the whole list is replaced here.
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
        // Chinese and Korean readers should not get Japanese shapes, so
        // those stay with the platform's choice.
        if han_unified && !locale.starts_with("zh") && !locale.starts_with("ko") {
            return JAPANESE_FALLBACK;
        }
        PlatformFallback.script_fallback(script, locale)
    }
}

/// Normalises a locale into the form the Han unification check expects.
///
/// The check is exact, so `ja-JP` and `ja_JP.UTF-8` miss. Only Chinese needs
/// the region, so everything else is truncated to the primary tag.
fn normalize_locale(locale: &str) -> String {
    let tag = locale.replace('_', "-");
    // Forms like `ja_JP.UTF-8` also arrive.
    let tag = tag.split('.').next().unwrap_or("");
    let mut parts = tag.split('-');
    let lang = parts.next().unwrap_or("").to_ascii_lowercase();

    if lang != "zh" {
        return lang;
    }

    // Traditional or Simplified comes from the script or the region.
    for p in parts {
        match p.to_ascii_uppercase().as_str() {
            "HANT" | "TW" => return "zh-TW".to_owned(),
            "HK" | "MO" => return "zh-HK".to_owned(),
            _ => {}
        }
    }
    "zh-CN".to_owned()
}

/// Body size when the theme says nothing.
pub const DEFAULT_FONT_SIZE: f32 = 15.0;
/// Line height when the theme says nothing.
pub const DEFAULT_LINE_HEIGHT: f32 = 22.0;

/// A resolved font, with the theme's gaps filled from the defaults.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ResolvedFont {
    pub family: Option<String>,
    /// Quantised, so a float is never a map key.
    size_q: u32,
    line_height_q: u32,
    /// Extra letter spacing; zero means unset.
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
            // With no line height, scale it from the size using the default
            // ratio, or changing only the size makes lines look cramped.
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

/// The key a shaped result is cached under.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ShapeKey {
    /// The runs to shape; plain text is a list of one.
    ///
    /// Never shaped per decoration: each part would wrap independently and
    /// the line breaks would stop lining up.
    runs: Vec<(String, ResolvedFont)>,
    /// Wrap width; `u32::MAX` means no wrapping.
    max_w_q: u32,
    /// The scale factor, which changes the physical-pixel result.
    scale_q: u32,
}

/// One glyph, positioned from the origin in physical pixels.
#[derive(Debug, Clone, Copy)]
pub struct PlacedGlyph {
    pub cache_key: CacheKey,
    /// Rounded for rasterisation.
    pub x: i32,
    pub y: i32,
    /// Byte range in the source, for placing the caret and selection.
    pub start: usize,
    pub end: usize,
    /// Unrounded position and advance; the rounded `x` leaves gaps in a
    /// selection.
    pub left: f32,
    pub advance: f32,
    /// Which line it sits on.
    pub line_top: f32,
    pub line_height: f32,
    /// Which run it came from, which is what colours it.
    pub run: u32,
}

/// Shaped text.
#[derive(Debug, Clone)]
pub struct Shaped {
    /// Size in logical pixels.
    pub size: Size,
    /// Glyphs, positioned from the top left in physical pixels.
    pub glyphs: Vec<PlacedGlyph>,
    /// Rects per run, for underlines, strikethroughs and spoilers.
    ///
    /// Split per line: one rect over a wrapped run would paint the leading
    /// too and swallow the text.
    pub runs: Vec<RunRect>,
    /// Line height, which is also the caret's height in empty text.
    pub line_height: f32,
}

/// A rect over text. Carets, selections and underlines are all rects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// What one run occupies on one line.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunRect {
    pub run: u32,
    pub rect: TextRect,
}

impl Shaped {
    /// The caret rect at a byte offset: the left edge of the glyph starting
    /// there, or the right edge of the previous one at end of line.
    pub fn caret(&self, at: usize, width: f32) -> TextRect {
        // The glyph starting there.
        if let Some(g) = self.glyphs.iter().find(|g| g.start >= at) {
            return TextRect {
                x: g.left,
                y: g.line_top,
                w: width,
                h: g.line_height,
            };
        }
        // End of line: right of the last glyph.
        match self.glyphs.last() {
            Some(g) => TextRect {
                x: g.left + g.advance,
                y: g.line_top,
                w: width,
                h: g.line_height,
            },
            // Empty.
            None => TextRect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: self.line_height,
            },
        }
    }

    /// Rects covering a byte range, one per line: a single rect over a
    /// wrapped selection would paint the leading too.
    pub fn range_rects(&self, range: &core::ops::Range<usize>) -> Vec<TextRect> {
        let mut out: Vec<TextRect> = Vec::new();
        if range.is_empty() {
            return out;
        }

        for g in &self.glyphs {
            // Any glyph overlapping the range.
            if g.end <= range.start || g.start >= range.end {
                continue;
            }
            // Extend while it stays on the same line.
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

/// A glyph in the atlas.
#[derive(Debug, Clone, Copy)]
pub struct GlyphEntry {
    /// Which page it is on. Each page is its own texture, so without this
    /// everything reads from the first one.
    pub page: u32,
    /// UV within the atlas.
    pub uv: [f32; 4],
    /// Offset from the pen position.
    pub left: i32,
    pub top: i32,
    pub w: u32,
    pub h: u32,
    /// A colour glyph, which the shader must not tint.
    pub is_color: bool,
}

/// Shapes text, without a GPU.
///
/// Layout only needs how many pixels a string takes at a given width, not a
/// texture. Keeping this separate from the engine is what lets layout be
/// tested without a GPU.
pub struct Shaper {
    font_system: FontSystem,
    swash: SwashCache,
    shaped: HashMap<ShapeKey, Shaped>,
    scale: f32,
    /// Normalised once and reused when the font set is extended.
    locale: String,
    /// System fonts waiting to be folded in; `None` when they load eagerly.
    fonts_rx: Option<std::sync::mpsc::Receiver<fontdb::Database>>,
    /// Set by the background thread once fonts are waiting; read without
    /// consuming to wake a sleeping loop.
    fonts_ready: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl Shaper {
    fn locale() -> String {
        let raw = sys_locale::get_locale().unwrap_or_else(|| "en-US".to_owned());
        let locale = normalize_locale(&raw);
        tracing::debug!(%raw, %locale, "font locale");
        locale
    }

    /// A font system from a given database, with the bundled body font on top
    /// and the default family pointing at it.
    fn font_system(locale: &str, mut db: fontdb::Database) -> FontSystem {
        db.load_font_data(BUNDLED_SANS.to_vec());
        // A theme that writes no family gets sans-serif, so pointing that at
        // the bundled font means themes need say nothing.
        db.set_sans_serif_family(BUNDLED_SANS_FAMILY);
        FontSystem::new_with_locale_and_db_and_fallback(locale.to_owned(), db, GumicordFallback)
    }

    /// System fonts and the bundled font, both ready now.
    ///
    /// Enumerating system fonts measures at 360ms on a cold start, so this is
    /// for tests and non-window callers only. The window goes through
    /// [`Shaper::new_fast`] and folds fonts in later, off the startup path.
    pub fn new(scale: f32) -> Self {
        let locale = Self::locale();
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let font_system = Self::font_system(&locale, db);
        Shaper {
            font_system,
            swash: SwashCache::new(),
            shaped: HashMap::new(),
            scale,
            locale,
            fonts_rx: None,
            fonts_ready: None,
        }
    }

    /// The bundled font only, immediately, with system fonts enumerated on a
    /// background thread and folded in later.
    ///
    /// `wake` is called once the system fonts are ready, so the event loop
    /// does not sleep through the update. The window starts drawing with the
    /// bundled font alone, and Japanese (which falls back to a system font)
    /// becomes correct only after the background thread reports in.
    pub fn new_fast(
        scale: f32,
        wake: Box<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        let locale = Self::locale();
        let ready = std::sync::Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let th_ready = ready.clone();
        std::thread::spawn(move || {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            // Ready before the send is observed, so a poll right after the
            // wake always finds the fonts.
            th_ready.store(true, std::sync::atomic::Ordering::Release);
            let _ = tx.send(db);
            wake();
        });
        let font_system = Self::font_system(&locale, fontdb::Database::new());
        Shaper {
            font_system,
            swash: SwashCache::new(),
            shaped: HashMap::new(),
            scale,
            locale,
            fonts_rx: Some(rx),
            fonts_ready: Some(ready),
        }
    }

    /// Whether system fonts are waiting to be folded in. Read without
    /// consuming, so a sleeping loop can be woken to apply them.
    pub fn fonts_pending(&self) -> bool {
        self.fonts_ready
            .as_ref()
            .is_some_and(|r| r.load(std::sync::atomic::Ordering::Acquire))
    }

    /// Folds in the system fonts the background thread collected, if they have
    /// arrived, and clears everything shaped with the old font set.
    ///
    /// A true result means the glyph atlas is now stale and the caller must
    /// rebuild it.
    pub fn bring_system_fonts(&mut self) -> bool {
        let Some(rx) = &self.fonts_rx else { return false };
        let Ok(system) = rx.try_recv() else { return false };
        self.font_system = Self::font_system(&self.locale, system);
        // New font ids mean old swash and shape results are unreachable or
        // wrong; drop both so the next draw starts clean.
        self.swash = SwashCache::new();
        self.shaped.clear();
        if let Some(ready) = &self.fonts_ready {
            ready.store(false, std::sync::atomic::Ordering::Release);
        }
        true
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Discards everything shaped in physical pixels when the DPI changes.
    /// A true result means the caller must drop the glyph atlas too.
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
        // `entry` would clone the key every time; hits vastly outnumber
        // misses, so hits stay cheap.
        if !self.shaped.contains_key(key) {
            let shaped = self.shape_uncached(&key.runs, max_w);
            self.shaped.insert(key.clone(), shaped);
        }
    }

    /// Shapes a string. `None` means no wrapping.
    pub fn shape(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> &Shaped {
        let key = self.key(text, font, max_w);
        self.ensure(&key, max_w);
        &self.shaped[&key]
    }

    /// Shapes mixed decoration in one pass.
    ///
    /// Shaping each run separately and laying them side by side wraps each
    /// independently, and the line breaks stop lining up.
    pub fn shape_rich(&mut self, runs: &[(String, ResolvedFont)], max_w: Option<f32>) -> &Shaped {
        let key = self.key_rich(runs, max_w);
        self.ensure(&key, max_w);
        &self.shaped[&key]
    }

    /// Shapes and returns only the size, for layout measurement.
    pub fn measure(&mut self, text: &str, font: &ResolvedFont, max_w: Option<f32>) -> Size {
        self.shape(text, font, max_w).size
    }

    pub fn measure_rich(&mut self, runs: &[(String, ResolvedFont)], max_w: Option<f32>) -> Size {
        self.shape_rich(runs, max_w).size
    }

    /// Shapes a list of runs through one buffer.
    ///
    /// One line height for the whole thing, so per-run sizes are not possible
    /// here. Only headings change size today, and they are separate nodes.
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
            // Tagged with the run index: recovering it from byte offsets
            // afterwards breaks as soon as a run is empty.
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
                // Extend one rect while the run continues on the same line.
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
            // Shaped in physical pixels; convert back.
            //
            // Rounded up: for a content-sized node this width becomes the
            // rect, and wrapping runs against it again at draw time. One ulp
            // narrower drops the last character to the next line.
            size: Size::new((w / scale).ceil(), (h / scale).ceil()),
            glyphs,
            runs: rects,
            line_height: metrics.line_height,
        }
    }
}

/// Converts a font into the shaping library's terms.
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
        // Themes write logical pixels; the library takes em.
        attrs = attrs.letter_spacing(dequantize(font.letter_spacing_q) / font.size());
    }
    attrs
}

/// A shaper plus the GPU glyph atlas.
pub struct TextEngine {
    shaper: Shaper,
    atlas: Atlas,
}

impl TextEngine {
    pub fn new(
        device: &wgpu::Device,
        scale: f32,
        wake: Box<dyn Fn() + Send + Sync + 'static>,
    ) -> Self {
        TextEngine {
            shaper: Shaper::new_fast(scale, wake),
            atlas: Atlas::new(device),
        }
    }

    /// Whether system fonts are waiting, without consuming them.
    pub fn fonts_pending(&self) -> bool {
        self.shaper.fonts_pending()
    }

    /// Folds in system fonts and rebuilds the atlas; true means the layout
    /// has changed and the caller must rebind the atlas.
    pub fn system_fonts(&mut self, device: &wgpu::Device) -> bool {
        if self.shaper.bring_system_fonts() {
            self.atlas = Atlas::new(device);
            // The old atlas's image entries are gone with it, so the fetcher
            // must be told to re-read them or they never come back. The flag
            // rides the same `took_recycle` path as an eviction for that.
            self.atlas.recycled = true;
            true
        } else {
            false
        }
    }

    /// For callers that only need shaping.
    pub fn shaper(&mut self) -> &mut Shaper {
        &mut self.shaper
    }

    /// One texture per page.
    pub fn atlas_views(&self) -> Vec<&wgpu::TextureView> {
        self.atlas.pages.iter().map(|p| &p.view).collect()
    }

    /// Whether a page was added; true once.
    pub fn took_atlas_growth(&mut self) -> bool {
        self.atlas.took_growth()
    }

    /// Rebuilds shaping and glyphs when the DPI changes.
    pub fn set_scale(&mut self, device: &wgpu::Device, scale: f32) {
        if self.shaper.set_scale(scale) {
            self.atlas = Atlas::new(device);
        }
    }

    /// Shapes, uploads to the atlas, and hands over each glyph.
    ///
    /// Borrowing the shaped result while appending to the atlas conflicts, so
    /// `self` is destructured field by field.
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

    /// The same, for text with mixed decoration. Also returns per-run rects,
    /// which paint underlines, strikethroughs and spoilers, and the run index,
    /// since each run has its own colour.
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
        // Cloned for the borrow. There are as many runs as decorations, not
        // as characters.
        s.runs.clone()
    }

    /// Uploads an icon and returns its place. An unknown name gives `None`,
    /// which is not an error.
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

    /// How much is in the atlas, as a rough performance signal.
    pub fn glyph_count(&self) -> usize {
        self.atlas.uploaded
    }
}

// ─────────────────────────────────────────────────────────────── Atlas

/// A shelf-packed glyph atlas.
/// What an atlas entry is keyed by.
///
/// Glyphs and icons share a texture: separating them would add pipeline
/// switches and shorten the batches that can preserve draw order, and both
/// are RGBA8 masks drawn as textured quads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum AtlasKey {
    Glyph(CacheKey),
    /// An icon name and the size it is drawn at.
    Icon(&'static str, u32),
    /// A fetched image, keyed by a hash of its URL.
    Image(u64),
}

/// Everything about an entry except its pixels.
#[derive(Debug, Clone, Copy)]
struct Placement {
    /// Offset from the pen position.
    left: i32,
    top: i32,
    /// A colour glyph.
    is_color: bool,
}

impl Placement {
    /// Icons are square and have no pen offset.
    const ICON: Placement = Placement {
        left: 0,
        top: 0,
        is_color: false,
    };
}

/// Which side of the atlas an entry packs into.
///
/// A shelf is as thick as its tallest occupant, so one avatar landing on a
/// shelf of small glyphs wastes the rest of that row. A few of those fill the
/// page and no glyph fits again — Japanese text came out with holes in it.
///
/// Glyphs pack downwards from the top and images upwards from the bottom, so
/// they never share a shelf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Text: small and numerous.
    Glyph,
    /// Images: large and few.
    Image,
}

/// The shelf state alone, with no GPU, so it can be tested directly.
#[derive(Debug, Default)]
struct Shelves {
    /// Text shelves, growing downwards.
    cursor_x: u32,
    cursor_y: u32,
    shelf_h: u32,
    /// Image shelves, growing upwards.
    image_x: u32,
    image_top: u32,
    image_shelf_h: u32,
    /// Per side: giving up on text because images filled up would put holes
    /// in the body.
    glyphs_full: bool,
    images_full: bool,
}

/// One atlas page: a texture and how full it is.
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

/// How many pages to allow. Each is 16MB of resident memory, so four come to
/// 64MB — still under the official Electron client. Reaching this takes a lot
/// of Japanese, emoji and avatars; whether it is enough is for measurement.
const MAX_PAGES: usize = 4;

struct Atlas {
    /// The last page is the one being packed into. Earlier pages are never
    /// revisited; shelf packing leaves little worth going back for.
    pages: Vec<Page>,
    entries: HashMap<AtlasKey, Option<GlyphEntry>>,
    uploaded: usize,
    /// Entries added since the last rebuild, to detect spinning.
    images_since_recycle: usize,
    /// Images were rebuilt; the caller collects this.
    recycled: bool,
    /// A page was added, so batches need rebinding.
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

    /// Whether a page was added; true once.
    fn took_growth(&mut self) -> bool {
        std::mem::take(&mut self.grew)
    }

    /// Reserves space, adding a page if needed. Once no more pages can be
    /// added it gives up, and only then do holes appear.
    fn alloc(&mut self, device: &wgpu::Device, w: u32, h: u32, side: Side) -> Option<(u32, u32)> {
        let last = self.pages.len() - 1;
        if let Some(at) = self.pages[last].shelves.alloc(w, h, side) {
            return Some(at);
        }
        if self.pages.len() >= MAX_PAGES {
            tracing::warn!(
                pages = MAX_PAGES,
                "the atlas is full; nothing more can be drawn"
            );
            return None;
        }
        tracing::info!(pages = self.pages.len() + 1, "adding an atlas page");
        self.pages.push(Page::new(device, self.pages.len()));
        self.grew = true;
        let last = self.pages.len() - 1;
        self.pages[last].shelves.alloc(w, h, side)
    }

    /// Clears the image side so it can be repacked.
    ///
    /// Shelf packing cannot free one entry in the middle, so everything on
    /// that side is forgotten at once. The fetcher still holds them, and they
    /// go back next frame; avatars vanish for one frame.
    ///
    /// The text side is untouched: holes in the body are far worse than a
    /// missing avatar.
    ///
    /// Not rebuilt while few entries have been added: something too large to
    /// fit will not fit after a rebuild either, and it would spin.
    fn recycle_images(&mut self) -> bool {
        /// Overflowing after this many is worth repacking.
        const WORTH_IT: usize = 16;

        if self.images_since_recycle < WORTH_IT {
            return false;
        }
        tracing::info!(
            images = self.images_since_recycle,
            "repacking the atlas images"
        );
        self.entries.retain(|k, _| !matches!(k, AtlasKey::Image(_)));
        for p in &mut self.pages {
            p.shelves.reset_images();
        }
        self.images_since_recycle = 0;
        self.recycled = true;
        true
    }

    /// Whether images were forgotten; true once.
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
            // Starting full-width makes the first entry create the shelf.
            image_x: ATLAS_SIZE,
            image_top: ATLAS_SIZE,
            image_shelf_h: 0,
            glyphs_full: false,
            images_full: false,
        }
    }

    /// Empties the image shelves, leaving the text ones alone.
    fn reset_images(&mut self) {
        self.image_x = ATLAS_SIZE;
        self.image_top = ATLAS_SIZE;
        self.image_shelf_h = 0;
        self.images_full = false;
    }

    /// Reserves one slot.
    fn alloc(&mut self, w: u32, h: u32, side: Side) -> Option<(u32, u32)> {
        /// A pixel of padding, so neighbours do not bleed.
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
                // Never crosses into the image side.
                if self.cursor_y + h + PAD > self.image_top {
                    tracing::debug!(
                        y = self.cursor_y,
                        "this page's text side is full; moving on"
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
                // Too wide or too tall for this shelf; start a new one.
                if self.image_x + w + PAD > ATLAS_SIZE || h > self.image_shelf_h {
                    let need = h + PAD;
                    if self.image_top < need {
                        self.images_full = true;
                        return None;
                    }
                    let top = self.image_top - need;
                    // Never crosses into the text side.
                    if top < self.cursor_y + self.shelf_h + PAD {
                        tracing::debug!(
                            top = self.image_top,
                            "this page's image side is full; moving on"
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

    /// Uploads an icon.
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
        // Square, with no pen offset.
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
            // A space: nothing to draw, but the position still matters.
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
                // A white mask; the shader tints it.
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
    /// Packs one RGBA8 image. Glyphs and pictures both come through here.
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

    /// An image must not thicken a text shelf: one avatar on a shelf of small
    /// glyphs wastes the rest of the row, and a few of those filled the page
    /// until no glyph fit. Japanese text came out with holes in it.
    #[test]
    fn a_picture_does_not_thicken_the_glyph_shelf() {
        let mut s = Shelves::new();

        // One glyph, then a large image.
        s.alloc(20, 20, Side::Glyph).expect("should fit");
        let before = s.shelf_h;
        s.alloc(128, 128, Side::Image).expect("should fit");

        assert_eq!(s.shelf_h, before, "the text shelf grew thicker");
    }

    /// Glyphs from the top, images from the bottom, never crossing.
    #[test]
    fn glyphs_grow_down_and_pictures_grow_up() {
        let mut s = Shelves::new();

        let (_, gy) = s.alloc(20, 20, Side::Glyph).expect("should fit");
        let (_, iy) = s.alloc(128, 128, Side::Image).expect("should fit");

        assert_eq!(gy, 0, "glyphs should start at the top");
        assert!(iy > gy, "images should be lower");
        assert!(iy + 128 <= ATLAS_SIZE);
    }

    /// They share a shelf until the width runs out.
    #[test]
    fn pictures_share_a_shelf_until_the_width_runs_out() {
        let mut s = Shelves::new();

        let (_, first) = s.alloc(128, 128, Side::Image).expect("should fit");
        let (x, same) = s.alloc(128, 128, Side::Image).expect("should fit");
        assert_eq!(same, first, "should share a shelf");
        assert!(x > 0, "should sit side by side");

        // Exhaust the width.
        for _ in 0..20 {
            s.alloc(128, 128, Side::Image);
        }
        let (_, next) = s.alloc(128, 128, Side::Image).expect("should fit");
        assert!(next < first, "the next shelf should be higher");
    }

    /// Glyphs still fit once images have filled up; giving up would put holes
    /// in the body.
    #[test]
    fn a_full_picture_side_does_not_stop_the_glyphs() {
        let mut s = Shelves::new();

        // Fill up with images.
        while s.alloc(256, 256, Side::Image).is_some() {}
        assert!(s.images_full);

        assert!(
            s.alloc(20, 20, Side::Glyph).is_some(),
            "glyphs should still fit"
        );
        assert!(!s.glyphs_full);
    }

    /// Repacking makes room again; shelf packing cannot free one entry.
    #[test]
    fn resetting_the_picture_side_makes_room_again() {
        let mut s = Shelves::new();
        while s.alloc(256, 256, Side::Image).is_some() {}
        assert!(s.images_full);

        s.reset_images();
        assert!(!s.images_full);
        assert!(s.alloc(256, 256, Side::Image).is_some());
    }

    /// A repack leaves the text side alone.
    #[test]
    fn resetting_the_picture_side_leaves_the_glyphs_alone() {
        let mut s = Shelves::new();
        s.alloc(20, 20, Side::Glyph).expect("should fit");
        let (before_x, before_y) = (s.cursor_x, s.cursor_y);

        s.alloc(128, 128, Side::Image).expect("should fit");
        s.reset_images();

        assert_eq!((s.cursor_x, s.cursor_y), (before_x, before_y));
        assert!(!s.glyphs_full);
    }

    /// Packing from both ends never hands out the same space twice.
    #[test]
    fn the_two_sides_never_overlap() {
        let mut s = Shelves::new();

        let mut lowest_glyph = 0;
        while let Some((_, y)) = s.alloc(64, 64, Side::Glyph) {
            lowest_glyph = lowest_glyph.max(y + 64);
        }
        // Once glyphs have filled it, no image fits.
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

    /// Shaping per run and placing them side by side wraps each
    /// independently. This gives a width where the combined text needs two
    /// lines but each run alone fits on one.
    #[test]
    fn mixed_decoration_wraps_as_one_run_of_text() {
        let mut sh = Shaper::new(1.0);
        let runs = [
            ("aaaa ".to_owned(), plain(16.0)),
            ("bbbb".to_owned(), bold(16.0)),
        ];

        let one = sh.measure_rich(&runs, None);
        // Wide enough for either run alone, not for both.
        let narrow = one.w * 0.7;
        let two = sh.measure_rich(&runs, Some(narrow));

        assert!(
            two.h > one.h,
            "not wrapped as one run: single {:?}, wrapped {:?}",
            one,
            two
        );
        assert!(two.w <= narrow.ceil(), "exceeded the wrap width {two:?}");
    }

    /// The em space a custom emoji's run is shaped as must carry real
    /// advance: a font without it would collapse the run to nothing and the
    /// picture would have nowhere to sit.
    #[test]
    fn an_em_space_holds_a_square_of_advance() {
        let mut sh = Shaper::new(1.0);
        let runs = [("\u{2003}".to_owned(), plain(16.0))];

        let shaped = sh.shape_rich(&runs, None);
        assert!(
            shaped.runs.len() == 1 && shaped.runs[0].rect.w >= 16.0,
            "the em space has no advance: {shaped:?}"
        );
    }

    /// The run index reaches the glyphs, which is what colours them.
    #[test]
    fn each_glyph_remembers_its_span() {
        let mut sh = Shaper::new(1.0);
        let runs = [
            ("ab".to_owned(), plain(16.0)),
            ("cd".to_owned(), bold(16.0)),
        ];
        let shaped = sh.shape_rich(&runs, None);

        let which: Vec<u32> = shaped.glyphs.iter().map(|g| g.run).collect();
        assert_eq!(
            which,
            vec![0, 0, 1, 1],
            "the run index did not reach the glyphs"
        );
    }

    /// One rect over a wrapped run would paint the leading too.
    #[test]
    fn a_span_rect_is_split_per_line() {
        let mut sh = Shaper::new(1.0);
        let runs = [("aaaa bbbb".to_owned(), plain(16.0))];
        let wide = sh.measure_rich(&runs, None).w;
        let shaped = sh.shape_rich(&runs, Some(wide * 0.6));

        assert!(
            shaped.runs.len() >= 2,
            "only one rect for wrapped text {:?}",
            shaped.runs
        );
        let ys: Vec<f32> = shaped.runs.iter().map(|r| r.rect.y).collect();
        assert!(ys[0] != ys[1], "different lines at the same height {ys:?}");
    }

    /// A single run matches plain text, confirming both take the same path.
    #[test]
    fn a_single_span_matches_plain_text() {
        let mut sh = Shaper::new(1.0);
        let f = plain(16.0);
        let a = sh.measure("こんにちは world", &f, None);
        let b = sh.measure_rich(&[("こんにちは world".to_owned(), f)], None);
        assert_eq!(a, b);
    }

    /// Splitting a run in the same font must not change anything by a pixel;
    /// if it does, runs are being placed independently and decoration would
    /// shift the line breaks.
    #[test]
    fn span_boundaries_do_not_affect_line_breaking() {
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
                "results differ at wrap width {w}"
            );
        }
    }

    /// The Han unification check is exact; getting this wrong renders
    /// Japanese with Chinese shapes.
    #[test]
    fn locale_is_normalised_for_han_unification() {
        assert_eq!(normalize_locale("ja-JP"), "ja");
        assert_eq!(normalize_locale("ja_JP.UTF-8"), "ja");
        assert_eq!(normalize_locale("ja"), "ja");
        assert_eq!(normalize_locale("en-US"), "en");
        assert_eq!(normalize_locale("ko-KR"), "ko");

        // Only Chinese needs the region or script to pick a variant.
        assert_eq!(normalize_locale("zh-CN"), "zh-CN");
        assert_eq!(normalize_locale("zh-TW"), "zh-TW");
        assert_eq!(normalize_locale("zh-Hant-TW"), "zh-TW");
        assert_eq!(normalize_locale("zh-Hans-CN"), "zh-CN");
        assert_eq!(normalize_locale("zh-HK"), "zh-HK");
        assert_eq!(normalize_locale("zh-MO"), "zh-HK");
    }

    /// Japanese readers do not get Chinese shapes, and Chinese and Korean
    /// readers keep theirs.
    #[test]
    fn han_scripts_fall_back_by_locale() {
        let f = GumicordFallback;
        assert_eq!(f.script_fallback(Script::Han, "ja"), JAPANESE_FALLBACK);
        assert_eq!(f.script_fallback(Script::Hiragana, "ja"), JAPANESE_FALLBACK);
        assert_eq!(f.script_fallback(Script::Katakana, "en"), JAPANESE_FALLBACK);

        assert_ne!(f.script_fallback(Script::Han, "zh-CN"), JAPANESE_FALLBACK);
        assert_ne!(f.script_fallback(Script::Han, "zh-TW"), JAPANESE_FALLBACK);
        assert_ne!(f.script_fallback(Script::Han, "ko"), JAPANESE_FALLBACK);

        // Outside CJK nothing is taken over.
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

    /// With no line height, it scales from the size.
    #[test]
    fn line_height_scales_with_size() {
        let f = ResolvedFont::from_font(&Font {
            size: Some(30.0),
            ..Default::default()
        });
        assert_eq!(f.size(), 30.0);
        assert_eq!(f.line_height(), 44.0, "should keep the default ratio");
    }

    /// An `f32` key breaks on NaN and negative zero; quantising avoids it.
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

    /// The lazy constructor draws from the bundled font first and folds system
    /// fonts in once the background thread reports, waking the loop. Exercises
    /// the thread, the channel and the rebuild without a GPU.
    #[test]
    fn lazy_fonts_arrive_from_the_background_thread() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        let woke = Arc::new(AtomicBool::new(false));
        let th_woke = woke.clone();
        let wake = Box::new(move || {
            th_woke.store(true, Ordering::Release);
        }) as Box<dyn Fn() + Send + Sync + 'static>;

        let mut sh = Shaper::new_fast(1.0, wake);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);

        // Enumerating system fonts can take seconds without a parity of speed,
        // so wait, bounded, rather than assume it has happened.
        while !sh.fonts_pending() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            sh.fonts_pending(),
            "the background thread never reported the fonts"
        );

        assert!(sh.bring_system_fonts(), "fonts should fold in once");
        assert!(
            !sh.fonts_pending(),
            "pending stays cleared after folding in"
        );
        assert!(
            !sh.bring_system_fonts(),
            "nothing left to fold in after the first"
        );

        while !woke.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(woke.load(Ordering::Acquire), "the wake never fired");

        // Shaping still works on the rebuilt font system.
        let f = plain(16.0);
        assert!(sh.measure("hello", &f, None).w > 0.0);
    }
}

/// Truncates a string to one line with an ellipsis.
///
/// Channel and guild names are single rows; wrapping them makes the row
/// heights uneven and the list unreadable.
///
/// Cut at a character boundary, decided from the shaped positions: cutting by
/// byte breaks multi-byte text and emoji.
impl Shaper {
    pub fn fit_single_line(&mut self, text: &str, font: &ResolvedFont, max_w: f32) -> String {
        /// One character, marking the cut.
        const ELLIPSIS: &str = "…";

        if text.is_empty() || !max_w.is_finite() || max_w <= 0.0 {
            return text.to_owned();
        }

        // Measured unwrapped; wrapped, everything "fits".
        if self.measure(text, font, None).w <= max_w {
            return text.to_owned();
        }

        let room = max_w - self.measure(ELLIPSIS, font, None).w;
        if room <= 0.0 {
            return ELLIPSIS.to_owned();
        }

        // Cut before the last glyph that fits.
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

        // Shaping groups graphemes, so `end` should be a boundary; checked
        // anyway, because breaking here puts mojibake on screen.
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

    /// What fits passes through.
    #[test]
    fn text_that_fits_is_untouched() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        let wide = s.measure("あい", &font, None).w + 10.0;
        assert_eq!(s.fit_single_line("あい", &font, wide), "あい");
    }

    /// What does not gets an ellipsis and comes back shorter.
    #[test]
    fn overflowing_text_is_cut_with_an_ellipsis() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        let long = "とてもながいチャンネルめい";
        let narrow = s.measure(long, &font, None).w * 0.4;

        let cut = s.fit_single_line(long, &font, narrow);
        assert!(cut.ends_with('…'), "does not end with an ellipsis: {cut}");
        assert!(cut.chars().count() < long.chars().count());
        // And actually fits; truncating that still overflows is pointless.
        assert!(s.measure(&cut, &font, None).w <= narrow + 0.5);
    }

    /// Never cut mid-character.
    #[test]
    fn multibyte_text_is_never_split_mid_character() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        let text = "🍣🍣🍣🍣🍣🍣🍣🍣";

        // Valid at every width.
        for n in 1..40 {
            let cut = s.fit_single_line(text, &font, n as f32 * 3.0);
            assert!(
                cut.chars().all(|c| c == '🍣' || c == '…'),
                "mojibake: {cut}"
            );
        }
    }

    /// Survives a width of effectively zero.
    #[test]
    fn an_impossible_width_does_not_panic() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        assert_eq!(s.fit_single_line("あ", &font, 0.0), "あ");
        assert_eq!(s.fit_single_line("あいうえお", &font, 1.0), "…");
        assert_eq!(s.fit_single_line("", &font, 100.0), "");
    }
}

/// One fetched image. The renderer never touches the network, so this
/// arrives already fetched and decoded.
#[derive(Debug, Clone)]
pub struct ImageData {
    /// Where it came from; the same URL is the same image.
    pub url: String,
    pub width: u32,
    pub height: u32,
    /// RGBA8, `width * height * 4` long.
    pub rgba: Vec<u8>,
}

/// A hash of a URL, used as the atlas key: the string itself would be cloned
/// dozens of times per frame, and URLs run past a hundred characters.
pub fn image_key(url: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut h);
    h.finish()
}

impl TextEngine {
    /// Puts a fetched image in the atlas, once per URL. False when it does not
    /// fit, which the caller may simply drop: the image is missing and nothing
    /// else breaks.
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
                    // Images keep their own colour; no tinting.
                    is_color: true,
                },
                Side::Image,
            )
        };

        // On failure, repack the image side and retry once.
        //
        // Once only: something that does not fit in an empty side is simply
        // too large.
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

    /// Whether images were forgotten; true once. The fetcher must re-add
    /// them, or they never come back.
    pub fn took_image_recycle(&mut self) -> bool {
        self.atlas.took_recycle()
    }

    /// An image already in the atlas; `None` means draw nothing.
    pub fn image(&self, url: &str) -> Option<GlyphEntry> {
        self.atlas
            .entries
            .get(&AtlasKey::Image(image_key(url)))
            .copied()
            .flatten()
    }

    /// Whether the image is held, which decides whether to fetch it.
    pub fn has_image(&self, url: &str) -> bool {
        self.atlas
            .entries
            .contains_key(&AtlasKey::Image(image_key(url)))
    }
}
