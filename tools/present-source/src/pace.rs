//! Frame deadlines, computed and never accumulated.
//!
//! A software pacer that adds a period to the previous deadline drifts by the
//! rounding error of the period, every frame, forever: at 120 fps the true
//! period is 8333333.33 ns, so an integer accumulator loses a third of a
//! nanosecond per frame and a whole second every three million frames. A
//! source whose rate is wrong by a known-but-uncorrected amount would make the
//! capture comparison measure the producer instead of the capture API.
//!
//! So each deadline is derived from the start instant and the frame index:
//! `start + frame * 1e9 / fps`, in integer nanoseconds. The error against the
//! exact rational deadline is under one nanosecond at every index and does not
//! grow.
//!
//! The origin used to be movable, so a viewer could ask for the next frame to be
//! drawn later and so change how old its content was when it was finally scanned
//! out. It is fixed again, because moving it was measured to move nothing:
//! Desktop Duplication follows the compositor rather than the program drawing
//! into it, so a draw moved inside a composition interval is composited at the
//! same virtual-display vblank and reaches the viewer at the same instant. The
//! phase belongs to that vblank. Both the measurement that retired this lever and
//! the one that replaced it are recorded where a request now goes, in
//! `tools/nvenc-probe/src/phase.rs`.

use lanplay_telemetry::{Nanos, Timestamp};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// The schedule a run is held to.
#[derive(Debug)]
pub struct Pacer {
    start: Timestamp,
    fps: u32,
}

impl Pacer {
    /// # Panics
    ///
    /// If `fps` is zero. A zero-rate producer has no schedule to keep, and the
    /// CLI rejects it before this is ever reached.
    pub fn new(start: Timestamp, fps: u32) -> Pacer {
        assert!(fps > 0, "a pacer needs a positive frame rate");
        Pacer { start, fps }
    }

    /// Nominal period. For reporting: the schedule never adds this to anything.
    pub const fn period(&self) -> Nanos {
        Nanos((NANOS_PER_SECOND / self.fps as u128) as u64)
    }

    /// When frame `index` is due. `index` 0 is due at the start instant.
    ///
    /// The multiply happens in 128 bits so that a run long enough to overflow
    /// `frame * 1e9` in 64 bits, around 18 years at 240 fps, still produces the
    /// right answer rather than wrapping into the past.
    pub const fn deadline(&self, index: u64) -> Timestamp {
        let offset = (index as u128 * NANOS_PER_SECOND) / self.fps as u128;
        Timestamp::from_nanos(self.start.as_nanos() + offset as u64)
    }

    /// Index of the last frame that fits in `seconds` of running, or `None`
    /// for an open-ended run.
    ///
    /// Frame 0 is presented immediately, so `seconds` at `fps` yields
    /// `seconds * fps` frames: indices 0 through `seconds * fps - 1`.
    pub const fn last_index(&self, seconds: u64) -> Option<u64> {
        if seconds == 0 {
            return None;
        }
        Some(seconds.saturating_mul(self.fps as u64).saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPOCH: u64 = 1_234_567_890;

    fn pacer(fps: u32) -> Pacer {
        Pacer::new(Timestamp::from_nanos(EPOCH), fps)
    }

    /// The deadline for every index is the exact rational instant, floored to
    /// the nanosecond, at 60, 120 and 240 fps alike.
    #[test]
    fn deadlines_are_exact() {
        for fps in [60u32, 120, 240] {
            let pacer = pacer(fps);
            assert_eq!(pacer.deadline(0), Timestamp::from_nanos(EPOCH));
            for index in [1u64, 2, 7, 999, 100_000] {
                let exact = (index as u128 * NANOS_PER_SECOND) / fps as u128;
                assert_eq!(
                    pacer.deadline(index).as_nanos(),
                    EPOCH + exact as u64,
                    "fps {fps} index {index}"
                );
            }
        }
    }

    /// Over 100000 frames at 120 fps the schedule stays within a nanosecond of
    /// the true instant, where an accumulator built from the same integer
    /// period would already be a third of a second early.
    #[test]
    fn does_not_drift_over_100000_frames() {
        const FRAMES: u64 = 100_000;
        let pacer = pacer(120);
        let period = pacer.period().get();

        let mut accumulated = EPOCH;
        for index in 0..=FRAMES {
            let scheduled = pacer.deadline(index).as_nanos() - EPOCH;
            let truth = index as f64 * NANOS_PER_SECOND as f64 / 120.0;
            assert!(
                (scheduled as f64 - truth).abs() < 1.0,
                "index {index}: scheduled {scheduled} vs {truth}"
            );
            if index > 0 {
                accumulated += period;
            }
        }

        let scheduled_end = pacer.deadline(FRAMES).as_nanos();
        assert_eq!(scheduled_end - EPOCH, 833_333_333_333);
        // 100000 * (1e9/120 - 8333333) ns of loss the accumulator never
        // recovers; the whole reason deadlines are computed, not summed.
        assert_eq!(scheduled_end - accumulated, 33_333);
    }

    /// Deadlines advance by one period, give or take the rounding, and never
    /// go backwards.
    #[test]
    fn deadlines_advance_monotonically() {
        let pacer = pacer(240);
        let period = pacer.period().get();
        let mut previous = pacer.deadline(0);
        for index in 1..10_000 {
            let next = pacer.deadline(index);
            let step = next.as_nanos() - previous.as_nanos();
            assert!(next > previous, "index {index} did not advance");
            assert!(
                step == period || step == period + 1,
                "index {index} stepped {step}, period {period}"
            );
            previous = next;
        }
    }

    #[test]
    fn zero_seconds_runs_until_closed() {
        assert_eq!(pacer(120).last_index(0), None);
        assert_eq!(pacer(120).last_index(1), Some(119));
        assert_eq!(pacer(240).last_index(30), Some(7_199));
    }
}
