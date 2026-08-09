use core::fmt;

use hdrhistogram::Histogram;

use crate::clock::Nanos;
use crate::timeline::SPANS;

/// Widest interval a histogram can hold: 10 s. Anything longer is a stall, not
/// a latency, and gets clipped and counted.
const MAX_NANOS: u64 = 10_000_000_000;
/// Two significant figures, i.e. 1% bucket precision. Enough for p99 latency
/// and ~26 KB per histogram.
const SIGNIFICANT_FIGURES: u8 = 2;

pub(crate) struct Histograms {
    pub spans: Vec<Histogram<u32>>,
    pub frame_age: Histogram<u32>,
    pub present_interval: Histogram<u32>,
    pub capture_interval: Histogram<u32>,
    pub clipped: u64,
}

impl Histograms {
    pub fn new() -> Self {
        Histograms {
            spans: SPANS.iter().map(|_| new_histogram()).collect(),
            frame_age: new_histogram(),
            present_interval: new_histogram(),
            capture_interval: new_histogram(),
            clipped: 0,
        }
    }

    pub fn record(histogram: &mut Histogram<u32>, value: Nanos, clipped: &mut u64) {
        let raw = value.get();
        if raw > MAX_NANOS {
            *clipped += 1;
        }
        // Clamped rather than dropped: a clipped sample still belongs in the
        // tail, and losing it would flatter the p99.
        let clamped = raw.clamp(1, MAX_NANOS);
        histogram
            .record(clamped)
            .expect("value clamped into histogram range");
    }
}

fn new_histogram() -> Histogram<u32> {
    Histogram::new_with_bounds(1, MAX_NANOS, SIGNIFICANT_FIGURES).expect("valid histogram bounds")
}

/// Percentile summary of one interval.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SpanStats {
    pub name: &'static str,
    pub count: u64,
    pub mean: Nanos,
    pub p50: Nanos,
    pub p95: Nanos,
    pub p99: Nanos,
    pub max: Nanos,
}

impl SpanStats {
    pub(crate) fn from_histogram(name: &'static str, histogram: &Histogram<u32>) -> Self {
        SpanStats {
            name,
            count: histogram.len(),
            mean: Nanos(histogram.mean() as u64),
            p50: Nanos(histogram.value_at_quantile(0.50)),
            p95: Nanos(histogram.value_at_quantile(0.95)),
            p99: Nanos(histogram.value_at_quantile(0.99)),
            max: Nanos(histogram.max()),
        }
    }
}

/// Everything the pipeline lost or double-counted. All of these should be zero
/// in a healthy run; each non-zero value points at a specific defect.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counters {
    /// Frames for which at least one mark was seen.
    pub frames_started: u64,
    /// Frames that reached `present_submit`.
    pub frames_presented: u64,
    /// Frames evicted from the collector ring without ever being presented.
    pub frames_incomplete: u64,
    pub events_recorded: u64,
    /// Marks lost because the lock-free queue was full.
    pub events_dropped: u64,
    /// A stage marked twice for the same frame.
    pub duplicate_marks: u64,
    /// Marks that arrived after their frame was already finalised.
    pub late_events: u64,
    /// Samples larger than the histogram ceiling.
    pub clipped_samples: u64,
}

/// Aggregate view of a run.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub spans: Vec<SpanStats>,
    pub frame_age: SpanStats,
    /// Interval between consecutive `present_submit` marks: client cadence.
    pub present_interval: SpanStats,
    /// Interval between consecutive `frame_created` marks: source cadence.
    pub capture_interval: SpanStats,
    pub counters: Counters,
    /// Wall time between the first and last presented frame.
    pub window: Nanos,
}

impl Snapshot {
    /// Presented frames per second over the measured window.
    pub fn presented_per_second(&self) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds <= 0.0 || self.counters.frames_presented < 2 {
            return 0.0;
        }
        // n frames span n-1 intervals.
        (self.counters.frames_presented - 1) as f64 / seconds
    }

    /// True when nothing was lost: no dropped marks, no incomplete frames, no
    /// duplicates. The Fase 0 gate needs this to hold before any number below
    /// it can be trusted.
    pub fn is_lossless(&self) -> bool {
        let c = &self.counters;
        c.events_dropped == 0
            && c.frames_incomplete == 0
            && c.duplicate_marks == 0
            && c.late_events == 0
    }
}

impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{:<18} {:>7} {:>9} {:>9} {:>9} {:>9}",
            "span", "count", "p50", "p95", "p99", "max"
        )?;
        for stats in &self.spans {
            write_stats(f, stats)?;
        }
        writeln!(f)?;
        write_stats(f, &self.frame_age)?;
        write_stats(f, &self.present_interval)?;
        write_stats(f, &self.capture_interval)?;
        writeln!(f)?;
        writeln!(
            f,
            "presented {} frames in {:.2} s ({:.1}/s), started {}, incomplete {}",
            self.counters.frames_presented,
            self.window.as_secs_f64(),
            self.presented_per_second(),
            self.counters.frames_started,
            self.counters.frames_incomplete,
        )?;
        write!(
            f,
            "events {} recorded, {} dropped, {} duplicate, {} late, {} clipped",
            self.counters.events_recorded,
            self.counters.events_dropped,
            self.counters.duplicate_marks,
            self.counters.late_events,
            self.counters.clipped_samples,
        )
    }
}

fn write_stats(f: &mut fmt::Formatter<'_>, stats: &SpanStats) -> fmt::Result {
    if stats.count == 0 {
        return writeln!(f, "{:<18} {:>7}", stats.name, 0);
    }
    writeln!(
        f,
        "{:<18} {:>7} {:>6.2} ms {:>6.2} ms {:>6.2} ms {:>6.2} ms",
        stats.name,
        stats.count,
        stats.p50.as_millis_f64(),
        stats.p95.as_millis_f64(),
        stats.p99.as_millis_f64(),
        stats.max.as_millis_f64(),
    )
}
