//! What the jitter buffer was holding, window by window rather than run by run.
//!
//! The run-wide distribution the producer keeps says how much audio the buffer
//! held over the whole of an arm, and A7 asks a different question: whether
//! that figure moved while the arm was passing. A p50 of 15 ms over ten minutes
//! and a p50 of 15 ms over one are the same number and not the same finding,
//! because the first is also what a buffer that spent five minutes at 10 ms and
//! five at 20 reports. Nothing in a run-wide figure separates the two, so a
//! drift measured from it is two runs compared instead of one run measured.
//!
//! Which is why these figures reset. Every other counter in a window row is a
//! difference taken across the boundary, and a percentile cannot be
//! differenced: the median of a window is not recoverable from the median of
//! the run up to its start and the median of the run up to its end. What is
//! kept instead is the distribution itself, emptied when a window closes, and
//! the order statistics come out of what one window put in it.
//!
//! # Why a histogram and not a reservoir
//!
//! Occupancy is a small integer. The buffer holds whole frames and its slot
//! count bounds how many, which for the targets this phase is allowed to
//! consider is fourteen at the most, so one counter per attainable occupancy
//! spans the entire range. The percentiles are then exact rather than sampled,
//! where a reservoir would have to choose between a cap that discards the tail
//! and a store that grows; and the write is a single increment on the one path
//! here that has a deadline. The producer computes an index and adds one. It
//! allocates nothing, waits on nothing, locks nothing and prints nothing.
//!
//! The read is a swap per counter, so a pull that lands in the middle of a
//! drain is counted in exactly one of the two windows it could belong to.
//! Which one is not determined, and does not need to be: the boundary is an
//! instant on a wall clock and the pull was within microseconds of it.

use core::sync::atomic::{AtomicU64, Ordering};

use lanplay_audio_capture::Percentiles;

/// The producer's end: one counter per occupancy the buffer can report.
#[derive(Debug)]
pub struct WindowOccupancy {
    frames: Box<[AtomicU64]>,
    /// Microseconds of audio one frame holds, applied at the point of reading.
    frame_us: u64,
}

impl WindowOccupancy {
    /// Builds a histogram for a buffer of `slots` slots whose frames each hold
    /// `frame_us` of audio.
    ///
    /// The slot count rather than the ceiling, because the ceiling is the bound
    /// the buffer applies after admitting a frame and the slots are the bound on
    /// what it can be holding when it applies it.
    pub fn new(slots: usize, frame_us: u64) -> WindowOccupancy {
        WindowOccupancy {
            frames: (0..=slots).map(|_| AtomicU64::new(0)).collect(),
            frame_us,
        }
    }

    /// Records the occupancy one pull found, in frames.
    ///
    /// The clamp cannot bind, because the histogram spans every occupancy the
    /// buffer's own slot bound permits. It is here so that a buffer built to one
    /// bound and a histogram built to another produces a figure in the last
    /// bucket instead of a panic on the thread the audio comes off.
    pub fn record(&self, frames: usize) {
        let index = frames.min(self.frames.len() - 1);
        self.frames[index].fetch_add(1, Ordering::Relaxed);
    }

    /// The watcher's end, carrying the snapshot a drain fills.
    ///
    /// Allocated here, once, so that closing a window allocates nothing. One
    /// reader at a time is the whole arrangement: many threads record and the
    /// thread that closes windows is the only one that empties.
    pub fn reader(&self) -> OccupancyReader<'_> {
        OccupancyReader {
            histogram: self,
            counts: vec![0; self.frames.len()].into_boxed_slice(),
        }
    }
}

/// One window's worth of occupancy, taken and cleared in the same act.
#[derive(Debug)]
pub struct OccupancyReader<'a> {
    histogram: &'a WindowOccupancy,
    counts: Box<[u64]>,
}

impl OccupancyReader<'_> {
    /// Empties the histogram and states what had accumulated in it, in
    /// microseconds of audio.
    ///
    /// `None` when no pull happened, which is not an occupancy of zero. An empty
    /// buffer and an absent measurement are the two readings this project has
    /// most often mistaken for each other, and a window with no pulls in it is
    /// the second.
    ///
    /// The scale is applied to the order statistics rather than to each sample,
    /// which is the same figure: multiplying by a positive constant preserves
    /// order, so it commutes with ranking.
    pub fn take(&mut self) -> Option<Percentiles> {
        let mut total = 0u64;
        for (bucket, count) in self.histogram.frames.iter().zip(self.counts.iter_mut()) {
            *count = bucket.swap(0, Ordering::Relaxed);
            total += *count;
        }
        if total == 0 {
            return None;
        }

        // Nearest rank, and the same nearest rank the run-wide store computes,
        // so that a window's figures and the run's are read on one convention.
        let rank = |quantile: f64| ((quantile * total as f64).ceil() as u64).clamp(1, total) - 1;
        let (r50, r95, r99) = (rank(0.50), rank(0.95), rank(0.99));

        let mut min = 0;
        let mut max = 0;
        let mut p50 = 0;
        let mut p95 = 0;
        let mut p99 = 0;
        let mut below = 0u64;
        for (frames, &count) in self.counts.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let micros = frames as u64 * self.histogram.frame_us;
            if below == 0 {
                min = micros;
            }
            max = micros;
            let occupied = below..below + count;
            if occupied.contains(&r50) {
                p50 = micros;
            }
            if occupied.contains(&r95) {
                p95 = micros;
            }
            if occupied.contains(&r99) {
                p99 = micros;
            }
            below += count;
        }

        Some(Percentiles {
            count: total as usize,
            min,
            p50,
            p95,
            p99,
            max,
        })
    }
}

#[cfg(test)]
mod tests {
    use lanplay_audio_capture::Samples;

    use super::*;

    /// The wire contract's frame, so the figures below read as the milliseconds
    /// a report would print.
    const FRAME_US: u64 = 5_000;

    /// A buffer aiming at 10 ms with a 5 ms frame: six frames of ceiling and two
    /// slots above it.
    const SLOTS: usize = 8;

    /// The property the whole module exists for, and the one a running
    /// accumulator would pass every other test without having.
    ///
    /// A buffer that held two frames for a window and then six for the next has
    /// to read two and then six. An accumulator that never resets reads two and
    /// then two, because the first window's ninety samples outvote the second
    /// window's hundred at every percentile below the top - and it would look
    /// entirely correct in the count, in the maximum and in the first window.
    #[test]
    fn a_window_states_only_what_was_recorded_inside_it() {
        let occupancy = WindowOccupancy::new(SLOTS, FRAME_US);
        let mut reader = occupancy.reader();

        for _ in 0..90 {
            occupancy.record(2);
        }
        for _ in 0..10 {
            occupancy.record(3);
        }
        let first = reader.take().expect("a hundred pulls were recorded");
        assert_eq!(first.count, 100);
        assert_eq!(first.min, 2 * FRAME_US);
        assert_eq!(first.p50, 2 * FRAME_US);
        assert_eq!(first.p95, 3 * FRAME_US);
        assert_eq!(first.max, 3 * FRAME_US);

        for _ in 0..100 {
            occupancy.record(6);
        }
        let second = reader.take().expect("another hundred pulls were recorded");
        assert_eq!(
            second.count, 100,
            "the first window's pulls were counted twice"
        );
        assert_eq!(
            second.min,
            6 * FRAME_US,
            "a store that had not been emptied would still be showing the two frames the first \
             window held"
        );
        assert_eq!(second.p50, 6 * FRAME_US);
        assert_eq!(second.max, 6 * FRAME_US);
    }

    /// An absence, and not a buffer that was empty. The distinction decides
    /// whether a reader looking at a window with no pulls in it concludes that
    /// the buffer had run dry.
    #[test]
    fn a_window_with_no_pull_has_no_occupancy() {
        let occupancy = WindowOccupancy::new(SLOTS, FRAME_US);
        let mut reader = occupancy.reader();
        assert!(reader.take().is_none());

        occupancy.record(0);
        assert!(
            reader.take().is_some(),
            "a pull that found an empty buffer is a measurement of zero and not an absence"
        );
        assert!(reader.take().is_none());
    }

    /// The window's figures and the run's have to be on one convention, or the
    /// two cannot be compared - which is the comparison A7 makes.
    #[test]
    fn the_percentiles_are_the_ones_the_run_wide_store_computes() {
        let occupancy = WindowOccupancy::new(SLOTS, FRAME_US);
        let mut reader = occupancy.reader();
        let mut store = Samples::with_capacity(1_000);

        // Spread across every bucket and deliberately uneven, so that a
        // convention off by one rank lands on a different frame count.
        for step in 0..1_000usize {
            let frames = (step * 7) % (SLOTS + 1);
            occupancy.record(frames);
            store.record(frames as u64 * FRAME_US);
        }

        assert_eq!(reader.take(), store.percentiles());
    }
}
