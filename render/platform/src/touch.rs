//! Touch gesture recognition: taps, scrolls and swipes from raw touches.
//!
//! Pure logic over points; winit feeds it, the app answers. Unit-tested
//! here because no test machine has a touchscreen.

/// A movement smaller than this is still a tap, not a drag.
pub const TAP_SLOP: f32 = 10.0;
/// A release past this, going mostly one way, is a swipe.
pub const SWIPE_MIN: f32 = 32.0;
/// Below this speed a release just stops; there is nothing to coast on.
pub const FLING_MIN: f32 = 120.0;
/// Past this the finger must have teleported; clamp before coasting.
pub const FLING_MAX: f32 = 6000.0;
/// Below this speed coasting stops; slower is invisible frame to frame.
pub const FLING_STOP: f32 = 40.0;
/// Exponential decay: after this many seconds ~37% of the speed remains.
pub const FLING_TAU: f32 = 0.12;

/// Which way a swipe went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDir {
    Left,
    Right,
    Up,
    Down,
}

/// A recognised gesture: where it started, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Swipe {
    Point { dir: SwipeDir, x: f32, y: f32 },
}

/// What one touch turned into, if anything yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TouchAction {
    /// A short touch that never moved: acts like a click.
    Tap { x: f32, y: f32 },
    /// Movement since the last report: scrolls under the finger.
    Scroll { dx: f32, dy: f32 },
    /// A release after a long mostly-straight move.
    Swipe(Swipe),
}

#[derive(Debug, Clone, Copy)]
struct Active {
    id: u64,
    x0: f32,
    y0: f32,
    x: f32,
    y: f32,
    moved: bool,
}

/// Tracks one finger; extra fingers are ignored until it lifts, since
/// pinches mean something else entirely (and nothing handles them yet).
#[derive(Debug, Default)]
pub struct Tracker {
    active: Option<Active>,
}

impl Tracker {
    pub fn press(&mut self, id: u64, x: f32, y: f32) {
        if self.active.is_none() {
            self.active = Some(Active {
                id,
                x0: x,
                y0: y,
                x,
                y,
                moved: false,
            });
        }
    }

    pub fn mov(&mut self, id: u64, x: f32, y: f32) -> Option<TouchAction> {
        let a = self.active.as_mut().filter(|a| a.id == id)?;
        let (dx, dy) = (x - a.x, y - a.y);
        if !a.moved && (x - a.x0).abs() <= TAP_SLOP && (y - a.y0).abs() <= TAP_SLOP {
            a.x = x;
            a.y = y;
            return None;
        }
        a.moved = true;
        a.x = x;
        a.y = y;
        (dx != 0.0 || dy != 0.0).then_some(TouchAction::Scroll { dx, dy })
    }

    pub fn release(&mut self, id: u64, x: f32, y: f32) -> Option<TouchAction> {
        let a = self.active.take().filter(|a| a.id == id)?;
        if !a.moved && (x - a.x0).abs() <= TAP_SLOP && (y - a.y0).abs() <= TAP_SLOP {
            return Some(TouchAction::Tap { x, y });
        }
        let (dx, dy) = (x - a.x0, y - a.y0);
        let dir = if dx.abs() >= SWIPE_MIN && dx.abs() > 2.0 * dy.abs() {
            Some(if dx < 0.0 {
                SwipeDir::Left
            } else {
                SwipeDir::Right
            })
        } else if dy.abs() >= SWIPE_MIN && dy.abs() > 2.0 * dx.abs() {
            Some(if dy < 0.0 {
                SwipeDir::Up
            } else {
                SwipeDir::Down
            })
        } else {
            None
        };
        dir.map(|dir| {
            TouchAction::Swipe(Swipe::Point {
                dir,
                x: a.x0,
                y: a.y0,
            })
        })
    }

    pub fn cancel(&mut self, id: u64) {
        if self.active.is_some_and(|a| a.id == id) {
            self.active = None;
        }
    }
}

/// A released scroll that keeps coasting.
///
/// Offset-space pixels per second, decaying exponentially. Pure numbers
/// over explicit steps, so tests drive it without a clock; the host
/// feeds real time and the scrolled region.
#[derive(Debug, Clone, Copy)]
pub struct Fling {
    velocity: f32,
}

impl Fling {
    /// Starts coasting, unless too slow to see or NaN. Clamps teleports.
    pub fn new(velocity: f32) -> Option<Self> {
        if !velocity.is_finite() {
            return None;
        }
        let velocity = velocity.clamp(-FLING_MAX, FLING_MAX);
        (velocity.abs() >= FLING_MIN).then_some(Fling { velocity })
    }

    /// Advances by `dt` seconds, returning the offset-space delta. `None`
    /// when spent: stop asking for frames. Non-positive steps hold still
    /// without decaying, so a paused loop resumes where it left off.
    pub fn step(&mut self, dt: f32) -> Option<f32> {
        if self.velocity.abs() < FLING_STOP {
            return None;
        }
        if dt <= 0.0 {
            return Some(0.0);
        }
        let delta = self.velocity * dt;
        self.velocity *= (-dt / FLING_TAU).exp();
        Some(delta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tap never moves.
    #[test]
    fn a_still_touch_is_a_tap() {
        let mut t = Tracker::default();
        t.press(1, 100.0, 200.0);
        assert_eq!(
            t.release(1, 103.0, 197.0),
            Some(TouchAction::Tap { x: 103.0, y: 197.0 })
        );
    }

    /// Small wanderings stay a tap; crossing the slop starts scrolling.
    #[test]
    fn wandering_past_the_slop_scrolls() {
        let mut t = Tracker::default();
        t.press(1, 0.0, 0.0);
        assert_eq!(t.mov(1, 5.0, 5.0), None);
        let scrolled = t.mov(1, 5.0, 20.0);
        assert!(
            matches!(scrolled, Some(TouchAction::Scroll { dy, .. }) if dy > 0.0),
            "{scrolled:?}"
        );
        // A scrolled touch never becomes a tap on release.
        assert_eq!(t.release(1, 5.0, 20.0), None);
    }

    /// A long straight release is a swipe, reporting where it started.
    #[test]
    fn a_long_straight_release_is_a_swipe() {
        for (to, dir) in [
            ((-50.0, 2.0), SwipeDir::Left),
            ((50.0, -2.0), SwipeDir::Right),
            ((2.0, -50.0), SwipeDir::Up),
            ((-2.0, 50.0), SwipeDir::Down),
        ] {
            let mut t = Tracker::default();
            t.press(7, 100.0, 100.0);
            let swipe = t.release(7, 100.0 + to.0, 100.0 + to.1);
            assert_eq!(
                swipe,
                Some(TouchAction::Swipe(Swipe::Point {
                    dir,
                    x: 100.0,
                    y: 100.0
                }))
            );
        }
    }

    /// A diagonal release is neither a scroll direction nor a swipe.
    #[test]
    fn a_diagonal_release_is_nothing() {
        let mut t = Tracker::default();
        t.press(1, 0.0, 0.0);
        t.mov(1, 30.0, 30.0);
        assert_eq!(t.release(1, 40.0, 40.0), None);
    }

    /// A second finger voids the gesture; other fingers are ignored.
    #[test]
    fn an_extra_finger_is_ignored() {
        let mut t = Tracker::default();
        t.press(1, 0.0, 0.0);
        t.press(2, 50.0, 50.0);
        assert_eq!(t.mov(2, 60.0, 50.0), None);
        assert_eq!(
            t.release(1, 0.0, 0.0),
            Some(TouchAction::Tap { x: 0.0, y: 0.0 })
        );
        t.cancel(2);
    }

    /// Too slow to see never starts; NaN never starts either.
    #[test]
    fn a_slow_or_nan_release_does_not_fling() {
        assert!(Fling::new(119.0).is_none());
        assert!(Fling::new(-119.0).is_none());
        assert!(Fling::new(f32::NAN).is_none());
        assert!(Fling::new(1000.0).is_some());
    }

    /// Coasting decays and then stops asking for frames.
    #[test]
    fn a_fling_decays_then_stops() {
        let mut f = Fling::new(2000.0).expect("fast enough to fling");
        // First step moves with nearly the full speed.
        let first = f.step(1.0 / 60.0).expect("still coasting");
        assert!(first > 20.0, "{first}");
        // A second of steps spends it.
        let mut frames = 0;
        while f.step(1.0 / 60.0).is_some() {
            frames += 1;
            assert!(frames < 600, "coasting never stopped");
        }
        assert!(frames > 5, "stopped without coasting");
    }

    /// Teleports clamp instead of jumping across the whole list.
    #[test]
    fn a_wild_velocity_clamps() {
        let mut f = Fling::new(1e9).expect("clamped, not refused");
        let first = f.step(1.0 / 60.0).expect("still coasting");
        assert!(first <= FLING_MAX / 60.0 + 1.0, "{first}");
    }

    /// A paused loop holds still without spending the fling.
    #[test]
    fn a_nonpositive_step_holds_still() {
        let mut f = Fling::new(2000.0).expect("fast enough to fling");
        assert_eq!(f.step(0.0), Some(0.0));
        assert_eq!(f.step(-1.0), Some(0.0));
        assert!(f.step(1.0 / 60.0).expect("still coasting") > 20.0);
    }
}
