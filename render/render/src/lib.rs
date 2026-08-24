//! The renderer: takes a UITree and emits GPU draw commands.
//!
//! Contains no platform-specific code; anything touching the OS belongs in
//! the platform crate.
//!
//! There are three primitives: rounded rects, textured quads, and clip rects.
//!
//! No compute shaders, which would rule out the GL and GLES backends. On the
//! machine this was measured on, DX12 held sixteen times the resident memory
//! GL did.

pub mod draw;
pub mod geom;
pub mod gpu;
pub mod icon;
pub mod intrinsic;
pub mod layout;
pub mod motion;
pub mod text;

pub use geom::{Rect, Size};
pub use gpu::{GpuError, Presented};
pub use intrinsic::{Axis, Cross, Intrinsic, intrinsic};
pub use layout::{SCROLL_TO_END, ScrollBar, ScrollState};
pub use motion::Motion;
pub use text::ImageData;

use gumicord_uitree::{Key, NodeId, UiNode};

use crate::gpu::Gpu;
use crate::text::TextEngine;

/// What one frame drew, for watching performance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub nodes: usize,
    pub rects: u32,
    pub glyphs: u32,
    /// Draw calls; one more per pipeline or clip change.
    pub draw_calls: usize,
    /// Whether it reached the screen; a failure asks for another redraw.
    pub presented: Presented,
}

/// One hit.
///
/// A placed node borrows the tree and cannot outlive the frame, so this copies
/// just what hit testing needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub id: NodeId,
    pub key: Option<Key>,
    pub rect: Rect,
    /// Anything outside this does not hit; a row scrolled out of view must
    /// not respond.
    pub clip: Option<Rect>,
}

/// A scrollbar being dragged.
///
/// Holds the measurements taken when it was grabbed, so a relayout mid-drag
/// does not pull the thumb out from under the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollGrab {
    bar: ScrollBar,
    /// From the thumb's top edge to where it was grabbed.
    grab: f32,
}

impl ScrollGrab {
    /// The region it drives.
    pub fn owner(&self) -> NodeId {
        self.bar.owner
    }
}

/// Draws a UITree.
pub struct Renderer {
    gpu: Gpu,
    text: TextEngine,
    /// Per-page bind groups, rebuilt when a page is added.
    atlas_binds: Vec<wgpu::BindGroup>,
    scale: f32,
    scroll: ScrollState,
    /// The previous frame's layout, for hit testing.
    hits: Vec<Hit>,
    /// Overflow per scroll region.
    overflow: std::collections::HashMap<NodeId, f32>,
    /// Scrollbars placed last frame.
    scrollbars: Vec<ScrollBar>,
    /// A region that grew at the top and should hold its position, for one
    /// frame.
    keep_place: Option<NodeId>,
    /// Images the last frame wanted and did not have. Only drawing reveals
    /// them, since visibility comes from layout and clipping.
    missing_images: Vec<String>,
    /// Whether the caret is lit. The blink is timed by the platform layer,
    /// since the rate is an OS setting and can be off entirely.
    caret_visible: bool,
}

impl Renderer {
    pub fn new(
        target: wgpu::SurfaceTarget<'static>,
        width: u32,
        height: u32,
        scale: f32,
    ) -> Result<Self, GpuError> {
        let gpu = Gpu::new(target, width, height)?;
        let text = TextEngine::new(&gpu.device, scale);
        let atlas_binds = bind_pages(&gpu, &text);
        Ok(Renderer {
            gpu,
            text,
            atlas_binds,
            scale,
            scroll: ScrollState::new(),
            hits: Vec::new(),
            overflow: std::collections::HashMap::new(),
            scrollbars: Vec::new(),
            keep_place: None,
            missing_images: Vec::new(),
            caret_visible: true,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.gpu.resize(width, height);
    }

    /// The DPI changed. Glyphs are rasterised in physical pixels, so the
    /// atlas is rebuilt.
    pub fn set_scale(&mut self, scale: f32) {
        if (scale - self.scale).abs() < f32::EPSILON {
            return;
        }
        self.scale = scale;
        self.text.set_scale(&self.gpu.device, scale);
        self.atlas_binds = bind_pages(&self.gpu, &self.text);
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The viewport, which themes also match `when.maxWidth` against.
    pub fn viewport(&self) -> Size {
        let (w, h) = self.gpu.size();
        Size::new(w as f32 / self.scale, h as f32 / self.scale)
    }

    pub fn backend(&self) -> wgpu::Backend {
        self.gpu.backend
    }

    pub fn adapter_name(&self) -> &str {
        &self.gpu.adapter_name
    }

    /// Scrolls, and reports whether a redraw is needed.
    ///
    /// Bounded by the previous frame's overflow: a frame behind, but measuring
    /// after moving would cost an extra layout.
    ///
    /// Tiny movements still accumulate. A precision touchpad sends one gesture
    /// as many small deltas, and discarding them means nothing happens while
    /// the finger moves. Only the redraw request is skipped.
    pub fn scroll_by(&mut self, id: NodeId, delta: f32) -> bool {
        let max = self.overflow.get(&id).copied().unwrap_or(0.0);
        if max <= 0.0 || !delta.is_finite() {
            return false;
        }
        // An untouched region starts at its default anchor.
        let default = if intrinsic(id).anchor_end { max } else { 0.0 };
        let cur = self
            .scroll
            .get(&id)
            .copied()
            .unwrap_or(default)
            .clamp(0.0, max);

        let next = (cur + delta).clamp(0.0, max);
        self.scroll.insert(id, layout::remember(id, next, max));

        // Sub-half-pixel movement looks identical redrawn.
        (next - cur).abs() >= 0.5
    }

    /// The current position and overflow, both in the previous frame's terms.
    /// A remembered "bottom" is resolved to pixels here.
    pub fn scroll_place(&self, id: NodeId) -> (f32, f32) {
        let max = self.overflow.get(&id).copied().unwrap_or(0.0);
        let default = if intrinsic(id).anchor_end { max } else { 0.0 };
        let at = self
            .scroll
            .get(&id)
            .copied()
            .unwrap_or(default)
            .clamp(0.0, max);
        (at, max)
    }

    /// Sets the position directly.
    pub fn set_scroll(&mut self, id: NodeId, at: f32) {
        self.scroll.insert(id, at);
    }

    /// Holds the scroll position across a prepend, for one frame.
    ///
    /// The caller says so: the renderer cannot tell which end grew, and
    /// applying this to an append would move what is being read.
    pub fn keep_place(&mut self, id: NodeId) {
        self.keep_place = Some(id);
    }

    /// Grabs a scrollbar.
    ///
    /// On the thumb it grabs in place; on the track it jumps the thumb's
    /// centre there first, so dragging works from the same press.
    pub fn grab_scrollbar(&mut self, x: f32, y: f32) -> Option<ScrollGrab> {
        let bar = *self.scrollbars.iter().find(|b| b.track.contains(x, y))?;

        let grab = if bar.thumb.contains(x, y) {
            y - bar.thumb.y
        } else {
            bar.thumb.h * 0.5
        };
        let grab = ScrollGrab { bar, grab };

        self.drag_scrollbar(&grab, y);
        Some(grab)
    }

    /// Drags a grabbed scrollbar. The thumb's travel is the track minus the
    /// thumb, which maps to the whole overflow.
    pub fn drag_scrollbar(&mut self, grab: &ScrollGrab, y: f32) -> bool {
        let max = self.overflow.get(&grab.bar.owner).copied().unwrap_or(0.0);
        let travel = grab.bar.track.h - grab.bar.thumb.h;
        if max <= 0.0 || travel <= 0.0 {
            return false;
        }

        let t = ((y - grab.grab - grab.bar.track.y) / travel).clamp(0.0, 1.0);
        let next = t * max;
        let cur = self
            .scroll
            .get(&grab.bar.owner)
            .copied()
            .unwrap_or(0.0)
            .clamp(0.0, max);
        self.scroll
            .insert(grab.bar.owner, layout::remember(grab.bar.owner, next, max));
        (next - cur).abs() >= 0.5
    }

    /// Sets whether the caret is drawn; the blinking is the platform layer's.
    pub fn set_caret_visible(&mut self, visible: bool) {
        self.caret_visible = visible;
    }

    /// Draws one frame. The tree must already have its style resolved.
    pub fn render(&mut self, root: &UiNode) -> FrameStats {
        let viewport = self.viewport();
        let mut layout = layout::layout(root, viewport, self.text.shaper(), &self.scroll);

        // Shift the position down by however much was prepended.
        //
        // Positions are measured from the top, so prepending pushes the row
        // being read downwards — which defeats the point of paging back.
        //
        // The growth is the change in overflow, since the box did not change
        // size.
        //
        // Measured again rather than corrected after drawing, which would
        // jump for one frame. Prepends are rare enough to pay for two passes.
        if let Some(id) = self.keep_place.take()
            && let (Some(before), Some(after)) =
                (self.overflow.get(&id).copied(), layout.overflow.get(&id))
        {
            let grew = after - before;
            let at = self.scroll.get(&id).copied().unwrap_or(0.0);
            // Someone pinned to the bottom stays there; the intent wins.
            if grew > 0.0 && at != layout::SCROLL_TO_END {
                self.scroll.insert(id, (at + grew).min(*after));
                layout = layout::layout(root, viewport, self.text.shaper(), &self.scroll);
            }
        }

        self.hits.clear();
        self.hits.extend(layout.placed.iter().map(|p| Hit {
            id: p.node.id,
            key: p.node.key.clone(),
            rect: p.rect,
            clip: p.clip,
        }));
        self.overflow.clone_from(&layout.overflow);
        self.scrollbars.clone_from(&layout.scrollbars);

        let dl = draw::build(
            &layout,
            &mut self.text,
            &self.gpu.device,
            &self.gpu.queue,
            self.scale,
            self.gpu.size(),
            self.caret_visible,
        );

        self.missing_images.clone_from(&dl.missing_images);

        FrameStats {
            nodes: layout.placed.len(),
            rects: dl.rect_count(),
            glyphs: dl.glyph_count(),
            draw_calls: dl.runs.len(),
            presented: self.gpu.submit(&dl, &self.atlas_binds, CLEAR),
        }
    }

    /// The previous frame's placed nodes, for when the question is where a
    /// given ID landed rather than what is under a point.
    pub fn hit_boxes(&self) -> &[Hit] {
        &self.hits
    }

    /// Nodes under a point, front to back, against the last frame's layout.
    pub fn hit_test(&self, x: f32, y: f32) -> impl Iterator<Item = &Hit> {
        self.hits
            .iter()
            .rev()
            .filter(move |h| h.rect.contains(x, y) && h.clip.is_none_or(|c| c.contains(x, y)))
    }
}

/// The colour before the first frame; the theme covers it immediately.
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

impl Renderer {
    /// Puts a fetched image in the atlas. The renderer never touches the
    /// network; this arrives already decoded.
    ///
    /// A failure to fit is ignored: the image does not appear, and nothing
    /// else breaks.
    pub fn put_image(&mut self, image: &crate::text::ImageData) {
        self.text
            .put_image(&self.gpu.device, &self.gpu.queue, image);
        // A new page needs rebinding, or its texture can never be selected.
        if self.text.took_atlas_growth() {
            self.atlas_binds = bind_pages(&self.gpu, &self.text);
        }
    }

    /// Images that were about to draw and were missing, for the fetcher.
    /// Only what survived clipping.
    pub fn missing_images(&self) -> &[String] {
        &self.missing_images
    }

    /// Whether images were forgotten; true once.
    ///
    /// The image side is cleared and repacked when it fills. The fetcher must
    /// re-add them, or they never come back.
    pub fn took_image_recycle(&mut self) -> bool {
        self.text.took_image_recycle()
    }

    /// Whether the image is already held, which decides whether to fetch it.
    pub fn has_image(&self, url: &str) -> bool {
        self.text.has_image(url)
    }
}

/// Builds one bind group per atlas page; each can name only one texture.
fn bind_pages(gpu: &Gpu, text: &TextEngine) -> Vec<wgpu::BindGroup> {
    text.atlas_views()
        .into_iter()
        .map(|v| gpu.atlas_bind_group(v))
        .collect()
}

/// Lays out a tree without a GPU, for tests and diagnostics. Rebuilds the
/// shaper each call, so it is slow, and drawing never goes through it.
#[doc(hidden)]
pub fn layout_for_test(
    tree: &gumicord_uitree::UiNode,
    viewport: Size,
) -> Vec<(gumicord_uitree::NodeId, crate::geom::Rect)> {
    let mut shaper = crate::text::Shaper::new(1.0);
    let r = crate::layout::layout(tree, viewport, &mut shaper, &ScrollState::default());
    r.placed.iter().map(|p| (p.node.id, p.rect)).collect()
}
