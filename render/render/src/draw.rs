//! 配置結果から描画コマンドを組み立てる。
//!
//! ここが**論理ピクセルから物理ピクセルへ変換する唯一の場所**である
//! ([`spec/06-renderer.md`] 3 章)。
//!
//! # 描画順がそのまま重なり順である
//!
//! 深度バッファを使わない。UITree の深さ優先・前順がそのまま描画順になる
//! ([`spec/06-renderer.md`] 7.5)。半透明の合成を全ノードで正しく行うために、
//! これ以外の順序にはできない。
//!
//! # 1 ノードの背景は最大 3 回の描画になる (`EXT-022`)
//!
//! ```text
//! [1] color   角丸矩形
//! [2] image   テクスチャ付きクアッド   ← R5。まだない
//! [3] tint    角丸矩形
//! ```
//!
//! 枠線はこれとは別に、`color` の下へ一回り大きい角丸矩形を敷いて表現する。
//! 背景色がない場合はそれができないので、4 辺を細い矩形で描く。

use gumicord_uitree::value::Color;
use gumicord_uitree::{Content, Style};

use crate::geom::Rect;
use crate::intrinsic::{Axis, intrinsic};
use crate::layout::LayoutResult;
use crate::text::{ResolvedFont, TextEngine};

/// 角丸矩形 1 個ぶんの float 数。48 バイト ([`spec/06-renderer.md`] 5.2)
pub const FLOATS_PER_RECT: usize = 12;
/// グリフ 1 個ぶんの float 数。64 バイト
pub const FLOATS_PER_GLYPH: usize = 16;

/// 入力欄が空のときの薄さ
const PLACEHOLDER_ALPHA: f32 = 0.45;
/// 選択範囲の濃さ。文字が読める程度に抑える
const SELECTION_ALPHA: f32 = 0.30;
/// キャレットの太さ (論理 px)
const CARET_WIDTH: f32 = 2.0;
/// 変換中を示す下線の太さ (論理 px)
const UNDERLINE_THICKNESS: f32 = 2.0;

/// テーマが文字色を指定しなかったときの色。
///
/// 真っ白ではなく少し落とす。テーマなしで起動したときに目が痛くならない程度
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

/// 同じパイプライン・同じ切り取りで連続して描けるひとかたまり。
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub kind: RunKind,
    /// インスタンスの範囲
    pub first: u32,
    pub count: u32,
    /// シザー矩形 (物理 px)。`None` なら画面全体
    pub scissor: Option<[u32; 4]>,
}

/// 1 フレームぶんの描画コマンド。
#[derive(Debug, Default)]
pub struct DrawList {
    pub rects: Vec<f32>,
    pub glyphs: Vec<f32>,
    pub runs: Vec<Run>,
}

impl DrawList {
    pub fn rect_count(&self) -> u32 {
        (self.rects.len() / FLOATS_PER_RECT) as u32
    }

    pub fn glyph_count(&self) -> u32 {
        (self.glyphs.len() / FLOATS_PER_GLYPH) as u32
    }

    /// 角丸矩形を 1 個積む。`border` が 0 より大きければ**輪だけ**を描く。
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
        self.extend_run(RunKind::Rect, first, scissor);
    }

    fn push_glyph(
        &mut self,
        r: [f32; 4],
        uv: [f32; 4],
        color: [f32; 4],
        is_color: bool,
        scissor: Option<[u32; 4]>,
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
            0.0,
            0.0,
            0.0,
        ]);
        self.extend_run(RunKind::Glyph, first, scissor);
    }

    /// 直前の run と同じ種類・同じ切り取りなら伸ばし、違えば新しく作る。
    fn extend_run(&mut self, kind: RunKind, first: u32, scissor: Option<[u32; 4]>) {
        if let Some(last) = self.runs.last_mut()
            && last.kind == kind
            && last.scissor == scissor
        {
            last.count += 1;
            return;
        }
        self.runs.push(Run {
            kind,
            first,
            count: 1,
            scissor,
        });
    }
}

/// sRGB の 1 成分をリニアへ。
///
/// サーフェスが sRGB 形式なので、シェーダの出力はリニアでなければならない。
/// `EXT-020` (全プラットフォームで同一の描画結果) は、この変換を全環境で
/// 同じに行うことに依存する。
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

/// 論理矩形を物理ピクセルグリッドへ吸着させる。
///
/// 丸めないと 1px の区切り線が 2px にぼやける ([`spec/06-renderer.md`] 3.1)。
/// 幅ではなく**両端**を丸めるのは、隣り合う矩形の間に隙間を作らないため。
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

/// 配置結果を描画コマンドへ落とす。
///
/// `viewport` は物理ピクセルでのサーフェスの大きさ。
pub fn build(
    layout: &LayoutResult<'_>,
    text: &mut TextEngine,
    queue: &wgpu::Queue,
    scale: f32,
    viewport: (u32, u32),
) -> DrawList {
    let mut dl = DrawList::default();

    for placed in &layout.placed {
        let node = placed.node;
        let style = &node.style;
        let scissor = scissor_of(placed.clip, scale, viewport);

        // 切り取りの外なら、そもそも積まない。
        // 仮想化 (NFR-007) の代わりにはならないが、無駄な転送は減る
        if let Some(c) = placed.clip
            && c.intersect(placed.rect).is_empty()
        {
            continue;
        }

        // ⚠️ 本来の opacity はノードを一枚の層に描いてから合成する操作である。
        // ここでは自分の描画の alpha に掛けているだけで、子には効かない。
        // 層への描画は R6 (クリップと仮想化) と同じ機構が要るので、そこで直す
        let opacity = style.opacity.unwrap_or(1.0).clamp(0.0, 1.0);
        let radius = style.radius.unwrap_or(0.0);
        let rect = snap(placed.rect, scale);
        let radius_px = radius * scale;

        draw_background(&mut dl, style, rect, radius_px, opacity, scale, scissor);

        match &node.content {
            Content::Text(s) if !s.is_empty() => {
                draw_text(&mut dl, text, queue, placed, s, opacity, scale, scissor);
            }
            Content::Icon(name) => {
                draw_icon(&mut dl, text, queue, placed, name, opacity, scale, scissor);
            }
            Content::Editable(e) => {
                draw_editable(&mut dl, text, queue, placed, e, opacity, scale, scissor);
            }
            _ => {}
        }
    }

    dl
}

/// アイコンを 1 個描く。
///
/// **整数のピクセルに合わせる。** アイコンは物理ピクセルちょうどの大きさで
/// ラスタライズしてあるので、半端な位置に置くと折角の輪郭がぼやける。
#[allow(clippy::too_many_arguments)]
fn draw_icon(
    dl: &mut DrawList,
    text: &mut TextEngine,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    name: &str,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let style = &placed.node.style;
    let inner = placed.inner;

    // 文字と同じ大きさを基準にし、入れ物からはみ出さないところまで詰める
    let logical = ResolvedFont::from_style(style)
        .size()
        .min(inner.w)
        .min(inner.h);
    let size = (logical * scale).round().max(1.0);

    let Some(e) = text.icon(queue, name, size as u32) else {
        // 知らない名前。描かずに進む
        return;
    };

    let box_px = snap(inner, scale);
    let x = box_px[0] + ((box_px[2] - size) * 0.5).round();
    let y = box_px[1] + ((box_px[3] - size) * 0.5).round();

    let color = linear(style.color.unwrap_or(FALLBACK_TEXT), opacity);
    dl.push_glyph([x, y, size, size], e.uv, color, e.is_color, scissor);
}

fn draw_background(
    dl: &mut DrawList,
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

    // [2] image — TODO: R5 (EXT-021〜EXT-024)。
    // 画像が読めるまでは color がフォールバックであり、仕様上もその挙動でよい

    // [3] tint
    if let Some(tint) = style.background.as_ref().and_then(|b| b.tint) {
        dl.push_rect(rect, linear(tint, opacity), radius_px, 0.0, scissor);
    }

    // 枠線は背景の**上**に、輪として描く。
    // 半透明の背景でも枠線の色が中へ透けない (EXT-024)
    if let Some((bc, bw)) = border {
        dl.push_rect(rect, linear(bc, opacity), radius_px, bw * scale, scissor);
    }
}

/// 編集中のテキスト (`PLT-001`)。
///
/// 重ねる順序が意味を持つ。**選択 → 文字 → 変換中の下線 → キャレット**。
/// 選択を文字の後に描くと文字が隠れ、キャレットを文字の前に描くと文字に
/// 隠れる。
///
/// ⚠️ 選択と変換中の色は文字色から作っている。テーマに専用のトークンが
/// ないためで、`spec/04-theme.md` に足すまでの当座の措置である。
#[allow(clippy::too_many_arguments)]
fn draw_editable(
    dl: &mut DrawList,
    text: &mut TextEngine,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    e: &gumicord_uitree::Editable,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let style = &placed.node.style;
    let font = ResolvedFont::from_style(style);
    let inner = placed.inner;
    let fg = style.color.unwrap_or(FALLBACK_TEXT);

    // 中身が空なら placeholder を薄く出すだけ。印は出さない
    if e.text.is_empty() {
        if !e.placeholder.is_empty() {
            let faded = Color {
                a: (fg.a as f32 * PLACEHOLDER_ALPHA) as u8,
                ..fg
            };
            draw_glyph_run(
                dl,
                text,
                queue,
                placed,
                &e.placeholder,
                linear(faded, opacity),
                scale,
                scissor,
            );
        }
        return;
    }

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

    // [1] 選択
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

    // [2] 文字
    draw_glyph_run(
        dl,
        text,
        queue,
        placed,
        &e.text,
        linear(fg, opacity),
        scale,
        scissor,
    );

    // [3] 変換中の下線。**確定していないことが見て分かる必要がある**
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

    // [4] キャレット
    mark(
        shaped.caret(e.caret, (CARET_WIDTH * scale).max(1.0)),
        linear(fg, opacity),
        dl,
    );
}

/// テキストの原点 (物理 px)。文字も印もここを基準に置く
fn text_origin(
    text: &mut TextEngine,
    placed: &crate::layout::Placed<'_>,
    s: &str,
    scale: f32,
) -> (f32, f32) {
    let font = ResolvedFont::from_style(&placed.node.style);
    let inner = placed.inner;
    let size = text.shaper().measure(s, &font, Some(inner.w));

    let y = inner.y + ((inner.h - size.h) * 0.5).max(0.0);
    let x = if intrinsic(placed.node.id).axis == Axis::Stack {
        inner.x + ((inner.w - size.w) * 0.5).max(0.0)
    } else {
        inner.x
    };
    ((x * scale).round(), (y * scale).round())
}

/// グリフを積む。色と文字列だけが違う 2 つの呼び出し元で共有する
#[allow(clippy::too_many_arguments)]
fn draw_glyph_run(
    dl: &mut DrawList,
    text: &mut TextEngine,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    s: &str,
    color: [f32; 4],
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let font = ResolvedFont::from_style(&placed.node.style);
    let (ox, oy) = text_origin(text, placed, s, scale);

    let mut out: Vec<([f32; 4], [f32; 4], bool)> = Vec::new();
    text.draw_glyphs(queue, s, &font, Some(placed.inner.w), |e, gx, gy| {
        out.push((
            [
                ox + (gx + e.left) as f32,
                oy + (gy - e.top) as f32,
                e.w as f32,
                e.h as f32,
            ],
            e.uv,
            e.is_color,
        ));
    });

    for (r, uv, is_color) in out {
        dl.push_glyph(r, uv, color, is_color, scissor);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    dl: &mut DrawList,
    text: &mut TextEngine,
    queue: &wgpu::Queue,
    placed: &crate::layout::Placed<'_>,
    s: &str,
    opacity: f32,
    scale: f32,
    scissor: Option<[u32; 4]>,
) {
    let color = linear(placed.node.style.color.unwrap_or(FALLBACK_TEXT), opacity);
    draw_glyph_run(dl, text, queue, placed, s, color, scale, scissor);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_keeps_adjacent_rects_flush() {
        // 論理 10.4..20.4 と 20.4..30.4 が隣り合う。
        // 幅を丸めると隙間か重なりが出るが、両端を丸めれば必ず接する
        let a = snap(Rect::new(10.4, 0.0, 10.0, 1.0), 1.5);
        let b = snap(Rect::new(20.4, 0.0, 10.0, 1.0), 1.5);
        assert_eq!(a[0] + a[2], b[0]);
    }

    #[test]
    fn srgb_endpoints_are_exact() {
        assert_eq!(srgb_to_linear(0), 0.0);
        assert!((srgb_to_linear(255) - 1.0).abs() < 1e-6);
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
        assert_eq!(dl.runs.len(), 2, "切り取りが変われば run が分かれる");
    }

    /// 透明・幅ゼロのものは積まない。GPU へ送る量を無駄に増やさない
    #[test]
    fn degenerate_rects_are_dropped() {
        let mut dl = DrawList::default();
        dl.push_rect([0.0, 0.0, 0.0, 10.0], [1.0; 4], 0.0, 0.0, None);
        dl.push_rect([0.0, 0.0, 10.0, 10.0], [1.0, 1.0, 1.0, 0.0], 0.0, 0.0, None);
        assert_eq!(dl.rect_count(), 0);
        assert!(dl.runs.is_empty());
    }
}
