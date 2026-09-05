//! Layout: constraints go down, resolved sizes come back up.
//!
//! No flexbox and no grid — only three axes and whether something scrolls.
//!
//! Main-axis space is handed out in two passes:
//!
//! ```text
//! 1. measure the children that take no slack
//! 2. divide the remainder among those that do, by their grow
//! ```
//!
//! The order matters: a message is an avatar plus a body that takes the rest,
//! and the body's wrap width is only known once the avatar's width is
//! subtracted. Giving everyone the same constraint overflows the body.
//!
//! Measurements are cached by (node, constraint): a parent touches each child
//! twice, once to measure and once to place, which is exponential in the
//! tree's depth otherwise.
//!
//! Everything here is in logical pixels; the conversion happens once, later.

use std::collections::HashMap;

use gumicord_uitree::value::Edges;
use gumicord_uitree::{Content, NodeId, UiNode};

use crate::geom::{EdgesExt, Rect, Size};
use crate::intrinsic::{Axis, Cross, Intrinsic, intrinsic, is_overlay};
use crate::text::{ResolvedFont, Shaper};

/// Scroll offsets, keyed by stable ID.
///
/// Two scroll regions sharing an ID share a position. Only one of each is on
/// screen today; tabs or split views would need the key in there too.
pub type ScrollState = HashMap<NodeId, f32>;

/// Below this the thumb cannot be grabbed.
const MIN_THUMB: f32 = 24.0;
/// Bottom sheets rise to this share of the window at most, like the
/// official client; taller content scrolls inside.
const SHEET_MAX_H: f32 = 0.7;

/// Pinned to the bottom; the message list starts here.
pub const SCROLL_TO_END: f32 = f32::MAX;

/// A remembered scroll position.
///
/// The bottom is stored as an intent, not a number: a pixel offset would drift
/// upwards as new messages grow the content, which for someone waiting at the
/// bottom means the new row never appears. Storing a sentinel and clamping at
/// the end lands on the new bottom instead.
///
/// Only for lists that pin to the end. Someone scrolled to the bottom of the
/// guild list is looking at a guild, not at the bottom, and must not move.
pub fn remember(id: NodeId, at: f32, max: f32) -> f32 {
    if intrinsic(id).anchor_end && at >= max {
        SCROLL_TO_END
    } else {
        at
    }
}

/// One placed node.
#[derive(Debug, Clone)]
pub struct Placed<'a> {
    pub node: &'a UiNode,
    /// Logical px from the window's top left.
    pub rect: Rect,
    /// Nothing outside this is drawn.
    pub clip: Option<Rect>,
    /// The content box, which is also the text wrap width.
    pub inner: Rect,
}

/// One placed scrollbar.
///
/// Hit testing is not enough: how far the thumb can travel is the track minus
/// the thumb, which only layout knows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollBar {
    /// The region this drives.
    pub owner: NodeId,
    /// The track the thumb moves in.
    pub track: Rect,
    pub thumb: Rect,
}

/// One frame's layout, in draw order.
#[derive(Debug, Default)]
pub struct LayoutResult<'a> {
    pub placed: Vec<Placed<'a>>,
    /// Overflow per scroll region, which bounds scrolling.
    pub overflow: HashMap<NodeId, f32>,
    pub scrollbars: Vec<ScrollBar>,
}

impl<'a> LayoutResult<'a> {
    /// The frontmost node containing a point.
    ///
    /// Walked in reverse draw order: whatever was drawn last is on top, so it
    /// wins the hit too.
    pub fn hit(&self, x: f32, y: f32) -> Option<&Placed<'a>> {
        self.placed
            .iter()
            .rev()
            .find(|p| p.rect.contains(x, y) && p.clip.is_none_or(|c| c.contains(x, y)))
    }

    /// Every node containing a point, front to back. Hover usually applies to
    /// an item and its ancestors, so one is not enough.
    pub fn hits(&self, x: f32, y: f32) -> impl Iterator<Item = &Placed<'a>> {
        self.placed
            .iter()
            .rev()
            .filter(move |p| p.rect.contains(x, y) && p.clip.is_none_or(|c| c.contains(x, y)))
    }

    /// Where a stable ID landed, used to bound scrolling.
    pub fn find(&self, id: NodeId) -> Option<&Placed<'a>> {
        self.placed.iter().find(|p| p.node.id == id)
    }
}

/// Lays out the tree.
pub fn layout<'a>(
    root: &'a UiNode,
    viewport: Size,
    text: &mut Shaper,
    scroll: &ScrollState,
) -> LayoutResult<'a> {
    let mut cx = Cx {
        text,
        scroll,
        cache: HashMap::new(),
        out: Vec::new(),
        overflow: HashMap::new(),
        scrollbars: Vec::new(),
    };
    cx.place(root, Rect::from_size(viewport), None);
    LayoutResult {
        placed: cx.out,
        overflow: cx.overflow,
        scrollbars: cx.scrollbars,
    }
}

struct Cx<'a, 't, 's> {
    text: &'t mut Shaper,
    scroll: &'s ScrollState,
    /// (node address, constraint) -> size
    cache: HashMap<(usize, u32, u32), Size>,
    out: Vec<Placed<'a>>,
    /// Overflow per scroll region.
    overflow: HashMap<NodeId, f32>,
    scrollbars: Vec<ScrollBar>,
}

/// Quantised so a constraint can be a map key; `f32` cannot.
fn q(v: f32) -> u32 {
    if !v.is_finite() {
        return u32::MAX;
    }
    (v.max(0.0) * 16.0).round() as u32
}

/// The explicit size on an axis; the theme beats the default.
fn explicit(node: &UiNode, it: &Intrinsic, axis_is_horizontal: bool) -> Option<f32> {
    if axis_is_horizontal {
        node.style.width.or(it.width)
    } else {
        node.style.height.or(it.height)
    }
}

fn clamp_size(node: &UiNode, mut w: f32, mut h: f32) -> Size {
    let s = &node.style;
    if let Some(v) = s.min_width {
        w = w.max(v);
    }
    if let Some(v) = s.max_width {
        w = w.min(v);
    }
    if let Some(v) = s.min_height {
        h = h.max(v);
    }
    if let Some(v) = s.max_height {
        h = h.min(v);
    }
    Size::new(w.max(0.0), h.max(0.0))
}

impl<'a> Cx<'a, '_, '_> {
    // ───────────────────────────────────────────────── Measuring

    fn measure(&mut self, node: &UiNode, avail: Size) -> Size {
        let key = (std::ptr::from_ref(node) as usize, q(avail.w), q(avail.h));
        if let Some(s) = self.cache.get(&key) {
            return *s;
        }
        let size = self.measure_uncached(node, avail);
        self.cache.insert(key, size);
        size
    }

    fn measure_uncached(&mut self, node: &UiNode, avail: Size) -> Size {
        let it = intrinsic(node.id);
        let pad = node.style.padding.unwrap_or_default();

        let ex_w = explicit(node, &it, true);
        let ex_h = explicit(node, &it, false);

        let inner = Size::new(
            (ex_w.unwrap_or(avail.w) - pad.horizontal()).max(0.0),
            (ex_h.unwrap_or(avail.h) - pad.vertical()).max(0.0),
        );

        let content = self.measure_content(node, &it, inner);

        clamp_size(
            node,
            ex_w.unwrap_or(content.w + pad.horizontal()),
            ex_h.unwrap_or(content.h + pad.vertical()),
        )
    }

    /// The content's size, excluding padding.
    fn measure_content(&mut self, node: &UiNode, it: &Intrinsic, inner: Size) -> Size {
        match &node.content {
            // Editable text measures as plain text: the caret and selection
            // draw over existing lines and do not grow the box.
            Content::Text(_) | Content::Editable(_) => {
                let s = node.content.as_text().unwrap_or_default();
                let font = ResolvedFont::from_style(&node.style);

                // Single-line text is not measured wrapped, or it reports two
                // lines and doubles the row height. Channel names did this.
                if intrinsic(node.id).single_line {
                    let mut size = self.text.measure(s, &font, None);
                    if inner.w.is_finite() {
                        size.w = size.w.min(inner.w);
                    }
                    return size;
                }

                // An infinite wrap width means no wrapping.
                let max_w = inner.w.is_finite().then_some(inner.w);
                self.text.measure(s, &font, max_w)
            }
            // Measured as one mixed run: summing per-span measurements stops
            // matching the moment a wrap appears.
            Content::Rich(spans) => {
                let runs = crate::draw::rich_runs(spans, &node.style);
                let max_w = inner.w.is_finite().then_some(inner.w);
                self.text.measure_rich(&runs, max_w)
            }
            // Square, and the size of the text, so it lines up inline.
            Content::Icon(_) => {
                let s = ResolvedFont::from_style(&node.style).size();
                Size::new(s, s)
            }
            // Images fill their container; the content implies no size.
            Content::Image(_) => Size::ZERO,
            // Square and container-filling; the content implies no size.
            Content::Qr(_) => {
                let s = inner.w.min(inner.h);
                if s.is_finite() {
                    Size::new(s, s)
                } else {
                    Size::ZERO
                }
            }
            Content::None if node.children.is_empty() => Size::ZERO,
            Content::None => self.size_children(node, it, inner).1,
        }
    }

    /// Sizes children along the main axis, returning each size and the total.
    fn size_children(&mut self, node: &UiNode, it: &Intrinsic, inner: Size) -> (Vec<Size>, Size) {
        let n = node.children.len();
        let gap = node.style.gap.unwrap_or(0.0);
        let mut sizes = vec![Size::ZERO; n];

        if it.axis == Axis::Stack {
            let mut content = Size::ZERO;
            for (i, c) in node.children.iter().enumerate() {
                let m = c.style.margin.unwrap_or_default();
                let s = self.measure(
                    c,
                    Size::new(
                        (inner.w - m.horizontal()).max(0.0),
                        (inner.h - m.vertical()).max(0.0),
                    ),
                );
                sizes[i] = s;
                if intrinsic(c.id).follows_cross {
                    continue;
                }
                content.w = content.w.max(s.w + m.horizontal());
                content.h = content.h.max(s.h + m.vertical());
            }
            return (sizes, content);
        }

        let horizontal = it.axis == Axis::Row;
        let main_avail = if horizontal { inner.w } else { inner.h };
        let cross_avail = if horizontal { inner.h } else { inner.w };

        let margins: Vec<Edges> = node
            .children
            .iter()
            .map(|c| c.style.margin.unwrap_or_default())
            .collect();
        let margin_main: f32 = margins
            .iter()
            .map(|m| {
                if horizontal {
                    m.horizontal()
                } else {
                    m.vertical()
                }
            })
            .sum();

        // Overlaid children consume no main axis, and no gap either.
        let overlay: Vec<bool> = node.children.iter().map(|c| is_overlay(c.id)).collect();
        let in_flow = overlay.iter().filter(|o| !**o).count();

        let gaps = gap * (in_flow.saturating_sub(1)) as f32;
        let mut remaining = main_avail - gaps - margin_main;

        // An explicit main-axis size wins over `grow`, which is only the
        // renderer's default. The other way round silently stretches anything
        // a theme gave a width.
        let grows: Vec<f32> = node
            .children
            .iter()
            .map(|c| {
                let ci = intrinsic(c.id);
                if explicit(c, &ci, horizontal).is_some() {
                    0.0
                } else {
                    ci.grow
                }
            })
            .collect();

        // Measure the children that take no slack.

        for (i, c) in node.children.iter().enumerate() {
            if overlay[i] || (grows[i] > 0.0 && remaining.is_finite()) {
                continue;
            }
            let m = margins[i];
            let avail = if horizontal {
                Size::new(remaining.max(0.0), (cross_avail - m.vertical()).max(0.0))
            } else {
                Size::new((cross_avail - m.horizontal()).max(0.0), remaining.max(0.0))
            };
            let s = self.measure(c, avail);
            sizes[i] = s;
            remaining -= if horizontal { s.w } else { s.h };
        }

        // Divide the remainder by grow. An infinite constraint, as on a
        // scroll region's main axis, leaves nothing to divide.
        let total_grow: f32 = grows.iter().sum();
        if total_grow > 0.0 && remaining.is_finite() {
            let pool = remaining.max(0.0);
            for (i, c) in node.children.iter().enumerate() {
                if grows[i] <= 0.0 {
                    continue;
                }
                let m = margins[i];
                let main = pool * grows[i] / total_grow;
                let avail = if horizontal {
                    Size::new(main, (cross_avail - m.vertical()).max(0.0))
                } else {
                    Size::new((cross_avail - m.horizontal()).max(0.0), main)
                };
                let mut s = self.measure(c, avail);
                // Pinned to what was handed out, not what was measured.
                if horizontal {
                    s.w = main;
                } else {
                    s.h = main;
                }
                sizes[i] = s;
            }
        }

        let mut main_total = gaps + margin_main;
        let mut cross_max = 0.0f32;
        for (i, s) in sizes.iter().enumerate() {
            if overlay[i] {
                continue;
            }
            let m = margins[i];
            if horizontal {
                main_total += s.w;
            } else {
                main_total += s.h;
            }
            // Children that do not set the cross axis are excluded from it,
            // but still occupy the main axis.
            if intrinsic(node.children[i].id).follows_cross {
                continue;
            }
            if horizontal {
                cross_max = cross_max.max(s.h + m.vertical());
            } else {
                cross_max = cross_max.max(s.w + m.horizontal());
            }
        }

        let content = if horizontal {
            Size::new(main_total, cross_max)
        } else {
            Size::new(cross_max, main_total)
        };
        (sizes, content)
    }

    // ───────────────────────────────────────────────── Placing

    fn place(&mut self, node: &'a UiNode, rect: Rect, clip: Option<Rect>) {
        let it = intrinsic(node.id);
        let pad = node.style.padding.unwrap_or_default();
        let inner = rect.deflate(pad);

        self.out.push(Placed {
            node,
            rect,
            clip,
            inner,
        });

        // A node that draws something is a leaf — except an image, whose
        // children ride on it: the presence dot sits on its avatar. They
        // follow the stack rules over the whole rect and draw after it.
        let riders = matches!(node.content, Content::Image(_)) && !node.children.is_empty();
        if node.content.is_leaf() && !riders {
            return;
        }

        // A scroll region drops the main-axis constraint to get the real
        // content size.
        let avail = if it.scroll {
            match it.axis {
                Axis::Row => Size::new(f32::INFINITY, inner.h),
                _ => Size::new(inner.w, f32::INFINITY),
            }
        } else {
            inner.size()
        };

        let (sizes, content) = self.size_children(node, &it, avail);

        let mut offset = 0.0;
        let mut over = 0.0;
        if it.scroll {
            over = match it.axis {
                Axis::Row => content.w - inner.w,
                _ => content.h - inner.h,
            }
            .max(0.0);
            self.overflow.insert(node.id, over);
            offset = self
                .scroll
                .get(&node.id)
                .copied()
                .unwrap_or(if it.anchor_end { over } else { 0.0 })
                .clamp(0.0, over);
        }

        // Under-full content in an end-anchored list is pushed down: placing
        // it at the top made a channel with one message stick to the ceiling.
        // Nothing overflows, so this is not scrolling — the start position is
        // simply moved down.
        if it.anchor_end {
            let short = match it.axis {
                Axis::Row => inner.w - content.w,
                _ => inner.h - content.h,
            };
            if short > 0.0 {
                offset -= short;
            }
        }

        // A scrollbar does not take the content's clip: the content clips
        // inside the padding, while the bar stands at the container's edge,
        // and the same clip removes it entirely.
        let bar_clip = clip.map(|c| c.intersect(rect));

        let clip = if it.scroll {
            Some(clip.map_or(inner, |c| c.intersect(inner)))
        } else {
            clip
        };

        let gap = node.style.gap.unwrap_or(0.0);
        let horizontal = it.axis == Axis::Row;
        let mut cursor = if horizontal { inner.x } else { inner.y } - offset;

        for (i, child) in node.children.iter().enumerate() {
            // Overlaid children stay out of the flow.
            if is_overlay(child.id) {
                // The outer rect, so it is not pulled inside the padding.
                self.place_scrollbar(node.id, child, rect, offset, over, bar_clip);
                continue;
            }

            let m = child.style.margin.unwrap_or_default();
            let ci = intrinsic(child.id);
            let size = sizes[i];

            let child_rect = if let Some(a) = child.anchor {
                // Anchored children follow neither the flow nor the stack's
                // centring; they start at the point and flip on overflow.
                let s = Self::stack_size(child, &ci, size, inner);
                anchored(a, s, inner, m)
            } else if it.axis == Axis::Stack {
                let s = Self::stack_size(child, &ci, size, inner);
                if child.id == NodeId::OverlaySheet {
                    // Bottom sheets span the width and rise to ~70% of the
                    // window; taller content scrolls inside. Centring a
                    // capped sheet would strand it mid-screen.
                    let h = s.h.min(inner.h * SHEET_MAX_H);
                    Rect::new(
                        inner.x + m.left,
                        inner.y + inner.h - h - m.bottom,
                        inner.w - m.horizontal(),
                        h,
                    )
                } else {
                    Rect::new(
                        inner.x + m.left + (inner.w - m.horizontal() - s.w).max(0.0) * 0.5,
                        inner.y + m.top + (inner.h - m.vertical() - s.h).max(0.0) * 0.5,
                        s.w,
                        s.h,
                    )
                }
            } else if horizontal {
                let avail = inner.h - m.vertical();
                let h = Self::cross_size(child, &ci, size.h, avail, it.cross, false);
                let y = inner.y + m.top + Self::cross_offset(it.cross, avail, h);
                Rect::new(cursor + m.left, y, size.w, h)
            } else {
                let avail = inner.w - m.horizontal();
                let w = Self::cross_size(child, &ci, size.w, avail, it.cross, true);
                let x = inner.x + m.left + Self::cross_offset(it.cross, avail, w);
                Rect::new(x, cursor + m.top, w, size.h)
            };

            // Anchored children do not advance the cursor.
            if it.axis != Axis::Stack && child.anchor.is_none() {
                cursor += if horizontal {
                    size.w + m.horizontal() + gap
                } else {
                    size.h + m.vertical() + gap
                };
            }

            self.place(child, child_rect, clip);
        }
    }

    /// Places a scrollbar at the list's edge.
    ///
    /// The thumb's size and position follow from the overflow, so a theme
    /// cannot express them; it decides the width, padding and colour.
    ///
    /// The track is the rect *before* padding: taking it after moves the bar
    /// inside and puts it on top of the content, which it did.
    ///
    /// Nothing is placed when there is nothing to scroll; an immobile
    /// scrollbar lies.
    fn place_scrollbar(
        &mut self,
        owner: NodeId,
        node: &'a UiNode,
        track: Rect,
        offset: f32,
        over: f32,
        clip: Option<Rect>,
    ) {
        if over <= 0.0 || track.is_empty() {
            return;
        }

        let ci = intrinsic(node.id);
        let w = explicit(node, &ci, true).unwrap_or(0.0).min(track.w);
        let bar = Rect::new(track.right() - w, track.y, w, track.h);
        let inner = bar.deflate(node.style.padding.unwrap_or_default());

        self.out.push(Placed {
            node,
            rect: bar,
            clip,
            inner,
        });

        let Some(thumb) = node
            .children
            .iter()
            .find(|c| c.id == NodeId::LayoutScrollbarThumb)
        else {
            return;
        };

        // The visible fraction is the thumb's fraction, floored so it stays
        // grabbable.
        let visible = track.h;
        let content = visible + over;
        let h = (inner.h * (visible / content)).max(MIN_THUMB).min(inner.h);
        let t = (offset / over).clamp(0.0, 1.0);
        let rect = Rect::new(inner.x, inner.y + (inner.h - h) * t, inner.w, h);

        self.out.push(Placed {
            node: thumb,
            rect,
            clip,
            inner: rect.deflate(thumb.style.padding.unwrap_or_default()),
        });

        // Kept for dragging: hit testing does not say how far it can travel.
        self.scrollbars.push(ScrollBar {
            owner,
            track: inner,
            thumb: rect,
        });
    }

    /// A stacked child's size: explicit if given, otherwise the parent's.
    fn stack_size(child: &UiNode, ci: &Intrinsic, measured: Size, inner: Rect) -> Size {
        Size::new(
            if ci.hugs_content || explicit(child, ci, true).is_some() {
                measured.w
            } else {
                inner.w
            },
            if ci.hugs_content || explicit(child, ci, false).is_some() {
                measured.h
            } else {
                inner.h
            },
        )
    }

    /// A child's cross-axis size. An explicit size is never stretched: an
    /// avatar must not grow vertically even under `Stretch`.
    fn cross_size(
        child: &UiNode,
        ci: &Intrinsic,
        measured: f32,
        avail: f32,
        cross: Cross,
        cross_is_horizontal: bool,
    ) -> f32 {
        if explicit(child, ci, cross_is_horizontal).is_some() {
            return measured;
        }
        match cross {
            Cross::Stretch if avail.is_finite() => avail.max(0.0),
            _ => measured,
        }
    }

    fn cross_offset(cross: Cross, avail: f32, size: f32) -> f32 {
        match cross {
            Cross::Center => ((avail - size) * 0.5).max(0.0),
            Cross::Start | Cross::Stretch => 0.0,
        }
    }
}

/// Places from an anchor point, flipping on overflow.
///
/// A menu opened at the right edge would otherwise hang half off screen, with
/// its items visible but unreachable.
///
/// Flip first, then clamp. Flipping alone fails for a menu larger than the
/// container, and clamping alone puts a right-edge menu under the finger.
fn anchored(a: gumicord_uitree::Anchor, size: Size, inner: Rect, m: Edges) -> Rect {
    let w = size.w + m.horizontal();
    let h = size.h + m.vertical();

    // Down and right by default; the other side if it does not fit.
    let mut x = a.x;
    if x + w > inner.right() {
        x = a.x - w;
    }
    let mut y = a.y;
    if y + h > inner.bottom() {
        y = a.y - h;
    }

    // Then clamp. The `max` must come last, or something larger than the
    // container aligns to the bottom and loses its top.
    x = x.min(inner.right() - w).max(inner.x);
    y = y.min(inner.bottom() - h).max(inner.y);

    Rect::new(x + m.left, y + m.top, size.w, size.h)
}

#[cfg(test)]
mod tests {
    use gumicord_uitree::Style;
    use gumicord_uitree::value::Edges;

    use super::*;

    /// Layout needs no GPU, so a shaper can be built directly.
    fn shaper() -> Shaper {
        Shaper::new(1.0)
    }

    fn styled(id: NodeId, f: impl FnOnce(&mut Style)) -> UiNode {
        let mut n = UiNode::new(id);
        f(&mut n.style);
        n
    }

    fn rect_of<'a>(r: &'a LayoutResult<'a>, id: NodeId) -> Rect {
        r.find(id)
            .unwrap_or_else(|| panic!("{id} が配置されていない"))
            .rect
    }

    /// Growing nodes fill the main axis; the rest keep their real size.
    #[test]
    fn grow_children_share_the_remainder() {
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(styled(NodeId::NavGuildList, |s| s.width = Some(64.0)))
            .child(styled(NodeId::NavChannelList, |s| s.width = Some(240.0)))
            .child(UiNode::new(NodeId::ChatView));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        assert_eq!(rect_of(&r, NodeId::NavGuildList).w, 64.0);
        assert_eq!(rect_of(&r, NodeId::NavChannelList).w, 240.0);
        // Only the chat view grows, so it takes the rest.
        assert_eq!(rect_of(&r, NodeId::ChatView).w, 1000.0 - 64.0 - 240.0);
        assert_eq!(rect_of(&r, NodeId::ChatView).x, 304.0);
    }

    /// A theme's size beats the default `grow`, on the main axis too.
    #[test]
    fn an_explicit_main_size_beats_the_default_grow() {
        // The chat view grows by default; a written width wins.
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(styled(NodeId::ChatView, |s| s.width = Some(300.0)))
            .child(UiNode::new(NodeId::NavChannelList));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        assert_eq!(rect_of(&r, NodeId::ChatView).w, 300.0);
    }

    /// Under `Stretch` an unsized child fills the cross axis; an explicit
    /// size is never stretched.
    #[test]
    fn stretch_does_not_override_an_explicit_size() {
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(styled(NodeId::NavChannelList, |s| {
                s.width = Some(240.0);
                s.height = Some(100.0);
            }))
            .child(UiNode::new(NodeId::ChatView));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        assert_eq!(
            rect_of(&r, NodeId::NavChannelList).h,
            100.0,
            "指定は伸ばさない"
        );
        assert_eq!(
            rect_of(&r, NodeId::ChatView).h,
            600.0,
            "指定がなければ広がる"
        );
    }

    /// The user panel takes the sidebar's width rather than setting it.
    /// Without that it widens the sidebar until chat has no slack left and
    /// disappears, which it did.
    #[test]
    fn the_user_panel_takes_the_width_it_is_given() {
        let lists = UiNode::new(NodeId::NavSidebarLists)
            .child(styled(NodeId::NavGuildList, |s| s.width = Some(64.0)))
            .child(styled(NodeId::NavChannelList, |s| s.width = Some(240.0)));
        let panel = UiNode::new(NodeId::NavUserPanel).child(
            UiNode::new(NodeId::LayoutColumn).child(UiNode::text(
                NodeId::NavUserPanelName,
                "ずいぶん長い名前のひと".to_owned(),
            )),
        );
        let tree = UiNode::new(NodeId::AppScreenMain)
            .child(UiNode::new(NodeId::NavSidebar).child(lists).child(panel))
            .child(UiNode::new(NodeId::ChatView));

        let r = layout(
            &tree,
            Size::new(1000.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        assert_eq!(
            rect_of(&r, NodeId::NavSidebar).w,
            304.0,
            "the lists decide the width"
        );
        assert_eq!(
            rect_of(&r, NodeId::NavUserPanel).w,
            304.0,
            "帯は両方にまたがる"
        );
        assert_eq!(
            rect_of(&r, NodeId::ChatView).w,
            696.0,
            "チャットに余りが残る"
        );
    }

    /// A quote hugs its content: it must not grow to fill the leftover height
    /// of a tall message and drift the body text after it (regression).
    #[test]
    fn quote_does_not_grow_into_leftover() {
        let quote_body = UiNode::new(NodeId::LayoutColumn)
            .child(UiNode::text(
                NodeId::PrimitiveText,
                "quote first line".to_owned(),
            ))
            .child(UiNode::text(
                NodeId::PrimitiveText,
                "quote second line".to_owned(),
            ));
        let quote = UiNode::new(NodeId::ChatMessageQuoteRow)
            .child(styled(NodeId::PrimitiveDivider, |s| s.width = Some(4.0)))
            .child(quote_body);
        let after = UiNode::text(NodeId::PrimitiveText, "after".to_owned())
            .with_key(gumicord_uitree::Key::Slot("after"));
        let content = UiNode::new(NodeId::ChatMessageContent)
            .child(UiNode::text(NodeId::PrimitiveText, "before".to_owned()))
            .child(quote)
            .child(after);
        let body = styled(NodeId::LayoutColumn, |s| s.height = Some(200.0)).child(content);
        let message = UiNode::new(NodeId::ChatMessage)
            .child(styled(NodeId::ChatMessageAvatar, |s| {
                s.width = Some(40.0);
                s.height = Some(40.0);
            }))
            .child(body);
        let tree = UiNode::new(NodeId::ChatView)
            .child(UiNode::new(NodeId::ChatMessageList).child(message));

        let r = layout(
            &tree,
            Size::new(400.0, 600.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let quote_rect = rect_of(&r, NodeId::ChatMessageQuoteRow);
        assert_eq!(quote_rect.h, 44.0, "引用は中身の高さだけに収まる");

        let after_rect = r
            .placed
            .iter()
            .find(|p| p.node.key == Some(gumicord_uitree::Key::Slot("after")))
            .expect("after が配置されている")
            .rect;
        assert_eq!(after_rect.y, quote_rect.y + 44.0, "引用の直後に本文が来る");
    }

    /// Padding and gaps come out of the main axis.
    ///
    /// The root always gets the whole viewport regardless of its own size, so
    /// anything whose width should apply needs a parent.
    #[test]
    fn padding_and_gap_come_out_of_the_main_axis() {
        let tree = UiNode::new(NodeId::AppScreenMain).child(
            styled(NodeId::NavChannelList, |s| {
                s.width = Some(200.0);
                s.height = Some(300.0);
                s.padding = Some(Edges::all(10.0));
                s.gap = Some(6.0);
            })
            .child(styled(NodeId::NavChannelListItem, |s| {
                s.height = Some(30.0)
            }))
            .child(styled(NodeId::NavChannelListItem, |s| {
                s.height = Some(30.0)
            })),
        );

        let r = layout(
            &tree,
            Size::new(400.0, 400.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        assert_eq!(rect_of(&r, NodeId::NavChannelList).w, 200.0);

        let items: Vec<_> = r
            .placed
            .iter()
            .filter(|p| p.node.id == NodeId::NavChannelListItem)
            .map(|p| p.rect)
            .collect();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].y, 10.0, "offset by the top padding");
        assert_eq!(items[1].y, 10.0 + 30.0 + 6.0, "separated by the gap");
        assert_eq!(items[0].x, 10.0);
        assert_eq!(items[0].w, 180.0, "fills the cross axis inside the padding");
    }

    /// A scroll region measures without a main-axis constraint, so children
    /// overflow rather than shrink.
    #[test]
    fn a_scroll_region_reports_its_overflow() {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..10 {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        let tree = UiNode::new(NodeId::ChatView).child(list);

        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        // 500px of content in a 100px box overflows by 400.
        assert_eq!(r.overflow.get(&NodeId::ChatMessageList), Some(&400.0));

        // End-anchored, so it pins to the bottom with no explicit position.
        let last = r
            .placed
            .iter()
            .rfind(|p| p.node.id == NodeId::ChatMessage)
            .unwrap();
        assert_eq!(last.rect.bottom(), 100.0);
    }

    /// Builds a message list of `n` rows.
    fn messages(n: u64) -> UiNode {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..n {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        UiNode::new(NodeId::ChatView).child(list)
    }

    /// Waiting at the bottom stays at the bottom. A pixel offset would drift
    /// upwards as content grows, hiding the row that just arrived.
    #[test]
    fn a_list_pinned_to_the_bottom_follows_new_rows() {
        let mut scroll = ScrollState::new();
        // The intent, not the offset.
        scroll.insert(NodeId::ChatMessageList, SCROLL_TO_END);

        for n in [10, 11, 20] {
            let tree = messages(n);
            let r = layout(&tree, Size::new(400.0, 100.0), &mut shaper(), &scroll);
            let last = r
                .placed
                .iter()
                .rfind(|p| p.node.id == NodeId::ChatMessage)
                .expect("a message");
            assert_eq!(last.rect.bottom(), 100.0, "not at the bottom with {n} rows");
        }
    }

    /// Under-full content still sits at the bottom; a single message used to
    /// stick to the ceiling.
    #[test]
    fn a_short_list_still_sits_at_the_bottom() {
        let tree = messages(1);
        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let only = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::ChatMessage)
            .expect("one row");
        assert_eq!(only.rect.bottom(), 100.0, "not at the bottom");
        assert_eq!(
            only.rect.y, 50.0,
            "a 50px row should sit in the lower half of a 100px box"
        );

        // Nothing overflows, so there is nothing to scroll.
        assert_eq!(r.overflow.get(&NodeId::ChatMessageList), Some(&0.0));
    }

    /// A list that does not pin to the end stays at the top.
    #[test]
    fn a_list_that_does_not_anchor_stays_at_the_top() {
        let mut list = styled(NodeId::NavGuildList, |s| s.height = Some(100.0));
        list = list.child(styled(NodeId::NavGuildListItem, |s| {
            s.width = Some(48.0);
            s.height = Some(48.0);
        }));
        let tree = UiNode::new(NodeId::AppScreenMain).child(list);

        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        let only = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::NavGuildListItem)
            .expect("one item");
        assert_eq!(only.rect.y, 0.0);
    }

    /// Stopped partway, it stays there.
    #[test]
    fn a_list_stopped_midway_stays_where_it_was() {
        let mut scroll = ScrollState::new();
        scroll.insert(NodeId::ChatMessageList, 100.0);

        let tree = messages(20);
        let r = layout(&tree, Size::new(400.0, 100.0), &mut shaper(), &scroll);
        let first = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::ChatMessage)
            .expect("a message");
        assert_eq!(first.rect.y, -100.0, "should stay 100px down");
    }

    /// The bottom of a non-pinning list is not an intent: someone there is
    /// looking at what is there, not at the bottom.
    #[test]
    fn only_lists_that_anchor_to_the_end_stick_there() {
        assert_eq!(
            remember(NodeId::ChatMessageList, 400.0, 400.0),
            SCROLL_TO_END
        );
        assert_eq!(remember(NodeId::ChatMessageList, 399.0, 400.0), 399.0);
        assert_eq!(remember(NodeId::NavGuildList, 400.0, 400.0), 400.0);
    }

    /// Scrolled children are clipped, and what leaves the box stops being
    /// hit.
    #[test]
    fn scrolled_children_are_clipped() {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..10 {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        let tree = UiNode::new(NodeId::ChatView).child(list);
        let r = layout(
            &tree,
            Size::new(400.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let first = r
            .placed
            .iter()
            .find(|p| p.node.id == NodeId::ChatMessage)
            .unwrap();
        assert!(first.rect.y < 0.0, "the first row should be above the box");
        assert_eq!(first.clip, Some(Rect::new(0.0, 0.0, 400.0, 100.0)));
        // The row scrolled out no longer hits.
        assert!(r.hit(200.0, first.rect.y + 1.0).is_none());
    }

    /// Two-pass main-axis sizing: the body's width follows the avatar's.
    #[test]
    fn the_remainder_is_computed_after_fixed_children() {
        let tree = styled(NodeId::ChatMessage, |s| s.gap = Some(8.0))
            .child(UiNode::new(NodeId::ChatMessageAvatar))
            .child(UiNode::new(NodeId::LayoutColumn));

        let r = layout(
            &tree,
            Size::new(500.0, 200.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        // The avatar defaults to 40 square.
        assert_eq!(rect_of(&r, NodeId::ChatMessageAvatar).w, 40.0);
        // The body gets what is left after the avatar and the gap.
        let col = rect_of(&r, NodeId::LayoutColumn);
        assert_eq!(col.w, 452.0);
        assert_eq!(col.x, 48.0);
    }

    /// Draw order is depth-first pre-order, which alpha blending depends on.
    #[test]
    fn placement_order_is_depth_first_pre_order() {
        let tree = UiNode::new(NodeId::AppWindow)
            .child(
                UiNode::new(NodeId::ChromeTitlebar).child(UiNode::new(NodeId::ChromeTitlebarTitle)),
            )
            .child(UiNode::new(NodeId::AppScreen));

        let r = layout(
            &tree,
            Size::new(400.0, 300.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        let ids: Vec<_> = r.placed.iter().map(|p| p.node.id).collect();
        assert_eq!(
            ids,
            vec![
                NodeId::AppWindow,
                NodeId::ChromeTitlebar,
                NodeId::ChromeTitlebarTitle,
                NodeId::AppScreen,
            ]
        );
    }

    /// An image draws itself, yet its children ride on it: the presence dot
    /// sits on its avatar, centred by the stack rules and drawn after.
    #[test]
    fn an_image_s_children_ride_on_it() {
        let tree = UiNode::new(NodeId::AppWindow).child(
            UiNode::image(NodeId::NavUserPanelAvatar, "https://example/a.png")
                .child(UiNode::new(NodeId::NavUserPanelPresence)),
        );
        let r = layout(
            &tree,
            Size::new(200.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let avatar = rect_of(&r, NodeId::NavUserPanelAvatar);
        assert_eq!((avatar.w, avatar.h), (32.0, 32.0));
        let dot = rect_of(&r, NodeId::NavUserPanelPresence);
        assert_eq!((dot.w, dot.h), (12.0, 12.0));
        // No theme, so the stack centres it.
        assert_eq!((dot.x, dot.y), (avatar.x + 10.0, avatar.y + 10.0));

        // After the avatar, so it paints over the image.
        let at = |id| r.placed.iter().position(|p| p.node.id == id).unwrap();
        assert!(at(NodeId::NavUserPanelPresence) > at(NodeId::NavUserPanelAvatar));
    }

    /// Any other drawing node stays a leaf: its children are ignored.
    #[test]
    fn other_drawing_nodes_stay_leaves() {
        let tree = UiNode::new(NodeId::AppWindow).child(
            UiNode::text(NodeId::ChatMessageContent, "hi")
                .child(UiNode::new(NodeId::NavUserPanelPresence)),
        );
        let r = layout(
            &tree,
            Size::new(200.0, 100.0),
            &mut shaper(),
            &ScrollState::new(),
        );
        assert!(
            !r.placed
                .iter()
                .any(|p| p.node.id == NodeId::NavUserPanelPresence)
        );
    }

    /// Text wraps, so a narrower wrap width means more height.
    ///
    /// ASCII only: the CI runner may have no Japanese font. Pinning Japanese
    /// shaping needs the bundled font first.
    #[test]
    fn narrow_text_wraps_and_grows_taller() {
        let mut s = shaper();
        let font = ResolvedFont::from_style(&Style::default());
        if s.measure("MMMMMMMM", &font, None).w == 0.0 {
            // No usable font at all; shaping cannot happen, so assert
            // nothing.
            eprintln!("フォントが見つからないため、この試験は飛ばす");
            return;
        }

        let long = "The quick brown fox jumps over the lazy dog. \
                    Pack my box with five dozen liquor jugs.";
        let mut make = |w: f32| {
            let tree = styled(NodeId::ChatMessageList, |s| s.width = Some(w))
                .child(UiNode::text(NodeId::ChatMessageContent, long));
            let r = layout(&tree, Size::new(w, 2000.0), &mut s, &ScrollState::new());
            rect_of(&r, NodeId::ChatMessageContent).h
        };

        let wide = make(1000.0);
        let narrow = make(200.0);
        assert!(
            narrow > wide,
            "narrower should be taller ({narrow} <= {wide})"
        );
    }
}

#[cfg(test)]
mod scrollbar_tests {
    use gumicord_uitree::Style;

    use super::*;

    fn shaper() -> Shaper {
        Shaper::new(1.0)
    }

    fn styled(id: NodeId, f: impl FnOnce(&mut Style)) -> UiNode {
        let mut n = UiNode::new(id);
        f(&mut n.style);
        n
    }

    // ═══════════════════════════════════════════════════════════════
    //  Floating surfaces

    /// Floats a `w` by `h` box at `(ax, ay)` in a 400 by 300 window.
    fn floated(ax: f32, ay: f32, w: f32, h: f32) -> Rect {
        let tree = UiNode::new(NodeId::AppRoot).child(
            styled(NodeId::OverlayLayer, |_| {}).child(
                styled(NodeId::OverlayPopover, |st| {
                    st.width = Some(w);
                    st.height = Some(h);
                })
                .with_anchor(gumicord_uitree::Anchor::at(ax, ay)),
            ),
        );
        let r = layout(
            &tree,
            Size::new(400.0, 300.0),
            &mut shaper(),
            &ScrollState::default(),
        );
        r.find(NodeId::OverlayPopover).expect("not placed").rect
    }

    /// With room, it grows down and right from the press.
    #[test]
    fn a_floating_box_grows_down_and_right_from_its_anchor() {
        let r = floated(50.0, 60.0, 100.0, 80.0);
        assert_eq!((r.x, r.y), (50.0, 60.0));
        assert_eq!((r.w, r.h), (100.0, 80.0));
    }

    /// A menu at the edge flips, or its items sit off screen: visible but
    /// unreachable.
    #[test]
    fn it_flips_when_pressed_near_the_bottom_right() {
        // A 100-wide box does not fit 20px from the right edge.
        let r = floated(380.0, 290.0, 100.0, 80.0);
        assert_eq!(r.right(), 380.0, "grew right and off screen");
        assert_eq!(r.bottom(), 290.0, "grew down and off screen");
    }

    /// Only the axis that overflows flips.
    #[test]
    fn only_the_axis_that_overflows_flips() {
        let r = floated(380.0, 10.0, 100.0, 80.0);
        assert_eq!(r.right(), 380.0, "should flip horizontally");
        assert_eq!(r.y, 10.0, "should not flip vertically");
    }

    /// What still does not fit after flipping is clamped: a box larger than
    /// its container overflows either way.
    #[test]
    fn it_is_pushed_inside_when_flipping_is_not_enough() {
        let r = floated(200.0, 150.0, 500.0, 400.0);
        assert_eq!((r.x, r.y), (0.0, 0.0), "should clamp to the top left");
    }

    /// An anchored child does not advance the flow.
    ///
    /// Advancing it would push everything after it by the box's size.
    #[test]
    fn a_floating_child_does_not_advance_the_flow() {
        let row = |float: bool| {
            let mut n = styled(NodeId::LayoutColumn, |st| st.height = Some(300.0))
                .child(styled(NodeId::ChatMessage, |st| st.height = Some(50.0)));
            if float {
                n = n.child(
                    styled(NodeId::OverlayPopover, |st| {
                        st.width = Some(80.0);
                        st.height = Some(80.0);
                    })
                    .with_anchor(gumicord_uitree::Anchor::at(10.0, 10.0)),
                );
            }
            n.child(styled(NodeId::ChatInput, |st| st.height = Some(40.0)))
        };
        let y = |t: &UiNode| {
            layout(
                t,
                Size::new(400.0, 300.0),
                &mut shaper(),
                &ScrollState::default(),
            )
            .find(NodeId::ChatInput)
            .expect("not placed")
            .rect
            .y
        };
        assert_eq!(y(&row(false)), y(&row(true)));
    }

    /// `n` rows of 50px plus a scrollbar, in a 100px box.
    fn list(n: u64, scroll: Option<f32>) -> (UiNode, ScrollState) {
        let mut list = styled(NodeId::ChatMessageList, |s| s.height = Some(100.0));
        for i in 0..n {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        list = list.child(
            styled(NodeId::LayoutScrollbar, |s| s.width = Some(10.0))
                .child(UiNode::new(NodeId::LayoutScrollbarThumb)),
        );

        let mut state = ScrollState::new();
        if let Some(at) = scroll {
            state.insert(NodeId::ChatMessageList, at);
        }
        (UiNode::new(NodeId::ChatView).child(list), state)
    }

    fn place<'a>(tree: &'a UiNode, state: &ScrollState) -> LayoutResult<'a> {
        layout(tree, Size::new(400.0, 100.0), &mut shaper(), state)
    }

    /// A scrollbar stays out of the flow and does not change the height.
    #[test]
    fn a_scrollbar_does_not_consume_the_main_axis() {
        let (with_bar, state) = list(10, None);
        let over_with = *place(&with_bar, &state)
            .overflow
            .get(&NodeId::ChatMessageList)
            .unwrap();

        // 500px of content in a 100px box overflows by 400; a scrollbar in
        // the flow would change that.
        assert_eq!(over_with, 400.0);
    }

    /// Placed at the edge, and does not scroll with the content.
    #[test]
    fn the_scrollbar_sits_on_the_trailing_edge() {
        let (tree, state) = list(10, None);
        let r = place(&tree, &state);
        let bar = r.find(NodeId::LayoutScrollbar).expect("not placed");

        assert_eq!(bar.rect.w, 10.0);
        assert_eq!(bar.rect.right(), 400.0, "右端に付く");
        assert_eq!(bar.rect.y, 0.0, "スクロールしても上端のまま");
        assert_eq!(bar.rect.h, 100.0, "枠いっぱいの高さ");
    }

    /// A badge stays content-sized even when stacked; filling turned it into
    /// a red circle covering the icon.
    #[test]
    fn a_badge_does_not_fill_the_stack_it_sits_on() {
        let item = styled(NodeId::NavGuildListItem, |s| {
            s.width = Some(56.0);
            s.height = Some(48.0);
        })
        .child(UiNode::new(NodeId::NavGuildListItemIcon))
        .child(UiNode::text(NodeId::NavGuildListItemBadge, "1".to_owned()));

        let r = layout(
            &item,
            Size::new(56.0, 48.0),
            &mut shaper(),
            &ScrollState::new(),
        );

        let icon = r.find(NodeId::NavGuildListItemIcon).expect("絵がある").rect;
        assert_eq!((icon.w, icon.h), (56.0, 48.0), "絵は入れ物いっぱい");

        let badge = r
            .find(NodeId::NavGuildListItemBadge)
            .expect("印がある")
            .rect;
        assert!(badge.w < 56.0, "印は数字のぶんだけ: {}", badge.w);
        assert!(badge.h < 48.0, "印は 1 行ぶんだけ: {}", badge.h);
    }

    /// Not pulled inside the padding, which would put it on top of the
    /// content.
    #[test]
    fn the_scrollbar_ignores_the_padding() {
        let mut list = styled(NodeId::ChatMessageList, |s| {
            s.height = Some(100.0);
            s.padding = Some(Edges::all(12.0));
        });
        for i in 0..10 {
            list =
                list.child(styled(NodeId::ChatMessage, |s| s.height = Some(50.0)).with_id_key(i));
        }
        list = list.child(
            styled(NodeId::LayoutScrollbar, |s| s.width = Some(10.0))
                .child(UiNode::new(NodeId::LayoutScrollbarThumb)),
        );
        let tree = UiNode::new(NodeId::ChatView).child(list);

        let r = place(&tree, &ScrollState::new());
        let bar = r.find(NodeId::LayoutScrollbar).expect("not placed");

        assert_eq!(bar.rect.right(), 400.0, "余白があっても外縁に付く");
        assert_eq!(bar.rect.h, 100.0, "高さも余白を引かない");

        // The content's clip would remove it entirely.
        if let Some(c) = bar.clip {
            assert!(
                !c.intersect(bar.rect).is_empty(),
                "切り取られて何も描かれない: clip={c:?} bar={:?}",
                bar.rect
            );
        }
        let thumb = r.find(NodeId::LayoutScrollbarThumb).expect("摘みがある");
        if let Some(c) = thumb.clip {
            assert!(!c.intersect(thumb.rect).is_empty(), "摘みも消えている");
        }
    }

    /// The thumb's size is the visible fraction; its position follows the
    /// offset.
    #[test]
    fn the_thumb_reflects_how_far_we_are() {
        // At the start.
        let (tree, state) = list(10, Some(0.0));
        let r = place(&tree, &state);
        let top = r.find(NodeId::LayoutScrollbarThumb).unwrap().rect;
        // 100 of 500 visible, so the thumb is a fifth.
        assert_eq!(top.h, 20.0_f32.max(MIN_THUMB));
        assert_eq!(top.y, 0.0);

        // At the end.
        let (tree, state) = list(10, Some(f32::MAX));
        let r = place(&tree, &state);
        let bottom = r.find(NodeId::LayoutScrollbarThumb).unwrap().rect;
        assert_eq!(bottom.bottom(), 100.0, "下端まで来る");
        assert_eq!(bottom.h, top.h, "大きさは変わらない");
    }

    /// Nothing to scroll means no scrollbar; an immobile one lies.
    #[test]
    fn no_scrollbar_when_nothing_overflows() {
        let (tree, state) = list(1, None);
        let r = place(&tree, &state);
        assert!(r.find(NodeId::LayoutScrollbar).is_none());
        assert!(r.find(NodeId::LayoutScrollbarThumb).is_none());
    }
}
