//! Turning the two kinds of scrolling AppKit reports into the one kind the
//! wire carries.
//!
//! A scroll event arrives in one of two forms and `hasPreciseScrollingDeltas`
//! is what tells them apart. A wheel with real detents is the discrete form:
//! it reports whole notches, one per click of the wheel, which is exactly what
//! the protocol carries and needs no conversion. A trackpad, or a mouse whose
//! driver smooths its wheel, is the precise form: it reports a continuous
//! distance in points, and there is no notch in it at all. Only the discrete
//! form maps to the wire, so the precise form has to be converted.
//!
//! Ten points to the notch, which is not a figure invented here: AppKit's own
//! legacy `deltaY` on a precise event is the point distance divided by ten, so
//! anything already written against the old field agrees with this one.
//!
//! Converting by truncation would make a trackpad useless. A slow, deliberate
//! two-finger drag arrives as a long run of distances well under ten points,
//! every one of which truncates to nothing, so the remote view would sit still
//! while the fingers plainly moved. Accumulating instead, and spending a notch
//! only once a whole one has been earned, means the fractions add up and the
//! drag eventually scrolls. The residue is kept by the same rounding the
//! pointer uses, so a fraction that could not be spent this event is still
//! owed rather than discarded, and an axis nobody touched stays at zero
//! instead of emitting corrections.

use crate::residue::Residue;

/// Points of precise scrolling that stand for one notch of a wheel.
pub const POINTS_PER_NOTCH: f64 = 10.0;

/// What a scroll event was measured with, which decides whether its numbers
/// are notches already or a distance that has to be earned into them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scrolling {
    /// A wheel with detents: whole notches, one per click.
    Discrete,
    /// A trackpad or a smoothed wheel: a continuous distance in points.
    Precise,
}

/// The fraction of a notch that rounding has not yet been able to send.
///
/// One per capture rather than one per event, because the whole point is that
/// what this event could not spend is what the next one starts with.
#[derive(Clone, Copy, Default, Debug)]
pub struct Notches {
    residue: Residue,
}

impl Notches {
    pub const fn new() -> Notches {
        Notches {
            residue: Residue::new(),
        }
    }

    /// Folds one scroll event in and returns the whole notches to send now,
    /// which is `(0, 0)` for an event that has not yet earned one.
    ///
    /// Never allocates, so it is safe to call from an event callback.
    #[inline]
    pub fn spend(&mut self, dx: f64, dy: f64, scrolling: Scrolling) -> (i16, i16) {
        let scale = match scrolling {
            Scrolling::Discrete => 1.0,
            Scrolling::Precise => 1.0 / POINTS_PER_NOTCH,
        };
        let (x, y) = self.residue.spend(dx * scale, dy * scale);
        (clamp(x), clamp(y))
    }

    /// What is still owed to the remote view, for tests and diagnostics.
    pub const fn pending(&self) -> (f64, f64) {
        self.residue.pending()
    }
}

/// The wire carries a notch count in sixteen bits. A single event past that is
/// not a scroll anybody performed, and saturating keeps the direction right
/// where a cast would flip it.
#[inline]
const fn clamp(notches: i32) -> i16 {
    if notches > i16::MAX as i32 {
        i16::MAX
    } else if notches < i16::MIN as i32 {
        i16::MIN
    } else {
        notches as i16
    }
}

#[cfg(test)]
mod tests {
    use super::{Notches, Scrolling};

    /// The trackpad case the accumulation exists for: fractions of a notch
    /// that individually round to nothing still scroll once they add up.
    #[test]
    fn precise_fractions_accumulate_into_a_whole_notch() {
        let mut notches = Notches::new();
        // Four points at a time, which is 0.4 of a notch: below the half that
        // would round up, so nothing is spent on the first event.
        assert_eq!(notches.spend(0.0, 4.0, Scrolling::Precise), (0, 0));
        assert_eq!(notches.pending().1, 0.4);

        // 0.8 of a notch now, which rounds to one and leaves a fifth of a
        // notch owed rather than thrown away.
        assert_eq!(notches.spend(0.0, 4.0, Scrolling::Precise), (0, 1));
        let owed = notches.pending().1;
        assert!((owed - -0.2).abs() < 1e-9, "residue was {owed}");

        // Twenty points more is two notches on top of what was owed, and the
        // total sent tracks the total scrolled rather than drifting behind it.
        assert_eq!(notches.spend(0.0, 20.0, Scrolling::Precise), (0, 2));
    }

    /// A run of small drags reaches the remote view rather than vanishing, and
    /// the total notches sent is the total distance divided by the notch.
    #[test]
    fn a_slow_drag_eventually_scrolls_by_the_distance_it_covered() {
        let mut notches = Notches::new();
        let mut total = 0i32;
        for _ in 0..100 {
            total += i32::from(notches.spend(0.0, 3.0, Scrolling::Precise).1);
        }
        // Three hundred points is thirty notches, and at most half a notch may
        // still be owed at the end.
        assert_eq!(total, 30);
    }

    /// The other half of keeping a residue: an axis nobody touched must emit
    /// nothing at all, however busy the axis beside it is.
    #[test]
    fn an_untouched_axis_emits_nothing_while_the_other_scrolls() {
        let mut notches = Notches::new();
        for _ in 0..50 {
            let (dx, _) = notches.spend(0.0, 7.0, Scrolling::Precise);
            assert_eq!(dx, 0);
        }
        assert_eq!(notches.pending().0, 0.0);
    }

    /// A wheel already reports notches, so nothing is converted and nothing is
    /// owed: one click of the wheel is one notch on the wire.
    #[test]
    fn discrete_notches_pass_through_untouched() {
        let mut notches = Notches::new();
        assert_eq!(notches.spend(0.0, 1.0, Scrolling::Discrete), (0, 1));
        assert_eq!(notches.spend(0.0, -3.0, Scrolling::Discrete), (0, -3));
        assert_eq!(notches.spend(2.0, 0.0, Scrolling::Discrete), (2, 0));
        assert_eq!(notches.pending(), (0.0, 0.0));
    }

    #[test]
    fn a_scroll_beyond_the_wire_saturates_rather_than_flipping_direction() {
        let mut notches = Notches::new();
        let (_, dy) = notches.spend(0.0, 100_000.0, Scrolling::Discrete);
        assert_eq!(dy, i16::MAX);
        let (_, dy) = notches.spend(0.0, -100_000.0, Scrolling::Discrete);
        assert_eq!(dy, i16::MIN);
    }
}
