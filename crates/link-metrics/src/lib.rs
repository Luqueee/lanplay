//! When the network started and finished delivering each access unit.
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
//! sees the first datagram of an access unit and the instant it hands the
//! completed unit over, both on the receive thread's own monotonic clock. No
//! decoder, no renderer, no window, no display.
//!
//! Two cadences rather than one, because they separate two different faults:
//!
//! ```text
//! first  p99 30 ms, complete p99 31 ms   the whole unit starts late
//! first  p99  9 ms, complete p99 30 ms   units start on time and finish badly
//! ```
//!
//! The first indicts delivery of the unit; the second indicts what happens
//! to its datagrams once they are in the air.
//!
//! Percentiles are not enough to characterise the fault. A p99 of 15.92 ms
//! against a 16.67 ms threshold says nothing about how many units crossed
//! it, and inferring "one percent are late" from a percentile that sits
//! below the threshold is simply wrong. The thresholds below are counted.

use hdrhistogram::Histogram;
use lanplay_telemetry::{Nanos, Timestamp};
use parking_lot::Mutex;

/// Longest gap the histogram can hold. Ten seconds is a dead link, and
/// anything past it is clipped and counted rather than resized into.
const MAX_NANOS: u64 = 10_000_000_000;

/// Multiples of the source period an access unit interval is counted against.
///
/// Ranking on a percentile makes two links with different failure shapes
/// look alike. Counting crossings does not: an AP that delivers forty units
/// a minute more than two periods late is describable, comparable, and
/// falsifiable in a way "p99 18 ms" is not.
pub const THRESHOLDS: [f64; 6] = [1.25, 1.5, 2.0, 3.0, 4.0, 6.0];

/// The multiple above which an interval is treated as a stall, and below
/// whose reciprocal the units that follow are treated as catching up.
const STALL_MULTIPLE: f64 = 2.0;

/// Access unit delivery: two cadences, the tail of the second, and the
/// clusters in it.
///
/// Two sets of everything, for the same reason the telemetry collector keeps
/// two: a cumulative percentile cannot be differenced, so a ten-minute run
/// whose middle ten seconds collapsed still reports a healthy p99. The
/// windowed set is drained by the sampler and starts again.
pub struct Delivery {
    inner: Mutex<Inner>,
    /// The source period. Everything in [`THRESHOLDS`] is a multiple of it.
    period: Nanos,
}

struct Inner {
    first: Series,
    complete: Series,
    tail: Tail,
    window_tail: Tail,
    /// Open catch-up run, if the last interval was part of one.
    catching_up: Option<u64>,
    /// When the last stall began, and the intervals between them.
    last_stall: Option<Timestamp>,
    stall_gap: Histogram<u32>,
    stall_gap_window: Histogram<u32>,
    span_start: Option<Timestamp>,
    span_end: Option<Timestamp>,
    window_start: Option<Timestamp>,
}

/// One cadence: the interval between consecutive marks of the same kind.
struct Series {
    cumulative: Histogram<u32>,
    window: Histogram<u32>,
    previous: Option<Timestamp>,
    clipped: u64,
    count: u64,
}

impl Series {
    fn new() -> Self {
        Series {
            cumulative: new_histogram(),
            window: new_histogram(),
            previous: None,
            clipped: 0,
            count: 0,
        }
    }

    /// Returns the interval this mark closed, if it closed one.
    fn mark(&mut self, at: Timestamp) -> Option<Nanos> {
        self.count += 1;
        let interval = self.previous.and_then(|previous| at.since(previous));
        if let Some(interval) = interval {
            let value = interval.get().min(MAX_NANOS);
            if value != interval.get() {
                self.clipped += 1;
            }
            let _ = self.cumulative.record(value);
            let _ = self.window.record(value);
        }
        self.previous = Some(at);
        interval
    }
}

/// How far into the tail a run went, counted rather than estimated.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tail {
    /// Intervals at or above each multiple in [`THRESHOLDS`].
    pub over: [u64; THRESHOLDS.len()],
    /// Stalls that were followed by at least one unit arriving early.
    ///
    /// This is the shape of bunching: the link holds units back and then
    /// releases them together. A stall with no catch-up is a plain gap and
    /// says something different, so the two are not pooled.
    pub clusters: u64,
    /// Units delivered early across every catch-up, so a caller can take a
    /// mean without this module deciding how to round it.
    pub catch_up_total: u64,
    pub catch_up_max: u64,
    /// Interval between the starts of consecutive stalls.
    ///
    /// A stall rate alone cannot tell a periodic cause from a random one,
    /// and the difference decides where to look next: a tight distribution
    /// around a fixed period indicts a timer somewhere - a scan, a beacon,
    /// a power-save cycle - while a broad one indicts contention. Held as a
    /// pair of percentiles because a mean would hide exactly that.
    pub stall_gap_p50_ms: f64,
    pub stall_gap_p95_ms: f64,
}

impl Tail {
    /// Crossings per minute, which is the figure two access points can be
    /// compared on regardless of how long each was measured for.
    pub fn per_minute(&self, index: usize, span_s: f64) -> f64 {
        if span_s <= 0.0 {
            return 0.0;
        }
        self.over[index] as f64 * 60.0 / span_s
    }

    pub fn clusters_per_minute(&self, span_s: f64) -> f64 {
        if span_s <= 0.0 {
            return 0.0;
        }
        self.clusters as f64 * 60.0 / span_s
    }

    pub fn mean_catch_up(&self) -> f64 {
        if self.clusters == 0 {
            return 0.0;
        }
        self.catch_up_total as f64 / self.clusters as f64
    }
}

/// What one window of delivery looked like.
#[derive(Clone, Copy, Debug, Default)]
pub struct Window {
    pub delivered: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
    /// The same percentiles over the interval between the *first* datagram
    /// of consecutive access units.
    pub first_p50_ms: f64,
    pub first_p95_ms: f64,
    pub first_p99_ms: f64,
    pub first_max_ms: f64,
    /// Wall time the window covered, which turns counts into rates.
    pub span_s: f64,
    pub tail: Tail,
}

impl Delivery {
    /// `period` is the source period: the reciprocal of the rate the host
    /// was asked to produce. Thresholds mean nothing without it.
    pub fn new(period: Nanos) -> Self {
        Delivery {
            inner: Mutex::new(Inner {
                first: Series::new(),
                complete: Series::new(),
                tail: Tail::default(),
                window_tail: Tail::default(),
                catching_up: None,
                last_stall: None,
                stall_gap: new_histogram(),
                stall_gap_window: new_histogram(),
                span_start: None,
                span_end: None,
                window_start: None,
            }),
            period,
        }
    }

    /// Records the first datagram of an access unit arriving at `at`.
    ///
    /// Called once per access unit, not once per datagram: the depacketiser
    /// already knows which arrival opened a unit.
    pub fn first_seen(&self, at: Timestamp) {
        self.inner.lock().first.mark(at);
    }

    /// Records that an access unit became complete at `at`.
    ///
    /// Called from the receive loop, once per access unit rather than once
    /// per datagram, so the lock is taken 120 times a second and never on
    /// the path of an individual packet.
    pub fn completed(&self, at: Timestamp) {
        let mut inner = self.inner.lock();
        inner.span_start.get_or_insert(at);
        inner.window_start.get_or_insert(at);
        inner.span_end = Some(at);
        let Some(interval) = inner.complete.mark(at) else {
            return;
        };
        let period = self.period.get() as f64;
        let value = interval.get() as f64;

        for (index, multiple) in THRESHOLDS.iter().enumerate() {
            if value >= period * multiple {
                inner.tail.over[index] += 1;
                inner.window_tail.over[index] += 1;
            }
        }

        // A stall opens a catch-up; units arriving inside a period continue
        // it; the first unit back on cadence closes it. Counted at the close
        // rather than at the stall, because a stall nothing follows is a gap
        // and not the bunching this is looking for.
        if value >= period * STALL_MULTIPLE {
            close_catch_up(&mut inner);
            inner.catching_up = Some(0);
            if let Some(previous) = inner.last_stall
                && let Some(gap) = at.since(previous)
            {
                let gap = gap.get().min(MAX_NANOS);
                let _ = inner.stall_gap.record(gap);
                let _ = inner.stall_gap_window.record(gap);
            }
            inner.last_stall = Some(at);
        } else if let Some(count) = inner.catching_up.as_mut() {
            if value < period {
                *count += 1;
            } else {
                close_catch_up(&mut inner);
            }
        }
    }

    /// Percentiles and counts over the whole run so far.
    pub fn cumulative(&self) -> Window {
        let inner = self.inner.lock();
        let span = match (inner.span_start, inner.span_end) {
            (Some(start), Some(end)) => end.since(start).unwrap_or(Nanos::ZERO),
            _ => Nanos::ZERO,
        };
        let mut tail = inner.tail;
        tail.stall_gap_p50_ms = ms(inner.stall_gap.value_at_quantile(0.50));
        tail.stall_gap_p95_ms = ms(inner.stall_gap.value_at_quantile(0.95));
        summarise(&inner, inner.complete.count, tail, span)
    }

    /// Percentiles and counts since the previous call, then starts fresh.
    pub fn take_window(&self) -> Window {
        let mut inner = self.inner.lock();
        let span = match (inner.window_start, inner.span_end) {
            (Some(start), Some(end)) => end.since(start).unwrap_or(Nanos::ZERO),
            _ => Nanos::ZERO,
        };
        let taken = Window {
            delivered: inner.complete.window.len(),
            p50_ms: ms(inner.complete.window.value_at_quantile(0.50)),
            p95_ms: ms(inner.complete.window.value_at_quantile(0.95)),
            p99_ms: ms(inner.complete.window.value_at_quantile(0.99)),
            max_ms: ms(inner.complete.window.max()),
            first_p50_ms: ms(inner.first.window.value_at_quantile(0.50)),
            first_p95_ms: ms(inner.first.window.value_at_quantile(0.95)),
            first_p99_ms: ms(inner.first.window.value_at_quantile(0.99)),
            first_max_ms: ms(inner.first.window.max()),
            span_s: span.get() as f64 / 1e9,
            tail: Tail {
                stall_gap_p50_ms: ms(inner.stall_gap_window.value_at_quantile(0.50)),
                stall_gap_p95_ms: ms(inner.stall_gap_window.value_at_quantile(0.95)),
                ..inner.window_tail
            },
        };
        inner.complete.window.reset();
        inner.first.window.reset();
        inner.window_tail = Tail::default();
        inner.stall_gap_window.reset();
        inner.window_start = inner.span_end;
        taken
    }
}

/// Ends an open catch-up, keeping it only if anything actually caught up.
fn close_catch_up(inner: &mut Inner) {
    let Some(count) = inner.catching_up.take() else {
        return;
    };
    if count == 0 {
        return;
    }
    for tail in [&mut inner.tail, &mut inner.window_tail] {
        tail.clusters += 1;
        tail.catch_up_total += count;
        tail.catch_up_max = tail.catch_up_max.max(count);
    }
}

impl Default for Delivery {
    /// 120 fps, which is what every harness in this repository asks for.
    fn default() -> Self {
        Self::new(Nanos::from_millis_f64(1000.0 / 120.0))
    }
}

fn new_histogram() -> Histogram<u32> {
    Histogram::new_with_bounds(1, MAX_NANOS, 3).expect("valid histogram bounds")
}

fn ms(value: u64) -> f64 {
    value as f64 / 1e6
}

fn summarise(inner: &Inner, delivered: u64, tail: Tail, span: Nanos) -> Window {
    Window {
        delivered,
        p50_ms: ms(inner.complete.cumulative.value_at_quantile(0.50)),
        p95_ms: ms(inner.complete.cumulative.value_at_quantile(0.95)),
        p99_ms: ms(inner.complete.cumulative.value_at_quantile(0.99)),
        max_ms: ms(inner.complete.cumulative.max()),
        first_p50_ms: ms(inner.first.cumulative.value_at_quantile(0.50)),
        first_p95_ms: ms(inner.first.cumulative.value_at_quantile(0.95)),
        first_p99_ms: ms(inner.first.cumulative.value_at_quantile(0.99)),
        first_max_ms: ms(inner.first.cumulative.max()),
        span_s: span.get() as f64 / 1e9,
        tail,
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

#[cfg(test)]
mod tests {
    use super::*;

    const T: f64 = 1000.0 / 120.0;

    fn delivery() -> Delivery {
        Delivery::new(Nanos::from_millis_f64(T))
    }

    /// Feeds a sequence of intervals in milliseconds.
    fn feed(delivery: &Delivery, intervals: &[f64]) {
        let mut at = 1_000_000_000u64;
        delivery.completed(Timestamp::from_nanos(at));
        for interval in intervals {
            at += (interval * 1e6) as u64;
            delivery.completed(Timestamp::from_nanos(at));
        }
    }

    #[test]
    fn thresholds_are_counted_not_inferred_from_a_percentile() {
        // Nine on cadence and one at four periods. A p99 over ten samples
        // cannot express "one crossing of 2T"; a count can.
        let delivery = delivery();
        let mut intervals = vec![T; 9];
        intervals.push(T * 4.0);
        feed(&delivery, &intervals);

        let window = delivery.cumulative();
        assert_eq!(window.tail.over[0], 1, "1.25T");
        assert_eq!(window.tail.over[2], 1, "2T");
        assert_eq!(window.tail.over[4], 1, "4T");
        assert_eq!(window.tail.over[5], 0, "6T is not crossed");
    }

    #[test]
    fn a_stall_followed_by_a_burst_is_one_cluster() {
        // The shape the experiment is looking for: the link holds units back
        // and releases them together.
        let delivery = delivery();
        feed(&delivery, &[T, T, T * 4.0, 0.8, 1.1, 5.0, T, T]);
        let tail = delivery.cumulative().tail;
        assert_eq!(tail.clusters, 1);
        assert_eq!(
            tail.catch_up_total, 3,
            "three units arrived inside a period"
        );
        assert_eq!(tail.catch_up_max, 3);
    }

    #[test]
    fn a_gap_with_nothing_behind_it_is_not_a_cluster() {
        // A stall the link never makes up is a different fault from bunching
        // and must not be counted as one.
        let delivery = delivery();
        feed(&delivery, &[T, T * 4.0, T, T, T]);
        let tail = delivery.cumulative().tail;
        assert_eq!(tail.over[2], 1, "the stall is still counted");
        assert_eq!(tail.clusters, 0);
        assert_eq!(tail.catch_up_total, 0);
    }

    #[test]
    fn a_periodic_stall_is_visible_as_a_tight_gap_distribution() {
        // The measurement that separates a timer from contention: stalls
        // every hundred units land on a fixed interval, and a mean would
        // report the same number for stalls scattered at random.
        let delivery = delivery();
        let mut at = 1_000_000_000u64;
        delivery.completed(Timestamp::from_nanos(at));
        for index in 1..=1000u64 {
            at += if index % 100 == 0 {
                (T * 4.0 * 1e6) as u64
            } else {
                (T * 1e6) as u64
            };
            delivery.completed(Timestamp::from_nanos(at));
        }
        let tail = delivery.cumulative().tail;
        // A hundred units at 8.33 ms plus the stall itself.
        assert!(
            (tail.stall_gap_p50_ms - 858.0).abs() < 20.0,
            "p50 gap {:.1}",
            tail.stall_gap_p50_ms
        );
        assert!(
            (tail.stall_gap_p95_ms - tail.stall_gap_p50_ms).abs() < 20.0,
            "a fixed period must not spread: p50 {:.1} p95 {:.1}",
            tail.stall_gap_p50_ms,
            tail.stall_gap_p95_ms
        );
    }

    #[test]
    fn consecutive_stalls_do_not_merge() {
        let delivery = delivery();
        feed(&delivery, &[T * 3.0, 0.5, T * 3.0, 0.5, 0.5, T]);
        let tail = delivery.cumulative().tail;
        assert_eq!(tail.clusters, 2);
        assert_eq!(tail.catch_up_max, 2);
    }

    #[test]
    fn rates_are_per_minute_of_measured_span() {
        // Thirty seconds with two crossings is four a minute, and the span
        // has to come from the marks rather than from what a caller assumed.
        let delivery = delivery();
        let mut at = 1_000_000_000u64;
        delivery.completed(Timestamp::from_nanos(at));
        for index in 0..3600 {
            at += if index % 1800 == 1799 {
                (T * 4.0 * 1e6) as u64
            } else {
                (T * 1e6) as u64
            };
            delivery.completed(Timestamp::from_nanos(at));
        }
        let window = delivery.cumulative();
        assert!(
            (window.span_s - 30.2).abs() < 0.5,
            "span {:.2}",
            window.span_s
        );
        let rate = window.tail.per_minute(2, window.span_s);
        assert!((rate - 4.0).abs() < 0.2, "2T per minute {rate:.2}");
    }

    #[test]
    fn the_two_cadences_are_independent() {
        // Units that start on time and finish late: the fault this split
        // exists to name.
        let delivery = delivery();
        let mut at = 1_000_000_000u64;
        for index in 0..100u64 {
            let start = at + (index as f64 * T * 1e6) as u64;
            delivery.first_seen(Timestamp::from_nanos(start));
            // Every tenth unit takes twenty milliseconds longer to finish.
            let extra = if index % 10 == 0 { 20_000_000 } else { 500_000 };
            delivery.completed(Timestamp::from_nanos(start + extra));
        }
        at += 1;
        let _ = at;
        let window = delivery.cumulative();
        assert!(
            window.first_p99_ms < 9.0,
            "first p99 {:.2} should track the source",
            window.first_p99_ms
        );
        assert!(
            window.p99_ms > 20.0,
            "complete p99 {:.2} should show the finishing fault",
            window.p99_ms
        );
    }

    #[test]
    fn a_window_reports_only_what_happened_inside_it() {
        let delivery = delivery();
        feed(&delivery, &[T, T * 4.0, T, T]);
        let first = delivery.take_window();
        assert_eq!(first.tail.over[2], 1);
        feed(&delivery, &[T, T, T]);
        let second = delivery.take_window();
        assert_eq!(
            second.tail.over[2], 0,
            "the stall belonged to the first window"
        );
    }
}
