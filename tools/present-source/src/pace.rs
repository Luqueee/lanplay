//! Frame deadlines, computed and never accumulated, and the phase they are on.
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
//! # Why the phase of this loop is worth moving
//!
//! The largest term in the pipeline's latency is not work. A frame finishes
//! decoding at an arbitrary point inside the viewer's refresh period and then
//! waits out the rest of it, which over a soak averages half a period and
//! measures indistinguishably from the prediction for a uniformly distributed
//! phase. Two unsynchronised clocks at the same nominal rate produce exactly
//! that, and no extra rate can shorten it.
//!
//! The lever is here and nowhere downstream. Take a period T of 8.33 ms, work
//! of 3 ms between a frame being drawn and being ready to show, a draw at
//! t = 0, and display ticks at 0, T, 2T. Drawn at 0, the frame is ready at 3
//! and shown at 8.33: its content is 8.33 ms old on screen. Delay the *capture*
//! of that same frame by 4 and it is ready at 7, still shown at 8.33, still
//! 8.33 ms old; delay it by 6 and it is ready at 9, shown at 16.67, and it has
//! cost a whole period. A capture delay moves when a frame is ready and not
//! when its content was drawn, so it is neutral by construction until it
//! crosses a tick and then it is a loss. Now move this loop instead: drawn at
//! 4, ready at 7, shown at 8.33, and the content is 4.33 ms old. That is the
//! whole win, and the source's draw is the only thing that produces it.
//!
//! A phase request therefore lands on the origin. Move `start` forward by the
//! requested delay and every later deadline moves with it, once, while the
//! index keeps its own multiplier: the interval between any two frames after
//! the shift is still exactly one period, and only the one interval spanning
//! the shift is longer. Adding the delay to each deadline, or sleeping an extra
//! amount inside one iteration of a loop that paces from "now", would turn a
//! one-off correction into a permanent rate change.
//!
//! Later only. Asking for an earlier frame asks for one that has already been
//! drawn, so a needed advance of x arrives as one period minus x, which reaches
//! the same phase and is always in the future. A delay of a period or more
//! folds modulo the period rather than stalling the producer, and no index is
//! ever skipped to reach a phase: dropping a frame to save a few milliseconds
//! is a trade nobody asked for, and the viewer already copes with a late frame.
//!
//! The request is advisory both ways. A run that never receives one paces
//! exactly as it did before any of this existed, and a producer that ignores
//! them still feeds a correct stream.
//!
//! Outside the laboratory the source is a game, which cannot be told to move
//! its phase. It does vsync to the display it is on, and when that display is
//! ours the vblank is ours to time through IddCx, so the lever survives the
//! move to a real workload. That is a reason the virtual display is worth more
//! than somewhere to put pixels, and it is not built here.
//!
//! Measured after the fact: moving this origin does not move the phase the
//! receiver sees. A 3.00 ms shift, confirmed applied here during a live run, left
//! a 50-sample phase trace on the Mac inside 3.61 to 4.69 ms with a largest
//! movement between samples of 0.374 ms. Desktop Duplication follows the
//! compositor rather than this program, so a draw moved inside a composition
//! interval is composited at the same virtual-display vblank and leaves the host
//! at the same instant. The mechanism below is correct and its lever is the wrong
//! one; the phase belongs to that vblank, which the display driver owns.
//!
//! Kept because it is the tested path a vblank will be driven through, and
//! because deleting the measurement that says it does nothing would invite the
//! next person to build it again.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lanplay_telemetry::{Nanos, Timestamp};

const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// Marks the slot as full. A delay of zero nanoseconds is a legal request that
/// moves nothing, and it has to stay distinguishable from an empty slot: a
/// request that arrives and does nothing must be visible as such, which it
/// cannot be if the hand-off silently swallows it.
const OCCUPIED: u64 = 1 << 32;

/// The single slot a viewer's phase request waits in.
///
/// Whoever hears the request posts here and the present loop takes from here,
/// because the present loop must never touch a socket: a receive with any
/// usable timeout costs a slice of an 8.33 ms period on every frame, which
/// would spend more latency than the correction buys back.
///
/// One slot rather than a queue, and the newest request wins. Two arriving
/// before the loop looks means the viewer measured the same uncorrected phase
/// twice, and obeying both would over-correct by an amount it only asked for
/// once. The displaced request is counted rather than forgotten, so a producer
/// too slow to keep up with its viewer says so.
#[derive(Debug, Default)]
pub struct PhaseInbox {
    slot: AtomicU64,
    posted: AtomicU64,
    superseded: AtomicU64,
}

impl PhaseInbox {
    pub const fn new() -> Self {
        PhaseInbox {
            slot: AtomicU64::new(0),
            posted: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
        }
    }

    /// Records a request, displacing any the present loop has not taken yet.
    pub fn post(&self, delay_nanos: u32) {
        self.posted.fetch_add(1, Ordering::Relaxed);
        let displaced = self
            .slot
            .swap(OCCUPIED | u64::from(delay_nanos), Ordering::AcqRel);
        if displaced != 0 {
            self.superseded.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Takes the pending request, leaving the slot empty.
    pub fn take(&self) -> Option<u32> {
        match self.slot.swap(0, Ordering::AcqRel) {
            0 => None,
            held => Some((held & u64::from(u32::MAX)) as u32),
        }
    }

    /// How many requests have arrived over the life of the run.
    pub fn posted(&self) -> u64 {
        self.posted.load(Ordering::Relaxed)
    }

    /// How many were displaced by a newer one before the loop looked.
    pub fn superseded(&self) -> u64 {
        self.superseded.load(Ordering::Relaxed)
    }
}

/// The schedule a run is held to.
#[derive(Clone, Debug)]
pub struct Pacer {
    start: Timestamp,
    fps: u32,
    inbox: Arc<PhaseInbox>,
    /// Everything the origin has been moved by, kept for the report: `start`
    /// alone cannot say how far it has travelled.
    phase: u64,
    taken: u64,
    applied: u64,
    folded: u64,
}

impl Pacer {
    /// # Panics
    ///
    /// If `fps` is zero. A zero-rate producer has no schedule to keep, and the
    /// CLI rejects it before this is ever reached.
    pub fn new(start: Timestamp, fps: u32) -> Pacer {
        assert!(fps > 0, "a pacer needs a positive frame rate");
        Pacer {
            start,
            fps,
            inbox: Arc::new(PhaseInbox::new()),
            phase: 0,
            taken: 0,
            applied: 0,
            folded: 0,
        }
    }

    /// The origin every deadline is derived from, and the only thing a phase
    /// request moves.
    pub const fn start(&self) -> Timestamp {
        self.start
    }

    pub const fn fps(&self) -> u32 {
        self.fps
    }

    /// A handle for whoever hears the viewer's requests.
    pub fn inbox(&self) -> Arc<PhaseInbox> {
        Arc::clone(&self.inbox)
    }

    /// Nominal period. Reporting, and the modulus a delay is folded against:
    /// the schedule never adds this to anything.
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
    ///
    /// Answered against the schedule as it was at the start. A phase shift
    /// moves the last frame later by under a period rather than removing it,
    /// which is the point: a run asked for 40 seconds of frames delivers all of
    /// them.
    pub const fn last_index(&self, seconds: u64) -> Option<u64> {
        if seconds == 0 {
            return None;
        }
        Some(seconds.saturating_mul(self.fps as u64).saturating_sub(1))
    }

    /// Obeys whatever the viewer asked for since the last frame.
    ///
    /// `None` means nothing was waiting; `Some(Nanos::ZERO)` means a request
    /// was waiting and folded to nothing, which is a different event and one a
    /// caller has to be able to report.
    ///
    /// Called before the next deadline is computed rather than after waiting
    /// for the current one: a request that arrives during a wait is about the
    /// frame after the one already being drawn.
    pub fn apply_pending(&mut self) -> Option<Nanos> {
        self.inbox.take().map(|delay_nanos| self.shift(delay_nanos))
    }

    /// Holds every frame from here on back by `delay_nanos`, folded into one
    /// period, and reports the amount actually applied.
    pub fn shift(&mut self, delay_nanos: u32) -> Nanos {
        self.taken += 1;
        let period = self.period().get();
        let asked = u64::from(delay_nanos);
        if asked >= period {
            self.folded += 1;
        }
        let delay = asked % period;
        if delay != 0 {
            self.applied += 1;
            self.phase += delay;
            self.start = self.start.add(Nanos(delay));
        }
        Nanos(delay)
    }

    /// What the viewer asked for and what became of it.
    pub fn shifts(&self) -> PhaseShifts {
        PhaseShifts {
            requested: self.inbox.posted(),
            superseded: self.inbox.superseded(),
            taken: self.taken,
            applied: self.applied,
            folded: self.folded,
            moved: Nanos(self.phase),
        }
    }
}

/// What the viewer asked for and what became of it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseShifts {
    /// Requests that arrived.
    pub requested: u64,
    /// Requests a newer one displaced before the loop took them.
    pub superseded: u64,
    /// Requests the loop took.
    pub taken: u64,
    /// Requests that moved the schedule.
    pub applied: u64,
    /// Requests of a whole period or more, reduced rather than obeyed.
    pub folded: u64,
    /// Phase moved in total.
    pub moved: Nanos,
}

impl PhaseShifts {
    /// Requests that arrived while nothing was reading the inbox.
    pub const fn unread(self) -> u64 {
        self.requested
            .saturating_sub(self.superseded)
            .saturating_sub(self.taken)
    }

    /// Requests taken and obeyed to no effect, because they folded to nothing.
    pub const fn inert(self) -> u64 {
        self.taken.saturating_sub(self.applied)
    }

    /// Mean phase moved per request that moved any.
    pub const fn mean(self) -> Nanos {
        match self.applied {
            0 => Nanos::ZERO,
            applied => Nanos(self.moved.get() / applied),
        }
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

    #[test]
    fn a_run_that_never_gets_a_shift_paces_from_the_origin_and_nothing_else() {
        let mut pacer = pacer(120);
        for index in 0..1_000 {
            pacer.apply_pending();
            let exact = (index as u128 * NANOS_PER_SECOND) / 120;
            assert_eq!(pacer.deadline(index).as_nanos(), EPOCH + exact as u64);
        }
        assert_eq!(
            pacer.shifts(),
            PhaseShifts::default(),
            "an untouched run must report nothing at all"
        );
    }

    /// The contract the whole mechanism rests on: after a shift the producer
    /// still makes the frames per second it was asked for, and only where in
    /// the period it makes them has moved.
    #[test]
    fn a_shift_moves_the_phase_and_leaves_the_rate_alone() {
        const FRAMES: u64 = 600;
        const AT: u64 = 300;
        const DELAY: u32 = 2_000_000;

        let plain = pacer(120);
        let mut shifted = pacer(120);
        let mut due = Vec::with_capacity(FRAMES as usize);
        for index in 0..FRAMES {
            if index == AT {
                shifted.inbox().post(DELAY);
            }
            shifted.apply_pending();
            due.push(shifted.deadline(index).as_nanos());
        }

        assert_eq!(due.len(), FRAMES as usize, "the shift cost a frame");
        for index in 0..AT {
            assert_eq!(
                due[index as usize],
                plain.deadline(index).as_nanos(),
                "a frame before the shift moved"
            );
        }
        for index in AT..FRAMES {
            assert_eq!(
                due[index as usize],
                plain.deadline(index).as_nanos() + u64::from(DELAY),
                "a frame after the shift is not exactly the delay later"
            );
        }

        // The rate as the run sees it: the same number of frames span the same
        // stretch of time, plus the one-off delay and nothing per frame.
        assert_eq!(
            due[FRAMES as usize - 1] - due[0],
            plain.deadline(FRAMES - 1).as_nanos() - EPOCH + u64::from(DELAY)
        );

        // And as the intervals see it: one is longer, the rest are a period.
        let period = plain.period().get();
        let mut longer = 0;
        for (index, pair) in due.windows(2).enumerate() {
            let interval = pair[1] - pair[0];
            // Consecutive truncated deadlines differ by a period or one
            // nanosecond more, which is the rate being exact rather than the
            // period being wrong.
            if index as u64 == AT - 1 {
                longer += 1;
                assert!((period..=period + 1).contains(&(interval - u64::from(DELAY))));
            } else {
                assert!((period..=period + 1).contains(&interval));
            }
        }
        assert_eq!(longer, 1, "exactly one interval carries the whole shift");

        let shifts = shifted.shifts();
        assert_eq!(shifts.requested, 1);
        assert_eq!(shifts.applied, 1);
        assert_eq!(shifts.moved, Nanos(u64::from(DELAY)));
    }

    #[test]
    fn a_delay_of_a_period_or_more_folds_instead_of_stalling() {
        let mut pacer = pacer(120);
        let period = pacer.period().get();

        assert_eq!(pacer.shift(period as u32 + 1_234).get(), 1_234);
        // Three whole periods reaches the phase it started from, so it asks
        // for nothing rather than for the producer to stop for 25 ms.
        assert_eq!(pacer.shift(period as u32 * 3).get(), 0);

        let shifts = pacer.shifts();
        assert_eq!(shifts.folded, 2);
        assert_eq!(shifts.applied, 1);
        assert_eq!(shifts.moved, Nanos(1_234));
        assert_eq!(
            pacer.deadline(0).as_nanos(),
            EPOCH + 1_234,
            "the schedule moved by more than the folded amount"
        );
    }

    #[test]
    fn no_frame_is_ever_scheduled_before_the_one_that_came_first() {
        let mut pacer = pacer(120);
        let period = pacer.period().get();
        let mut previous = pacer.deadline(0);
        for index in 1..2_000u64 {
            // A shift at every point in the cycle, zero and the far end
            // included: 997 is coprime with the period, so the sequence walks
            // the whole of it.
            let before = pacer.deadline(index);
            pacer.shift(((index * 997) % period) as u32);
            let after = pacer.deadline(index);
            assert!(after >= before, "a shift pulled a deadline earlier");
            assert!(after > previous, "frame {index} lands before {}", index - 1);
            // No index is skipped to reach a phase, which on the schedule means
            // no gap wide enough to have held a frame.
            assert!(
                after.as_nanos() - previous.as_nanos() < 2 * period,
                "frame {index} is a whole frame's worth of time away"
            );
            previous = after;
        }
    }

    #[test]
    fn a_shift_at_any_point_in_a_run_preserves_the_count_and_the_rate() {
        const FRAMES: u64 = 240;
        let plain = pacer(120);
        for at in 0..FRAMES {
            let mut pacer = pacer(120);
            let mut presented = 0u64;
            let mut last: Option<Timestamp> = None;
            for index in 0..FRAMES {
                if index == at {
                    pacer.inbox().post(4_321_000);
                }
                pacer.apply_pending();
                let due = pacer.deadline(index);
                if let Some(last) = last {
                    assert!(due > last);
                }
                last = Some(due);
                presented += 1;
            }
            assert_eq!(presented, FRAMES);
            assert_eq!(
                pacer.deadline(FRAMES - 1).as_nanos(),
                plain.deadline(FRAMES - 1).as_nanos() + 4_321_000,
                "a shift at frame {at} cost more or less than itself"
            );
            // The frame count a bounded run delivers is fixed before any shift
            // arrives, so the shift moves the last frame rather than dropping
            // it.
            assert_eq!(pacer.last_index(2), plain.last_index(2));
        }
    }

    #[test]
    fn the_newer_request_wins_and_the_one_it_displaced_is_counted() {
        let inbox = PhaseInbox::new();
        inbox.post(1_000);
        inbox.post(2_000);
        assert_eq!(inbox.take(), Some(2_000));
        assert_eq!(inbox.take(), None, "one request served twice");
        assert_eq!(inbox.posted(), 2);
        assert_eq!(inbox.superseded(), 1);
    }

    #[test]
    fn a_request_that_moves_nothing_is_still_visible() {
        let mut pacer = pacer(120);
        pacer.inbox().post(0);
        assert_eq!(
            pacer.apply_pending(),
            Some(Nanos::ZERO),
            "a request that folds to nothing is not the same as no request"
        );
        assert_eq!(pacer.deadline(1), self::pacer(120).deadline(1));

        let shifts = pacer.shifts();
        assert_eq!(shifts.requested, 1, "an empty slot swallowed a zero delay");
        assert_eq!(shifts.taken, 1);
        assert_eq!(shifts.applied, 0);
        assert_eq!(shifts.inert(), 1);
    }

    #[test]
    fn a_request_the_loop_never_reads_is_still_reported() {
        let pacer = pacer(120);
        pacer.inbox().post(1_000_000);
        let shifts = pacer.shifts();
        assert_eq!(shifts.requested, 1);
        assert_eq!(shifts.taken, 0);
        assert_eq!(shifts.unread(), 1);
        assert_eq!(shifts.moved, Nanos::ZERO);
    }
}
