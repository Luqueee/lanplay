use core::fmt;

use hdrhistogram::Histogram;

use crate::clock::{ClockDomain, Nanos};
use crate::timeline::{SEGMENT_COUNT, Segment};

/// Widest interval a histogram can hold: 10 s. Anything longer is a stall, not
/// a latency, and gets clipped and counted.
const MAX_NANOS: u64 = 10_000_000_000;
/// Two significant figures, i.e. 1% bucket precision. Enough for p99 latency
/// and ~26 KB per histogram.
const SIGNIFICANT_FIGURES: u8 = 2;
/// Below this many frames a p99 rests on fewer than ~36 tail observations,
/// which is not enough to call a regression.
pub const P99_SOAK_FRAMES: u64 = 3_600;

pub(crate) struct Histograms {
    pub segments: Vec<Histogram<u32>>,
    pub frame_age: Histogram<u32>,
    pub local_age: Histogram<u32>,
    pub unattributed_gap: Histogram<u32>,
    pub present_interval: Histogram<u32>,
    pub source_interval: Histogram<u32>,
    pub clipped: u64,
    /// A second set covering only the frames since the last
    /// [`crate::Telemetry::take_window`].
    ///
    /// A cumulative percentile cannot be differenced, so a ten-minute run
    /// whose middle ten seconds collapsed still reports a healthy p99. The
    /// only way to see that is to keep a set that gets reset.
    pub window: WindowHistograms,
}

pub(crate) struct WindowHistograms {
    pub local_age: Histogram<u32>,
    pub present_interval: Histogram<u32>,
    pub source_interval: Histogram<u32>,
    pub presented: u64,
}

impl WindowHistograms {
    fn new() -> Self {
        WindowHistograms {
            local_age: new_histogram(),
            present_interval: new_histogram(),
            source_interval: new_histogram(),
            presented: 0,
        }
    }

    /// Empties the window so the next one starts clean.
    pub fn reset(&mut self) {
        self.local_age.reset();
        self.present_interval.reset();
        self.source_interval.reset();
        self.presented = 0;
    }
}

/// What happened since the previous [`crate::Telemetry::take_window`].
#[derive(Clone, Debug)]
pub struct Window {
    pub local_age: Percentiles,
    pub present_interval: Percentiles,
    /// Cadence of whatever this machine sees first: a capture on the host, a
    /// datagram on the client. The series a link stall shows up in.
    pub source_interval: Percentiles,
    pub presented: u64,
    /// Wall time the window covered.
    pub span: Nanos,
}

impl Histograms {
    pub fn new() -> Self {
        Histograms {
            segments: Segment::ALL.iter().map(|_| new_histogram()).collect(),
            frame_age: new_histogram(),
            local_age: new_histogram(),
            unattributed_gap: new_histogram(),
            present_interval: new_histogram(),
            source_interval: new_histogram(),
            clipped: 0,
            window: WindowHistograms::new(),
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

/// Percentile summary of one series.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Percentiles {
    pub label: &'static str,
    pub count: u64,
    pub mean: Nanos,
    pub p50: Nanos,
    pub p95: Nanos,
    pub p99: Nanos,
    pub max: Nanos,
}

impl Percentiles {
    pub(crate) fn from_histogram(label: &'static str, histogram: &Histogram<u32>) -> Self {
        Percentiles {
            label,
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
    /// Segments measured between two different clocks; only as good as the
    /// offset estimate between them.
    pub cross_domain_segments: u64,
}

/// Aggregate view of a run.
#[derive(Clone, Debug)]
pub struct Snapshot {
    /// Clock every local mark was read from.
    pub clock_domain: ClockDomain,
    /// Indexed by [`Segment::index`].
    pub segments: Vec<Percentiles>,
    pub frame_age: Percentiles,
    /// From this machine's first sight of a frame to putting it on screen.
    /// The end-to-end number a receiver can measure without a synchronised
    /// clock; equal to `frame_age` when the frame was born here.
    pub local_age: Percentiles,
    /// Frame age no named segment accounts for: missing instrumentation.
    pub unattributed_gap: Percentiles,
    /// Interval between consecutive `present_submit` marks: client cadence.
    pub present_interval: Percentiles,
    /// Interval between the first mark this machine makes for consecutive
    /// frames: the rate at which work arrives here, whether that is a capture
    /// or a datagram.
    pub source_interval: Percentiles,
    pub counters: Counters,
    /// Wall time between the first and last presented frame.
    pub window: Nanos,
}

impl Snapshot {
    pub fn segment(&self, segment: Segment) -> &Percentiles {
        &self.segments[segment.index()]
    }

    /// Presented frames per second over the measured window.
    pub fn presented_per_second(&self) -> f64 {
        let seconds = self.window.as_secs_f64();
        if seconds <= 0.0 || self.counters.frames_presented < 2 {
            return 0.0;
        }
        // n frames span n-1 intervals.
        (self.counters.frames_presented - 1) as f64 / seconds
    }

    /// No mark was lost, duplicated, or arrived after its frame closed.
    ///
    /// Deliberately says nothing about frames completing: a pipeline whose
    /// job is to drop frames when it falls behind still has intact
    /// instrumentation, and conflating the two would make every
    /// latest-frame-wins renderer look broken.
    pub fn marks_intact(&self) -> bool {
        let c = &self.counters;
        c.events_dropped == 0 && c.duplicate_marks == 0 && c.late_events == 0
    }

    /// Marks intact *and* every frame reached present. Only meaningful for a
    /// pipeline that is not allowed to drop frames, such as the synthetic
    /// harness.
    pub fn is_lossless(&self) -> bool {
        self.marks_intact() && self.counters.frames_incomplete == 0
    }

    /// Whether the run is long enough for its p99 to mean anything.
    pub fn p99_is_soaked(&self) -> bool {
        self.counters.frames_presented >= P99_SOAK_FRAMES
    }
}

impl fmt::Display for Snapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "clock domain: {}", self.clock_domain.label())?;
        writeln!(f)?;
        writeln!(
            f,
            "{:<18} {:>7} {:>9} {:>9} {:>9} {:>9}  kind",
            "segment", "count", "p50", "p95", "p99", "max"
        )?;
        for segment in Segment::ALL {
            write_series(
                f,
                &self.segments[segment.index()],
                Some(segment.kind().label()),
            )?;
        }
        writeln!(f)?;
        write_series(f, &self.frame_age, None)?;
        write_series(f, &self.local_age, None)?;
        write_series(f, &self.unattributed_gap, None)?;
        write_series(f, &self.present_interval, None)?;
        write_series(f, &self.source_interval, None)?;
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
        writeln!(
            f,
            "events {} recorded, {} dropped, {} duplicate, {} late, {} clipped, {} cross-domain",
            self.counters.events_recorded,
            self.counters.events_dropped,
            self.counters.duplicate_marks,
            self.counters.late_events,
            self.counters.clipped_samples,
            self.counters.cross_domain_segments,
        )?;
        write!(f, "{}", self.tail_confidence())
    }
}

impl Snapshot {
    /// One line stating exactly how much tail the p99 column rests on, so a
    /// five-second run is never quoted as if it were a soak.
    pub fn tail_confidence(&self) -> String {
        let frames = self.counters.frames_presented;
        let tail = frames / 100;
        if self.p99_is_soaked() {
            format!("p99 backed by {tail} tail observations from {frames} frames")
        } else {
            format!(
                "p99 backed by only {tail} tail observations from {frames} frames; \
                 soak {P99_SOAK_FRAMES}+ before trusting it"
            )
        }
    }
}

fn write_series(
    f: &mut fmt::Formatter<'_>,
    series: &Percentiles,
    kind: Option<&str>,
) -> fmt::Result {
    if series.count == 0 {
        return writeln!(f, "{:<18} {:>7}", series.label, 0);
    }
    writeln!(
        f,
        "{:<18} {:>7} {:>6.2} ms {:>6.2} ms {:>6.2} ms {:>6.2} ms  {}",
        series.label,
        series.count,
        series.p50.as_millis_f64(),
        series.p95.as_millis_f64(),
        series.p99.as_millis_f64(),
        series.max.as_millis_f64(),
        kind.unwrap_or(""),
    )
}

const _: () = assert!(SEGMENT_COUNT == Segment::ALL.len());
