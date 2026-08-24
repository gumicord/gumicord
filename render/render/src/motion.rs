//! Animation: moves resolved styles towards their targets.
//!
//! Driven by time, not frames. Stepping a fraction per frame is simpler but
//! runs at different speeds on different refresh rates, which is
//! incompatible with identical rendering everywhere.
//!
//! ```text
//!   t     = (now - started) / duration
//!   value = from + (to - from) * ease(t)
//! ```
//!
//! The tree's shape never animates; only already-resolved style values do.
//! Nodes that appear and disappear would need something else, which does not
//! exist here.
//!
//! Colours, radii, border widths, opacity and sizes move. A moving size moves
//! the layout with it.
//!
//! Only nodes whose theme wrote a transition animate; everything else
//! switches instantly. Animating by default would set dozens of rows in
//! motion on every scroll, which is noise rather than polish.
//!
//! [`Motion::apply`] reports whether anything is still moving, and the loop
//! sleeps once nothing is.

use std::collections::HashMap;
use std::time::Instant;

use gumicord_uitree::value::{Background, Color};
use gumicord_uitree::{Style, UiNode};

/// Differences below this count as equal.
const EPSILON: f32 = 0.01;

/// Identifies a node across frames by its path from the root.
///
/// A stable ID and key are not enough: a key only distinguishes siblings
/// under the same parent. A guild icon carries no key, since its parent guild
/// does, and using the ID alone made every guild's icon share one record —
/// the target was rewritten several times per frame and the animation
/// vanished.
///
/// Siblings are counted per stable ID rather than by overall position: an
/// overall index shifts as soon as a node of another kind appears beside
/// them, and the icon would look like a node never seen before.
///
/// Reordering keyless siblings of the same ID restarts their animations,
/// since position is the only handle. Adding a key is the answer where that
/// matters.
type Ident = u64;

fn ident(parent: Ident, node: &UiNode, index: usize) -> Ident {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parent.hash(&mut h);
    node.id.hash(&mut h);
    match &node.key {
        Some(k) => k.hash(&mut h),
        None => index.hash(&mut h),
    }
    h.finish()
}

/// One node's in-flight animation.
#[derive(Debug, Clone)]
struct Track {
    /// What was on screen when it started.
    from: Style,
    /// Where it is going: the resolved value.
    to: Style,
    started: Instant,
    /// Milliseconds.
    duration: f32,
    /// Seen this frame; anything unseen is dropped.
    seen: u64,
}

/// Everything currently animating.
#[derive(Debug, Default)]
pub struct Motion {
    tracks: HashMap<Ident, Track>,
    /// The frame counter, used only to drop vanished nodes.
    frame: u64,
}

impl Motion {
    pub fn new() -> Self {
        Motion::default()
    }

    /// Rewrites the tree's styles to their current values, reporting whether
    /// anything is still moving. The caller keeps requesting frames while it
    /// is, and stops when it is not.
    pub fn apply(&mut self, root: &mut UiNode, now: Instant) -> bool {
        self.frame = self.frame.wrapping_add(1);
        let moving = self.walk(root, ident(0, root, 0), now);
        // Drop vanished nodes, or this grows with every scroll.
        let frame = self.frame;
        self.tracks.retain(|_, t| t.seen == frame);
        moving
    }

    fn walk(&mut self, node: &mut UiNode, at: Ident, now: Instant) -> bool {
        let mut moving = self.node(node, at, now);

        // Counted per stable ID, not by overall sibling position.
        //
        // An overall index shifts when a node of another kind appears
        // beside them, and the icon then looks like a node never seen before.
        let mut nth: HashMap<gumicord_uitree::NodeId, usize> = HashMap::new();
        for child in &mut node.children {
            let n = nth.entry(child.id).or_default();
            let child_at = ident(at, child, *n);
            *n += 1;
            moving |= self.walk(child, child_at, now);
        }
        moving
    }

    fn node(&mut self, node: &mut UiNode, ident: Ident, now: Instant) -> bool {
        let Some(duration) = node.style.transition.filter(|d| *d > 0.0) else {
            return false;
        };
        let frame = self.frame;

        let Some(track) = self.tracks.get_mut(&ident) else {
            // A node seen for the first time does not animate; the whole
            // screen moving on open is not polish.
            self.tracks.insert(
                ident,
                Track {
                    from: node.style.clone(),
                    to: node.style.clone(),
                    started: now,
                    duration,
                    seen: frame,
                },
            );
            return false;
        };
        track.seen = frame;

        // A new target restarts from what is on screen; starting from the old
        // target would jump when the direction reverses.
        if track.to != node.style {
            track.from = displayed(&track.from, &track.to, progress(track, now));
            track.to = node.style.clone();
            track.started = now;
            track.duration = duration;
        }

        let t = progress(track, now);
        node.style = displayed(&track.from, &track.to, t);
        t < 1.0
    }
}

/// Progress from 0 to 1; a zero duration is immediately 1.
fn progress(track: &Track, now: Instant) -> f32 {
    if track.duration <= 0.0 {
        return 1.0;
    }
    let elapsed = now.saturating_duration_since(track.started).as_secs_f32() * 1000.0;
    (elapsed / track.duration).clamp(0.0, 1.0)
}

/// Fast out, quiet in.
///
/// Never linear: linear motion reads as machinery and makes the stop
/// conspicuous.
fn ease_out(t: f32) -> f32 {
    let inv = 1.0 - t;
    1.0 - inv * inv * inv
}

/// The value at `t` between two others.
fn displayed(from: &Style, to: &Style, t: f32) -> Style {
    if t >= 1.0 {
        return to.clone();
    }
    let e = ease_out(t);

    // What cannot be interpolated jumps to the target: an intermediate font
    // or shadow means nothing, and switching is less confusing than a
    // half-blend.
    let mut out = to.clone();
    out.color = lerp_color(from.color, to.color, e);
    out.border_color = lerp_color(from.border_color, to.border_color, e);
    out.background = lerp_background(from.background.as_ref(), to.background.as_ref(), e);
    out.border_width = lerp_opt(from.border_width, to.border_width, e);
    out.radius = lerp_opt(from.radius, to.radius, e);
    out.opacity = lerp_opt(from.opacity, to.opacity, e);
    out.width = lerp_opt(from.width, to.width, e);
    out.height = lerp_opt(from.height, to.height, e);
    out.gap = lerp_opt(from.gap, to.gap, e);
    out
}

/// Nothing is blended when either side is unset: "unset" is not a value, and
/// treating it as zero passes through black or zero width.
fn lerp_opt(from: Option<f32>, to: Option<f32>, t: f32) -> Option<f32> {
    match (from, to) {
        (Some(a), Some(b)) if (a - b).abs() > EPSILON => Some(a + (b - a) * t),
        _ => to,
    }
}

fn lerp_color(from: Option<Color>, to: Option<Color>, t: f32) -> Option<Color> {
    let (Some(a), Some(b)) = (from, to) else {
        return to;
    };
    Some(Color {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
        a: lerp_u8(a.a, b.a, t),
    })
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    let v = f32::from(a) + (f32::from(b) - f32::from(a)) * t;
    v.round().clamp(0.0, 255.0) as u8
}

/// Only a background's colour is blended.
///
/// Images are not blended: an intermediate between two of them means
/// nothing, and producing one would mean drawing both.
fn lerp_background(
    from: Option<&Background>,
    to: Option<&Background>,
    t: f32,
) -> Option<Background> {
    let to = to?;
    let Some(from) = from else {
        return Some(to.clone());
    };
    if from.image.is_some() || to.image.is_some() {
        return Some(to.clone());
    }
    let mut out = to.clone();
    out.color = lerp_color(from.color, to.color, t);
    out.tint = lerp_color(from.tint, to.tint, t);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gumicord_uitree::NodeId;
    use std::time::Duration;

    fn node(radius: f32, transition: Option<f32>) -> UiNode {
        let mut n = UiNode::new(NodeId::NavGuildListItemIcon);
        n.style.radius = Some(radius);
        n.style.transition = transition;
        n
    }

    /// A node seen for the first time does not animate.
    #[test]
    fn the_first_sight_of_a_node_does_not_move() {
        let mut m = Motion::new();
        let mut n = node(12.0, Some(100.0));
        let now = Instant::now();

        assert!(!m.apply(&mut n, now));
        assert_eq!(n.style.radius, Some(12.0));
    }

    /// A new target is moved to over time.
    #[test]
    fn a_changed_value_travels_over_time() {
        let mut m = Motion::new();
        let start = Instant::now();

        m.apply(&mut node(12.0, Some(100.0)), start);

        // Just after the change it is still near the old value.
        let mut n = node(8.0, Some(100.0));
        assert!(m.apply(&mut n, start), "should be moving");
        assert_eq!(n.style.radius, Some(12.0), "should not have moved yet");

        // Partway.
        let mut n = node(8.0, Some(100.0));
        assert!(m.apply(&mut n, start + Duration::from_millis(50)));
        let mid = n.style.radius.expect("a value");
        assert!(mid < 12.0 && mid > 8.0, "{mid}");

        // Done.
        let mut n = node(8.0, Some(100.0));
        assert!(
            !m.apply(&mut n, start + Duration::from_millis(200)),
            "should have stopped"
        );
        assert_eq!(n.style.radius, Some(8.0));
    }

    /// Without a transition it switches instantly; animating everything would
    /// set dozens of rows moving on every scroll.
    #[test]
    fn without_a_transition_nothing_moves() {
        let mut m = Motion::new();
        let start = Instant::now();

        m.apply(&mut node(12.0, None), start);
        let mut n = node(8.0, None);
        assert!(!m.apply(&mut n, start));
        assert_eq!(n.style.radius, Some(8.0), "should arrive immediately");
    }

    /// A reversal restarts from what is on screen; from the target it would
    /// jump.
    #[test]
    fn reversing_midway_starts_from_where_it_is() {
        let mut m = Motion::new();
        let start = Instant::now();

        m.apply(&mut node(12.0, Some(100.0)), start);
        let mut n = node(8.0, Some(100.0));
        m.apply(&mut n, start + Duration::from_millis(50));
        let mid = n.style.radius.expect("a value");

        // Reverse.
        let mut n = node(12.0, Some(100.0));
        m.apply(&mut n, start + Duration::from_millis(50));
        assert_eq!(
            n.style.radius,
            Some(mid),
            "should continue from where it was"
        );
    }

    /// Nothing is blended when either side is unset; zero would pass through
    /// black or zero width.
    #[test]
    fn an_unset_value_is_not_treated_as_zero() {
        assert_eq!(lerp_opt(None, Some(10.0), 0.5), Some(10.0));
        assert_eq!(lerp_opt(Some(10.0), None, 0.5), None);
        assert_eq!(
            lerp_color(None, Color::parse("#ffffff"), 0.5),
            Color::parse("#ffffff")
        );
    }

    /// Siblings sharing a stable ID animate independently.
    ///
    /// Guild icons carry no key, since their parent does. Keying records by ID
    /// alone made them all share one, the target was rewritten several times
    /// per frame, and the animation vanished.
    #[test]
    fn siblings_with_the_same_id_move_on_their_own() {
        fn rail(hovered_radius: f32) -> UiNode {
            let mut list = UiNode::new(NodeId::NavGuildList);
            for i in 0..3u64 {
                // Only the parent carries a key.
                let mut icon = UiNode::new(NodeId::NavGuildListItemIcon);
                icon.style.radius = Some(if i == 1 { hovered_radius } else { 12.0 });
                icon.style.transition = Some(100.0);
                list = list.child(
                    UiNode::new(NodeId::NavGuildListItem)
                        .with_id_key(i)
                        .child(icon),
                );
            }
            list
        }

        fn radii(n: &UiNode, out: &mut Vec<f32>) {
            if n.id == NodeId::NavGuildListItemIcon {
                out.push(n.style.radius.expect("a value"));
            }
            for c in &n.children {
                radii(c, out);
            }
        }

        let mut m = Motion::new();
        let start = Instant::now();
        m.apply(&mut rail(12.0), start);

        // The pointer lands on the middle one; the first frame does not move
        // yet.
        m.apply(&mut rail(8.0), start);

        let mut tree = rail(8.0);
        assert!(m.apply(&mut tree, start + Duration::from_millis(50)));

        let mut got = Vec::new();
        radii(&tree, &mut got);
        assert_eq!(got[0], 12.0, "an unhovered one moved");
        assert_eq!(got[2], 12.0, "an unhovered one moved");
        assert!(
            got[1] > 8.0 && got[1] < 12.0,
            "only the hovered one should be partway: {}",
            got[1]
        );
    }

    /// A node of another kind appearing beside them does not restart the
    /// animation.
    ///
    /// Hovering a guild changes the icon's radius and adds the pill at the
    /// same moment. Counting siblings overall would shift the icon's index and
    /// make it look new.
    #[test]
    fn a_new_kind_of_sibling_does_not_restart_the_animation() {
        fn item(radius: f32, with_pill: bool) -> UiNode {
            let mut icon = UiNode::new(NodeId::NavGuildListItemIcon);
            icon.style.radius = Some(radius);
            icon.style.transition = Some(100.0);

            let mut item = UiNode::new(NodeId::NavGuildListItem).with_id_key(1);
            if with_pill {
                // The pill comes before the icon.
                item = item.child(UiNode::new(NodeId::NavGuildListItemPill));
            }
            item.child(icon)
        }

        let mut m = Motion::new();
        let start = Instant::now();
        m.apply(&mut item(12.0, false), start);

        // Hovered: the pill appears and the radius changes together.
        m.apply(&mut item(8.0, true), start);

        let mut tree = item(8.0, true);
        assert!(m.apply(&mut tree, start + Duration::from_millis(50)));

        let icon = tree
            .children
            .iter()
            .find(|c| c.id == NodeId::NavGuildListItemIcon)
            .expect("an icon");
        let r = icon.style.radius.expect("a value");
        assert!(r > 8.0 && r < 12.0, "should be moving, not jumping: {r}");
    }

    /// Records for vanished nodes are dropped, or they grow with every
    /// scroll.
    #[test]
    fn a_node_that_went_away_is_forgotten() {
        let mut m = Motion::new();
        let now = Instant::now();

        m.apply(&mut node(12.0, Some(100.0)), now);
        assert_eq!(m.tracks.len(), 1);

        // A tree holding only the other node.
        let mut other = UiNode::new(NodeId::ChatView);
        other.style.transition = Some(100.0);
        m.apply(&mut other, now);
        assert_eq!(m.tracks.len(), 1, "前のは捨てられている");
    }

    /// Quick to leave, quiet to land.
    #[test]
    fn the_curve_starts_fast_and_lands_quietly() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "前半で半分より進む");
    }
}
