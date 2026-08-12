//! When the network finished delivering each access unit.
//!
//! This exists because the cadence of the link was being read off the
//! cadence of the display. `source_interval` in the telemetry collector is
//! computed from finalised frames, and a frame finalises when it is
//! presented, so a suspended display link turned a perfectly healthy link
//! into a series reading 141 ms at p99 while it was losing nothing at all.
//!
//! A stage must never be measured through a later one:
//!
//! ```text
//! source cadence        host
//! delivery cadence      this module, at the depacketiser
//! decode cadence        VideoToolbox
//! presentation cadence  the display link
//! ```
//!
//! Delivery is timestamped where it happens: the instant the depacketiser
//! hands over a complete access unit, on the receive thread's own monotonic
//! clock. No decoder, no renderer, no window, no display.

use hdrhistogram::Histogram;
use lanplay_telemetry::Timestamp;
use parking_lot::Mutex;

/// Longest gap the histogram can hold. Ten seconds is a dead link, and
/// anything past it is clipped and counted rather than resized into.
const MAX_NANOS: u64 = 10_000_000_000;

/// Intervals between consecutive complete access units.
///
/// Two sets, for the same reason the telemetry collector keeps two: a
/// cumulative percentile cannot be differenced, so a ten-minute run whose
/// middle ten seconds collapsed still reports a healthy p99. The windowed
/// set is drained by the sampler and starts again.
pub struct Delivery {
    inner: Mutex<Inner>,
}

struct Inner {
    cumulative: Histogram<u32>,
    window: Histogram<u32>,
    previous: Option<Timestamp>,
    clipped: u64,
    delivered: u64,
}

/// What one window of delivery looked like.
#[derive(Clone, Copy, Debug, Default)]
pub struct Window {
    pub delivered: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl Delivery {
    pub fn new() -> Self {
        Delivery {
            inner: Mutex::new(Inner {
                cumulative: new_histogram(),
                window: new_histogram(),
                previous: None,
                clipped: 0,
                delivered: 0,
            }),
        }
    }

    /// Records that an access unit became complete at `at`.
    ///
    /// Called from the receive loop, once per access unit rather than once
    /// per datagram, so the lock is taken 120 times a second and never on
    /// the path of an individual packet.
    pub fn completed(&self, at: Timestamp) {
        let mut inner = self.inner.lock();
        inner.delivered += 1;
        if let Some(previous) = inner.previous
            && let Some(interval) = at.since(previous)
        {
            let value = interval.get().min(MAX_NANOS);
            if value != interval.get() {
                inner.clipped += 1;
            }
            let _ = inner.cumulative.record(value);
            let _ = inner.window.record(value);
        }
        inner.previous = Some(at);
    }

    /// Percentiles over the whole run so far.
    pub fn cumulative(&self) -> Window {
        let inner = self.inner.lock();
        summarise(&inner.cumulative, inner.delivered)
    }

    /// Percentiles since the previous call, then starts a fresh window.
    pub fn take_window(&self) -> Window {
        let mut inner = self.inner.lock();
        let taken = summarise(&inner.window, inner.window.len());
        inner.window.reset();
        taken
    }
}

impl Default for Delivery {
    fn default() -> Self {
        Self::new()
    }
}

fn new_histogram() -> Histogram<u32> {
    Histogram::new_with_bounds(1, MAX_NANOS, 3).expect("valid histogram bounds")
}

fn summarise(histogram: &Histogram<u32>, delivered: u64) -> Window {
    let ms = |value: u64| value as f64 / 1e6;
    Window {
        delivered,
        p50_ms: ms(histogram.value_at_quantile(0.50)),
        p95_ms: ms(histogram.value_at_quantile(0.95)),
        p99_ms: ms(histogram.value_at_quantile(0.99)),
        max_ms: ms(histogram.max()),
    }
}

impl core::fmt::Display for Window {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "n={} p50 {:.2} ms p95 {:.2} ms p99 {:.2} ms max {:.2} ms",
            self.delivered, self.p50_ms, self.p95_ms, self.p99_ms, self.max_ms
        )
    }
}
