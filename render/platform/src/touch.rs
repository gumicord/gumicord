//! Touch gesture recognition: taps, scrolls and swipes from raw touches.
//!
//! Pure logic over points; winit feeds it, the app answers. Unit-tested
//! here because no test machine has a touchscreen.

/// A movement smaller than this is still a tap, not a drag.
pub const TAP_SLOP: f32 = 10.0;
/// A release past this, going mostly one way, is a swipe.
pub const SWIPE_MIN: f32 = 32.0;
/// Below this speed a release just stops; there is nothing to coast on.
/// Matches Android's minimum fling velocity, in logical pixels per second.
pub const FLING_MIN: f32 = 50.0;
/// A drag past this coasts even when slow: reaching it means intent,
/// whatever the speed was. Signed offset-space pixels along the scroll
/// axis, so dragging back to the start still stops.
pub const FLING_DIST_MIN: f32 = 96.0;
/// Past this the finger must have teleported; clamp before coasting.
pub const FLING_MAX: f32 = 6000.0;
/// Below this speed coasting stops; slower is invisible frame to frame.
pub const FLING_STOP: f32 = 40.0;
/// Exponential decay: after this many seconds ~37% of the speed remains.
/// Long enough to feel like a coast on a phone.
pub const FLING_TAU: f32 = 0.25;

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

/// How far back release velocity looks, in seconds. Android fits over a
/// similar window: a single paired sample can report absurd speeds when
/// the loop runs hot, and one pair must never decide the coast.
pub const VELOCITY_WINDOW: f32 = 0.12;
/// How many move samples the window keeps; bounds a frantic gesture.
pub const VELOCITY_MAX_SAMPLES: usize = 32;

/// A released scroll that keeps coasting.
///
/// Offset-space pixels per second, decaying exponentially. Pure numbers
/// over explicit steps, so tests drive it without a clock; the host
/// feeds real time and the scrolled region.
#[derive(Debug, Clone, Copy)]
pub struct Fling {
    velocity: f32,
    /// Sub-pixel remainder. Hot loops step with tiny deltas, and dropping
    /// the coast on the first sub-half-pixel one strands it mid-list.
    carry: f32,
}

/// What one coasting step asks for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Move this many offset-space pixels; whole pixels only.
    Move(f32),
    /// Nothing visible yet; keep asking without deciding.
    Wait,
    /// Spent; stop asking for frames.
    Spent,
}

impl Fling {
    /// The current speed, for diagnostics.
    pub fn velocity(&self) -> f32 {
        self.velocity
    }

    /// Starts coasting, unless too slow to see or NaN. Clamps teleports.
    pub fn new(velocity: f32) -> Option<Self> {
        if !velocity.is_finite() {
            return None;
        }
        let velocity = velocity.clamp(-FLING_MAX, FLING_MAX);
        (velocity.abs() >= FLING_MIN).then_some(Fling {
            velocity,
            carry: 0.0,
        })
    }

    /// Starts coasting on release. Fast enough always coasts; a long drag
    /// coasts even when slow, at the minimum speed towards where the drag
    /// went. `net` is the signed offset-space distance the drag moved
    /// along its scroll axis.
    pub fn new_release(velocity: f32, net: f32) -> Option<Self> {
        if !velocity.is_finite() {
            return None;
        }
        let velocity = velocity.clamp(-FLING_MAX, FLING_MAX);
        if velocity.abs() >= FLING_MIN {
            return Some(Fling {
                velocity,
                carry: 0.0,
            });
        }
        if net.is_finite() && net.abs() >= FLING_DIST_MIN {
            return Some(Fling {
                velocity: net.signum() * FLING_MIN,
                carry: 0.0,
            });
        }
        None
    }

    /// Advances by `dt` seconds. Non-positive steps hold still without
    /// decaying, so a paused loop resumes where it left off. Sub-pixel
    /// steps accumulate instead of deciding: only whole pixels move, and
    /// only a bound or a spent coast ends it.
    pub fn step(&mut self, dt: f32) -> Step {
        if self.velocity.abs() < FLING_STOP {
            return Step::Spent;
        }
        if dt <= 0.0 {
            return Step::Wait;
        }
        self.carry += self.velocity * dt;
        self.velocity *= (-dt / FLING_TAU).exp();
        let whole = self.carry.trunc();
        if whole == 0.0 {
            return Step::Wait;
        }
        self.carry -= whole;
        Step::Move(whole)
    }
}

/// Release velocity over a trailing window.
///
/// Fed every move with the elapsed seconds and the cumulative finger
/// offset; answers with the window's slope. A burst of tiny-dt pairs
/// reports wild instantaneous speeds, so no single pair may decide.
#[derive(Debug, Default)]
pub struct VelocityTracker {
    samples: std::collections::VecDeque<(f32, f32, f32)>,
}

impl VelocityTracker {
    /// Records a move. Times must not go backwards within one gesture.
    pub fn push(&mut self, at: f32, x: f32, y: f32) {
        while self
            .samples
            .front()
            .is_some_and(|s| s.0 < at - VELOCITY_WINDOW)
        {
            self.samples.pop_front();
        }
        self.samples.push_back((at, x, y));
        while self.samples.len() > VELOCITY_MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    /// The window's slope per axis, or zero with fewer than two samples.
    /// A wiggle that returns holds nearly still, as it should.
    pub fn velocity(&self) -> (f32, f32) {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return (0.0, 0.0);
        };
        let dt = last.0 - first.0;
        if dt <= 0.0 {
            return (0.0, 0.0);
        }
        ((last.1 - first.1) / dt, (last.2 - first.2) / dt)
    }

    /// How many samples the window holds, for diagnostics.
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn clear(&mut self) {
        self.samples.clear();
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
        assert!(Fling::new(49.0).is_none());
        assert!(Fling::new(-49.0).is_none());
        assert!(Fling::new(f32::NAN).is_none());
        assert!(Fling::new(1000.0).is_some());
    }

    /// A slow but long drag still coasts, towards where it went.
    #[test]
    fn a_long_slow_drag_flings() {
        let f = Fling::new_release(10.0, 200.0).expect("long drag should coast");
        assert_eq!(f.velocity, FLING_MIN);
        let f = Fling::new_release(-5.0, -200.0).expect("long drag should coast");
        assert_eq!(f.velocity, -FLING_MIN);
    }

    /// A slow short drag stops, and so does a drag back to its start.
    #[test]
    fn a_short_or_cancelled_drag_does_not_fling() {
        assert!(Fling::new_release(10.0, 20.0).is_none());
        assert!(Fling::new_release(10.0, -20.0).is_none());
        assert!(Fling::new_release(10.0, 0.0).is_none());
    }

    /// A fast release keeps its own speed, and NaN never starts.
    #[test]
    fn a_fast_release_keeps_its_speed() {
        let f = Fling::new_release(1000.0, 5.0).expect("fast enough to fling");
        assert_eq!(f.velocity, 1000.0);
        assert!(Fling::new_release(f32::NAN, 500.0).is_none());
    }

    /// Coasting decays and then stops asking for frames.
    #[test]
    fn a_fling_decays_then_stops() {
        let mut f = Fling::new(2000.0).expect("fast enough to fling");
        // First step moves with nearly the full speed.
        let Step::Move(first) = f.step(1.0 / 60.0) else {
            panic!("still coasting");
        };
        assert!(first > 20.0, "{first}");
        // A second of steps spends it.
        let mut frames = 0;
        loop {
            match f.step(1.0 / 60.0) {
                Step::Spent => break,
                Step::Move(_) | Step::Wait => {
                    frames += 1;
                    assert!(frames < 600, "coasting never stopped");
                }
            }
        }
        assert!(frames > 5, "stopped without coasting");
    }

    /// Teleports clamp instead of jumping across the whole list.
    #[test]
    fn a_wild_velocity_clamps() {
        let mut f = Fling::new(1e9).expect("clamped, not refused");
        let Step::Move(first) = f.step(1.0 / 60.0) else {
            panic!("still coasting");
        };
        assert!(first <= FLING_MAX / 60.0 + 1.0, "{first}");
    }

    /// A paused loop holds still without spending the fling.
    #[test]
    fn a_nonpositive_step_holds_still() {
        let mut f = Fling::new(2000.0).expect("fast enough to fling");
        assert_eq!(f.step(0.0), Step::Wait);
        assert_eq!(f.step(-1.0), Step::Wait);
        let Step::Move(next) = f.step(1.0 / 60.0) else {
            panic!("still coasting");
        };
        assert!(next > 20.0, "{next}");
    }

    /// Sub-pixel steps accumulate instead of ending the coast: a hot loop
    /// must not strand it mid-list on the first tiny delta.
    #[test]
    fn sub_pixel_steps_accumulate() {
        let mut f = Fling::new(120.0).expect("fast enough to fling");
        let mut moved = 0.0;
        for _ in 0..20 {
            match f.step(0.001) {
                Step::Move(d) => moved += d,
                Step::Wait => {}
                Step::Spent => panic!("spent on crumbs"),
            }
        }
        assert!(moved >= 1.0, "crumbs never became a pixel: {moved}");
    }

    /// One wild pair inside the window barely moves the slope.
    #[test]
    fn a_spike_pair_does_not_decide_the_velocity() {
        let mut v = VelocityTracker::default();
        v.push(0.00, 0.0, 0.0);
        v.push(0.04, -20.0, 0.0);
        v.push(0.08, -40.0, 0.0);
        // Two moves processed back-to-back: 69.5px in half a millisecond,
        // the shape behind the absurd peaks on device.
        v.push(0.0805, -109.5, 0.0);
        v.push(0.12, -60.0, 0.0);
        let (vx, _) = v.velocity();
        assert!(vx < 0.0 && vx > -2000.0, "{vx}");
    }

    /// A single sample, a still finger and a wiggle answer nothing to fling.
    #[test]
    fn too_little_history_answers_zero() {
        let mut v = VelocityTracker::default();
        assert_eq!(v.velocity(), (0.0, 0.0));
        v.push(0.0, 0.0, 0.0);
        assert_eq!(v.velocity(), (0.0, 0.0));
        v.push(0.05, 30.0, 0.0);
        v.push(0.10, 0.0, 0.0);
        assert_eq!(v.velocity(), (0.0, 0.0));
    }

    /// Old samples fall out of the window instead of dragging it.
    #[test]
    fn old_samples_expire() {
        let mut v = VelocityTracker::default();
        v.push(0.0, 0.0, 0.0);
        v.push(0.05, -500.0, 0.0);
        v.push(0.50, -500.0, 0.0);
        v.push(0.55, -510.0, 0.0);
        let (vx, _) = v.velocity();
        assert!(vx < 0.0 && vx > -1000.0, "{vx}");
        assert!(v.len() <= 3, "kept {}", v.len());
    }
}
