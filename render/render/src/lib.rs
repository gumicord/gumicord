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

use gumicord_uitree::{Content, Key, NodeId, UiNode};

use crate::gpu::Gpu;
use crate::text::{RunRect, Shaper, TextEngine};

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

/// One pressable run of text.
///
/// A link lives inside a [`Content::Rich`] node and must not become a node of
/// its own — siblings wrap independently, which would break the line. So the
/// press target is recovered from shaping instead: one rect per line the run
/// landed on.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkHit {
    pub url: String,
    /// Logical px from the window's top left, over exactly what was drawn.
    pub rect: Rect,
    pub clip: Option<Rect>,
}

/// One covered spoiler run, which pressing opens.
///
/// The same shape problem as a link's, answered the same way. The run is
/// named by its message and its place among that message's spoiler runs,
/// because those are what survive a redraw — the rects do not.
#[derive(Debug, Clone, PartialEq)]
pub struct SpoilerHit {
    /// The `data` of the node carrying it; a message's id here.
    pub owner: u64,
    /// Which spoiler run of that message, counting every one whether open or
    /// not.
    pub no: usize,
    pub rect: Rect,
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
    /// The previous frame's pressable runs, for link presses and hover.
    links: Vec<LinkHit>,
    /// The previous frame's covered spoiler runs, for opening one alone.
    spoilers: Vec<SpoilerHit>,
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
    /// Theme background images the last frame wanted and did not have.
    missing_backgrounds: Vec<String>,
    /// Which theme background images belong to. The app sets it on every
    /// theme load; drawing needs it to key bundled paths and data URIs.
    theme_namespace: Option<String>,
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
        wake: Box<dyn Fn() + Send + Sync + 'static>,
    ) -> Result<Self, GpuError> {
        let gpu = Gpu::new(target, width, height)?;
        let text = TextEngine::new(&gpu.device, scale, wake);
        let atlas_binds = bind_pages(&gpu, &text);
        Ok(Renderer {
            gpu,
            text,
            atlas_binds,
            scale,
            scroll: ScrollState::new(),
            hits: Vec::new(),
            links: Vec::new(),
            spoilers: Vec::new(),
            overflow: std::collections::HashMap::new(),
            scrollbars: Vec::new(),
            keep_place: None,
            missing_images: Vec::new(),
            missing_backgrounds: Vec::new(),
            theme_namespace: None,
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

    /// Whether system fonts are waiting to be folded in. A true result means
    /// a redraw should be requested so [`Renderer::process_font_update`] can
    /// apply them.
    pub fn fonts_pending(&self) -> bool {
        self.text.fonts_pending()
    }

    /// Folds in the system fonts the background thread enumerated, if they
    /// have arrived, and rebuilds the glyph atlas. True means the layout has
    /// changed and the following draw will re-shape with the full font set.
    pub fn process_font_update(&mut self) -> bool {
        if self.text.system_fonts(&self.gpu.device) {
            self.atlas_binds = bind_pages(&self.gpu, &self.text);
            true
        } else {
            false
        }
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
        // Drop shapes no frame uses any more, or a resize drag leaves one
        // copy of every text behind per wrap width it passed through.
        self.text.shaper().sweep();
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
        // Shaping already happened in layout; this only reads the cache.
        self.links.clear();
        self.spoilers.clear();
        let shaper = self.text.shaper();
        let (links, spoilers) = collect_pressables(&layout.placed, self.scale, shaper);
        self.links.extend(links);
        self.spoilers.extend(spoilers);
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
            self.theme_namespace.as_deref(),
        );

        self.missing_images.clone_from(&dl.missing_images);
        self.missing_backgrounds.clone_from(&dl.missing_backgrounds);

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

    /// The link under a point, if any, against the last frame's layout.
    ///
    /// Whatever was drawn last wins, so an overlap answers with the front one.
    pub fn link_at(&self, x: f32, y: f32) -> Option<&str> {
        link_hit(&self.links, x, y).map(|l| l.url.as_str())
    }

    /// The covered spoiler run under a point: which message and which of its
    /// spoiler runs. An open one still answers here — pressing again covers
    /// it — but its link, if any, answers to [`Self::link_at`] instead.
    pub fn spoiler_at(&self, x: f32, y: f32) -> Option<(u64, usize)> {
        spoiler_hit(&self.spoilers, x, y).map(|s| (s.owner, s.no))
    }
}

/// Collects the pressable runs from one frame's layout.
///
/// Runs were already shaped during layout, so this is a cache lookup per text
/// node that carries any. The rects land where drawing puts them: same wrap
/// width, same vertical centring.
///
/// Spoiler runs are numbered while walking in draw order, which is the order
/// the builder produced them in — that is what makes the numbers two sides
/// agree on.
fn collect_pressables(
    placed: &[layout::Placed<'_>],
    scale: f32,
    shaper: &mut Shaper,
) -> (Vec<LinkHit>, Vec<SpoilerHit>) {
    let mut links = Vec::new();
    let mut spoilers: Vec<SpoilerHit> = Vec::new();
    // Spoiler runs numbered so far, per message. A run that wrapped has one
    // rect per line but one number.
    let mut numbered: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    for p in placed {
        let Content::Rich(spans) = &p.node.content else {
            continue;
        };
        let owner = p.node.data.as_ref().map(|d| d.id);
        if !spans.iter().any(|s| s.link.is_some() || s.hidden) {
            continue;
        }

        // Numbered whether open or not, in span order, so an opened one
        // cannot reshuffle its neighbours; and only where a name exists to
        // hang the state on.
        let mut numbers: Vec<(usize, usize)> = Vec::new();
        if let Some(owner) = owner {
            let next = numbered.entry(owner).or_default();
            for (i, s) in spans.iter().enumerate() {
                if s.hidden {
                    numbers.push((i, *next));
                    *next += 1;
                }
            }
        }

        let runs = draw::rich_runs(spans, &p.node.style);
        // The same wrap width drawing used, or the lines differ.
        let max_w = p.inner.w.is_finite().then_some(p.inner.w);
        let (size, run_rects): (_, Vec<RunRect>) = {
            let shaped = shaper.shape_rich(&runs, max_w);
            (shaped.size, shaped.runs.clone())
        };
        // Drawing centres the block when the box is taller than the text.
        let oy = p.inner.y + ((p.inner.h - size.h) * 0.5).max(0.0);
        for r in run_rects {
            let Some(sp) = spans.get(r.run as usize) else {
                continue;
            };
            let rect = Rect {
                x: p.inner.x + r.rect.x / scale,
                y: oy + r.rect.y / scale,
                w: r.rect.w / scale,
                h: r.rect.h / scale,
            };
            // A covered run keeps its space but is not pressable as a link
            // until it is open.
            if let Some(url) = &sp.link
                && !sp.concealed()
            {
                links.push(LinkHit {
                    url: url.clone(),
                    rect,
                    clip: p.clip,
                });
            }
            if sp.hidden
                && let Some(owner) = owner
                && let Some((_, no)) = numbers.iter().find(|(i, _)| *i == r.run as usize)
            {
                spoilers.push(SpoilerHit {
                    owner,
                    no: *no,
                    rect,
                    clip: p.clip,
                });
            }
        }
    }
    (links, spoilers)
}

/// The frontmost link holding a point.
fn link_hit(links: &[LinkHit], x: f32, y: f32) -> Option<&LinkHit> {
    links
        .iter()
        .rev()
        .find(|l| l.rect.contains(x, y) && l.clip.is_none_or(|c| c.contains(x, y)))
}

/// The frontmost covered spoiler run holding a point; same rules as links.
fn spoiler_hit(spoilers: &[SpoilerHit], x: f32, y: f32) -> Option<&SpoilerHit> {
    spoilers
        .iter()
        .rev()
        .find(|s| s.rect.contains(x, y) && s.clip.is_none_or(|c| c.contains(x, y)))
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

    /// Theme background images that were about to draw and were missing.
    pub fn missing_backgrounds(&self) -> &[String] {
        &self.missing_backgrounds
    }

    /// Which theme the background images currently drawing belong to.
    pub fn set_theme_namespace(&mut self, namespace: Option<&str>) {
        if self.theme_namespace.as_deref() != namespace {
            self.theme_namespace = namespace.map(str::to_owned);
        }
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

#[cfg(test)]
mod press_tests {
    use super::*;
    use gumicord_uitree::{NodeId, Span};

    fn span(text: &str, url: Option<&str>, hidden: bool) -> Span {
        Span {
            text: text.to_owned(),
            link: url.map(str::to_owned),
            hidden,
            ..Span::default()
        }
    }

    /// Marks a covered run as opened; pressing it again covers it back.
    fn opened(mut s: Span) -> Span {
        s.revealed = true;
        s
    }

    fn pressables_for(tree: &UiNode, viewport: (f32, f32)) -> (Vec<LinkHit>, Vec<SpoilerHit>) {
        let mut shaper = Shaper::new(1.0);
        let l = layout::layout(
            tree,
            Size::new(viewport.0, viewport.1),
            &mut shaper,
            &ScrollState::default(),
        );
        collect_pressables(&l.placed, 1.0, &mut shaper)
    }

    fn links_for(tree: &UiNode, viewport: (f32, f32)) -> Vec<LinkHit> {
        pressables_for(tree, viewport).0
    }

    fn spoilers_for(tree: &UiNode, viewport: (f32, f32)) -> Vec<SpoilerHit> {
        pressables_for(tree, viewport).1
    }

    fn text_node(spans: Vec<Span>) -> UiNode {
        UiNode::new(NodeId::PrimitiveText).with_content(Content::Rich(spans))
    }

    /// A message body: the only place a run has a message to hang state on.
    fn message_node(id: u64, spans: Vec<Span>) -> UiNode {
        UiNode::new(NodeId::ChatMessageContent)
            .with_data(id)
            .with_content(Content::Rich(spans))
    }

    /// A press lands on what was drawn, and nothing around it.
    #[test]
    fn a_link_answers_where_it_is_drawn() {
        let tree = text_node(vec![
            span("look at ", None, false),
            span(
                "https://example.com/a",
                Some("https://example.com/a"),
                false,
            ),
            span(" now", None, false),
        ]);
        let links = links_for(&tree, (600.0, 400.0));
        assert_eq!(links.len(), 1, "{links:?}");

        let r = links[0].rect;
        assert!(
            r.x > 0.0 && r.right() < 600.0,
            "the run should sit after its prefix, at {r:?}"
        );
        let cx = r.x + r.w / 2.0;
        assert!(link_hit(&links, cx, r.y + 2.0).is_some());
        // Left of the whole node: plain text.
        assert!(link_hit(&links, 1.0, r.y + 2.0).is_none());
    }

    /// Colour alone decorates; only a target is pressable.
    #[test]
    fn plain_text_yields_nothing_to_press() {
        let tree = text_node(vec![span("just words", None, false)]);
        assert!(links_for(&tree, (600.0, 400.0)).is_empty());
    }

    /// A hidden run keeps its space but must not open anything: what is
    /// under there is not known yet.
    #[test]
    fn a_hidden_run_is_not_pressable() {
        let tree = text_node(vec![span(
            "https://example.com/s",
            Some("https://example.com/s"),
            true,
        )]);
        let links = links_for(&tree, (600.0, 400.0));
        assert!(links.is_empty(), "{links:?}");
    }

    /// Revealing makes it pressable again.
    #[test]
    fn a_revealed_run_presses_normally() {
        let tree = text_node(vec![span(
            "https://example.com/s",
            Some("https://example.com/s"),
            false,
        )]);
        assert_eq!(links_for(&tree, (600.0, 400.0)).len(), 1);
    }

    /// One rect per line: both halves of a wrapped URL answer.
    #[test]
    fn a_wrapped_link_presses_on_every_line_it_lands_on() {
        let long = format!("https://example.com/{}", "p".repeat(80));
        let tree = text_node(vec![span(&long, Some(&long), false)]);
        let links = links_for(&tree, (200.0, 400.0));
        assert!(links.len() >= 2, "{links:?}");
        for (i, l) in links.iter().enumerate() {
            assert_eq!(l.url, long);
            if i > 0 {
                assert!(
                    l.rect.y > links[i - 1].rect.y,
                    "lines should stack downward"
                );
            }
        }
        // The middle of each line hits.
        for l in &links {
            let mid = (l.rect.x + l.rect.w / 2.0, l.rect.y + l.rect.h / 2.0);
            assert_eq!(
                link_hit(&links, mid.0, mid.1).map(|h| h.url.as_str()),
                Some(long.as_str())
            );
        }
    }

    /// Whatever is drawn last wins an overlap.
    #[test]
    fn the_frontmost_layer_wins_an_overlap() {
        use gumicord_uitree::NodeId as Id;
        let under = text_node(vec![span("xxxx", Some("https://under.test"), false)]);
        let over = text_node(vec![span("xxxx", Some("https://over.test"), false)]);
        let tree = UiNode::new(Id::LayoutStack).child(under).child(over);
        let links = links_for(&tree, (600.0, 400.0));
        assert_eq!(links.len(), 2, "{links:?}");
        let top = links.last().unwrap();
        let mid = (top.rect.x + top.rect.w / 2.0, top.rect.y + top.rect.h / 2.0);
        assert_eq!(
            link_hit(&links, mid.0, mid.1).map(|h| h.url.as_str()),
            Some("https://over.test")
        );
    }

    /// A covered run answers where it landed, named by message and its place
    /// among that message's runs; the plain text between answers nothing.
    #[test]
    fn a_covered_run_answers_where_it_lands() {
        let tree = message_node(
            42,
            vec![
                span("one", None, true),
                span(" plain ", None, false),
                span("two", None, true),
            ],
        );
        let spoilers = spoilers_for(&tree, (600.0, 400.0));
        assert_eq!(spoilers.len(), 2, "{spoilers:?}");
        for (want_no, s) in spoilers.iter().enumerate() {
            assert_eq!(s.owner, 42);
            assert_eq!(s.no, want_no, "{spoilers:?}");
            let mid = (s.rect.x + s.rect.w / 2.0, s.rect.y + s.rect.h / 2.0);
            assert_eq!(
                spoiler_hit(&spoilers, mid.0, mid.1).map(|h| h.no),
                Some(want_no)
            );
        }
        // Between the two runs: not covered, not pressable.
        assert!(
            spoiler_hit(
                &spoilers,
                spoilers[0].rect.right() + 5.0,
                spoilers[0].rect.y + 2.0
            )
            .is_none()
        );
    }

    /// A run without a message behind it has no name to keep state under, so
    /// it never becomes a target.
    #[test]
    fn a_run_without_an_owner_is_not_a_target() {
        let tree = text_node(vec![span("covered", None, true)]);
        assert!(spoilers_for(&tree, (600.0, 400.0)).is_empty());
    }

    /// One run that wrapped keeps one number on every line it lands on; the
    /// app would otherwise open one line and leave its own tail covered.
    #[test]
    fn a_wrapped_run_keeps_one_number_on_every_line() {
        let long = "x".repeat(80);
        let tree = message_node(7, vec![span(&format!("{long} {long}"), None, true)]);
        let spoilers = spoilers_for(&tree, (200.0, 400.0));
        assert!(spoilers.len() >= 2, "{spoilers:?}");
        assert!(
            spoilers.iter().all(|s| s.owner == 7 && s.no == 0),
            "{spoilers:?}"
        );
    }

    /// An opened run stays numbered where it was — pressing it again covers
    /// it — while a link under it now answers to the link instead.
    #[test]
    fn an_opened_run_keeps_its_number_and_hands_presses_to_its_link() {
        let tree = message_node(
            9,
            vec![
                span("a", None, true),
                opened(span("secret", Some("https://example.com/s"), true)),
                span("c", None, true),
            ],
        );
        let (links, spoilers) = pressables_for(&tree, (600.0, 400.0));

        // Still three targets: an open one can be pressed back shut.
        assert_eq!(spoilers.len(), 3, "{spoilers:?}");
        // The middle run kept the number it was built with.
        assert!(
            spoilers.iter().any(|s| s.owner == 9 && s.no == 1),
            "{spoilers:?}"
        );
        // And its contents are reachable as a link now.
        assert_eq!(links.len(), 1, "{links:?}");
        assert_eq!(links[0].url, "https://example.com/s");
    }
}
