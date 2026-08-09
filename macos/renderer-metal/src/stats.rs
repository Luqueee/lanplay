use core::fmt;

use hdrhistogram::Histogram;
use lanplay_telemetry::Nanos;

/// Distribution of one renderer-side measurement.
///
/// The telemetry crate has a type of the same name, but it summarises whole
/// frame segments assembled off the hot path. These numbers are taken by the
/// render loop itself around individual calls, so they live and die with the
/// loop and must not drag a collector thread into the picture.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Percentiles {
    pub count: u64,
    pub p50: Nanos,
    pub p95: Nanos,
    pub p99: Nanos,
    pub max: Nanos,
}

impl fmt::Display for Percentiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "n={:<7} p50={:>9} p95={:>9} p99={:>9} max={:>9}",
            self.count, self.p50, self.p95, self.p99, self.max
        )
    }
}

/// One recorded quantity, in nanoseconds.
///
/// Three significant figures over a one-second ceiling costs a few kilobytes
/// and resolves a microsecond at the low end, which is the scale every
/// interesting renderer measurement lives at. Values above the ceiling are
/// saturated rather than dropped: a two-second stall is still a stall, and
/// losing the sample entirely would flatter the percentiles.
pub(crate) struct Track {
    histogram: Histogram<u64>,
    ceiling: u64,
}

impl Track {
    const CEILING_NS: u64 = 1_000_000_000;

    pub(crate) fn new() -> Track {
        Track {
            histogram: Histogram::new_with_bounds(1, Track::CEILING_NS, 3)
                .expect("hdrhistogram bounds are constant and valid"),
            ceiling: Track::CEILING_NS,
        }
    }

    #[inline]
    pub(crate) fn record(&mut self, value: Nanos) {
        let clamped = value.0.clamp(1, self.ceiling);
        self.histogram
            .record(clamped)
            .expect("value is clamped into the histogram range");
    }

    pub(crate) fn percentiles(&self) -> Percentiles {
        Percentiles {
            count: self.histogram.len(),
            p50: Nanos(self.histogram.value_at_quantile(0.50)),
            p95: Nanos(self.histogram.value_at_quantile(0.95)),
            p99: Nanos(self.histogram.value_at_quantile(0.99)),
            max: Nanos(self.histogram.max()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_track_reports_no_samples() {
        assert_eq!(Track::new().percentiles(), Percentiles::default());
    }

    /// Three significant figures means a reported value is the top of a
    /// bucket, so the contract is "within 0.1 %", not "exact".
    #[track_caller]
    fn assert_close(actual: Nanos, expected: u64) {
        let slack = expected / 1_000 + 1;
        assert!(
            actual.0 >= expected && actual.0 <= expected + slack,
            "{} is not within {slack} ns above {expected}",
            actual.0
        );
    }

    #[test]
    fn percentiles_follow_the_recorded_distribution() {
        let mut track = Track::new();
        for value in 1..=100u64 {
            track.record(Nanos(value * 1_000));
        }
        let p = track.percentiles();
        assert_eq!(p.count, 100);
        assert_close(p.p50, 50_000);
        assert_close(p.p95, 95_000);
        assert_close(p.p99, 99_000);
        assert_close(p.max, 100_000);
    }

    #[test]
    fn outliers_saturate_instead_of_vanishing() {
        let mut track = Track::new();
        track.record(Nanos(5_000_000_000));
        let p = track.percentiles();
        assert_eq!(p.count, 1);
        assert_close(p.max, Track::CEILING_NS);
    }
}
