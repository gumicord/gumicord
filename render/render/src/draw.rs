//! Builds draw commands from the layout.
//!
//! The only place logical pixels become physical ones.
//!
//! There is no depth buffer: the tree's depth-first pre-order is the draw
//! order, and alpha blending cannot be correct in any other one.
//!
//! A node's background is up to three draws:
//!
//! ```text
//! [1] color   rounded rect
//! [2] image   textured quad, from the atlas like avatars
//! [3] tint    rounded rect
//! ```
//!
//! A border is a slightly larger rect under the colour, or four thin rects
//! where there is no background colour to sit under.

use gumicord_uitree::value::{Background, Color, Font};
use gumicord_uitree::{Content, Span, State, Style};

use crate::geom::Rect;
use crate::intrinsic::{Axis, intrinsic};
use crate::layout::LayoutResult;
use crate::text::{GlyphEntry, ResolvedFont, TextEngine};

/// Floats per rounded rect.
pub const FLOATS_PER_RECT: usize = 12;
/// Floats per glyph.
pub const FLOATS_PER_GLYPH: usize = 16;

/// Placeholder opacity.
const PLACEHOLDER_ALPHA: f32 = 0.45;
/// Selection opacity, low enough to read through.
const SELECTION_ALPHA: f32 = 0.30;
/// Caret width.
const CARET_WIDTH: f32 = 2.0;
/// Preedit underline thickness.
const UNDERLINE_THICKNESS: f32 = 2.0;

/// Text colour when the theme sets none. Slightly off white, so starting
/// without a theme is not painful to look at.
/// Corner radius of a spoiler fill.
const SPOILER_RADIUS: f32 = 3.0;
/// Underline and strikethrough thickness.
const LINE_WIDTH: f32 = 1.0;
/// Where the underline sits, as a fraction of the line height.
const UNDERLINE_AT: f32 = 0.82;
/// Where the strikethrough sits.
const STRIKE_AT: f32 = 0.55;

const FALLBACK_TEXT: Color = Color {
    r: 0xea,
    g: 0xea,
    b: 0xf0,
    a: 0xff,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Rect,
    Glyph,
}

/// A run of draws sharing a pipeline and a clip.
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub kind: RunKind,
    /// Which atlas page to read; unused for rects.
    pub page: u32,
    /// The instance range.
    pub first: u32,
    pub count: u32,
    /// The scissor rect; `None` is the whole screen.
    pub scissor: Option<[u32; 4]>,
}

/// One frame's draw commands.
#[derive(Debug, Default)]
pub struct DrawList {
    pub rects: Vec<f32>,
    pub glyphs: Vec<f32>,
    pub runs: Vec<Run>,
    /// Images that were about to draw and were missing.
    ///
    /// Only what survived clipping reaches here, so a 300-row list asks for
    /// the dozen or so actually visible.
    pub missing_images: Vec<String>,
    /// Theme background images that were about to draw and were missing.
    /// Kept apart from `missing_images`: those go to the CDN fetcher, these
    /// to the theme asset resolver.
    pub missing_backgrounds: Vec<String>,
}

impl DrawList {
    pub fn rect_count(&self) -> u32 {
        (self.rects.len() / FLOATS_PER_RECT) as u32
    }

    pub fn glyph_count(&self) -> u32 {
        (self.glyphs.len() / FLOATS_PER_GLYPH) as u32
    }

    /// Adds a rounded rect; a non-zero border draws only the ring.
    fn push_rect(
        &mut self,
        r: [f32; 4],
        color: [f32; 4],
        radius: f32,
        border: f32,
        scissor: Option<[u32; 4]>,
    ) {
        if r[2] <= 0.0 || r[3] <= 0.0 || color[3] <= 0.0 {
            return;
        }
        let first = self.rect_count();
        self.rects.extend_from_slice(&[
            r[0], r[1], r[2], r[3], color[0], color[1], color[2], color[3], radius, border, 0.0,
            0.0,
        ]);
        self.extend_run(RunKind::Rect, first, scissor, 0);
    }

    /// Adds a textured quad, rounded when `radius` is non-zero. This is what
    /// makes avatars round; a scissor rect cannot.
    #[allow(clippy::too_many_arguments)]
    fn push_glyph(
        &mut self,
        r: [f32; 4],
        uv: [f32; 4],
        color: [f32; 4],
        is_color: bool,
        radius: f32,
        scissor: Option<[u32; 4]>,
        page: u32,
    ) {
        let first = self.glyph_count();
        self.glyphs.extend_from_slice(&[
            r[0],
            r[1],
            r[2],
            r[3],
            uv[0],
            uv[1],
            uv[2],
            uv[3],
            color[0],
            color[1],
            color[2],
            color[3],
            if is_color { 1.0 } else { 0.0 },
            radius,
            0.0,
            0.0,
        ]);
        self.extend_run(RunKind::Glyph, first, scissor, page);
    }

    /// Extends the previous run when it matches, otherwise starts one.
    /// A different page starts a new run: one draw binds one texture, and
    /// glyphs from another page would read from the wrong one.
    fn extend_run(&mut self, kind: RunKind, first: u32, scissor: Option<[u32; 4]>, page: u32) {
        if let Some(last) = self.runs.last_mut()
            && last.kind == kind
            && last.scissor == scissor
            && last.page == page
        {
            last.count += 1;
            return;
        }
        self.runs.push(Run {
            kind,
            first,
            count: 1,
            scissor,
            page,
        });
    }
}

/// Converts one sRGB component to linear.
///
/// The surface is sRGB, so the shader must output linear. Identical rendering
/// across platforms depends on doing this the same way everywhere.
fn srgb_to_linear(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

fn linear(c: Color, opacity: f32) -> [f32; 4] {
    [
        srgb_to_linear(c.r),
        srgb_to_linear(c.g),
        srgb_to_linear(c.b),
        (c.a as f32 / 255.0) * opacity,
    ]
}

/// Snaps a logical rect to the physical pixel grid.
///
/// Without it a one-pixel divider blurs across two. Both edges are rounded
/// rather than the width, so adjacent rects leave no gap.
fn snap(r: Rect, scale: f32) -> [f32; 4] {
    let x0 = (r.x * scale).round();
    let y0 = (r.y * scale).round();
    let x1 = ((r.x + r.w) * scale).round();
    let y1 = ((r.y + r.h) * scale).round();
    [x0, y0, x1 - x0, y1 - y0]
}

fn scissor_of(clip: Option<Rect>, scale: f32, viewport: (u32, u32)) -> Option<[u32; 4]> {
    let c = clip?;
    let r = snap(c, scale);
    let x = r[0].max(0.0) as u32;
    let y = r[1].max(0.0) as u32;
    let w = (r[2].max(0.0) as u32).min(viewport.0.saturating_sub(x));
    let h = (r[3].max(0.0) as u32).min(viewport.1.saturating_sub(y));
    Some([x, y, w, h])
}

/// Turns a layout into draw commands.
#[allow(clippy::too_many_arguments)]
pub fn build(
    layout: &LayoutResult<'_>,
    text: &mut TextEngine,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scale: f32,
    viewport: (u32, u32),
    // Whether the caret is lit; the blink is timed by the platform layer.
    caret_visible: bool,
    // Which theme background images belong to; without it none draw.
    theme_namespace: Option<&str>,
) -> DrawList {
    let mut dl = DrawList::default();

    for placed in &layout.placed {
        let node = placed.node;
        let style = &node.style;
        let scissor = scissor_of(placed.clip, scale, viewport);

        // Clipped-away nodes are skipped. Not a substitute for
        // virtualisation, but it cuts wasted uploads.
        if let Some(c) = placed.clip
            && c.intersect(placed.rect).is_empty()
        {
            continue;
        }

        // Real opacity composites a node's own layer; this only scales the
        // node's own alpha and does not reach its children. Fixing it needs
        // the same machinery as layered clipping.
        let opacity = style.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
        let radius = style.radius.unwrap_or(0.0);
        let rect = snap(placed.rect, scale);
        let radius_px = radius * scale;

        draw_background(
            &mut dl,
            text,
            theme_namespace,
            style,
            rect,
            radius_px,
            opacity,
            scale,
            scissor,
        );

        match &node.content {
            Content::Text(s) if !s.is_empty() => {
                draw_text(
                    &mut dl, text, device, queue, placed, s, opacity, scale, scissor,
                );
            }
            Content::Rich(spans) if !spans.is_empty() => {
                draw_rich(
                    &mut dl, text, device, queue, placed, spans, opacity, scale, scissor,
                );
            }
            Content::Icon(name) => {
                draw_icon(
                    &mut dl, text, device, queue, placed, name, opacity, scale, scissor,
                );
            }
            Content::Editable(e) => {
                draw_editable(
                    &mut dl,
                    text,
                    device,
                    queue,
                    placed,
                    e,
                    opacity,
                    scale,
                    scissor,
                    caret_visible,
                );
            }
            Content::Image(url) => draw_image(
                &mut dl, text, placed, url, opacity, scale, radius_px, scissor,
            ),
            Content::Qr(data) => draw_qr(&mut dl, placed, data, opacity, scale, scissor),
            _ => {}
        }
    }

    dl
}

/// Draws one icon, snapped to whole pixels: they are rasterised at an exact
/// physical size, and a fractional position blurs the outline.
#[allow(clippy::too_many_arguments)]
fn draw_icon(
    dl: &mut DrawList,
    text: &mut TextEngine,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    name: &str,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let style = &placed.node.style;
    let inner = placed.inner;

    // Sized from the text, shrunk to fit the container.
    let logical = ResolvedFont::from_style(style)
        .size()
        .min(inner.w)
        .min(inner.h);
    let size = (logical * scale).round().max(1.0);

    let Some(e) = text.icon(device, queue, name, size as u32) else {
        // Unknown name; draw nothing.
        return;
    };

    let box_px = snap(inner, scale);
    let x = box_px[0] + ((box_px[2] - size) * 0.5).round();
    let y = box_px[1] + ((box_px[3] - size) * 0.5).round();

    let color = linear(style.color.unwrap_or(FALLBACK_TEXT), opacity);
    dl.push_glyph(
        [x, y, size, size],
        e.uv,
        color,
        e.is_color,
        0.0,
        scissor,
        e.page,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_background(
    dl: &mut DrawList,
    text: &TextEngine,
    namespace: Option<&str>,
    style: &Style,
    rect: [f32; 4],
    radius_px: f32,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let bg_color = style.background.as_ref().and_then(|b| b.color);
    let border = style
        .border_color
        .zip(style.border_width)
        .filter(|(c, w)| *w > 0.0 && c.a > 0);

    // [1] color
    if let Some(bg) = bg_color {
        dl.push_rect(rect, linear(bg, opacity), radius_px, 0.0, scissor);
    }

    // [2] image, drawn over the colour so a missing one degrades to it.
    if let (Some(ns), Some(bg)) = (
        namespace,
        style.background.as_ref().filter(|b| b.image.is_some()),
    ) && let Some(image) = bg.image.as_ref()
    {
        let key = image.cache_key(ns);
        if let Some(e) = text.image(&key) {
            draw_background_image(dl, &e, bg, rect, radius_px, opacity, scale, scissor);
        } else {
            dl.missing_backgrounds.push(key);
        }
    }

    // [3] tint
    if let Some(tint) = style.background.as_ref().and_then(|b| b.tint) {
        dl.push_rect(rect, linear(tint, opacity), radius_px, 0.0, scissor);
    }

    // The border draws over the background as a ring, so a translucent
    // background does not show through it.
    if let Some((bc, bw)) = border {
        dl.push_rect(rect, linear(bc, opacity), radius_px, bw * scale, scissor);
    }
}

/// Draws one theme background image over its colour.
///
/// The pixels arrive through the same atlas as avatars; what differs is the
/// key. Until they do, the colour underneath is the documented fallback.
#[allow(clippy::too_many_arguments)]
fn draw_background_image(
    dl: &mut DrawList,
    e: &GlyphEntry,
    bg: &Background,
    rect: [f32; 4],
    radius_px: f32,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    use gumicord_uitree::value::Fit;

    let box_px = snap(
        Rect {
            x: rect[0] / scale,
            y: rect[1] / scale,
            w: rect[2] / scale,
            h: rect[3] / scale,
        },
        scale,
    );
    if box_px[2] <= 0.0 || box_px[3] <= 0.0 || e.w == 0 || e.h == 0 {
        return;
    }
    let alpha = (bg.opacity.clamp(0.0, 1.0) * opacity).clamp(0.0, 1.0);
    if alpha <= 0.0 {
        return;
    }
    let colour = [1.0, 1.0, 1.0, alpha];
    let pos = [
        bg.position[0].clamp(0.0, 1.0),
        bg.position[1].clamp(0.0, 1.0),
    ];

    // Tiles repeat on the CPU; the atlas sampler cannot.
    if bg.fit == Fit::Tile {
        let tw = (e.w as f32).min(box_px[2]).max(1.0);
        let th = (e.h as f32).min(box_px[3]).max(1.0);
        let (cols, rows) = (
            (box_px[2] / tw).ceil() as u32,
            (box_px[3] / th).ceil() as u32,
        );
        // A tiny tile over a large area is thousands of quads; the colour
        // underneath already drew, so stopping early only thins the pattern.
        for ty in 0..rows.min(32) {
            for tx in 0..cols.min(32) {
                let (dx, dy) = (box_px[0] + tx as f32 * tw, box_px[1] + ty as f32 * th);
                let (dw, dh) = (
                    tw.min(box_px[0] + box_px[2] - dx),
                    th.min(box_px[1] + box_px[3] - dy),
                );
                if dw <= 0.0 || dh <= 0.0 {
                    continue;
                }
                let (uw, uh) = (e.uv[2] - e.uv[0], e.uv[3] - e.uv[1]);
                dl.push_glyph(
                    [dx, dy, dw, dh],
                    [
                        e.uv[0],
                        e.uv[1],
                        e.uv[0] + uw * (dw / e.w as f32),
                        e.uv[1] + uh * (dh / e.h as f32),
                    ],
                    colour,
                    true,
                    0.0,
                    scissor,
                    e.page,
                );
            }
        }
        return;
    }

    let (dest, uv) = background_quad(box_px, e, bg.fit, pos);
    dl.push_glyph(dest, uv, colour, true, radius_px, scissor, e.page);
}

/// Destination rect and UV for a background image. Cover crops the overflow
/// around `position`, contain letterboxes, stretch fills, native places once.
fn background_quad(
    box_px: [f32; 4],
    e: &GlyphEntry,
    fit: gumicord_uitree::value::Fit,
    position: [f32; 2],
) -> ([f32; 4], [f32; 4]) {
    use gumicord_uitree::value::Fit;

    let (uw, uh) = (e.uv[2] - e.uv[0], e.uv[3] - e.uv[1]);
    let (iw, ih) = (e.w as f32, e.h as f32);
    match fit {
        Fit::Cover => {
            let (mut u0, mut v0, mut u1, mut v1) = (e.uv[0], e.uv[1], e.uv[2], e.uv[3]);
            let (want, have) = (box_px[2] / box_px[3], iw / ih);
            if have > want {
                let cut = uw * (1.0 - want / have);
                u0 += cut * position[0];
                u1 -= cut * (1.0 - position[0]);
            } else if have < want {
                let cut = uh * (1.0 - have / want);
                v0 += cut * position[1];
                v1 -= cut * (1.0 - position[1]);
            }
            (box_px, [u0, v0, u1, v1])
        }
        Fit::Contain => {
            let s = (box_px[2] / iw).min(box_px[3] / ih);
            let (dw, dh) = (iw * s, ih * s);
            (
                [
                    box_px[0] + (box_px[2] - dw) * position[0],
                    box_px[1] + (box_px[3] - dh) * position[1],
                    dw,
                    dh,
                ],
                [e.uv[0], e.uv[1], e.uv[2], e.uv[3]],
            )
        }
        Fit::Stretch => (box_px, [e.uv[0], e.uv[1], e.uv[2], e.uv[3]]),
        Fit::None => {
            let (dw, dh) = (iw.min(box_px[2]), ih.min(box_px[3]));
            (
                [
                    box_px[0] + (box_px[2] - dw) * position[0],
                    box_px[1] + (box_px[3] - dh) * position[1],
                    dw,
                    dh,
                ],
                [e.uv[0], e.uv[1], e.uv[2], e.uv[3]],
            )
        }
        // Handled by the caller.
        Fit::Tile => (box_px, [e.uv[0], e.uv[1], e.uv[2], e.uv[3]]),
    }
}

/// Draws text being edited.
///
/// The order matters: selection, text, preedit underline, caret. Selection
/// after the text hides it; the caret before the text is hidden by it.
///
/// Selection and preedit colours are derived from the text colour, until the
/// theme has tokens of its own for them.
#[allow(clippy::too_many_arguments)]
fn draw_editable(
    dl: &mut DrawList,
    text: &mut TextEngine,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    e: &gumicord_uitree::Editable,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
    caret_visible: bool,
) {
    let style = &placed.node.style;
    let font = ResolvedFont::from_style(style);
    let inner = placed.inner;
    let fg = style.color.unwrap_or(FALLBACK_TEXT);

    // The placeholder shows faintly; it is not editable.
    if e.text.is_empty() && !e.placeholder.is_empty() {
        let faded = Color {
            a: (fg.a as f32 * PLACEHOLDER_ALPHA) as u8,
            ..fg
        };
        draw_glyph_run(
            dl,
            text,
            device,
            queue,
            placed,
            &e.placeholder,
            linear(faded, opacity),
            scale,
            scissor,
        );
    }

    // The caret shows even when empty: it is the only sign the field can be
    // typed into. Positioned from the edited text, not the placeholder, whose
    // different width would put it somewhere arbitrary.
    let origin = text_origin(text, placed, &e.text, scale);
    let shaped = text.shaper().shape(&e.text, &font, Some(inner.w)).clone();

    let mark = |r: crate::text::TextRect, color: [f32; 4], dl: &mut DrawList| {
        dl.push_rect(
            [
                (origin.0 + r.x).round(),
                (origin.1 + r.y).round(),
                r.w.max(1.0).round(),
                r.h.round(),
            ],
            color,
            0.0,
            0.0,
            scissor,
        );
    };

    // Selection.
    let sel = linear(
        Color {
            a: (fg.a as f32 * SELECTION_ALPHA) as u8,
            ..fg
        },
        opacity,
    );
    for r in shaped.range_rects(&e.selection) {
        mark(r, sel, dl);
    }

    // Text.
    if !e.text.is_empty() {
        draw_glyph_run(
            dl,
            text,
            device,
            queue,
            placed,
            &e.text,
            linear(fg, opacity),
            scale,
            scissor,
        );
    }

    // The preedit underline, which is what shows the text is uncommitted.
    if let Some(c) = &e.composing {
        let thickness = (UNDERLINE_THICKNESS * scale).max(1.0);
        for r in shaped.range_rects(c) {
            mark(
                crate::text::TextRect {
                    y: r.y + r.h - thickness,
                    h: thickness,
                    ..r
                },
                linear(fg, opacity),
                dl,
            );
        }
    }

    // The caret only belongs to the focused field: with several boxes on
    // screen, the others must not draw one of their own. Skipped on a dark
    // blink.
    if caret_visible && placed.node.states.contains(State::Focus) {
        mark(
            shaped.caret(e.caret, (CARET_WIDTH * scale).max(1.0)),
            linear(fg, opacity),
            dl,
        );
    }
}

/// Draws a QR code.
///
/// No image is produced: a QR is a square grid, which the rounded-rect batcher
/// already draws.
///
/// Each module must be a whole number of pixels, or rounding shifts the
/// boundaries and warps the grid, and a warped QR does not scan. The scale is
/// therefore truncated.
///
/// The standard also requires a four-module quiet zone; without it a reader
/// cannot find the code's edges.
fn draw_qr(
    dl: &mut DrawList,
    placed: &crate::layout::Placed<'_>,
    data: &str,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    /// The quiet zone the standard requires, in modules.
    const QUIET: u32 = 4;

    let Ok(code) = qrcode::QrCode::new(data) else {
        tracing::warn!(len = data.len(), "cannot encode this as a QR");
        return;
    };

    let modules = code.width() as u32;
    let total = modules + QUIET * 2;
    let box_px = snap(placed.inner, scale);

    // Module size, truncated.
    let cell = (box_px[2].min(box_px[3]) / total as f32).floor();
    if cell < 1.0 {
        tracing::warn!("too small to draw a QR");
        return;
    }

    // Centre the square actually used.
    let side = cell * total as f32;
    let ox = box_px[0] + ((box_px[2] - side) * 0.5).round();
    let oy = box_px[1] + ((box_px[3] - side) * 0.5).round();

    let (light, dark) = qr_colors(&placed.node.style);

    dl.push_rect(
        [ox, oy, side, side],
        linear(light, opacity),
        0.0,
        0.0,
        scissor,
    );

    let colors = code.to_colors();
    let fg = linear(dark, opacity);
    for (i, c) in colors.iter().enumerate() {
        if *c != qrcode::Color::Dark {
            continue;
        }
        let x = (i as u32 % modules) + QUIET;
        let y = (i as u32 / modules) + QUIET;
        dl.push_rect(
            [ox + x as f32 * cell, oy + y as f32 * cell, cell, cell],
            fg,
            0.0,
            0.0,
            scissor,
        );
    }
}

/// Picks the QR's background and module colours.
///
/// Whether it scans is not a matter of taste. A theme may colour
/// `primitive.qr`, but what happens when it does not cannot be left to the
/// theme: colour inherits, so a dark theme's light text colour ends up as
/// nearly invisible modules on a white ground. That happened.
///
/// Two things are therefore enforced: the ground stays light, since the
/// standard assumes dark on light and not every reader handles inversion; and
/// the two colours stay far enough apart, falling back to black otherwise.
fn qr_colors(style: &Style) -> (Color, Color) {
    /// Below this the theme's choice is discarded. Black on white is 21:1.
    const MIN_CONTRAST: f32 = 4.5;
    /// The luminance above which a colour counts as light.
    const LIGHT_ENOUGH: f32 = 0.5;

    let themed_light = style.background.as_ref().and_then(|b| b.color);
    let light = match themed_light {
        Some(c) if luminance(c) >= LIGHT_ENOUGH => c,
        _ => QR_LIGHT,
    };

    let dark = match style.color {
        Some(c) if contrast(c, light) >= MIN_CONTRAST => c,
        _ => QR_DARK,
    };

    if themed_light != Some(light) || (style.color.is_some() && style.color != Some(dark)) {
        // Reached every frame, so warn once.
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            tracing::warn!("the theme's QR colours would not scan; using black on white");
        });
    }
    (light, dark)
}

/// Relative luminance: 0 is black, 1 is white.
fn luminance(c: Color) -> f32 {
    0.2126 * srgb_to_linear(c.r) + 0.7152 * srgb_to_linear(c.g) + 0.0722 * srgb_to_linear(c.b)
}

/// Contrast ratio: 1.0 is identical, 21.0 is black on white.
fn contrast(a: Color, b: Color) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// The QR's default ground; readers expect white.
const QR_LIGHT: Color = Color {
    r: 0xff,
    g: 0xff,
    b: 0xff,
    a: 0xff,
};

/// The QR's default module colour.
const QR_DARK: Color = Color {
    r: 0x00,
    g: 0x00,
    b: 0x00,
    a: 0xff,
};

/// The text origin, which glyphs and marks are placed from.
fn text_origin(
    text: &mut TextEngine,
    placed: &crate::layout::Placed<'_>,
    s: &str,
    scale: f32,
) -> (f32, f32) {
    let font = ResolvedFont::from_style(&placed.node.style);
    let inner = placed.inner;
    // Single-line text is measured unwrapped, or the vertical centring is
    // wrong.
    let wrap = (!intrinsic(placed.node.id).single_line).then_some(inner.w);
    let size = text.shaper().measure(s, &font, wrap);

    let y = inner.y + ((inner.h - size.h) * 0.5).max(0.0);
    let x = if intrinsic(placed.node.id).axis == Axis::Stack {
        inner.x + ((inner.w - size.w) * 0.5).max(0.0)
    } else {
        inner.x
    };
    ((x * scale).round(), (y * scale).round())
}

/// Adds glyphs, shared by the two callers that differ only in colour and
/// string.
#[allow(clippy::too_many_arguments)]
fn draw_glyph_run(
    dl: &mut DrawList,
    text: &mut TextEngine,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    s: &str,
    color: [f32; 4],
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let font = ResolvedFont::from_style(&placed.node.style);
    let (ox, oy) = text_origin(text, placed, s, scale);

    // The same wrap width as when measuring, or the glyph positions and the
    // origin disagree.
    let wrap = (!intrinsic(placed.node.id).single_line).then_some(placed.inner.w);

    let mut out: Vec<([f32; 4], [f32; 4], bool, u32)> = Vec::new();
    text.draw_glyphs(device, queue, s, &font, wrap, |e, gx, gy| {
        out.push((
            [
                ox + (gx + e.left) as f32,
                oy + (gy - e.top) as f32,
                e.w as f32,
                e.h as f32,
            ],
            e.uv,
            e.is_color,
            e.page,
        ));
    });

    for (r, uv, is_color, page) in out {
        dl.push_glyph(r, uv, color, is_color, 0.0, scissor, page);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    dl: &mut DrawList,
    text: &mut TextEngine,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    s: &str,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let color = linear(placed.node.style.color.unwrap_or(FALLBACK_TEXT), opacity);

    // Single-line text is ellipsised; wrapping makes row heights uneven.
    if intrinsic(placed.node.id).single_line {
        let font = ResolvedFont::from_style(&placed.node.style);
        let fitted = text.shaper().fit_single_line(s, &font, placed.inner.w);
        draw_glyph_run(
            dl, text, device, queue, placed, &fitted, color, scale, scissor,
        );
        return;
    }

    draw_glyph_run(dl, text, device, queue, placed, s, color, scale, scissor);
}

/// Draws one fetched image, filling its box while keeping the aspect ratio
/// and cropping the overflow: an avatar's box is square but the source often
/// is not, and stretching distorts faces.
///
/// Cropped in texture coordinates, since a scissor rect cannot coexist with
/// rounded corners.
///
/// Nothing is drawn until the image is in hand; fetching is the app's job and
/// this never waits.
#[allow(clippy::too_many_arguments)]
fn draw_image(
    dl: &mut DrawList,
    text: &TextEngine,
    placed: &crate::layout::Placed<'_>,
    url: &str,
    opacity: f32,
    scale: f32,
    radius_px: f32,
    scissor: Option<[u32; 4]>,
) {
    let box_px = snap(placed.inner, scale);
    if box_px[2] <= 0.0 || box_px[3] <= 0.0 {
        return;
    }
    // Only asked for here: clipped rows and collapsed boxes never reach this
    // point.
    let Some(e) = text.image(url) else {
        dl.missing_images.push(url.to_owned());
        return;
    };

    // Crop whichever axis overflows.
    let (uw, uh) = (e.uv[2] - e.uv[0], e.uv[3] - e.uv[1]);
    let want = box_px[2] / box_px[3];
    let have = if e.h > 0 {
        e.w as f32 / e.h as f32
    } else {
        want
    };

    let (mut u0, mut v0, mut u1, mut v1) = (e.uv[0], e.uv[1], e.uv[2], e.uv[3]);
    if have > want {
        // Wider than the box; crop the sides.
        let cut = uw * (1.0 - want / have) * 0.5;
        u0 += cut;
        u1 -= cut;
    } else if have < want {
        // Taller than the box; crop top and bottom.
        let cut = uh * (1.0 - have / want) * 0.5;
        v0 += cut;
        v1 -= cut;
    }

    dl.push_glyph(
        box_px,
        [u0, v0, u1, v1],
        [1.0, 1.0, 1.0, opacity],
        true,
        radius_px,
        scissor,
        e.page,
    );
}

/// One glyph before it is added.
type RichGlyph = ([f32; 4], [f32; 4], bool, u32, u32);

/// The square a picture fills, centred in the run it replaces. Physical
/// pixels; the run is one em wide, so a wide emoji box would leave the line
/// ragged.
fn picture_in(x: f32, y: f32, w: f32, h: f32, ox: f32, oy: f32) -> [f32; 4] {
    let side = w.min(h);
    [
        ox + x + (w - side) * 0.5,
        oy + y + (h - side) * 0.5,
        side,
        side,
    ]
}

/// Draws text with mixed decoration.
///
/// Never per run: passing each to `draw_text` and offsetting them wraps each
/// independently. Shaping happens once, and the glyphs are coloured by their
/// run index.
#[allow(clippy::too_many_arguments)]
fn draw_rich(
    dl: &mut DrawList,
    text: &mut TextEngine,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    spans: &[Span],
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let style = &placed.node.style;
    let runs = rich_runs(spans, style);
    let base = linear(style.color.unwrap_or(FALLBACK_TEXT), opacity);
    let colors: Vec<[f32; 4]> = spans
        .iter()
        .map(|s| s.color.map_or(base, |c| linear(c, opacity)))
        .collect();

    // The same wrap width as when measuring, or the lines differ.
    let max_w = placed.inner.w.is_finite().then_some(placed.inner.w);
    let size = text.shaper().measure_rich(&runs, max_w);
    let inner = placed.inner;
    let oy = ((inner.y + ((inner.h - size.h) * 0.5).max(0.0)) * scale).round();
    let ox = (inner.x * scale).round();

    let mut out: Vec<RichGlyph> = Vec::new();
    let rects = text.draw_rich_glyphs(device, queue, &runs, max_w, |e, gx, gy, which| {
        out.push((
            [
                ox + (gx + e.left) as f32,
                oy + (gy - e.top) as f32,
                e.w as f32,
                e.h as f32,
            ],
            e.uv,
            e.is_color,
            e.page,
            which,
        ));
    });

    // Spoiler fills go first; behind the glyphs they would be readable
    // through.
    for r in &rects {
        let Some(sp) = spans.get(r.run as usize) else {
            continue;
        };
        if !sp.concealed() {
            continue;
        }
        dl.push_rect(
            [ox + r.rect.x, oy + r.rect.y, r.rect.w, r.rect.h],
            linear(sp.color.unwrap_or(FALLBACK_TEXT), opacity),
            SPOILER_RADIUS * scale,
            0.0,
            scissor,
        );
    }

    // A run carrying a picture draws the picture instead of its text, which
    // is only the space it occupies. Until the pixels arrive the run stays
    // blank, and the URL goes out so they get fetched.
    for r in &rects {
        let Some(sp) = spans.get(r.run as usize) else {
            continue;
        };
        let Some(url) = &sp.image else {
            continue;
        };
        if sp.concealed() {
            continue;
        }
        let Some(e) = text.image(url) else {
            dl.missing_images.push(url.clone());
            continue;
        };
        dl.push_glyph(
            picture_in(r.rect.x, r.rect.y, r.rect.w, r.rect.h, ox, oy),
            e.uv,
            [1.0, 1.0, 1.0, opacity],
            true,
            0.0,
            scissor,
            e.page,
        );
    }

    for (rect, uv, is_color, page, which) in out {
        let Some(sp) = spans.get(which as usize) else {
            continue;
        };
        // Hidden glyphs are not added at all: painting over them leaks at the
        // rounded corners and through any transparency.
        if sp.concealed() {
            continue;
        }
        // An opened spoiler reads as normal text: its slot colour was there
        // only to paint the cover, and keeping it would wash the reveal
        // out. A run that carries its own colour (a link) still keeps it.
        let c = if sp.hidden && sp.revealed && sp.link.is_none() {
            base
        } else {
            colors.get(which as usize).copied().unwrap_or(base)
        };
        dl.push_glyph(rect, uv, c, is_color, 0.0, scissor, page);
    }

    // Lines draw over the glyphs.
    for r in &rects {
        let Some(sp) = spans.get(r.run as usize) else {
            continue;
        };
        if sp.concealed() || !sp.line.any() {
            continue;
        }
        let c = colors.get(r.run as usize).copied().unwrap_or(base);
        let w = (LINE_WIDTH * scale).max(1.0);
        // From the font size, not the line height, so a theme with generous
        // leading does not push the underline far away.
        let em = r.rect.h;
        if sp.line.under {
            let y = r.rect.y + em * UNDERLINE_AT;
            dl.push_rect([ox + r.rect.x, oy + y, r.rect.w, w], c, 0.0, 0.0, scissor);
        }
        if sp.line.through {
            let y = r.rect.y + em * STRIKE_AT;
            dl.push_rect([ox + r.rect.x, oy + y, r.rect.w, w], c, 0.0, 0.0, scissor);
        }
    }
}

/// Converts spans into what the shaper takes.
///
/// Measuring and drawing must build the same thing; differing font
/// inheritance makes the measured and drawn widths disagree and clips the
/// line.
pub(crate) fn rich_runs(spans: &[Span], style: &Style) -> Vec<(String, ResolvedFont)> {
    let base = ResolvedFont::from_style(style);
    spans
        .iter()
        .map(|s| {
            let font = match &s.font {
                // Unset fields inherit the node's font.
                Some(f) => ResolvedFont::from_style(&Style {
                    font: Some(merge_font(style.font.clone(), f.clone())),
                    ..Style::default()
                }),
                None => base.clone(),
            };
            (s.text.clone(), font)
        })
        .collect()
}

/// Layers a run's font over the node's.
fn merge_font(base: Option<Font>, over: Font) -> Font {
    let mut f = base.unwrap_or_default();
    if over.family.is_some() {
        f.family = over.family;
    }
    if over.size.is_some() {
        f.size = over.size;
    }
    if over.line_height.is_some() {
        f.line_height = over.line_height;
    }
    if over.weight.is_some() {
        f.weight = over.weight;
    }
    if over.italic.is_some() {
        f.italic = over.italic;
    }
    if over.letter_spacing.is_some() {
        f.letter_spacing = over.letter_spacing;
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture fills the narrower side of the run and sits in the
    /// middle of the other: a square in a box one em wide and one line tall.
    #[test]
    fn a_picture_fits_its_run_as_a_centred_square() {
        let square = picture_in(100.0, 50.0, 16.0, 16.0, 10.0, 20.0);
        assert_eq!(square, [110.0, 70.0, 16.0, 16.0]);

        // A taller line centres it horizontally.
        let tall = picture_in(100.0, 50.0, 16.0, 22.0, 0.0, 0.0);
        assert_eq!(tall, [100.0, 53.0, 16.0, 16.0]);
    }

    /// Readable even with no colour written. A dark theme's inherited text
    /// colour is light, and on white that produced a visible but unscannable
    /// QR.
    #[test]
    fn an_inherited_light_text_colour_is_not_used_for_the_qr() {
        let style = Style {
            color: Some(FALLBACK_TEXT),
            ..Style::default()
        };

        let (light, dark) = qr_colors(&style);
        assert_eq!(light, QR_LIGHT);
        assert_eq!(
            dark, QR_DARK,
            "drawing almost the same colour as the ground"
        );
        assert!(contrast(dark, light) > 20.0);
    }

    /// A dark enough colour is honoured.
    #[test]
    fn a_dark_enough_theme_colour_is_kept() {
        let navy = Color {
            r: 0x10,
            g: 0x20,
            b: 0x50,
            a: 0xff,
        };
        let style = Style {
            color: Some(navy),
            ..Style::default()
        };

        assert_eq!(qr_colors(&style).1, navy);
    }

    /// A dark ground is rejected; not every reader handles inversion.
    #[test]
    fn a_dark_background_is_refused() {
        let style = Style {
            background: Some(gumicord_uitree::value::Background {
                color: Some(Color {
                    r: 0x0f,
                    g: 0x0f,
                    b: 0x17,
                    a: 0xff,
                }),
                ..Default::default()
            }),
            ..Style::default()
        };

        assert_eq!(qr_colors(&style).0, QR_LIGHT);
    }

    /// Black on white is 21:1; identical colours are 1:1.
    #[test]
    fn contrast_matches_the_wcag_definition() {
        assert!((contrast(QR_DARK, QR_LIGHT) - 21.0).abs() < 0.01);
        assert!((contrast(QR_LIGHT, QR_LIGHT) - 1.0).abs() < 0.001);
    }

    #[test]
    fn snap_keeps_adjacent_rects_flush() {
        // Two adjacent logical rects: rounding the width leaves a gap or an
        // overlap, while rounding both edges always meets.
        let a = snap(Rect::new(10.4, 0.0, 10.0, 1.0), 1.5);
        let b = snap(Rect::new(20.4, 0.0, 10.0, 1.0), 1.5);
        assert_eq!(a[0] + a[2], b[0]);
    }

    #[test]
    fn srgb_endpoints_are_exact() {
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
    }

    /// A different page starts a new run: one draw binds one texture, and
    /// glyphs from another page come out as something else entirely.
    #[test]
    fn a_different_page_starts_a_new_run() {
        let mut dl = DrawList::default();
        let uv = [0.0, 0.0, 1.0, 1.0];
        let c = [1.0; 4];

        dl.push_glyph([0.0, 0.0, 1.0, 1.0], uv, c, false, 0.0, None, 0);
        dl.push_glyph([1.0, 0.0, 1.0, 1.0], uv, c, false, 0.0, None, 0);
        assert_eq!(dl.runs.len(), 1, "the same page should share a run");

        dl.push_glyph([2.0, 0.0, 1.0, 1.0], uv, c, false, 0.0, None, 1);
        assert_eq!(dl.runs.len(), 2, "a different page should split the run");
        assert_eq!(dl.runs[0].page, 0);
        assert_eq!(dl.runs[1].page, 1);

        // Returning splits again.
        dl.push_glyph([3.0, 0.0, 1.0, 1.0], uv, c, false, 0.0, None, 0);
        assert_eq!(dl.runs.len(), 3);
    }

    #[test]
    fn runs_merge_while_kind_and_scissor_match() {
        let mut dl = DrawList::default();
        let white = [1.0, 1.0, 1.0, 1.0];
        dl.push_rect([0.0, 0.0, 1.0, 1.0], white, 0.0, 0.0, None);
        dl.push_rect([1.0, 0.0, 1.0, 1.0], white, 0.0, 0.0, None);
        assert_eq!(dl.runs.len(), 1);
        assert_eq!(dl.runs[0].count, 2);

        dl.push_rect([2.0, 0.0, 1.0, 1.0], white, 0.0, 0.0, Some([0, 0, 4, 4]));
        assert_eq!(dl.runs.len(), 2, "a different clip should split the run");
    }

    /// Transparent and zero-width draws are skipped.
    #[test]
    fn degenerate_rects_are_dropped() {
        let mut dl = DrawList::default();
        dl.push_rect([0.0, 0.0, 0.0, 10.0], [1.0; 4], 0.0, 0.0, None);
        dl.push_rect([0.0, 0.0, 10.0, 10.0], [1.0, 1.0, 1.0, 0.0], 0.0, 0.0, None);
        assert_eq!(dl.rect_count(), 0);
        assert!(dl.runs.is_empty());
    }
}
