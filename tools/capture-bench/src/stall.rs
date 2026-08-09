//! Two questions about a stream of acquires that a percentile cannot answer.
//!
//! A p99 of the capture interval says how jittery the cadence was. It does not
//! say whether a tick was ever missed, and it says nothing at all about the
//! source clock, which can disagree with itself: both APIs hand back a QPC
//! instant chosen by someone else, and a mark that goes backwards makes every
//! delivery delay computed from it meaningless. Both checks are cheap, both
//! are structural rather than threshold-based, and both feed the gate.
//!
//! The period the classifier compares against is the measured display mode,
//! never a constant. The physical monitor runs at 3440x1440 at 100 Hz and the
//! virtual display used later runs at 1920x1080 at 120; a hardcoded 8.33 ms
//! would call every single interval on the first of those a stall.

use lanplay_telemetry::{Nanos, Timestamp};

/// How an acquire interval compares to the rate the source is running at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StallClass {
    /// At or inside one source period: the loop kept up.
    OnCadence,
    /// Longer than one period, shorter than two. At a steady rate this is
    /// mostly jitter around the tick, which is why the gate does not use it.
    OverOnePeriod,
    /// Longer than two periods: at least one source frame went by without an
    /// acquire completing. This one is a missed frame at any rate.
    OverTwoPeriods,
}

/// The period one source frame occupies at `hz`.
///
/// Zero when the rate is unknown, which callers must treat as "the cadence
/// cannot be judged" rather than as an infinitely fast source.
pub fn period_for(hz: f64) -> Nanos {
    if hz <= 0.0 {
        return Nanos::ZERO;
    }
    Nanos((1_000_000_000.0 / hz).round() as u64)
}

/// Classifies one interval against one source period.
///
/// Free-standing so the injected-stall recovery logic can reuse it without
/// polluting the run's own stall counts with the stall it deliberately caused.
pub fn classify(period: Nanos, interval: Nanos) -> StallClass {
    if interval > Nanos(period.get().saturating_mul(2)) {
        StallClass::OverTwoPeriods
    } else if interval > period {
        StallClass::OverOnePeriod
    } else {
        StallClass::OnCadence
    }
}

/// Cumulative: an interval over two periods is also over one. Reported that
/// way so that `over_one - over_two` is the count of merely-late intervals
/// without the reader having to know which convention was used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StallCounts {
    pub observed: u64,
    pub over_one_period: u64,
    pub over_two_periods: u64,
    pub max: Nanos,
}

#[derive(Clone, Copy, Debug)]
pub struct StallClassifier {
    period: Nanos,
    counts: StallCounts,
}

impl StallClassifier {
    pub fn new(period: Nanos) -> Self {
        StallClassifier {
            period,
            counts: StallCounts::default(),
        }
    }

    pub fn observe(&mut self, interval: Nanos) -> StallClass {
        let class = classify(self.period, interval);
        self.counts.observed += 1;
        self.counts.max = self.counts.max.max(interval);
        match class {
            StallClass::OnCadence => {}
            StallClass::OverOnePeriod => self.counts.over_one_period += 1,
            StallClass::OverTwoPeriods => {
                self.counts.over_one_period += 1;
                self.counts.over_two_periods += 1;
            }
        }
        class
    }

    pub fn counts(&self) -> StallCounts {
        self.counts
    }

    /// Forgets what the warm-up saw. The period is kept: it describes the
    /// source, not the window.
    pub fn reset_counts(&mut self) {
        self.counts = StallCounts::default();
    }
}

/// Whether a sequence of OS-supplied instants ever went backwards.
///
/// Not a percentile and not a tolerance: one backwards step invalidates every
/// delay derived from that clock, so the only interesting counts are zero and
/// non-zero. The size of the worst step is kept because it tells a reader
/// whether they are looking at a rounding artefact or at a different clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicCheck {
    regressions: u64,
    worst_backstep: Nanos,
    last: Option<Timestamp>,
}

impl MonotonicCheck {
    pub fn observe(&mut self, at: Timestamp) {
        if let Some(previous) = self.last
            && at < previous
        {
            self.regressions += 1;
            self.worst_backstep = self.worst_backstep.max(previous.saturating_since(at));
        }
        // Advance even on a regression: otherwise one bad mark makes every
        // subsequent good one look like a regression too, and the count stops
        // meaning "how many times the clock went backwards".
        self.last = Some(at);
    }

    pub fn regressions(&self) -> u64 {
        self.regressions
    }

    pub fn worst_backstep(&self) -> Nanos {
        self.worst_backstep
    }

    /// Clears the counts but keeps the last mark, so the warm-up/steady-state
    /// boundary is still checked rather than being a free pass.
    pub fn reset_counts(&mut self) {
        self.regressions = 0;
        self.worst_backstep = Nanos::ZERO;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 100 Hz: the rate of the physical monitor these results come from.
    const PERIOD: Nanos = Nanos(10_000_000);

    #[test]
    fn the_period_is_the_reciprocal_of_the_measured_rate() {
        assert_eq!(period_for(100.0), PERIOD);
        assert_eq!(period_for(120.0), Nanos(8_333_333));
        assert_eq!(period_for(60_000.0 / 1_001.0), Nanos(16_683_333));
    }

    #[test]
    fn a_rate_that_was_never_learned_yields_no_period() {
        // The caller has to refuse the run rather than judge a cadence
        // against a period it invented.
        assert_eq!(period_for(0.0), Nanos::ZERO);
        assert_eq!(period_for(-1.0), Nanos::ZERO);
    }

    #[test]
    fn gdi_rounding_and_the_real_timing_are_not_the_same_period() {
        assert_ne!(period_for(60_000.0 / 1_001.0), period_for(59.0));
    }

    #[test]
    fn an_interval_inside_the_period_is_on_cadence() {
        assert_eq!(classify(PERIOD, Nanos(9_000_000)), StallClass::OnCadence);
        assert_eq!(
            classify(PERIOD, PERIOD),
            StallClass::OnCadence,
            "exactly one period is the cadence, not a miss"
        );
    }

    #[test]
    fn the_boundaries_are_where_they_are_claimed_to_be() {
        assert_eq!(
            classify(PERIOD, Nanos(PERIOD.get() + 1)),
            StallClass::OverOnePeriod
        );
        assert_eq!(
            classify(PERIOD, Nanos(PERIOD.get() * 2)),
            StallClass::OverOnePeriod,
            "two periods exactly is still only one frame's worth of lateness"
        );
        assert_eq!(
            classify(PERIOD, Nanos(PERIOD.get() * 2 + 1)),
            StallClass::OverTwoPeriods
        );
    }

    #[test]
    fn the_period_comes_from_the_caller_not_from_a_constant() {
        // The same 12 ms interval is late at 120 Hz and on cadence at 60. A
        // classifier that assumed either rate would be wrong on this machine.
        let interval = Nanos(12_000_000);
        assert_eq!(
            classify(Nanos(8_333_333), interval),
            StallClass::OverOnePeriod
        );
        assert_eq!(classify(Nanos(16_666_667), interval), StallClass::OnCadence);
    }

    #[test]
    fn a_long_stall_counts_in_both_buckets() {
        let mut classifier = StallClassifier::new(PERIOD);
        classifier.observe(Nanos(9_000_000));
        classifier.observe(Nanos(15_000_000));
        classifier.observe(Nanos(500_000_000));

        let counts = classifier.counts();
        assert_eq!(counts.observed, 3);
        assert_eq!(counts.over_one_period, 2, "buckets are cumulative");
        assert_eq!(counts.over_two_periods, 1);
        assert_eq!(counts.max, Nanos(500_000_000));
    }

    #[test]
    fn resetting_keeps_the_period_and_drops_the_warmup_counts() {
        let mut classifier = StallClassifier::new(PERIOD);
        classifier.observe(Nanos(900_000_000));
        classifier.reset_counts();
        assert_eq!(classifier.counts(), StallCounts::default());
        // The period survived the reset: the same interval still classifies.
        assert_eq!(
            classifier.observe(Nanos(900_000_000)),
            StallClass::OverTwoPeriods
        );
    }

    #[test]
    fn a_rising_clock_never_reports_a_regression() {
        let mut check = MonotonicCheck::default();
        for nanos in [1, 2, 2, 100, 100_000] {
            check.observe(Timestamp::from_nanos(nanos));
        }
        assert_eq!(
            check.regressions(),
            0,
            "a repeated instant is a duplicate frame, not a clock going backwards"
        );
    }

    #[test]
    fn one_backwards_mark_is_one_regression_not_a_cascade() {
        let mut check = MonotonicCheck::default();
        check.observe(Timestamp::from_nanos(1_000));
        check.observe(Timestamp::from_nanos(400));
        check.observe(Timestamp::from_nanos(500));
        check.observe(Timestamp::from_nanos(600));

        assert_eq!(check.regressions(), 1);
        assert_eq!(check.worst_backstep(), Nanos(600));
    }

    #[test]
    fn the_worst_backstep_is_the_largest_one() {
        let mut check = MonotonicCheck::default();
        check.observe(Timestamp::from_nanos(1_000));
        check.observe(Timestamp::from_nanos(900));
        check.observe(Timestamp::from_nanos(10));
        assert_eq!(check.regressions(), 2);
        assert_eq!(check.worst_backstep(), Nanos(890));
    }

    #[test]
    fn a_reset_still_checks_across_the_boundary() {
        let mut check = MonotonicCheck::default();
        check.observe(Timestamp::from_nanos(1_000));
        check.reset_counts();
        check.observe(Timestamp::from_nanos(900));
        assert_eq!(
            check.regressions(),
            1,
            "the warm-up's last mark still has to be less than the first steady-state one"
        );
    }
}
