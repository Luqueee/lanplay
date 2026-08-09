//! Percentile summaries for the series this harness owns.
//!
//! The telemetry crate already summarises per-frame segments; these are the
//! per-*packet* series it cannot see, because a packet is not a frame:
//! scheduling error, syscall cost, and arrival spacing.

use core::fmt;

use hdrhistogram::Histogram;
use lanplay_telemetry::Nanos;

/// Widest interval a series can hold: 10 s, matching the telemetry crate.
/// Anything longer is a stall, not a latency, and is clipped and counted.
const MAX_NANOS: u64 = 10_000_000_000;

/// Three significant figures rather than the telemetry crate's two: send
/// syscalls land around a microsecond, and at 1% buckets the entire
/// distribution collapses into a handful of cells.
const SIGNIFICANT_FIGURES: u8 = 3;

pub struct Series {
    label: &'static str,
    histogram: Histogram<u64>,
    clipped: u64,
}

impl Series {
    pub fn new(label: &'static str) -> Self {
        Series {
            label,
            histogram: Histogram::new_with_bounds(1, MAX_NANOS, SIGNIFICANT_FIGURES)
                .expect("valid histogram bounds"),
            clipped: 0,
        }
    }

    pub fn record(&mut self, value: Nanos) {
        if value.get() > MAX_NANOS {
            self.clipped += 1;
        }
        self.histogram.saturating_record(value.get());
    }

    pub fn count(&self) -> u64 {
        self.histogram.len()
    }

    pub fn is_empty(&self) -> bool {
        self.histogram.is_empty()
    }

    pub fn mean(&self) -> Nanos {
        Nanos(self.histogram.mean() as u64)
    }

    pub fn quantile(&self, quantile: f64) -> Nanos {
        Nanos(self.histogram.value_at_quantile(quantile))
    }

    pub fn max(&self) -> Nanos {
        Nanos(self.histogram.max())
    }

    /// Column titles matching the row [`Series`] renders.
    pub const HEADER: &'static str = concat!(
        "series                     count       mean        p50        p95        p99      ",
        "p99.9        max"
    );
}

/// Microseconds, not the millisecond unit the rest of the project uses: every
/// number here is sub-millisecond, and in milliseconds they all print `0.00`.
fn micros(value: Nanos) -> f64 {
    value.get() as f64 / 1_000.0
}

impl fmt::Display for Series {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<20} {:>9} {:>9.2}µ {:>9.2}µ {:>9.2}µ {:>9.2}µ {:>9.2}µ {:>9.2}µ",
            self.label,
            self.count(),
            micros(self.mean()),
            micros(self.quantile(0.50)),
            micros(self.quantile(0.95)),
            micros(self.quantile(0.99)),
            micros(self.quantile(0.999)),
            micros(self.max()),
        )?;
        if self.clipped > 0 {
            write!(f, "  ({} clipped)", self.clipped)?;
        }
        Ok(())
    }
}
