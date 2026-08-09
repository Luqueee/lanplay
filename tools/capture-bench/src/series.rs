//! Turning a stream of measurements into numbers a report can quote.
//!
//! Samples are kept whole rather than folded into a histogram. Phase 3 has to
//! be able to say "these two APIs differ by 0.4 ms at p99", and a 1%-bucket
//! histogram cannot tell a 0.4 ms difference from a bucket boundary at the
//! latencies involved. A run is minutes, not hours, so the memory is a few
//! megabytes and the exactness is free.
//!
//! Keeping the samples also makes the `compare` subcommand possible: a block's
//! statistics are the tail of the same vector, so per-block and whole-run
//! numbers cannot drift apart.

use std::collections::BTreeMap;

use lanplay_telemetry::Nanos;
use serde::Serialize;

/// Percentile summary of one series, in the unit the whole project reasons in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub count: u64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Series {
    samples: Vec<u64>,
}

impl Series {
    pub fn new() -> Self {
        Series::default()
    }

    pub fn push(&mut self, value: Nanos) {
        self.samples.push(value.get());
    }

    /// Drops every sample. Used once, when the warm-up window ends: start-up
    /// costs are a separate result, not a tail on the steady-state one.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn summary(&self) -> Summary {
        self.summary_from(0)
    }

    /// The summary of everything pushed since the series held `start` samples.
    ///
    /// How a `compare` block reports itself without keeping a second copy of
    /// the data.
    pub fn summary_from(&self, start: usize) -> Summary {
        let start = start.min(self.samples.len());
        summarise(&self.samples[start..])
    }
}

fn summarise(samples: &[u64]) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let total: u128 = sorted.iter().map(|value| *value as u128).sum();
    Summary {
        count: sorted.len() as u64,
        mean_ms: nanos_to_ms((total / sorted.len() as u128) as u64),
        p50_ms: nanos_to_ms(quantile(&sorted, 0.50)),
        p95_ms: nanos_to_ms(quantile(&sorted, 0.95)),
        p99_ms: nanos_to_ms(quantile(&sorted, 0.99)),
        max_ms: nanos_to_ms(sorted[sorted.len() - 1]),
    }
}

/// Nearest-rank percentile: the smallest sample at or above the quantile.
///
/// No interpolation, because an interpolated p99 reports a latency that was
/// never observed, and the point of this benchmark is what the API actually
/// did.
fn quantile(sorted: &[u64], quantile: f64) -> u64 {
    let rank = (quantile * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn nanos_to_ms(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

/// A small-integer histogram, for the counts the APIs report about themselves.
///
/// Desktop Duplication's `AccumulatedFrames` and the WGC frame pool's depth
/// are not latencies; quoting a p99 of them would be theatre. What matters is
/// how often they exceeded one, and how bad the worst was.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Distribution {
    pub samples: u64,
    pub max: u32,
    pub over_one: u64,
    pub histogram: BTreeMap<u32, u64>,
}

impl Distribution {
    pub fn record(&mut self, value: u32) {
        self.samples += 1;
        self.max = self.max.max(value);
        if value > 1 {
            self.over_one += 1;
        }
        *self.histogram.entry(value).or_insert(0) += 1;
    }

    pub fn clear(&mut self) {
        *self = Distribution::default();
    }

    /// Arithmetic mean, or `None` when nothing was recorded. Used as the
    /// backlog sample fed to the growth trend.
    pub fn mean(&self) -> Option<f64> {
        if self.samples == 0 {
            return None;
        }
        let total: u64 = self
            .histogram
            .iter()
            .map(|(value, count)| *value as u64 * count)
            .sum();
        Some(total as f64 / self.samples as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(values: &[u64]) -> Series {
        let mut series = Series::new();
        for value in values {
            series.push(Nanos(*value));
        }
        series
    }

    #[test]
    fn an_empty_series_summarises_to_zero_rather_than_panicking() {
        // A backend that delivered nothing must still produce a report.
        let summary = Series::new().summary();
        assert_eq!(summary.count, 0);
        assert_eq!(summary.max_ms, 0.0);
    }

    #[test]
    fn percentiles_are_observed_samples_not_interpolations() {
        // 100 samples of 1..=100 ms. p99 must be the 99th, not something
        // between the 99th and the 100th.
        let values: Vec<u64> = (1..=100).map(|ms| ms * 1_000_000).collect();
        let summary = series(&values).summary();
        assert_eq!(summary.p50_ms, 50.0);
        assert_eq!(summary.p95_ms, 95.0);
        assert_eq!(summary.p99_ms, 99.0);
        assert_eq!(summary.max_ms, 100.0);
    }

    #[test]
    fn a_single_sample_is_its_own_every_percentile() {
        let summary = series(&[7_000_000]).summary();
        assert_eq!(summary.p50_ms, 7.0);
        assert_eq!(summary.p99_ms, 7.0);
        assert_eq!(summary.max_ms, 7.0);
        assert_eq!(summary.mean_ms, 7.0);
    }

    #[test]
    fn a_block_summary_sees_only_the_block() {
        // The whole point of summary_from: a compare block must not inherit
        // the previous block's tail.
        let mut series = series(&[100_000_000, 100_000_000]);
        let mark = series.len();
        series.push(Nanos(1_000_000));
        series.push(Nanos(3_000_000));

        let block = series.summary_from(mark);
        assert_eq!(block.count, 2);
        assert_eq!(block.max_ms, 3.0);
        assert_eq!(series.summary().max_ms, 100.0, "the run keeps everything");
    }

    #[test]
    fn a_mark_past_the_end_yields_an_empty_summary() {
        assert_eq!(series(&[1]).summary_from(9).count, 0);
    }

    #[test]
    fn clearing_discards_warmup_without_disturbing_later_samples() {
        let mut series = series(&[500_000_000]);
        series.clear();
        series.push(Nanos(2_000_000));
        assert_eq!(series.summary().max_ms, 2.0);
    }

    #[test]
    fn a_distribution_counts_only_values_above_one_as_backlog() {
        let mut distribution = Distribution::default();
        for value in [0, 1, 1, 2, 5] {
            distribution.record(value);
        }
        assert_eq!(distribution.samples, 5);
        assert_eq!(distribution.max, 5);
        assert_eq!(
            distribution.over_one, 2,
            "one accumulated frame is the API keeping up, not falling behind"
        );
        assert_eq!(distribution.histogram[&1], 2);
        assert_eq!(distribution.mean(), Some(9.0 / 5.0));
    }

    #[test]
    fn an_empty_distribution_has_no_mean_to_feed_a_trend() {
        assert_eq!(Distribution::default().mean(), None);
    }
}
