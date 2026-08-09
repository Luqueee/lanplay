//! Everything the capture loop learns, accumulated.
//!
//! Kept free of Direct3D types on purpose: the loop hands over plain numbers,
//! so the accounting can be reasoned about — and the report built — without a
//! GPU anywhere near it.

use lanplay_telemetry::{Nanos, Timestamp, Trend};

use crate::report::{CaptureReport, StabilityReport};
use crate::series::{Distribution, Series};
use crate::stall::{MonotonicCheck, StallClass, StallClassifier};

/// One frame, as the loop saw it.
#[derive(Clone, Copy, Debug)]
pub struct FrameObservation {
    /// When the backend's acquire returned, on the backend's own clock.
    pub acquired: Timestamp,
    /// The OS-supplied mark, whatever the API meant by it.
    pub source: Timestamp,
    /// Source mark to acquire return, where the mark was usable.
    pub delivery: Option<Nanos>,
    /// Time spent inside `acquire`.
    pub duration: Nanos,
    pub accumulated: Option<u32>,
    pub pending: Option<u32>,
    pub duplicate: bool,
}

pub struct Stats {
    period: Nanos,
    window_start: Option<Timestamp>,
    window_end: Option<Timestamp>,

    pub frames: u64,
    pub timeouts: u64,
    pub duplicates: u64,
    pub delivery_unusable: u64,

    pub delivery: Series,
    pub acquire: Series,
    pub interval: Series,
    pub source_hold: Series,

    pub accumulated: Distribution,
    pub pending: Distribution,

    last_acquire: Option<Timestamp>,
    /// Set when the loop caused the next gap itself, so a deliberate stall is
    /// not charged to the API as a missed frame.
    skip_next_interval: bool,

    stalls: StallClassifier,
    source_clock: MonotonicCheck,
    acquire_clock: MonotonicCheck,

    pub access_lost: u64,
    pub api_resets: u64,
    pub restart_failures: u64,
    pub frames_after_last_restart: u64,

    /// Backlog seen since the last trend sample, averaged rather than sampled
    /// instantaneously: one unlucky frame is not a trend.
    backlog_window: Distribution,
    pub backlog: Trend,
    pub memory: Trend,
}

impl Stats {
    pub fn new(period: Nanos) -> Self {
        Stats {
            period,
            window_start: None,
            window_end: None,
            frames: 0,
            timeouts: 0,
            duplicates: 0,
            delivery_unusable: 0,
            delivery: Series::new(),
            acquire: Series::new(),
            interval: Series::new(),
            source_hold: Series::new(),
            accumulated: Distribution::default(),
            pending: Distribution::default(),
            last_acquire: None,
            skip_next_interval: false,
            stalls: StallClassifier::new(period),
            source_clock: MonotonicCheck::default(),
            acquire_clock: MonotonicCheck::default(),
            access_lost: 0,
            api_resets: 0,
            restart_failures: 0,
            frames_after_last_restart: 0,
            backlog_window: Distribution::default(),
            backlog: Trend::new(),
            memory: Trend::new(),
        }
    }

    /// Records a frame and reports how its interval compared to the source
    /// cadence, which is what the stall-recovery tracking needs.
    pub fn frame(&mut self, observation: FrameObservation) -> Option<StallClass> {
        self.frames += 1;
        self.frames_after_last_restart += 1;
        self.acquire.push(observation.duration);
        self.acquire_clock.observe(observation.acquired);

        match observation.delivery {
            Some(delay) => self.delivery.push(delay),
            None => self.delivery_unusable += 1,
        }
        // A zero mark is the API saying it has nothing to report, not an
        // instant, so it must not enter the monotonicity check either.
        if observation.source.as_nanos() != 0 {
            self.source_clock.observe(observation.source);
        }
        if observation.duplicate {
            self.duplicates += 1;
        }

        let backlog = match (observation.accumulated, observation.pending) {
            (Some(count), _) => {
                self.accumulated.record(count);
                Some(count)
            }
            (None, Some(count)) => {
                self.pending.record(count);
                Some(count)
            }
            (None, None) => None,
        };
        if let Some(count) = backlog {
            self.backlog_window.record(count);
        }

        let class = match self.last_acquire {
            Some(previous) if !self.skip_next_interval => {
                let gap = observation.acquired.saturating_since(previous);
                self.interval.push(gap);
                Some(self.stalls.observe(gap))
            }
            _ => None,
        };
        self.skip_next_interval = false;
        self.last_acquire = Some(observation.acquired);
        class
    }

    pub fn timeout(&mut self) {
        self.timeouts += 1;
    }

    pub fn lost(&mut self) {
        self.access_lost += 1;
        self.frames_after_last_restart = 0;
    }

    pub fn restarted(&mut self) {
        self.api_resets += 1;
    }

    pub fn restart_failed(&mut self) {
        self.restart_failures += 1;
    }

    pub fn source_held(&mut self, held: Nanos) {
        self.source_hold.push(held);
    }

    /// The next gap between acquires was caused by the harness, not the API.
    pub fn skip_next_interval(&mut self) {
        self.skip_next_interval = true;
    }

    /// Folds the backlog seen since the last call into the growth trend.
    ///
    /// An interval in which no frame arrived contributes nothing rather than a
    /// zero: no frames is no evidence about the queue, and feeding zeroes in
    /// would let an idle stretch cancel out a real climb.
    pub fn sample_backlog(&mut self, at: Timestamp) {
        if let Some(mean) = self.backlog_window.mean() {
            self.backlog.record_at(at, mean);
        }
        self.backlog_window.clear();
    }

    pub fn sample_memory(&mut self, at: Timestamp, bytes: u64) {
        self.memory.record_at(at, bytes as f64);
    }

    /// Drops everything the warm-up saw and starts the measured window.
    ///
    /// Clock continuity is deliberately kept: the monotonicity checks hold on
    /// to their last mark, so the boundary itself is still checked.
    pub fn begin_window(&mut self, at: Timestamp) {
        self.frames = 0;
        self.timeouts = 0;
        self.duplicates = 0;
        self.delivery_unusable = 0;
        self.delivery.clear();
        self.acquire.clear();
        self.interval.clear();
        self.source_hold.clear();
        self.accumulated.clear();
        self.pending.clear();
        self.backlog_window.clear();
        self.backlog = Trend::new();
        self.memory = Trend::new();
        self.stalls.reset_counts();
        self.source_clock.reset_counts();
        self.acquire_clock.reset_counts();
        self.access_lost = 0;
        self.api_resets = 0;
        self.restart_failures = 0;
        self.frames_after_last_restart = 0;
        // The gap across the reset is ours, not the API's.
        self.skip_next_interval = true;
        self.window_start = Some(at);
        self.window_end = Some(at);
    }

    pub fn end_window(&mut self, at: Timestamp) {
        self.window_end = Some(at);
    }

    /// Replaces the measured span with an explicitly accumulated one.
    ///
    /// `compare` interleaves the two backends, so a backend's window is the
    /// sum of its own blocks and not the wall clock from its first acquire to
    /// its last: the other backend's blocks sit in between, and counting them
    /// halves every rate this backend reports.
    pub fn set_window(&mut self, measured: Nanos) {
        self.window_start = Some(Timestamp::from_nanos(0));
        self.window_end = Some(Timestamp::from_nanos(measured.get()));
    }

    /// Measured wall time. Zero before [`Stats::begin_window`].
    pub fn window(&self) -> Nanos {
        match (self.window_start, self.window_end) {
            (Some(start), Some(end)) => end.saturating_since(start),
            _ => Nanos::ZERO,
        }
    }

    pub fn capture_report(&self, source_hz: f64, source_mark: &str) -> CaptureReport {
        let window_s = self.window().as_secs_f64();
        CaptureReport {
            window_s,
            frames: self.frames,
            frames_per_second: if window_s > 0.0 {
                self.frames as f64 / window_s
            } else {
                0.0
            },
            expected_frames: source_hz * window_s,
            timeouts: self.timeouts,
            duplicates: self.duplicates,
            superseded: 0,
            drained: 0,
            signals: 0,
            source_mark: source_mark.to_owned(),
            delivery_delay: self.delivery.summary(),
            delivery_delay_unusable: self.delivery_unusable,
            acquire: self.acquire.summary(),
            interval: self.interval.summary(),
            accumulated_frames: self.accumulated.clone(),
            pending_frames: self.pending.clone(),
        }
    }

    pub fn stability_report(&self) -> StabilityReport {
        let stalls = self.stalls.counts();
        StabilityReport {
            access_lost: self.access_lost,
            api_resets: self.api_resets,
            restart_failures: self.restart_failures,
            frames_after_last_restart: self.frames_after_last_restart,
            intervals_measured: stalls.observed,
            intervals_over_1x: stalls.over_one_period,
            intervals_over_2x: stalls.over_two_periods,
            max_interval_ms: stalls.max.as_millis_f64(),
            period_ms: self.period.as_millis_f64(),
            pool_recreations: 0,
            border_suppressed: None,
            source_timestamp_regressions: self.source_clock.regressions(),
            source_regression_worst_ms: self.source_clock.worst_backstep().as_millis_f64(),
            acquire_timestamp_regressions: self.acquire_clock.regressions(),
            backlog_slope_per_min: self.backlog.slope_per_minute(),
            backlog_samples: self.backlog.count(),
            backlog_peak: self.backlog.max().unwrap_or(0.0),
            backlog_trailing: self.backlog.last().unwrap_or(0.0),
            pool_cpu_accessible: false,
            mapped_bytes: 0,
        }
    }

    /// Where the series stand now, so a compare block can be summarised as the
    /// tail added since.
    pub fn mark(&self) -> Mark {
        Mark {
            delivery: self.delivery.len(),
            acquire: self.acquire.len(),
            interval: self.interval.len(),
            frames: self.frames,
            over_two: self.stalls.counts().over_two_periods,
            access_lost: self.access_lost,
        }
    }

    pub fn block(&self, mark: Mark, seconds: f64) -> BlockStats {
        let frames = self.frames - mark.frames;
        BlockStats {
            frames,
            frames_per_second: if seconds > 0.0 {
                frames as f64 / seconds
            } else {
                0.0
            },
            delivery: self.delivery.summary_from(mark.delivery),
            acquire: self.acquire.summary_from(mark.acquire),
            interval: self.interval.summary_from(mark.interval),
            intervals_over_2x: self.stalls.counts().over_two_periods - mark.over_two,
            access_lost: self.access_lost - mark.access_lost,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mark {
    delivery: usize,
    acquire: usize,
    interval: usize,
    frames: u64,
    over_two: u64,
    access_lost: u64,
}

pub struct BlockStats {
    pub frames: u64,
    pub frames_per_second: f64,
    pub delivery: crate::series::Summary,
    pub acquire: crate::series::Summary,
    pub interval: crate::series::Summary,
    pub intervals_over_2x: u64,
    pub access_lost: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Nanos = Nanos(10_000_000);

    fn observation(acquired: u64, source: u64) -> FrameObservation {
        FrameObservation {
            acquired: Timestamp::from_nanos(acquired),
            source: Timestamp::from_nanos(source),
            delivery: acquired.checked_sub(source).map(Nanos),
            duration: Nanos(200_000),
            accumulated: Some(1),
            pending: None,
            duplicate: false,
        }
    }

    #[test]
    fn the_first_frame_has_no_interval_to_report() {
        let mut stats = Stats::new(PERIOD);
        assert_eq!(stats.frame(observation(1_000_000, 500_000)), None);
        assert_eq!(stats.interval.len(), 0);
    }

    #[test]
    fn consecutive_frames_produce_intervals_and_classes() {
        let mut stats = Stats::new(PERIOD);
        stats.frame(observation(0, 0));
        assert_eq!(
            stats.frame(observation(9_000_000, 8_000_000)),
            Some(StallClass::OnCadence)
        );
        assert_eq!(
            stats.frame(observation(40_000_000, 39_000_000)),
            Some(StallClass::OverTwoPeriods)
        );
        assert_eq!(stats.interval.len(), 2);
    }

    #[test]
    fn a_deliberate_gap_is_not_charged_to_the_api() {
        let mut stats = Stats::new(PERIOD);
        stats.frame(observation(0, 0));
        stats.skip_next_interval();
        assert_eq!(stats.frame(observation(500_000_000, 499_000_000)), None);
        assert_eq!(
            stats.stability_report().intervals_over_2x,
            0,
            "the harness caused that gap by not consuming"
        );
        // The suppression lasts exactly one interval.
        assert_eq!(
            stats.frame(observation(1_000_000_000, 999_000_000)),
            Some(StallClass::OverTwoPeriods)
        );
    }

    #[test]
    fn an_unusable_source_mark_is_excluded_rather_than_counted_as_zero() {
        let mut stats = Stats::new(PERIOD);
        let mut frame = observation(1_000_000, 0);
        frame.delivery = None;
        stats.frame(frame);
        assert_eq!(stats.frames, 1);
        assert_eq!(stats.delivery.len(), 0);
        assert_eq!(stats.delivery_unusable, 1);
        assert_eq!(
            stats.stability_report().source_timestamp_regressions,
            0,
            "a zero mark is not an instant and cannot regress"
        );
    }

    #[test]
    fn a_zero_mark_between_real_ones_does_not_fake_a_regression() {
        let mut stats = Stats::new(PERIOD);
        stats.frame(observation(1_000_000, 900_000));
        let mut cursor_only = observation(2_000_000, 0);
        cursor_only.delivery = None;
        stats.frame(cursor_only);
        stats.frame(observation(3_000_000, 2_900_000));
        assert_eq!(stats.stability_report().source_timestamp_regressions, 0);
    }

    #[test]
    fn backlog_samples_are_means_and_empty_intervals_contribute_nothing() {
        let mut stats = Stats::new(PERIOD);
        let mut frame = observation(0, 0);
        frame.accumulated = Some(3);
        stats.frame(frame);
        frame.accumulated = Some(1);
        stats.frame(frame);
        stats.sample_backlog(Timestamp::from_nanos(1_000));
        assert_eq!(stats.backlog.count(), 1);
        assert_eq!(stats.backlog.last(), Some(2.0));

        stats.sample_backlog(Timestamp::from_nanos(2_000));
        assert_eq!(
            stats.backlog.count(),
            1,
            "no frames arrived, so nothing is known about the queue"
        );
    }

    #[test]
    fn a_window_reset_drops_warmup_but_keeps_clock_continuity() {
        let mut stats = Stats::new(PERIOD);
        stats.frame(observation(1_000_000_000, 999_000_000));
        stats.timeout();
        stats.lost();
        stats.begin_window(Timestamp::from_nanos(1_000_000_000));

        assert_eq!(stats.frames, 0);
        assert_eq!(stats.timeouts, 0);
        assert_eq!(stats.access_lost, 0);
        assert_eq!(stats.acquire.len(), 0);

        // A mark before the reset's last one is still a regression.
        stats.frame(observation(1_100_000_000, 900_000_000));
        assert_eq!(stats.stability_report().source_timestamp_regressions, 1);
    }

    #[test]
    fn the_window_boundary_gap_is_not_a_stall() {
        let mut stats = Stats::new(PERIOD);
        stats.frame(observation(0, 0));
        stats.begin_window(Timestamp::from_nanos(0));
        stats.frame(observation(900_000_000, 899_000_000));
        assert_eq!(stats.stability_report().intervals_over_2x, 0);
    }

    #[test]
    fn frames_after_a_restart_are_counted_from_the_loss() {
        let mut stats = Stats::new(PERIOD);
        stats.frame(observation(0, 0));
        stats.frame(observation(10_000_000, 9_000_000));
        stats.lost();
        stats.restarted();
        stats.frame(observation(30_000_000, 29_000_000));
        assert_eq!(stats.frames_after_last_restart, 1);
        assert_eq!(stats.frames, 3);
    }

    #[test]
    fn the_capture_report_rate_comes_from_the_measured_window() {
        let mut stats = Stats::new(PERIOD);
        stats.begin_window(Timestamp::from_nanos(0));
        for index in 1..=200u64 {
            stats.frame(observation(
                index * 10_000_000,
                index * 10_000_000 - 1_000_000,
            ));
        }
        stats.end_window(Timestamp::from_nanos(2_000_000_000));

        let report = stats.capture_report(100.0, "desktop presented");
        assert_eq!(report.frames, 200);
        assert!((report.window_s - 2.0).abs() < 1e-9);
        assert!((report.frames_per_second - 100.0).abs() < 1e-9);
        assert!((report.expected_frames - 200.0).abs() < 1e-9);
    }

    #[test]
    fn a_block_reports_only_the_frames_it_saw() {
        let mut stats = Stats::new(PERIOD);
        stats.begin_window(Timestamp::from_nanos(0));
        for index in 1..=100u64 {
            stats.frame(observation(
                index * 10_000_000,
                index * 10_000_000 - 1_000_000,
            ));
        }
        let mark = stats.mark();
        for index in 101..=150u64 {
            stats.frame(observation(
                index * 10_000_000,
                index * 10_000_000 - 2_000_000,
            ));
        }

        let block = stats.block(mark, 0.5);
        assert_eq!(block.frames, 50);
        assert!((block.frames_per_second - 100.0).abs() < 1e-9);
        assert_eq!(block.delivery.count, 50);
        assert!(
            (block.delivery.p50_ms - 2.0).abs() < 1e-9,
            "the earlier block's 1 ms delays are not in this one"
        );
    }
}
