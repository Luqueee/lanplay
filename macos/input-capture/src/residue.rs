//! Spending fractional deltas as whole pixels without losing the fraction.
//!
//! macOS reports mouse deltas as `CGFloat`, and they are routinely fractional:
//! a slow, deliberate movement of the kind used to line up a shot arrives as a
//! long run of values well below one. Truncating each one sends nothing at all,
//! so the remote pointer sits still while the mouse is plainly moving, and
//! rounding each one independently is barely better because a run of `0.4`
//! still rounds to zero forever.
//!
//! What works is to keep what rounding could not spend and add it to the next
//! event, so a run of small deltas eventually crosses a whole pixel and the
//! total sent tracks the total moved. Rejected: scaling the wire format to
//! fixed point, which would move this problem onto the host and make the
//! additive property of motion depend on a scale factor both ends must agree
//! on.

/// The fraction of a pixel that rounding has not yet been able to send.
///
/// One of these per capture, not per axis pair passed around: the residue is
/// state, and two callers sharing one would each see the other's fractions.
#[derive(Clone, Copy, Default, Debug)]
pub struct Residue {
    x: f64,
    y: f64,
}

impl Residue {
    pub const fn new() -> Residue {
        Residue { x: 0.0, y: 0.0 }
    }

    /// Folds one event's deltas in and returns the whole pixels to send now.
    ///
    /// Never allocates, so it is safe to call from an event callback.
    #[inline]
    pub fn spend(&mut self, dx: f64, dy: f64) -> (i32, i32) {
        let (whole_x, rest_x) = split(self.x + dx);
        let (whole_y, rest_y) = split(self.y + dy);
        self.x = rest_x;
        self.y = rest_y;
        (whole_x, whole_y)
    }

    /// What is still owed to the remote pointer, for tests and diagnostics.
    pub const fn pending(&self) -> (f64, f64) {
        (self.x, self.y)
    }
}

/// Splits an accumulated axis into the pixels to send and the fraction to keep.
///
/// Ties go to the even side rather than away from zero, which matters more than
/// it looks. Rounding a tie away from zero leaves a residue of exactly half a
/// pixel, and the next event on that axis then spends it even if the mouse did
/// not move at all, so a mouse held still on one axis while the other moves
/// emits a stream of alternating single-pixel corrections. Ties to even keeps
/// half a pixel as half a pixel, and a stationary axis stays stationary.
#[inline]
fn split(value: f64) -> (i32, f64) {
    let whole = value
        .round_ties_even()
        .clamp(i32::MIN as f64, i32::MAX as f64);
    let rest = value - whole;
    // Rounding leaves at most half a pixel, so a larger remainder means the
    // value was too big for the wire or was not a number at all. Keeping it
    // would poison every later event, and the movement it describes is not
    // recoverable anyway, so the debt is written off here.
    if rest.abs() < 1.0 {
        (whole as i32, rest)
    } else {
        (whole as i32, 0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::Residue;

    /// The contract the whole module exists for: a slow drag of half-pixel
    /// deltas moves the pointer by the distance the mouse actually travelled.
    #[test]
    fn half_pixel_deltas_still_move_the_pointer() {
        let mut residue = Residue::new();
        let mut sent = 0;
        for _ in 0..10 {
            sent += residue.spend(0.5, 0.0).0;
        }
        assert_eq!(sent, 5);
    }

    /// A run that rounds to zero on its own must still add up, which is the
    /// case truncation and independent rounding both get wrong.
    #[test]
    fn sub_half_pixel_deltas_accumulate() {
        let mut residue = Residue::new();
        let mut sent = 0;
        for _ in 0..10 {
            sent += residue.spend(0.4, 0.0).0;
        }
        assert_eq!(sent, 4);
    }

    /// The residue is carried, not dropped: the first small delta sends
    /// nothing but leaves a debt behind.
    #[test]
    fn a_rounded_away_delta_leaves_a_debt() {
        let mut residue = Residue::new();
        assert_eq!(residue.spend(0.4, -0.4), (0, 0));
        assert_eq!(residue.pending(), (0.4, -0.4));
        assert_eq!(residue.spend(0.4, -0.4), (1, -1));
    }

    /// Rounding, not truncation: two thirds of a pixel is a pixel now rather
    /// than a pixel later.
    #[test]
    fn deltas_round_rather_than_truncate() {
        let mut residue = Residue::new();
        assert_eq!(residue.spend(0.6, 1.5), (1, 2));
    }

    /// Negative movement gets the same treatment, so a drag left and the same
    /// drag right cancel exactly.
    #[test]
    fn negative_movement_is_symmetric() {
        let mut residue = Residue::new();
        let mut sent = 0;
        for _ in 0..8 {
            sent += residue.spend(-0.25, 0.0).0;
        }
        for _ in 0..8 {
            sent += residue.spend(0.25, 0.0).0;
        }
        assert_eq!(sent, 0);
        assert_eq!(residue.pending(), (0.0, 0.0));
    }

    /// Both axes are independent, so one axis's debt cannot spend itself on the
    /// other.
    #[test]
    fn axes_do_not_share_a_residue() {
        let mut residue = Residue::new();
        assert_eq!(residue.spend(0.6, 0.0), (1, 0));
        assert_eq!(residue.pending(), (-0.4, 0.0));
        assert_eq!(residue.spend(0.0, 0.6), (0, 1));
    }

    /// An axis the mouse is not moving never moves the pointer, whatever debt
    /// an earlier event left on it. Rounding ties away from zero fails this.
    #[test]
    fn a_stationary_axis_stays_still() {
        let mut residue = Residue::new();
        assert_eq!(residue.spend(0.5, 0.0), (0, 0));
        for _ in 0..20 {
            assert_eq!(residue.spend(0.0, 0.0), (0, 0));
        }
        assert_eq!(residue.pending(), (0.5, 0.0));
    }

    /// A delta the wire cannot carry is clamped and forgotten rather than left
    /// to distort everything after it.
    #[test]
    fn an_unrepresentable_delta_does_not_poison_later_motion() {
        let mut residue = Residue::new();
        assert_eq!(residue.spend(1e18, f64::NAN).0, i32::MAX);
        assert_eq!(residue.pending(), (0.0, 0.0));
        assert_eq!(residue.spend(3.0, 3.0), (3, 3));
    }
}
