//! Does this number grow over a run?
//!
//! Backlogs and resident memory are judged by slope, not by their last value:
//! a decoder queue that gains one frame a second reads as "fine" at any single
//! instant and is fatal over ten minutes. Both gates in this project are
//! phrased as "no sustained growth", so both need the same fit.

use crate::clock::Timestamp;

/// A time series with a least-squares slope.
#[derive(Clone, Debug, Default)]
pub struct Trend {
    samples: Vec<(Timestamp, f64)>,
}

impl Trend {
    pub fn new() -> Self {
        Trend {
            samples: Vec::new(),
        }
    }

    pub fn record(&mut self, value: f64) {
        self.record_at(Timestamp::now(), value);
    }

    pub fn record_at(&mut self, at: Timestamp, value: f64) {
        self.samples.push((at, value));
    }

    pub fn count(&self) -> usize {
        self.samples.len()
    }

    pub fn first(&self) -> Option<f64> {
        self.samples.first().map(|(_, value)| *value)
    }

    pub fn last(&self) -> Option<f64> {
        self.samples.last().map(|(_, value)| *value)
    }

    pub fn max(&self) -> Option<f64> {
        self.samples
            .iter()
            .map(|(_, value)| *value)
            .fold(None, |acc: Option<f64>, value| {
                Some(acc.map_or(value, |current| current.max(value)))
            })
    }

    pub fn span(&self) -> Option<crate::clock::Nanos> {
        let first = self.samples.first()?.0;
        let last = self.samples.last()?.0;
        Some(last.saturating_since(first))
    }

    /// The samples taken at least `warmup` after the first one.
    ///
    /// Start-up costs — buffer pools filling, shaders compiling, a fixture
    /// being read — are not a leak, but a line fitted through them reads as
    /// one on a short run. A leak check belongs in steady state.
    pub fn after_warmup(&self, warmup: crate::clock::Nanos) -> Trend {
        let Some(origin) = self.samples.first().map(|(at, _)| *at) else {
            return Trend::new();
        };
        let threshold = origin.add(warmup);
        Trend {
            samples: self
                .samples
                .iter()
                .filter(|(at, _)| *at >= threshold)
                .copied()
                .collect(),
        }
    }

    /// Least-squares growth per minute.
    ///
    /// A slope rather than last-minus-first, because these series are noisy:
    /// one late allocation should not read as a leak, and one page returned to
    /// the OS should not hide one.
    pub fn slope_per_minute(&self) -> Option<f64> {
        if self.samples.len() < 3 {
            return None;
        }
        let origin = self.samples[0].0;
        let minutes = |at: Timestamp| at.saturating_since(origin).as_secs_f64() / 60.0;

        let n = self.samples.len() as f64;
        let mean_x = self.samples.iter().map(|(at, _)| minutes(*at)).sum::<f64>() / n;
        let mean_y = self.samples.iter().map(|(_, value)| value).sum::<f64>() / n;
        let covariance: f64 = self
            .samples
            .iter()
            .map(|(at, value)| (minutes(*at) - mean_x) * (value - mean_y))
            .sum();
        let variance: f64 = self
            .samples
            .iter()
            .map(|(at, _)| (minutes(*at) - mean_x).powi(2))
            .sum();
        if variance <= 0.0 {
            return None;
        }
        Some(covariance / variance)
    }

    /// Whether growth stays under `tolerance` per minute. A series too short
    /// to have a slope is not stable, it is unmeasured.
    pub fn is_stable(&self, tolerance_per_minute: f64) -> bool {
        self.slope_per_minute()
            .is_some_and(|slope| slope <= tolerance_per_minute)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_minute(minute: u64) -> Timestamp {
        Timestamp::from_nanos(minute * 60_000_000_000)
    }

    #[test]
    fn a_flat_series_has_no_growth() {
        let mut trend = Trend::new();
        for minute in 0..10 {
            trend.record_at(at_minute(minute), 100_000_000.0);
        }
        assert_eq!(trend.slope_per_minute(), Some(0.0));
        assert!(trend.is_stable(1.0));
    }

    #[test]
    fn a_leak_shows_up_as_a_slope_through_noise() {
        let mut trend = Trend::new();
        let noise = [0.0, 3e6, -2e6, 1e6, -1.5e6];
        for minute in 0..10 {
            let value = 100e6 + minute as f64 * 10e6 + noise[minute as usize % noise.len()];
            trend.record_at(at_minute(minute), value);
        }
        let slope = trend.slope_per_minute().expect("slope");
        assert!(
            (slope - 10e6).abs() < 1e6,
            "slope {slope} should be near 10 MB/min"
        );
        assert!(!trend.is_stable(1_048_576.0));
    }

    #[test]
    fn warm_up_can_be_excluded_from_the_fit() {
        let mut trend = Trend::new();
        // Ten seconds of start-up cost, then a flat steady state.
        for second in 0..10 {
            trend.record_at(
                Timestamp::from_nanos(second * 1_000_000_000),
                100e6 + second as f64 * 12e6,
            );
        }
        for second in 10..70 {
            trend.record_at(Timestamp::from_nanos(second * 1_000_000_000), 220e6);
        }

        // A naive fit over the whole run reads the start-up cost as a leak.
        assert!(!trend.is_stable(1_048_576.0));

        let steady = trend.after_warmup(crate::clock::Nanos::from_millis(10_000));
        assert_eq!(steady.count(), 60);
        assert_eq!(steady.slope_per_minute(), Some(0.0));
        assert!(steady.is_stable(1_048_576.0));
    }

    #[test]
    fn excluding_warm_up_from_a_short_series_leaves_nothing_to_fit() {
        let mut trend = Trend::new();
        for second in 0..5 {
            trend.record_at(Timestamp::from_nanos(second * 1_000_000_000), 100e6);
        }
        let steady = trend.after_warmup(crate::clock::Nanos::from_millis(10_000));
        assert_eq!(steady.count(), 0);
        assert_eq!(steady.slope_per_minute(), None);
        assert!(!steady.is_stable(f64::MAX));
    }

    #[test]
    fn a_backlog_that_drains_has_a_negative_slope() {
        let mut trend = Trend::new();
        for minute in 0..6 {
            trend.record_at(at_minute(minute), (10 - minute) as f64);
        }
        assert!(trend.slope_per_minute().is_some_and(|slope| slope < 0.0));
        assert!(trend.is_stable(0.0));
    }

    #[test]
    fn too_few_samples_is_unmeasured_not_stable() {
        let mut trend = Trend::new();
        trend.record_at(at_minute(0), 1.0);
        trend.record_at(at_minute(1), 2.0);
        assert_eq!(trend.slope_per_minute(), None);
        assert!(!trend.is_stable(f64::MAX));
    }
}
