//! The bounded PCM ring between the tone producer and the render callback.
//!
//! One producer, one consumer, no lock, no allocation after construction. The
//! consumer is a CoreAudio IOProc running on a real-time thread the HAL owns,
//! and the one thing it may never do is wait for the producer. A mutex here
//! would be a priority inversion waiting to happen: the producer is an ordinary
//! thread, the scheduler can leave it holding the lock, and the callback would
//! then miss its deadline through no fault of its own. So the two halves meet
//! over two cursors and nothing else.
//!
//! The cursors count frames since the run began and are never wrapped. Wrapping
//! them into the buffer happens only at the point of indexing, which is what
//! makes full and empty distinguishable without wasting a slot: occupancy is
//! the difference of the two, and it is capacity exactly when the ring is full
//! and zero exactly when it is empty. A pair of wrapped cursors would have to
//! leave one frame unused to tell those apart, and the arithmetic that decides
//! which case it is in is precisely the arithmetic that goes wrong.
//!
//! The producer publishes samples with a release store to the write cursor and
//! the consumer acquires them by loading it, and the other direction is the
//! mirror of that. Each side therefore only ever touches frames the other has
//! already finished with, so the regions the two threads write are disjoint
//! however close together they run.
//!
//! Underruns and overruns are counted separately and never folded together.
//! They are opposite failures with opposite remedies: an underrun is audio that
//! reached nobody because the producer was late, and an overrun is audio thrown
//! away because the consumer was. A single "ring errors" figure would leave an
//! operator unable to tell which end to look at, and both of them can be zero
//! in a run that is broken in the other way.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

/// What one drain of the ring achieved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Drained {
    /// Frames the ring had and handed over.
    pub frames: usize,
    /// Frames the caller asked for and the ring did not have. The caller is
    /// expected to have written silence over them.
    pub zero_filled: usize,
}

/// What one fill of the ring achieved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Filled {
    /// Frames the ring took.
    pub frames: usize,
    /// Frames offered that there was no room for. These are lost: the ring
    /// refuses rather than overwriting audio the consumer has not read.
    pub refused: usize,
}

/// Interleaved 32-bit float PCM, bounded, single producer, single consumer.
pub struct PcmRing {
    /// `capacity_frames * channels` samples. Cells rather than one boxed slice
    /// because both threads hold a shared reference to the ring and each needs
    /// to write part of the buffer; forming a `&mut` to the whole of it from
    /// either side would be an aliasing violation even where the bytes touched
    /// do not overlap.
    samples: Box<[UnsafeCell<f32>]>,
    capacity_frames: usize,
    channels: usize,
    /// Frames the producer has published, since the beginning of the run.
    write: AtomicU64,
    /// Frames the consumer has taken, since the beginning of the run.
    read: AtomicU64,
    underruns: AtomicU64,
    underrun_frames: AtomicU64,
    overruns: AtomicU64,
    overrun_frames: AtomicU64,
}

// SAFETY: the buffer is only ever reached through the two cursors, and the
// contract of the type is one producer calling `fill` and one consumer calling
// `drain`. `fill` writes only frames at or after the write cursor and below
// read + capacity, `drain` reads only frames at or after the read cursor and
// below the write cursor, so the two never address the same frame. The release
// store each side makes to its own cursor, paired with the acquire load the
// other makes of it, is what publishes the samples themselves.
unsafe impl Sync for PcmRing {}

impl PcmRing {
    /// A ring holding `capacity_frames` frames of `channels`-channel audio.
    ///
    /// Everything is allocated here, once, because neither the producer nor the
    /// callback may allocate later.
    pub fn new(capacity_frames: usize, channels: usize) -> PcmRing {
        assert!(capacity_frames > 0, "a ring of no frames holds no audio");
        assert!(channels > 0, "a ring of no channels carries no audio");
        let samples = (0..capacity_frames * channels)
            .map(|_| UnsafeCell::new(0.0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        PcmRing {
            samples,
            capacity_frames,
            channels,
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            overrun_frames: AtomicU64::new(0),
        }
    }

    pub fn capacity_frames(&self) -> usize {
        self.capacity_frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Frames written and not yet read.
    ///
    /// Safe to call from either side; each sees a value that was true at some
    /// instant, which is all an occupancy figure can ever be.
    pub fn occupancy_frames(&self) -> usize {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        (write - read) as usize
    }

    /// Room for new frames, as the producer sees it.
    pub fn space_frames(&self) -> usize {
        self.capacity_frames - self.occupancy_frames()
    }

    /// Total frames the producer has published.
    pub fn produced(&self) -> u64 {
        self.write.load(Ordering::Relaxed)
    }

    /// Total frames the consumer has taken. Zero-filled frames are not in here:
    /// they are audio nobody produced, and counting them would hide exactly the
    /// divergence the two totals exist to expose.
    pub fn consumed(&self) -> u64 {
        self.read.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    pub fn underrun_frames(&self) -> u64 {
        self.underrun_frames.load(Ordering::Relaxed)
    }

    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    pub fn overrun_frames(&self) -> u64 {
        self.overrun_frames.load(Ordering::Relaxed)
    }

    /// The producer's half. Offers `frames` frames to the ring and calls
    /// `source` with the interleaved room it has, in at most two runs because
    /// the free region can straddle the end of the buffer.
    ///
    /// A closure rather than a slice to copy from, so the tone is generated
    /// straight into the ring and the whole path from oscillator to device is
    /// one copy rather than two.
    ///
    /// Whatever there is no room for is refused and counted. It is never
    /// written over unread audio: a ring that overwrote would turn a producer
    /// running ahead into a discontinuity in the middle of the stream, which
    /// the listener hears and no counter records.
    pub fn fill(&self, frames: usize, source: &mut impl FnMut(usize, &mut [f32])) -> Filled {
        let write = self.write.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        let space = self.capacity_frames - (write - read) as usize;
        let take = frames.min(space);
        let refused = frames - take;
        if refused > 0 {
            self.overruns.fetch_add(1, Ordering::Relaxed);
            self.overrun_frames
                .fetch_add(refused as u64, Ordering::Relaxed);
        }
        if take == 0 {
            return Filled { frames: 0, refused };
        }

        let start = (write % self.capacity_frames as u64) as usize;
        let first = take.min(self.capacity_frames - start);
        // SAFETY: frames `start ..  start + first` lie between the write cursor
        // and read + capacity, so the consumer will not touch them until the
        // release store below hands them over. `UnsafeCell<f32>` has the layout
        // of `f32`, and the range is inside the allocation because `first` is
        // clamped to the distance from `start` to the end of the buffer.
        let head = unsafe {
            core::slice::from_raw_parts_mut(
                self.samples[start * self.channels].get(),
                first * self.channels,
            )
        };
        source(0, head);
        if take > first {
            // SAFETY: the same argument for the wrapped remainder, which starts
            // at frame zero and is shorter than the read cursor's position
            // because `take` never exceeds the free space.
            let tail = unsafe {
                core::slice::from_raw_parts_mut(
                    self.samples[0].get(),
                    (take - first) * self.channels,
                )
            };
            source(first, tail);
        }
        self.write.store(write + take as u64, Ordering::Release);
        Filled {
            frames: take,
            refused,
        }
    }

    /// The consumer's half, and the whole of what the render callback does with
    /// the ring. Takes up to `frames` frames and hands them to `sink` as at most
    /// two interleaved runs, each with the frame offset it belongs at, because
    /// the readable region can straddle the end of the buffer.
    ///
    /// Runs rather than a destination slice so that a device wanting one buffer
    /// per channel can scatter directly out of the ring; a staging buffer in
    /// between would be a second copy on the one path in this project that has a
    /// hard deadline.
    ///
    /// Whatever the ring did not have is reported as `zero_filled` and counted
    /// as an underrun. Nothing waits, and nothing asks the producer for more.
    pub fn drain(&self, frames: usize, sink: &mut impl FnMut(usize, &[f32])) -> Drained {
        let read = self.read.load(Ordering::Relaxed);
        let write = self.write.load(Ordering::Acquire);
        let available = (write - read) as usize;
        let take = frames.min(available);
        let zero_filled = frames - take;
        if zero_filled > 0 {
            self.underruns.fetch_add(1, Ordering::Relaxed);
            self.underrun_frames
                .fetch_add(zero_filled as u64, Ordering::Relaxed);
        }
        if take == 0 {
            return Drained {
                frames: 0,
                zero_filled,
            };
        }

        let start = (read % self.capacity_frames as u64) as usize;
        let first = take.min(self.capacity_frames - start);
        // SAFETY: frames `start .. start + first` lie between the read cursor
        // and the write cursor, so the producer published them before the
        // acquire load above and will not touch them again until the release
        // store below frees them. The range is inside the allocation because
        // `first` is clamped to the distance from `start` to the end.
        let head = unsafe {
            core::slice::from_raw_parts(
                self.samples[start * self.channels].get(),
                first * self.channels,
            )
        };
        sink(0, head);
        if take > first {
            // SAFETY: the same argument for the wrapped remainder.
            let tail = unsafe {
                core::slice::from_raw_parts(self.samples[0].get(), (take - first) * self.channels)
            };
            sink(first, tail);
        }
        self.read.store(read + take as u64, Ordering::Release);
        Drained {
            frames: take,
            zero_filled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fills `frames` frames with an ascending count starting at `from`, so a
    /// reader can say not merely that it got audio but that it got the audio
    /// that was written, in order.
    fn write_counting(ring: &PcmRing, from: f32, frames: usize) -> Filled {
        let channels = ring.channels();
        ring.fill(frames, &mut |offset, run| {
            for (index, frame) in run.chunks_exact_mut(channels).enumerate() {
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample = from + (offset + index) as f32 + channel as f32 / 10.0;
                }
            }
        })
    }

    fn read_into(ring: &PcmRing, frames: usize) -> (Vec<f32>, Drained) {
        let channels = ring.channels();
        let mut out = vec![f32::NAN; frames * channels];
        let drained = ring.drain(frames, &mut |offset, run| {
            out[offset * channels..offset * channels + run.len()].copy_from_slice(run);
        });
        for sample in out.iter_mut().skip(drained.frames * channels) {
            *sample = 0.0;
        }
        (out, drained)
    }

    #[test]
    fn an_empty_ring_holds_nothing_and_a_full_one_holds_its_capacity() {
        let ring = PcmRing::new(8, 2);
        assert_eq!(ring.occupancy_frames(), 0);
        assert_eq!(ring.space_frames(), 8);
        assert_eq!(write_counting(&ring, 0.0, 8).frames, 8);
        assert_eq!(ring.occupancy_frames(), 8);
        assert_eq!(ring.space_frames(), 0);
    }

    #[test]
    fn what_was_written_comes_back_in_order() {
        let ring = PcmRing::new(8, 2);
        write_counting(&ring, 0.0, 4);
        let (out, drained) = read_into(&ring, 4);
        assert_eq!(drained.frames, 4);
        assert_eq!(drained.zero_filled, 0);
        assert_eq!(out, vec![0.0, 0.1, 1.0, 1.1, 2.0, 2.1, 3.0, 3.1]);
        assert_eq!(ring.occupancy_frames(), 0);
    }

    /// The case the wrapped cursors exist for: a write that straddles the end
    /// of the buffer, read back by a drain that straddles it too.
    #[test]
    fn a_write_that_wraps_reads_back_contiguous() {
        let ring = PcmRing::new(8, 2);
        write_counting(&ring, 0.0, 6);
        let (_, drained) = read_into(&ring, 5);
        assert_eq!(drained.frames, 5);

        // Six frames written, five read: the next write starts at index 6 and
        // has to run over the end of the buffer.
        let filled = write_counting(&ring, 100.0, 6);
        assert_eq!(filled.frames, 6);
        assert_eq!(filled.refused, 0);
        assert_eq!(ring.occupancy_frames(), 7);

        let (out, drained) = read_into(&ring, 7);
        assert_eq!(drained.frames, 7);
        assert_eq!(
            out,
            vec![
                5.0, 5.1, 100.0, 100.1, 101.0, 101.1, 102.0, 102.1, 103.0, 103.1, 104.0, 104.1,
                105.0, 105.1,
            ]
        );
    }

    #[test]
    fn the_cursors_survive_many_wraps() {
        let ring = PcmRing::new(5, 2);
        let mut expected = 0.0f32;
        for _ in 0..200 {
            assert_eq!(write_counting(&ring, expected, 3).frames, 3);
            let (out, drained) = read_into(&ring, 3);
            assert_eq!(drained.frames, 3);
            assert_eq!(out[0], expected);
            assert_eq!(out[4], expected + 2.0);
            expected += 3.0;
        }
        assert_eq!(ring.produced(), 600);
        assert_eq!(ring.consumed(), 600);
        assert_eq!(ring.occupancy_frames(), 0);
    }

    #[test]
    fn a_full_ring_refuses_the_producer_rather_than_overwriting() {
        let ring = PcmRing::new(4, 2);
        assert_eq!(write_counting(&ring, 0.0, 4).frames, 4);

        let filled = write_counting(&ring, 900.0, 4);
        assert_eq!(filled.frames, 0);
        assert_eq!(filled.refused, 4);
        assert_eq!(ring.overruns(), 1);
        assert_eq!(ring.overrun_frames(), 4);
        assert_eq!(ring.produced(), 4);

        // The refused audio must not be anywhere in the ring: the four frames
        // the consumer had not read are still the four frames it was owed.
        let (out, _) = read_into(&ring, 4);
        assert_eq!(out, vec![0.0, 0.1, 1.0, 1.1, 2.0, 2.1, 3.0, 3.1]);
    }

    #[test]
    fn a_partly_full_ring_takes_what_fits_and_counts_the_rest() {
        let ring = PcmRing::new(4, 2);
        write_counting(&ring, 0.0, 3);
        let filled = write_counting(&ring, 50.0, 3);
        assert_eq!(filled.frames, 1);
        assert_eq!(filled.refused, 2);
        assert_eq!(ring.overruns(), 1);
        assert_eq!(ring.overrun_frames(), 2);
        assert_eq!(ring.produced(), 4);
    }

    #[test]
    fn an_empty_ring_reports_the_whole_request_as_shortfall() {
        let ring = PcmRing::new(4, 2);
        let (out, drained) = read_into(&ring, 4);
        assert_eq!(drained.frames, 0);
        assert_eq!(drained.zero_filled, 4);
        assert_eq!(ring.underruns(), 1);
        assert_eq!(ring.underrun_frames(), 4);
        assert_eq!(out, vec![0.0; 8]);
        assert_eq!(ring.consumed(), 0);
    }

    #[test]
    fn a_partial_drain_reports_only_the_frames_it_was_short() {
        let ring = PcmRing::new(8, 2);
        write_counting(&ring, 0.0, 3);
        let (out, drained) = read_into(&ring, 5);
        assert_eq!(drained.frames, 3);
        assert_eq!(drained.zero_filled, 2);
        assert_eq!(ring.underruns(), 1);
        assert_eq!(ring.underrun_frames(), 2);
        assert_eq!(&out[..6], &[0.0, 0.1, 1.0, 1.1, 2.0, 2.1]);
        assert_eq!(&out[6..], &[0.0, 0.0, 0.0, 0.0]);
    }

    /// Two shortfalls are two underruns and their frames add up, because a
    /// report that counted the second as a continuation of the first would say
    /// a run glitched once when it glitched twice.
    #[test]
    fn shortfalls_accumulate() {
        let ring = PcmRing::new(8, 2);
        read_into(&ring, 4);
        write_counting(&ring, 0.0, 1);
        read_into(&ring, 4);
        assert_eq!(ring.underruns(), 2);
        assert_eq!(ring.underrun_frames(), 7);
    }

    /// The accounting the report turns on: whatever the two ends did, the
    /// difference between what was produced and what was consumed is what is
    /// still sitting in the ring, and never more.
    #[test]
    fn produced_less_consumed_is_the_occupancy() {
        let ring = PcmRing::new(6, 2);
        for step in 0..50 {
            write_counting(&ring, step as f32 * 10.0, 4);
            read_into(&ring, if step % 3 == 0 { 5 } else { 2 });
            assert_eq!(
                ring.produced() - ring.consumed(),
                ring.occupancy_frames() as u64
            );
        }
    }

    /// A refused write and a short drain are different failures, and neither
    /// may show up in the other's counter.
    #[test]
    fn overruns_and_underruns_never_share_a_counter() {
        let ring = PcmRing::new(2, 2);
        write_counting(&ring, 0.0, 4);
        assert_eq!(ring.overruns(), 1);
        assert_eq!(ring.underruns(), 0);

        read_into(&ring, 6);
        assert_eq!(ring.overruns(), 1);
        assert_eq!(ring.overrun_frames(), 2);
        assert_eq!(ring.underruns(), 1);
        assert_eq!(ring.underrun_frames(), 4);
    }

    /// Mono and six channels go through the same arithmetic as stereo, because
    /// the device's channel count is not this project's to choose.
    #[test]
    fn any_channel_count_wraps_the_same_way() {
        for channels in [1usize, 2, 6] {
            let ring = PcmRing::new(3, channels);
            write_counting(&ring, 0.0, 2);
            let (_, drained) = read_into(&ring, 2);
            assert_eq!(drained.frames, 2);
            let filled = write_counting(&ring, 7.0, 3);
            assert_eq!(filled.frames, 3);
            let (out, drained) = read_into(&ring, 3);
            assert_eq!(drained.frames, 3);
            assert_eq!(out.len(), 3 * channels);
            assert_eq!(out[0], 7.0);
            assert_eq!(out[2 * channels], 9.0);
        }
    }

    /// The producer and the consumer really do run on two threads, so the ring
    /// is exercised on two threads: a million frames through a ring that holds
    /// a few hundred, with both ends checking the stream they see.
    #[test]
    fn a_million_frames_cross_between_two_threads_intact() {
        use std::sync::Arc;
        use std::thread;

        const FRAMES: u64 = 1_000_000;
        let ring = Arc::new(PcmRing::new(256, 2));
        let producer = Arc::clone(&ring);
        let feeder = thread::spawn(move || {
            let mut written = 0u64;
            while written < FRAMES {
                let want = ((FRAMES - written) as usize).min(64);
                let space = producer.space_frames();
                if space == 0 {
                    std::hint::spin_loop();
                    continue;
                }
                let filled = producer.fill(want.min(space), &mut |offset, run| {
                    for (index, frame) in run.chunks_exact_mut(2).enumerate() {
                        let position = written + (offset + index) as u64;
                        frame[0] = position as f32;
                        frame[1] = -(position as f32);
                    }
                });
                written += filled.frames as u64;
            }
        });

        let mut read = 0u64;
        while read < FRAMES {
            let mut seen = 0u64;
            let drained = ring.drain(48, &mut |offset, run| {
                for (index, frame) in run.chunks_exact(2).enumerate() {
                    let position = read + (offset + index) as u64;
                    assert_eq!(frame[0], position as f32, "sample out of order");
                    assert_eq!(frame[1], -(position as f32), "channels crossed");
                    seen += 1;
                }
            });
            assert_eq!(seen, drained.frames as u64);
            read += drained.frames as u64;
        }
        feeder.join().expect("the producer thread panicked");
        assert_eq!(ring.produced(), FRAMES);
        assert_eq!(ring.consumed(), FRAMES);
        assert_eq!(ring.overruns(), 0);
    }
}
