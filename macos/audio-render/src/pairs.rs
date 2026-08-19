//! Which of a captured packet's two Opus frames a datagram carried, and how the
//! two of them differ in the only quantity that has a predicted value.
//!
//! # What is being separated, and why the wire can do it
//!
//! The host endpoint delivers exactly 480 frames in every packet, which at the
//! 5 ms frame this phase fixed is exactly two Opus frames, so every captured
//! packet becomes two datagrams whose RTP timestamps differ by 240. Nothing
//! else in the stream steps by anything but 240, so the timestamps alone
//! partition every frame of a run into two residue classes modulo 480, and the
//! partition is exact: it survives loss, it survives reordering, and it needs no
//! arrival time to compute. That is what [`class_of`] is.
//!
//! What the timestamps cannot say is which of those two classes is a captured
//! packet's **first** frame. RFC 3550 requires the timestamp to start at a
//! random value and [`lanplay_transport::OpusPacketizer::new`] obeys it, and
//! this receiver joins a stream that is already running, so the absolute residue
//! of its own first datagram carries no information about where in a packet that
//! datagram sat. The classes below are therefore labelled by their offset from
//! the receiver's own anchor and never by the words first and second, and the
//! one bit that turns an offset into a position comes from the sender's envelope
//! stating the RTP timestamp its run began at. Labelling the classes from
//! anything this side could see - which of the two arrives earlier, say - would
//! be deciding the answer with the measurement, because arrival order within a
//! pair is exactly what is in question.
//!
//! # Why both pairings are measured
//!
//! A step is a difference of two lateness figures one frame apart, and there are
//! two ways to walk a stream in steps: pairing each class-0 frame with the
//! class-1 frame after it, or each class-1 frame with the class-0 frame after
//! it. Exactly one of those is the intra-packet pair; the other pairs a packet's
//! second frame with the next packet's first.
//!
//! Both are kept, because together they are a stronger statement than either.
//! Two frames of one packet leave within 44 microseconds of each other and their
//! deadlines are 5 ms apart, so the intra-packet step must be about -4.956 ms;
//! consecutive packets arrive a capture period apart and their deadlines are a
//! capture period apart too, so the cross-packet step must be about +4.956 ms.
//! A run where one step is near -5 and the other near +5 has confirmed the
//! cadence the timestamps require and left only the labelling to the sender's
//! anchor. A run where both are near zero has found a sender that spaced its
//! pair, which is the defect this audit exists to look for, and it has found it
//! without needing the anchor at all.
//!
//! # Why a histogram beside the percentiles
//!
//! A mean near -5 ms is what a stream that is always -5 looks like and also what
//! a stream that is half at -10 and half at zero looks like, and those two want
//! opposite conclusions. Order statistics from [`Samples`] answer the first
//! question and a histogram answers the second, so both are kept: the buckets
//! are a millisecond wide, which is a fifth of the distance between the two
//! candidate answers, and they span thirteen milliseconds either side because
//! the worst arrival this phase has recorded was 85 ms and the counted tails are
//! what say how much of the run fell outside.

use lanplay_audio_capture::Samples;
use lanplay_transport::RtpTimestamp;

/// Frames in a captured packet, and so residue classes to keep apart.
///
/// Two, and not a parameter. A1 measured the endpoint delivering exactly 480
/// frames in every packet and the sender splits rather than accumulates, so no
/// residue ever carries between packets and no third class can exist. A run
/// where that stopped being true would report a split residue on the sending
/// end, which is the number to read before any of these.
pub const CLASSES: usize = 2;

/// The shift that lets a signed distribution live in an unsigned store.
///
/// The same arithmetic as the arrival delay's, and its own constant rather than
/// a borrowed one because this module is platform independent while the
/// receiving path is not: the pairing is arithmetic and is unit-tested on any
/// machine, including one with no audio device to receive through.
const BIAS_US: i64 = 10_000_000;

/// One bucket of the step histogram, in microseconds.
pub const BUCKET_US: i64 = 1_000;

/// The lowest step the histogram names rather than counts as a tail.
pub const BUCKET_FLOOR_US: i64 = -13_000;

/// Named buckets plus a tail either side.
pub const BUCKETS: usize = 28;

/// Frames whose lateness is held while their neighbour is awaited.
///
/// Sixty-four frames is 320 milliseconds of stream, which is nearly four times
/// the worst arrival this phase has measured, so a step is lost only to a
/// datagram that never came or to one held longer than any this link has held.
/// Nothing here grows: a step that cannot be closed is a step this module does
/// not count, and the two populations are reported so that the shortfall is
/// visible rather than absorbed.
const RECENT: usize = 64;

/// Which residue class a frame belongs to, counting from `anchor` in frames.
///
/// Zero is the anchor's own class. The result is a property of the timestamp and
/// of nothing else, which is what makes it usable on a stream that lost packets
/// and on one whose datagrams arrived out of order.
pub fn class_of(timestamp: RtpTimestamp, anchor: RtpTimestamp, frame_samples: u32) -> usize {
    let step = i64::from(frame_samples);
    let packet = step * CLASSES as i64;
    (timestamp.distance_from(anchor).rem_euclid(packet) / step) as usize
}

/// Which histogram bucket a step falls in, with the tails at either end.
pub fn bucket_of(micros: i64) -> usize {
    if micros < BUCKET_FLOOR_US {
        return 0;
    }
    let index = (micros - BUCKET_FLOOR_US) / BUCKET_US + 1;
    (index as usize).min(BUCKETS - 1)
}

/// The microseconds a named bucket starts at, or `None` for the two tails.
pub fn bucket_floor_us(index: usize) -> Option<i64> {
    if index == 0 || index >= BUCKETS - 1 {
        return None;
    }
    Some(BUCKET_FLOOR_US + (index as i64 - 1) * BUCKET_US)
}

/// The order statistics of a signed distribution, in microseconds.
///
/// Signed at the point of reporting rather than in the store, so that a reader
/// never has to know a bias was applied and a store written for unsigned
/// measurements is not reimplemented for one caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Spread {
    pub count: usize,
    pub min: i64,
    pub p50: i64,
    pub p95: i64,
    pub p99: i64,
    pub max: i64,
}

/// One frame held while the frame next to it is awaited.
#[derive(Clone, Copy, Default)]
struct Recent {
    timestamp: u32,
    late_us: i64,
    present: bool,
}

/// Everything the arrival side of the audit counts, split by residue class.
///
/// Written by the receiving thread and by nothing else, so it takes no lock and
/// makes no allocation after construction.
pub struct PairTiming {
    frame_samples: u32,
    /// The frame the classes are counted from, which is the first this ever saw.
    anchor: Option<RtpTimestamp>,
    frames: [u64; CLASSES],
    late: [u64; CLASSES],
    delay_us: [Samples; CLASSES],
    step_us: [Samples; CLASSES],
    step_buckets: [[u64; BUCKETS]; CLASSES],
    recent: Box<[Recent]>,
    /// Frames offered before an anchor existed, which is a defect in the caller
    /// rather than in the stream and is counted rather than assumed impossible.
    unanchored: u64,
}

impl PairTiming {
    pub fn new(frame_samples: u32, store: usize) -> PairTiming {
        PairTiming {
            frame_samples,
            anchor: None,
            frames: [0; CLASSES],
            late: [0; CLASSES],
            delay_us: core::array::from_fn(|_| Samples::with_capacity(store)),
            step_us: core::array::from_fn(|_| Samples::with_capacity(store)),
            step_buckets: [[0; BUCKETS]; CLASSES],
            recent: vec![Recent::default(); RECENT].into_boxed_slice(),
            unanchored: 0,
        }
    }

    /// Fixes the frame the classes are counted from.
    ///
    /// Taken from the caller rather than from the first frame offered, because
    /// the same anchor has to serve the producer thread's underrun attribution,
    /// and two threads each anchoring on the first thing they saw would label
    /// the classes oppositely half the time.
    pub fn anchor(&mut self, anchor: RtpTimestamp) {
        if self.anchor.is_none() {
            self.anchor = Some(anchor);
        }
    }

    /// Records one arrival: how far past its own moment it came, and whether the
    /// buffer judged it past saving.
    pub fn arrived(&mut self, timestamp: RtpTimestamp, late_us: i64, late: bool) {
        let Some(anchor) = self.anchor else {
            self.unanchored += 1;
            return;
        };
        let class = class_of(timestamp, anchor, self.frame_samples);
        self.frames[class] += 1;
        if late {
            self.late[class] += 1;
        }
        self.delay_us[class].record(bias(late_us));

        // Both neighbours, because either of a pair may be the one that closes
        // it: a step is recorded when its second member lands, whichever member
        // that turned out to be, so reordering costs no pair and duplicates
        // none.
        let step = self.frame_samples;
        let earlier = RtpTimestamp(timestamp.0.wrapping_sub(step));
        if let Some(then) = self.recall(anchor, earlier) {
            let class = class_of(earlier, anchor, self.frame_samples);
            self.record_step(class, late_us - then);
        }
        let later = RtpTimestamp(timestamp.0.wrapping_add(step));
        if let Some(then) = self.recall(anchor, later) {
            self.record_step(class, then - late_us);
        }

        self.remember(anchor, timestamp, late_us);
    }

    pub fn finish(mut self) -> PairReport {
        let dropped: u64 = self
            .delay_us
            .iter()
            .chain(self.step_us.iter())
            .map(Samples::dropped)
            .sum();
        let classes = core::array::from_fn(|class| ClassReport {
            offset_samples: class as u32 * self.frame_samples,
            frames: self.frames[class],
            late: self.late[class],
            underruns: 0,
            delay_us: spread(&mut self.delay_us[class]),
            step_us: spread(&mut self.step_us[class]),
            step_buckets: self.step_buckets[class],
        });
        PairReport {
            anchor: self.anchor.map(|anchor| anchor.0),
            frame_samples: self.frame_samples,
            classes,
            samples_dropped: dropped,
            unanchored: self.unanchored,
        }
    }

    fn record_step(&mut self, class: usize, micros: i64) {
        self.step_us[class].record(bias(micros));
        self.step_buckets[class][bucket_of(micros)] += 1;
    }

    fn slot(&self, anchor: RtpTimestamp, timestamp: RtpTimestamp) -> usize {
        let index = timestamp.distance_from(anchor) / i64::from(self.frame_samples);
        index.rem_euclid(self.recent.len() as i64) as usize
    }

    /// The lateness of a frame still in the window, and nothing when the slot
    /// holds a different frame - which is how a wrapped slot is told from a hit.
    fn recall(&self, anchor: RtpTimestamp, timestamp: RtpTimestamp) -> Option<i64> {
        let held = self.recent[self.slot(anchor, timestamp)];
        (held.present && held.timestamp == timestamp.0).then_some(held.late_us)
    }

    fn remember(&mut self, anchor: RtpTimestamp, timestamp: RtpTimestamp, late_us: i64) {
        let slot = self.slot(anchor, timestamp);
        self.recent[slot] = Recent {
            timestamp: timestamp.0,
            late_us,
            present: true,
        };
    }
}

/// One residue class of a run, as a result.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClassReport {
    /// Samples past the anchor, modulo a captured packet. Zero or one frame.
    pub offset_samples: u32,
    pub frames: u64,
    /// Frames the buffer refused because their moment had gone.
    pub late: u64,
    /// Pulls that found the buffer empty at a position of this class.
    pub underruns: u64,
    /// How far past its own moment a frame of this class arrived, positive when
    /// late.
    pub delay_us: Option<Spread>,
    /// The lateness of the frame one step later, less this frame's.
    pub step_us: Option<Spread>,
    pub step_buckets: [u64; BUCKETS],
}

/// Everything the audit established, once both threads have stopped.
#[derive(Clone, Debug)]
pub struct PairReport {
    /// The RTP timestamp the classes are counted from, absent when nothing
    /// arrived. Reported because it is half of the join that turns an offset
    /// into a position in a captured packet; the sender's envelope has the other
    /// half.
    pub anchor: Option<u32>,
    pub frame_samples: u32,
    pub classes: [ClassReport; CLASSES],
    pub samples_dropped: u64,
    pub unanchored: u64,
}

impl PairReport {
    /// Folds in the underruns the producer thread attributed.
    ///
    /// Counted there and not here because an underrun is a pull that found
    /// nothing, which is an event on the producer's schedule and not an arrival
    /// at all; the two are joined once, at the end, rather than by sharing a
    /// counter across a lock.
    pub fn with_underruns(mut self, underruns: [u64; CLASSES]) -> PairReport {
        for (class, count) in self.classes.iter_mut().zip(underruns) {
            class.underruns = count;
        }
        self
    }

    /// The population every step figure was taken over.
    pub fn steps(&self) -> usize {
        self.classes
            .iter()
            .filter_map(|class| class.step_us)
            .map(|step| step.count)
            .sum()
    }
}

fn bias(micros: i64) -> u64 {
    (micros + BIAS_US).clamp(0, 2 * BIAS_US) as u64
}

fn spread(samples: &mut Samples) -> Option<Spread> {
    samples.percentiles().map(|held| Spread {
        count: held.count,
        min: unbias(held.min),
        p50: unbias(held.p50),
        p95: unbias(held.p95),
        p99: unbias(held.p99),
        max: unbias(held.max),
    })
}

fn unbias(biased: u64) -> i64 {
    biased as i64 - BIAS_US
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u32 = 240;

    /// The partition the whole audit rests on: consecutive frames alternate, and
    /// a frame two steps along is back in the class it started in.
    #[test]
    fn consecutive_frames_alternate_classes() {
        let anchor = RtpTimestamp(1_000_000);
        for step in 0..8u32 {
            let at = RtpTimestamp(anchor.0 + step * FRAME);
            assert_eq!(class_of(at, anchor, FRAME), (step % 2) as usize);
        }
    }

    /// A frame with a timestamp behind the anchor is a reordered frame and not a
    /// negative class. Truncating division would put it in the wrong one, which
    /// would silently swap the two classes for the whole tail of a run.
    #[test]
    fn a_frame_before_the_anchor_still_has_a_class() {
        let anchor = RtpTimestamp(1_000_000);
        assert_eq!(class_of(RtpTimestamp(anchor.0 - FRAME), anchor, FRAME), 1);
        assert_eq!(
            class_of(RtpTimestamp(anchor.0 - 2 * FRAME), anchor, FRAME),
            0
        );
        assert_eq!(
            class_of(RtpTimestamp(anchor.0 - 3 * FRAME), anchor, FRAME),
            1
        );
    }

    /// The classes are a property of the timestamp, so a stream that crosses the
    /// thirty-two-bit wrap does not change its mind about which class a frame is
    /// in. At 48 kHz the counter turns over every twenty-five hours, and a
    /// session may outlive that.
    #[test]
    fn the_classes_survive_the_timestamp_wrap() {
        let anchor = RtpTimestamp(u32::MAX - 3 * FRAME + 1);
        for step in 0..8u32 {
            let at = RtpTimestamp(anchor.0.wrapping_add(step * FRAME));
            assert_eq!(class_of(at, anchor, FRAME), (step % 2) as usize);
        }
    }

    /// A run of the predicted shape, read back. The pair that left together is
    /// -4.956 ms because its deadlines are 5 ms apart, and the pair that
    /// straddles two captured packets is +4.956 ms because their arrivals are a
    /// capture period apart as well as their deadlines. The two are told apart
    /// by their class and by nothing else.
    #[test]
    fn a_pair_that_left_together_reads_as_the_timestamps_require() {
        let anchor = RtpTimestamp(500_000);
        let mut pairs = PairTiming::new(FRAME, 1_024);
        pairs.anchor(anchor);

        // Ten captured packets. Both frames of a packet arrive at the same
        // instant to within 44 microseconds, and the packets are 10 ms apart, so
        // every frame's lateness is its arrival less its own deadline.
        for packet in 0..10i64 {
            let first = RtpTimestamp((anchor.0 as i64 + packet * 2 * i64::from(FRAME)) as u32);
            let second = RtpTimestamp(first.0 + FRAME);
            // Arrival is the same for both, so the first is late by whatever the
            // link cost and the second has 5 ms more margin.
            pairs.arrived(first, -8_000, false);
            pairs.arrived(second, -8_000 - 4_956, false);
        }

        let report = pairs.finish();
        assert_eq!(report.anchor, Some(anchor.0));
        assert_eq!(report.classes[0].frames, 10);
        assert_eq!(report.classes[1].frames, 10);

        let intra = report.classes[0].step_us.expect("ten pairs closed");
        assert_eq!(intra.count, 10);
        assert_eq!(intra.p50, -4_956);

        let across = report.classes[1].step_us.expect("nine pairs closed");
        assert_eq!(across.count, 9, "the last packet has no successor");
        assert_eq!(across.p50, 4_956);
    }

    /// A sender that held its second frame back by a frame period would put both
    /// steps at zero, which is the defect this audit is looking for and is not
    /// confusable with the shape above.
    #[test]
    fn a_spaced_pair_reads_as_no_step_at_all() {
        let anchor = RtpTimestamp(77);
        let mut pairs = PairTiming::new(FRAME, 1_024);
        pairs.anchor(anchor);
        for packet in 0..10u32 {
            let first = RtpTimestamp(anchor.0 + packet * 2 * FRAME);
            pairs.arrived(first, -8_000, false);
            pairs.arrived(RtpTimestamp(first.0 + FRAME), -8_000, false);
        }
        let report = pairs.finish();
        assert_eq!(report.classes[0].step_us.map(|step| step.p50), Some(0));
        assert_eq!(report.classes[1].step_us.map(|step| step.p50), Some(0));
    }

    /// A step is closed by whichever of its two frames lands second, so the
    /// reordered stream and the ordered one produce the same pairs. A mechanism
    /// that only ever looked backwards would lose one pair here and would lose
    /// it selectively, from the class that happened to arrive late.
    #[test]
    fn a_reordered_pair_is_still_one_pair() {
        let anchor = RtpTimestamp(9_000);
        let mut pairs = PairTiming::new(FRAME, 1_024);
        pairs.anchor(anchor);
        // The second frame of the packet arrives before the first.
        pairs.arrived(RtpTimestamp(anchor.0 + FRAME), -12_956, false);
        pairs.arrived(anchor, -8_000, false);
        let report = pairs.finish();
        let intra = report.classes[0].step_us.expect("the pair closed");
        assert_eq!(intra.count, 1);
        assert_eq!(intra.p50, -4_956);
    }

    /// A distribution's median can be right while its shape is the opposite of
    /// what a reader would infer, and the buckets are what say so. Half at -10
    /// and half at zero has a median of zero and no observation anywhere near
    /// it.
    #[test]
    fn the_buckets_show_a_split_a_median_hides() {
        let anchor = RtpTimestamp(3);
        let mut pairs = PairTiming::new(FRAME, 1_024);
        pairs.anchor(anchor);
        for packet in 0..10u32 {
            let first = RtpTimestamp(anchor.0 + packet * 2 * FRAME);
            let second = RtpTimestamp(first.0 + FRAME);
            pairs.arrived(first, 0, false);
            pairs.arrived(second, if packet % 2 == 0 { -10_000 } else { 0 }, false);
        }
        let report = pairs.finish();
        let buckets = report.classes[0].step_buckets;
        assert_eq!(buckets[bucket_of(-10_000)], 5);
        assert_eq!(buckets[bucket_of(0)], 5);
        assert_eq!(
            buckets[bucket_of(-5_000)],
            0,
            "nothing was measured near the median, which is the point of the buckets"
        );
    }

    /// The tails are counted rather than clamped into the nearest named bucket,
    /// because a step outside the span is a finding and a step at the edge is
    /// not.
    #[test]
    fn a_step_outside_the_span_lands_in_a_tail() {
        assert_eq!(bucket_of(-13_001), 0);
        assert_eq!(bucket_of(-13_000), 1);
        assert_eq!(bucket_of(12_999), BUCKETS - 2);
        assert_eq!(bucket_of(13_000), BUCKETS - 1);
        assert_eq!(bucket_floor_us(0), None);
        assert_eq!(bucket_floor_us(BUCKETS - 1), None);
        assert_eq!(bucket_floor_us(1), Some(-13_000));
        assert_eq!(bucket_floor_us(BUCKETS - 2), Some(12_000));
    }

    /// Underruns are the producer's and arrive separately, so a report that was
    /// never handed any says zero rather than pretending to a population it
    /// never had.
    #[test]
    fn underruns_are_folded_in_by_class() {
        let mut pairs = PairTiming::new(FRAME, 16);
        pairs.anchor(RtpTimestamp(0));
        pairs.arrived(RtpTimestamp(0), 0, false);
        let report = pairs.finish().with_underruns([3, 7]);
        assert_eq!(report.classes[0].underruns, 3);
        assert_eq!(report.classes[1].underruns, 7);
    }

    /// A frame offered before an anchor exists is counted and not classified.
    /// Guessing an anchor from it would put the two classes the wrong way round
    /// for the whole run, which is the one error this measurement cannot survive.
    #[test]
    fn a_frame_with_no_anchor_is_counted_and_not_guessed() {
        let mut pairs = PairTiming::new(FRAME, 16);
        pairs.arrived(RtpTimestamp(1_234), 0, false);
        let report = pairs.finish();
        assert_eq!(report.unanchored, 1);
        assert_eq!(report.anchor, None);
        assert_eq!(report.classes[0].frames, 0);
        assert_eq!(report.classes[1].frames, 0);
    }

    /// Late frames are timed like any other and counted apart, because a frame
    /// that missed its moment is the event the whole phase is about and dropping
    /// it from the distribution would remove the tail being measured.
    #[test]
    fn a_late_frame_is_timed_and_counted() {
        let anchor = RtpTimestamp(48_000);
        let mut pairs = PairTiming::new(FRAME, 64);
        pairs.anchor(anchor);
        pairs.arrived(anchor, 20_000, true);
        pairs.arrived(RtpTimestamp(anchor.0 + FRAME), 15_044, true);
        let report = pairs.finish();
        assert_eq!(report.classes[0].late, 1);
        assert_eq!(report.classes[1].late, 1);
        assert_eq!(
            report.classes[0].delay_us.map(|held| held.p50),
            Some(20_000)
        );
        assert_eq!(
            report.classes[0].step_us.map(|step| step.p50),
            Some(-4_956),
            "a pair of late frames still has the pair's own difference"
        );
    }
}
