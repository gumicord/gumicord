//! Where the iOS login autofill twins park themselves.
//!
//! ADR-0013 keeps two invisible `UITextField` siblings alive for the login
//! email and password, because the password manager fills a pair and pairs by
//! proximity. They must therefore be *near each other and near their own
//! field*, and they must stay inside the window: off-screen a `UITextField`
//! may refuse first responder and the keyboard never arrives.
//!
//! The arithmetic lives here, outside the iOS-gated proxy, so it can be
//! checked without a Mac. The mistake this replaces is quiet and total: giving
//! both twins one frame stacks them on the same pixel, which is the one
//! arrangement the pairing heuristic cannot use.

use gumicord_render::Rect;

/// How wide the strip is. A finger can still land on it, which is what AutoFill
/// needs, but there is nothing else in that column to steal.
pub const STRIP_W: f32 = 1.0;

/// The strip beside one login field: one pixel wide, hard against the field's
/// far edge, and inside `viewport_w`.
///
/// Clamps to the window rather than to the field: a field wider than the
/// window is clipped by it, so the rightmost pixel a finger can reach is the
/// window's, and the strip has to land there. Leaving the window is worse than
/// sitting on the field — an off-screen field may refuse first responder, and
/// then there is no keyboard at all.
pub fn strip_beside(field: Rect, viewport_w: f32) -> Rect {
    let x = (field.x + field.w + STRIP_W).min(viewport_w.max(STRIP_W)) - STRIP_W;
    Rect::new(x.max(0.0), field.y, STRIP_W, field.h)
}

/// Where each twin parks. A field with no rectangle yet is `None`, and the
/// caller must leave that twin out of the poll: half a paired fill is better
/// than a stale one replayed into the wrong document.
pub fn login_parking(
    email: Option<Rect>,
    password: Option<Rect>,
    viewport_w: f32,
) -> (Option<Rect>, Option<Rect>) {
    (
        email.map(|f| strip_beside(f, viewport_w)),
        password.map(|f| strip_beside(f, viewport_w)),
    )
}

/// Which slot a twin is. The order matches [`login_parking`]'s result, and
/// `Proxy`'s two fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Username,
    Password,
}

/// Where each twin sits, and whether it sits anywhere yet.
///
/// Lives here rather than in the proxy so the bookkeeping is checkable without
/// a Mac. The view layer only asks whether the frame changed.
#[derive(Debug, Default)]
pub struct Parking {
    slots: [Option<Rect>; 2],
}

impl Parking {
    pub fn new() -> Self {
        Self::default()
    }

    fn index(slot: Slot) -> usize {
        match slot {
            Slot::Username => 0,
            Slot::Password => 1,
        }
    }

    /// Records a slot's rectangle, answering whether the view must move.
    ///
    /// Setting the same frame every tick churns layout for no reason, so a
    /// repeat is silent. `None` means unplaced: the slot is forgotten rather
    /// than left claiming a rectangle the layout has withdrawn.
    pub fn set(&mut self, slot: Slot, rect: Option<Rect>) -> bool {
        let i = Self::index(slot);
        let changed = self.slots[i] != rect;
        self.slots[i] = rect;
        changed
    }

    /// Whether this twin has a rectangle. An unplaced twin is left out of the
    /// poll: its document would be judged against a baseline from before the
    /// layout settled.
    pub fn placed(&self, slot: Slot) -> bool {
        self.slots[Self::index(slot)].is_some()
    }

    /// Where a twin sits, for the log.
    pub fn rect(&self, slot: Slot) -> Option<Rect> {
        self.slots[Self::index(slot)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(x: f32, y: f32) -> Rect {
        Rect::new(x, y, 300.0, 44.0)
    }

    /// The whole point: two different pixels, one beside each field. The bug
    /// this fixes was handing both twins the same frame.
    #[test]
    fn the_twins_get_different_pixels() {
        let (user, pass) = login_parking(Some(field(20.0, 100.0)), Some(field(20.0, 200.0)), 400.0);
        assert_eq!(user.unwrap().x, 320.0);
        assert_eq!(pass.unwrap().x, 320.0);
        assert_ne!(
            user.unwrap().y,
            pass.unwrap().y,
            "双子が同じ行に重なっている"
        );
        assert_eq!(user.unwrap().w, STRIP_W);
        assert_eq!(pass.unwrap().w, STRIP_W);
    }

    /// The strip sits against the field's far edge, inside the window.
    #[test]
    fn the_strip_lands_after_the_field() {
        let strip = strip_beside(field(20.0, 100.0), 400.0);
        assert_eq!(strip.x, 320.0);
        assert_eq!(strip.y, 100.0);
        assert_eq!(strip.h, 44.0);
        assert!(strip.x + strip.w <= 400.0);
    }

    /// A field flush against the right edge keeps the pixel inside the window
    /// at its own last column.
    #[test]
    fn a_field_at_the_edge_stays_inside() {
        let flush = Rect::new(99.0, 10.0, 300.0, 44.0);
        let strip = strip_beside(flush, 400.0);
        assert!(strip.x + strip.w <= 400.0, "窓の外に出た: {strip:?}");
        assert!(strip.x >= flush.x, "欄の外に出た: {strip:?}");
        assert_eq!(strip.x, 399.0, "端の欄の右隣ではない");
    }

    /// A field wider than the window still yields a strip inside it.
    #[test]
    fn an_oversized_field_still_stays_inside() {
        let huge = Rect::new(0.0, 0.0, 900.0, 44.0);
        let strip = strip_beside(huge, 400.0);
        assert!(strip.x + strip.w <= 400.0, "窓の外に出た: {strip:?}");
        assert!(strip.x >= huge.x);
    }

    /// A field that has not been laid out yet is left out, so its twin is not
    /// polled against a stale baseline.
    #[test]
    fn an_unplaced_field_has_no_strip() {
        let (user, pass) = login_parking(Some(field(20.0, 100.0)), None, 400.0);
        assert!(user.is_some());
        assert!(pass.is_none(), "未配置の欄都有一个位置");
    }

    /// Both unplaced means neither twin is ready, and the caller must not
    /// attach anything.
    #[test]
    fn no_layout_means_nothing_to_place() {
        let (user, pass) = login_parking(None, None, 400.0);
        assert!(user.is_none());
        assert!(pass.is_none());
    }

    /// Nothing is placed before the first layout, so neither twin is polled.
    #[test]
    fn nothing_is_placed_to_begin_with() {
        let p = Parking::new();
        assert!(!p.placed(Slot::Username));
        assert!(!p.placed(Slot::Password));
    }

    /// The first frame moves the view; the same frame again does not.
    #[test]
    fn only_a_changed_frame_moves_the_view() {
        let mut p = Parking::new();
        let rect = strip_beside(field(20.0, 100.0), 400.0);
        assert!(
            p.set(Slot::Username, Some(rect)),
            "最初のフレームで動かさない"
        );
        assert!(!p.set(Slot::Username, Some(rect)), "同じフレームで動かした");
        assert!(p.placed(Slot::Username));
        // The sibling is untouched by the first twin's frame.
        assert!(!p.placed(Slot::Password));
    }

    /// A withdrawn rectangle forgets the slot, so the poll stops trusting it.
    #[test]
    fn a_withdrawn_rectangle_unplaces_the_slot() {
        let mut p = Parking::new();
        p.set(
            Slot::Password,
            Some(strip_beside(field(20.0, 200.0), 400.0)),
        );
        assert!(p.placed(Slot::Password));

        assert!(p.set(Slot::Password, None), "配置の取消しに気が付かない");
        assert!(!p.placed(Slot::Password));
        assert!(p.rect(Slot::Password).is_none());
    }

    /// The two twins are tracked apart, which is the bug this replaces.
    #[test]
    fn the_two_slots_are_tracked_apart() {
        let mut p = Parking::new();
        let (user, pass) = login_parking(Some(field(20.0, 100.0)), None, 400.0);
        p.set(Slot::Username, user);
        p.set(Slot::Password, pass);

        assert!(p.placed(Slot::Username));
        assert!(
            !p.placed(Slot::Password),
            "未配置の双子まで配置済みになった"
        );
        assert_ne!(p.rect(Slot::Username), p.rect(Slot::Password));
    }

    /// Moving a field moves its twin and leaves the other alone.
    #[test]
    fn moving_one_field_leaves_the_other_where_it_is() {
        let mut p = Parking::new();
        let (user, pass) = login_parking(Some(field(20.0, 100.0)), Some(field(20.0, 200.0)), 400.0);
        p.set(Slot::Username, user);
        p.set(Slot::Password, pass);
        let before = p.rect(Slot::Password);

        // The field slid down by 300.
        let moved = strip_beside(field(20.0, 400.0), 400.0);
        assert!(p.set(Slot::Username, Some(moved)));
        assert_eq!(p.rect(Slot::Password), before, "動かなかった双子まで動いた");
    }
}
