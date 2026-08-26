//! UITree nodes.
//!
//! The struct in the spec has no *content*: a stable ID says a node is the
//! message body, but not what string that body is. [`Content`] fills that in
//! so the tree can be drawn. It is not part of the extension ABI — plugins
//! build content only through SDK functions such as `ui.text()`.
//!
//! Layout direction is deliberately absent. The stable ID carries the
//! meaning, and how that meaning is arranged is the renderer's decision.
//! When themes gain layout overrides, those will live in the style, not here.

use crate::ids::{DataKind, NodeId};
use crate::style::Style;
use crate::value::{Color, Font};
use crate::{Key, State, StateSet};

/// What a node displays.
///
/// No `Eq`: [`Span`] carries `f32` sizes and colours.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Content {
    /// A container: children only.
    #[default]
    None,
    /// A string. Shaping is the renderer's job.
    Text(String),
    /// Text with mixed decoration.
    ///
    /// Decoration must not become separate nodes: siblings wrap
    /// independently, so a bold run inside a sentence would break the line in
    /// the wrong place. Wrapping is only correct when the whole mixed line is
    /// shaped at once, so the decoration goes in the content and the node
    /// stays single.
    Rich(Vec<Span>),
    /// An icon, addressed by name.
    ///
    /// What gets drawn is the renderer's decision; naming it means switching
    /// from glyphs to textures later touches nothing here. An unknown name
    /// is not an error — nothing is drawn.
    Icon(String),
    /// An image, carried as a URL rather than pixels.
    ///
    /// The tree is rebuilt every frame, so pixels here would mean copying
    /// megabytes per frame. Fetching and decoding belong to the app; the
    /// renderer draws only what it already holds, and nothing otherwise.
    Image(String),
    /// A QR code, carried as the string to encode.
    ///
    /// Encoding and drawing are the renderer's job; a QR code is a grid of
    /// rounded rectangles, which it already draws.
    Qr(String),
    /// Text being edited.
    ///
    /// Separate from plain text because drawing the caret, the selection and
    /// the preedit range is the renderer's job: only the shaper knows where
    /// characters are, while the app holds byte offsets.
    Editable(Editable),
}

/// A decorated run of text; an element of [`Content::Rich`].
///
/// Holds the theme's *decision*: "weight 700", never "bold". Carrying the
/// parse fact this far would put the renderer in charge of what bold looks
/// like.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Span {
    pub text: String,
    /// `None` uses the node's font.
    pub font: Option<Font>,
    /// `None` uses the node's colour.
    pub color: Option<Color>,
    /// Lines drawn through the text.
    pub line: Line,
    /// Whether to hide the contents (a spoiler).
    ///
    /// The space is kept: collapsing it would rewrap the line on reveal and
    /// make the body jump.
    pub hidden: bool,
    /// A hidden run that has been opened.
    ///
    /// A separate flag rather than clearing [`Self::hidden`], so the runs
    /// that came from spoilers stay tellable from plain text afterwards —
    /// which is what lets one be opened without shuffling the others.
    pub revealed: bool,
    /// Where a press on this run goes.
    ///
    /// A target, not an action: what opening a `https` URL means is the
    /// platform's, and anything else is refused there. Runs without one are
    /// plain text no matter how they are coloured.
    pub link: Option<String>,
    /// A picture drawn in place of the text: a custom emoji.
    ///
    /// The text is the stand-in while the picture loads, and shapes as its
    /// own advance — an em space, so the run is one square wide. A run with
    /// neither pixels nor a readable shape would collapse the line.
    pub image: Option<String>,
}

impl Span {
    /// Whether the contents are still covered.
    pub fn concealed(&self) -> bool {
        self.hidden && !self.revealed
    }
}

/// Lines drawn through text. Stackable: `__~~a~~__` draws both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Line {
    pub under: bool,
    pub through: bool,
}

impl Line {
    pub const fn any(self) -> bool {
        self.under || self.through
    }
}

/// Where a floating surface is anchored, in logical px from the window's top
/// left.
///
/// Not a style: a theme cannot express where a press happened, and neither is
/// it user data. Like [`UiNode::tint`], it carries something knowable only at
/// that moment.
///
/// Not a position either. Only the anchor point goes here; where the surface
/// actually lands depends on its size and the window's, and a menu opened at
/// the right edge has to flip. Only the renderer can decide that.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Anchor {
    pub x: f32,
    pub y: f32,
}

impl Anchor {
    pub const fn at(x: f32, y: f32) -> Self {
        Anchor { x, y }
    }
}

/// Text being edited and the marks on it. Offsets are byte offsets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Editable {
    pub text: String,
    /// Caret position.
    pub caret: usize,
    /// Selection; empty means none.
    pub selection: core::ops::Range<usize>,
    /// The preedit range, underlined until committed.
    pub composing: Option<core::ops::Range<usize>>,
    /// Shown faintly when the field is empty. Not editable.
    pub placeholder: String,
}

impl Content {
    /// The string to shape and draw.
    ///
    /// Editable text is still text, so it comes out here too, falling back to
    /// the placeholder when empty.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text(s) => Some(s),
            Content::Editable(e) if e.text.is_empty() => Some(&e.placeholder),
            Content::Editable(e) => Some(&e.text),
            _ => None,
        }
    }

    pub fn as_image(&self) -> Option<&str> {
        match self {
            Content::Image(url) => Some(url),
            _ => None,
        }
    }

    pub fn as_qr(&self) -> Option<&str> {
        match self {
            Content::Qr(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_editable(&self) -> Option<&Editable> {
        match self {
            Content::Editable(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_icon(&self) -> Option<&str> {
        match self {
            Content::Icon(s) => Some(s),
            _ => None,
        }
    }

    /// Decorated text.
    ///
    /// [`Content::as_text`] deliberately does not return this: joining the
    /// runs would drop the decoration, and a caller that cares would never
    /// notice.
    pub fn as_rich(&self) -> Option<&[Span]> {
        match self {
            Content::Rich(s) => Some(s),
            _ => None,
        }
    }

    /// Whether the node draws something itself; layout treats it as a leaf.
    pub fn is_leaf(&self) -> bool {
        !matches!(self, Content::None)
    }
}

/// A reference to the domain object a node represents.
///
/// A read-only snapshot, never the internal state. Fields are part of the ABI
/// too: additions are free, removals and renames are breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRef {
    pub kind: DataKind,
    /// A Discord snowflake.
    pub id: u64,
}

/// One UITree node.
#[derive(Debug, Clone, PartialEq)]
pub struct UiNode {
    /// The stable ID; the extension ABI itself.
    pub id: NodeId,
    /// Distinguishes siblings sharing an `id`.
    pub key: Option<Key>,
    /// State a theme can select on.
    pub states: StateSet,
    /// The domain object this node represents.
    pub data: Option<DataRef>,
    /// A colour the data brought with it: a role's colour, a folder's.
    ///
    /// Not a style but an ingredient for one. The theme decides appearance,
    /// yet a role's colour is something the user chose in Discord and is not
    /// the theme's to discard. So the colour is only carried here, and where
    /// to apply it — text, border, background, nowhere — is the theme's call
    /// via `$data.tint`.
    ///
    /// No identifier travels with it, so a theme cannot single out one guild
    /// or one person.
    pub tint: Option<Color>,
    /// The anchor point for a floating surface.
    ///
    /// Only floating nodes carry one. Reading it on a node in the flow would
    /// let a child override the position its parent chose.
    pub anchor: Option<Anchor>,
    /// What to display.
    pub content: Content,
    /// The style resolved by the theme and plugins.
    ///
    /// Empty at construction; the theme and plugin stages fill it in.
    pub style: Style,
    pub children: Vec<UiNode>,
}

impl UiNode {
    pub fn new(id: NodeId) -> Self {
        UiNode {
            id,
            key: None,
            states: StateSet::EMPTY,
            data: None,
            tint: None,
            anchor: None,
            content: Content::None,
            style: Style::default(),
            children: Vec::new(),
        }
    }

    /// A node holding text.
    pub fn text(id: NodeId, s: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Text(s.into()))
    }

    /// A node holding an icon.
    pub fn icon(id: NodeId, name: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Icon(name.into()))
    }

    /// A node holding an image, as a URL rather than pixels.
    pub fn image(id: NodeId, url: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Image(url.into()))
    }

    /// A node holding a QR code, as the string to encode.
    pub fn qr(id: NodeId, data: impl Into<String>) -> Self {
        UiNode::new(id).with_content(Content::Qr(data.into()))
    }

    /// A node holding text being edited.
    pub fn editable(id: NodeId, e: Editable) -> Self {
        UiNode::new(id).with_content(Content::Editable(e))
    }

    pub fn with_key(mut self, key: Key) -> Self {
        self.key = Some(key);
        self
    }

    /// Keys by snowflake, which is what list items almost always want.
    pub fn with_id_key(mut self, id: u64) -> Self {
        self.key = Some(Key::Id(id));
        self
    }

    pub fn with_state(mut self, state: State) -> Self {
        self.states = self.states.with(state);
        self
    }

    /// Sets a state conditionally, purely to spare the caller an `if`.
    pub fn with_state_if(mut self, cond: bool, state: State) -> Self {
        if cond {
            self.states = self.states.with(state);
        }
        self
    }

    pub fn with_states(mut self, states: StateSet) -> Self {
        self.states = states;
        self
    }

    /// Sets the anchor point for a floating surface.
    ///
    /// Only the point: flipping it when it would overflow the screen needs
    /// the surface's size, which only the renderer knows.
    pub fn with_anchor(mut self, anchor: Anchor) -> Self {
        self.anchor = Some(anchor);
        self
    }

    /// Attaches a colour the data brought. Where it lands is the theme's
    /// decision.
    pub fn with_tint(mut self, tint: Color) -> Self {
        self.tint = Some(tint);
        self
    }

    /// Attaches a colour if there is one, purely to spare the caller an
    /// `if`.
    pub fn with_tint_opt(mut self, tint: Option<Color>) -> Self {
        self.tint = tint;
        self
    }

    /// Attaches a reference to a domain object.
    ///
    /// The kind comes from the stable ID, so callers pass only the id. IDs
    /// that carry no `data` ignore it silently: failing here would break
    /// callers the day an ID gains a data kind.
    pub fn with_data(mut self, id: u64) -> Self {
        let kind = self.id.data_kind();
        if kind != DataKind::None {
            self.data = Some(DataRef { kind, id });
        }
        self
    }

    pub fn with_content(mut self, content: Content) -> Self {
        self.content = content;
        self
    }

    pub fn child(mut self, node: UiNode) -> Self {
        self.children.push(node);
        self
    }

    /// Adds a child conditionally.
    pub fn child_if(self, cond: bool, node: impl FnOnce() -> UiNode) -> Self {
        if cond { self.child(node()) } else { self }
    }

    pub fn children(mut self, nodes: impl IntoIterator<Item = UiNode>) -> Self {
        self.children.extend(nodes);
        self
    }

    /// Depth-first pre-order traversal, which is also the draw order.
    pub fn walk(&self, f: &mut impl FnMut(&UiNode, usize)) {
        self.walk_at(0, f);
    }

    fn walk_at(&self, depth: usize, f: &mut impl FnMut(&UiNode, usize)) {
        f(self, depth);
        for c in &self.children {
            c.walk_at(depth + 1, f);
        }
    }

    /// Total node count.
    pub fn count(&self) -> usize {
        1 + self.children.iter().map(UiNode::count).sum::<usize>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_is_attached_only_where_the_id_declares_it() {
        // chat.message carries MessageData.
        let m = UiNode::new(NodeId::ChatMessage).with_data(42);
        assert_eq!(
            m.data,
            Some(DataRef {
                kind: DataKind::Message,
                id: 42
            })
        );

        // layout.row carries none, so this is ignored.
        let r = UiNode::new(NodeId::LayoutRow).with_data(42);
        assert_eq!(r.data, None);
    }

    /// Traversal order is draw order; this pins it to pre-order.
    #[test]
    fn walk_is_depth_first_pre_order() {
        let tree = UiNode::new(NodeId::AppRoot)
            .child(UiNode::new(NodeId::NavGuildList).child(UiNode::new(NodeId::NavGuildListItem)))
            .child(UiNode::new(NodeId::ChatView));

        let mut seen = Vec::new();
        tree.walk(&mut |n, d| seen.push((n.id, d)));

        assert_eq!(
            seen,
            vec![
                (NodeId::AppRoot, 0),
                (NodeId::NavGuildList, 1),
                (NodeId::NavGuildListItem, 2),
                (NodeId::ChatView, 1),
            ]
        );
        assert_eq!(tree.count(), 4);
    }

    #[test]
    fn text_content_round_trips() {
        let n = UiNode::text(NodeId::ChatMessageContent, "こんにちは");
        assert_eq!(n.content.as_text(), Some("こんにちは"));
        assert_eq!(UiNode::new(NodeId::ChatView).content.as_text(), None);
    }
}
