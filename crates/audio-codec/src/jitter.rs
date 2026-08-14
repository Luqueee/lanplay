//! The buffer that turns arriving datagrams back into an ordered, continuous
//! stream, and the accounting that says whether it stayed continuous.
//!
//! Audio is not video, and the difference decides every rule here. The video
//! path is latest-frame-wins: a late picture is worthless because a newer one
//! describes the world better, so it is dropped and forgotten. Audio is an
//! ordered continuous stream. A gap in it is not skipped, it is concealed, and
//! playback carries on from where it was. Playing the newest frame available
//! and abandoning what came before would be an audible jump, and a listener
//! notices a discontinuity far more readily than a few milliseconds of extra
//! delay. So the video path's design is not reused, and it is not adapted; it
//! is the wrong shape.
//!
//! Four decisions follow from that, and each rejects an alternative that looks
//! reasonable until it is written down.
//!
//! A frame that has not arrived when its moment comes is concealed with the
//! codec's own concealer, [`crate::decoder::OpusDecoder::conceal`], and never
//! with zero-filled silence. Silence is a step to zero and back, which is a
//! click; the concealer extrapolates from the frames that did arrive, so the
//! waveform continues.
//!
//! A frame that arrives after it was concealed is discarded, counted as late,
//! and never played. Its moment has passed. Playing it would either delay
//! everything behind it by a frame, permanently, or duplicate a moment the
//! listener has already heard.
//!
//! A frame's deadline comes from its RTP timestamp, not from when its datagram
//! happened to turn up. Exactly one arrival time is ever consulted: the first
//! one, which anchors the stream's sample counter to this machine's clock.
//! Everything after that is arithmetic on the timestamp. A deadline computed
//! from arrival would drift with the jitter the buffer exists to absorb, which
//! is the same as having no deadline at all.
//!
//! And occupancy is bounded in time as well as in slots. A stall that releases
//! a burst must not leave the buffer holding more audio than it is meant to:
//! absorbing a bounded fault by growing trades it for unbounded latency that
//! never recovers, because nothing in a continuous stream ever gives the buffer
//! an opportunity to shrink again. When the ceiling is breached the buffer
//! skips forward, discarding what it skips over and counting every frame of it.
//!
//! Both bounds are fixed at construction. Nothing here allocates, blocks, logs
//! or waits once it is built.

use lanplay_telemetry::{Nanos, Timestamp};
use lanplay_transport::{MAX_OPUS_PAYLOAD, OpusPacket, RtpTimestamp, SequenceNumber};

use crate::config::CodecConfig;

/// Sequence numbers the duplicate check remembers.
///
/// A thousand packets is five seconds at 5 ms, several hundred times longer
/// than any path this phase measures can hold a datagram. A copy arriving later
/// than that would be counted as a fresh arrival and then rejected as late,
/// which is the right outcome by a slightly different name.
///
/// A ring indexed by the sequence number itself rather than a set, because
/// consecutive numbers land in consecutive slots: the check is one comparison
/// and no allocation, and a slot is only reused once a whole window has gone
/// past.
const SEEN_WINDOW: usize = 1_024;

/// What the buffer did with an arriving packet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
    /// Queued for the deadline its timestamp gives it.
    ///
    /// `reordered` says the packet arrived behind one the buffer had already
    /// seen. That is not a fault and the frame decodes normally: a jitter
    /// buffer exists so that reordering inside its window is invisible, and
    /// refusing a frame for arriving out of order would be refusing to do the
    /// job. The counter exists because reordering is what consumes the window.
    /// A stream that reorders by more than the target needs a larger target,
    /// and this number is the evidence for choosing one.
    Queued { reordered: bool },
    /// A second copy of a packet already accounted for. Dropped; nothing about
    /// the stream changes.
    Duplicate,
    /// Its moment has already gone by — the frame was concealed, played, or
    /// skipped to hold the ceiling. Discarded.
    Late,
    /// The timestamp is not a whole number of frames from the one that anchored
    /// the stream, so this frame can never line up with a playout position.
    ///
    /// Named rather than merely dropped because it has exactly one cause worth
    /// finding: a sender running at a frame duration this receiver was not
    /// configured for. Without the name the symptom is a run that conceals
    /// everything and plays nothing, which is a symptom this project has
    /// misread before.
    OffGrid,
    /// A payload longer than a slot. Impossible from a datagram this receiver
    /// can read, since [`MAX_OPUS_PAYLOAD`] is what is left of one after the
    /// fixed header, and refused rather than truncated so that it stays
    /// impossible.
    Oversize,
}

/// What the sink was handed when it pulled.
///
/// Two ways of concealing, kept apart because they mean different things about
/// the path. A gap has real audio on both sides of it and the concealer is
/// bridging between them; an underrun has nothing behind it at all and the
/// concealer is running on stale state, inventing. Counting the second as
/// delivered audio is exactly how a path that carries nothing can be made to
/// look like a path that works.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pull {
    /// A real frame. The payload was copied into the caller's buffer and this
    /// is its length.
    Frame(usize),
    /// The frame due now never arrived, but the buffer holds later ones: a hole
    /// in a stream that is otherwise running. Conceal it and carry on.
    Conceal,
    /// Nothing is buffered at all. Conceal it too — a render callback handed no
    /// samples produces a click, and this phase must not invent a failure the
    /// next phase will not have — but it is a hole in the audio, not a bridge
    /// across one.
    Underrun,
}

/// One pull, and the audio the buffer was still holding back afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pulled {
    pub outcome: Pull,
    /// Frames left after the one due now was taken.
    ///
    /// Measured after rather than before, because the frame being served is
    /// not latency — it is the audio the sink is playing this instant — and
    /// counting it would report every healthy stream as one frame deeper than
    /// the target it is holding to. What is left is what the buffer is holding
    /// back, and in a healthy stream it is exactly the target.
    ///
    /// The occupancy figures come from here rather than from a sampler of their
    /// own, because the number that matters is the one the consumer found.
    pub occupancy: usize,
}

/// Everything the buffer counted.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Counts {
    /// Well-formed Opus packets handed to the buffer, whatever it then did with
    /// them.
    pub received: u64,
    pub late: u64,
    pub duplicate: u64,
    pub reordered: u64,
    pub off_grid: u64,
    pub oversize: u64,
    /// Frames the sink was handed from a packet that arrived in time.
    pub played: u64,
    /// Frames the concealer produced, gaps and underruns together, because both
    /// are frames the sink consumed that no packet supplied.
    pub concealed: u64,
    /// Concealed frames where the buffer was empty rather than merely missing
    /// the one frame due.
    pub underruns: u64,
    /// Times occupancy reached the ceiling and the buffer skipped forward.
    pub overruns: u64,
    /// Frames given up across all of those skips.
    pub overrun_frames: u64,
    /// Per-channel samples the playout position travelled: every frame period
    /// the stream should have produced audio for, including the ones skipped to
    /// hold the ceiling.
    pub expected_samples: u64,
    /// Per-channel samples the sink was handed as this stream's audio: decoded
    /// frames and gap concealments.
    ///
    /// Concealed samples count here because the listener heard something
    /// continuous, which is the whole point of concealing. Underruns and
    /// skipped frames do not, because nothing of the stream was there — and a
    /// counter that credited those would report a silent path as a working one.
    pub played_samples: u64,
}

impl Counts {
    /// Samples the stream should have produced that the sink never got. Zero is
    /// the only good value; anything else is audio that was not there.
    pub fn continuity_hole(&self) -> u64 {
        self.expected_samples.saturating_sub(self.played_samples)
    }
}

/// Where the stream's sample counter meets this machine's clock.
///
/// Set once, by the first packet the buffer accepts, and never moved. Every
/// deadline in the run is derived from it, which is what makes a deadline a
/// property of the audio rather than of the network.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Anchor {
    rtp: RtpTimestamp,
    /// When the frame carrying `rtp` is due, which is its arrival plus the
    /// target. Playout of the whole stream starts here.
    playout: Timestamp,
}

/// One queued frame, or a free slot when `len` is zero.
///
/// The payload is stored inline at the largest size a datagram can carry, so a
/// frame costs a copy and never an allocation.
#[derive(Clone)]
struct Slot {
    timestamp: RtpTimestamp,
    len: usize,
    bytes: [u8; MAX_OPUS_PAYLOAD],
}

/// Sequence numbers already accounted for.
struct SeenWindow {
    slots: Box<[Option<SequenceNumber>; SEEN_WINDOW]>,
}

impl SeenWindow {
    fn new() -> SeenWindow {
        SeenWindow {
            slots: Box::new([None; SEEN_WINDOW]),
        }
    }

    /// Records an arrival, reporting whether it is the first of its number.
    fn arrived(&mut self, sequence: SequenceNumber) -> bool {
        let slot = &mut self.slots[usize::from(sequence.0) % SEEN_WINDOW];
        if *slot == Some(sequence) {
            return false;
        }
        *slot = Some(sequence);
        true
    }
}

/// An ordered, bounded jitter buffer for one Opus stream.
pub struct JitterBuffer {
    config: CodecConfig,
    /// Per-channel samples in one frame, which is also the RTP tick step
    /// between consecutive frames of this stream.
    frame_samples: u32,
    frame_period: Nanos,
    target_frames: usize,
    ceiling_frames: usize,
    slots: Box<[Slot]>,
    occupied: usize,
    anchor: Option<Anchor>,
    /// The next playout position, in the stream's own clock. This is the whole
    /// deadline mechanism: a frame is due when the cursor reaches it, the
    /// cursor advances exactly one frame per pull, and the sink pulls on the
    /// frame period. A timestamp behind the cursor is a timestamp whose moment
    /// has gone.
    cursor: RtpTimestamp,
    highest_sequence: Option<SequenceNumber>,
    highest_timestamp: Option<RtpTimestamp>,
    seen: SeenWindow,
    counts: Counts,
}

impl JitterBuffer {
    /// Builds a buffer for `config`, aiming to hold `target` of audio.
    ///
    /// The target is quantised to whole frames, because whole frames are the
    /// only thing the buffer can hold; [`JitterBuffer::target`] reports what it
    /// became. A request smaller than one frame becomes one frame rather than
    /// zero: a buffer with no target is not a jitter buffer, it is a queue that
    /// underruns on the first packet that is a microsecond late.
    pub fn new(config: CodecConfig, target: Nanos) -> JitterBuffer {
        let frame_period = Nanos(u64::from(config.frame.millis()) * 1_000_000);
        let target_frames = ((target.get() + frame_period.get() / 2) / frame_period.get()).max(1);
        let target_frames = target_frames as usize;

        // The ceiling: three times the target, and never less than the target
        // plus four frames.
        //
        // Three times, because the buffer has to absorb a fault comfortably
        // larger than its target without discarding anything — a ceiling close
        // to the target would fire on ordinary jitter, and then the discard
        // counter would be measuring the ceiling rather than the network — and
        // because audio held beyond three times the target is latency that will
        // never be paid back, since a continuous stream offers no quiet moment
        // in which to catch up. The floor of four frames keeps a small target
        // from producing a ceiling too tight for a single reordered frame and
        // its neighbours, which would turn a normal event into a discard.
        let ceiling_frames = (3 * target_frames).max(target_frames + 4);

        // Two slots more than the ceiling. The two bounds are separate on
        // purpose: the time bound is the one that decides policy, and it can
        // only do so if a frame can be admitted and then judged rather than
        // refused for want of somewhere to put it.
        let slots = vec![
            Slot {
                timestamp: RtpTimestamp(0),
                len: 0,
                bytes: [0; MAX_OPUS_PAYLOAD],
            };
            ceiling_frames + 2
        ]
        .into_boxed_slice();

        JitterBuffer {
            config,
            frame_samples: config.frame_samples() as u32,
            frame_period,
            target_frames,
            ceiling_frames,
            slots,
            occupied: 0,
            anchor: None,
            cursor: RtpTimestamp(0),
            highest_sequence: None,
            highest_timestamp: None,
            seen: SeenWindow::new(),
            counts: Counts::default(),
        }
    }

    pub fn config(&self) -> &CodecConfig {
        &self.config
    }

    /// The occupancy the buffer aims to hold, after quantisation to frames.
    pub fn target(&self) -> Nanos {
        Nanos(self.frame_period.get() * self.target_frames as u64)
    }

    /// The occupancy above which the buffer skips forward.
    pub fn ceiling(&self) -> Nanos {
        Nanos(self.frame_period.get() * self.ceiling_frames as u64)
    }

    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    pub fn ceiling_frames(&self) -> usize {
        self.ceiling_frames
    }

    /// Slots decided at construction, which is the buffer's other bound.
    pub fn slots(&self) -> usize {
        self.slots.len()
    }

    /// Frames queued right now, counting the one due next.
    ///
    /// This is what the two bounds are applied to. It differs by that one frame
    /// from the occupancy a pull reports, which excludes the frame it just
    /// served because that frame is being played rather than held back.
    pub fn occupancy(&self) -> usize {
        self.occupied
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn frame_samples(&self) -> u32 {
        self.frame_samples
    }

    pub fn frame_period(&self) -> Nanos {
        self.frame_period
    }

    /// When the first frame of the stream is due, once a first packet has
    /// arrived to anchor it. The sink has nothing to do before this.
    pub fn playout_start(&self) -> Option<Timestamp> {
        self.anchor.map(|anchor| anchor.playout)
    }

    /// Whether every frame the buffer has ever seen has now been played,
    /// concealed or discarded.
    ///
    /// The condition for stopping a run: with the sender finished, an empty
    /// buffer whose cursor has passed the furthest timestamp received means
    /// there is nothing left that could still be played, and every further pull
    /// would be an underrun the path did not cause.
    pub fn drained(&self) -> bool {
        match self.highest_timestamp {
            Some(highest) => self.occupied == 0 && self.cursor.distance_from(highest) > 0,
            None => false,
        }
    }

    /// Offers one arriving frame to the buffer.
    ///
    /// `at` is used only when it is the first packet accepted, to fix the
    /// anchor. No later arrival time influences any deadline.
    pub fn push(&mut self, packet: &OpusPacket<'_>, at: Timestamp) -> Admission {
        self.counts.received += 1;

        if packet.payload.len() > MAX_OPUS_PAYLOAD {
            self.counts.oversize += 1;
            return Admission::Oversize;
        }
        if !self.seen.arrived(packet.sequence) {
            self.counts.duplicate += 1;
            return Admission::Duplicate;
        }

        let anchor = match self.anchor {
            Some(anchor) => anchor,
            None => {
                // The stream starts here. Playout of this very frame is one
                // target away, which is the only place the target ever enters a
                // deadline: every later frame inherits it through the sample
                // counter.
                let anchor = Anchor {
                    rtp: packet.timestamp,
                    playout: at.add(self.target()),
                };
                self.anchor = Some(anchor);
                self.cursor = packet.timestamp;
                anchor
            }
        };

        let from_anchor = packet.timestamp.distance_from(anchor.rtp);
        if from_anchor.rem_euclid(i64::from(self.frame_samples)) != 0 {
            self.counts.off_grid += 1;
            return Admission::OffGrid;
        }

        let reordered = match self.highest_sequence {
            Some(highest) if packet.sequence.distance_from(highest) < 0 => true,
            _ => {
                self.highest_sequence = Some(packet.sequence);
                false
            }
        };
        if reordered {
            self.counts.reordered += 1;
        }

        // Behind the cursor is behind the deadline. The frame was concealed
        // when its moment came and the stream has moved on; playing it now
        // would either repeat a moment the listener has had or push everything
        // behind it back by a frame for the rest of the run.
        if packet.timestamp.distance_from(self.cursor) < 0 {
            self.counts.late += 1;
            return Admission::Late;
        }

        match self.highest_timestamp {
            Some(highest) if highest.distance_from(packet.timestamp) >= 0 => {}
            _ => self.highest_timestamp = Some(packet.timestamp),
        }

        if self.occupied == self.slots.len() {
            // The slot bound, reached only if the time bound somehow did not
            // fire first. Same remedy and same counters, because it is the same
            // condition seen through the other bound.
            self.skip_oldest();
            self.counts.overruns += 1;
        }
        self.store(packet.timestamp, packet.payload);
        self.hold_ceiling();
        Admission::Queued { reordered }
    }

    /// Serves the sink one frame period of audio.
    ///
    /// The payload of a real frame is copied into `into`, which must hold
    /// [`MAX_OPUS_PAYLOAD`]; the caller decodes outside whatever lock it holds
    /// this buffer under, so the buffer is never occupied for the length of a
    /// decode.
    pub fn pull(&mut self, into: &mut [u8]) -> Pulled {
        let held = self.occupied;
        let outcome = match self.take(self.cursor) {
            Some((offset, len)) => {
                into[..len].copy_from_slice(&self.slots[offset].bytes[..len]);
                self.counts.played += 1;
                self.counts.played_samples += u64::from(self.frame_samples);
                Pull::Frame(len)
            }
            None if held > 0 => {
                // The frame due now is missing, but the stream is still
                // arriving: a hole to bridge, not a stream that stopped.
                self.counts.concealed += 1;
                self.counts.played_samples += u64::from(self.frame_samples);
                Pull::Conceal
            }
            None => {
                self.counts.concealed += 1;
                self.counts.underruns += 1;
                Pull::Underrun
            }
        };

        self.cursor = RtpTimestamp(self.cursor.0.wrapping_add(self.frame_samples));
        self.counts.expected_samples += u64::from(self.frame_samples);
        Pulled {
            outcome,
            occupancy: self.occupied,
        }
    }

    /// Brings occupancy back under the ceiling by skipping forward.
    ///
    /// It skips all the way down to the target rather than to the ceiling. A
    /// buffer trimmed to its ceiling sits one frame away from breaching again,
    /// so a sink that is slightly too slow would discard a frame every few
    /// periods for the rest of the run — a permanent stutter, which is worse
    /// than one discontinuity that puts the latency back where it belongs.
    fn hold_ceiling(&mut self) {
        if self.occupied <= self.ceiling_frames {
            return;
        }
        self.counts.overruns += 1;
        while self.occupied > self.target_frames {
            self.skip_oldest();
        }
    }

    /// Gives up the oldest queued frame and moves the playout position past it.
    ///
    /// Both halves are necessary. Freeing the slot alone would reduce the count
    /// and not the latency, because the sink would still spend a frame period
    /// concealing the position it just emptied; only moving the cursor actually
    /// brings the stream forward.
    fn skip_oldest(&mut self) {
        let Some(oldest) = self.oldest() else {
            return;
        };
        let timestamp = self.slots[oldest].timestamp;
        self.slots[oldest].len = 0;
        self.occupied -= 1;
        self.counts.overrun_frames += 1;

        // The playout position moves past the frame given up and past every
        // position between here and there: those are holes the sink would
        // otherwise have concealed one period at a time, at the cost of the
        // latency this skip exists to recover. Expected samples move with it,
        // by exactly the ticks the cursor travelled.
        let travelled =
            timestamp.distance_from(self.cursor).max(0) as u64 + u64::from(self.frame_samples);
        self.cursor = RtpTimestamp(timestamp.0.wrapping_add(self.frame_samples));
        self.counts.expected_samples += travelled;
    }

    fn oldest(&self) -> Option<usize> {
        let mut best: Option<(usize, i64)> = None;
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.len == 0 {
                continue;
            }
            let distance = slot.timestamp.distance_from(self.cursor);
            if best.is_none_or(|(_, held)| distance < held) {
                best = Some((index, distance));
            }
        }
        best.map(|(index, _)| index)
    }

    fn store(&mut self, timestamp: RtpTimestamp, payload: &[u8]) {
        let free = self
            .slots
            .iter()
            .position(|slot| slot.len == 0)
            .expect("a slot was freed before storing");
        let slot = &mut self.slots[free];
        slot.timestamp = timestamp;
        slot.len = payload.len();
        slot.bytes[..payload.len()].copy_from_slice(payload);
        self.occupied += 1;
    }

    /// Frees the slot holding `timestamp`, returning where it was and how long
    /// its payload is.
    fn take(&mut self, timestamp: RtpTimestamp) -> Option<(usize, usize)> {
        let found = self
            .slots
            .iter()
            .position(|slot| slot.len != 0 && slot.timestamp == timestamp)?;
        let len = self.slots[found].len;
        self.slots[found].len = 0;
        self.occupied -= 1;
        Some((found, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FrameDuration;
    use lanplay_transport::{OpusPacketizer, Ssrc, parse_opus_packet};

    fn config() -> CodecConfig {
        CodecConfig::contract(FrameDuration::Ms5, CodecConfig::DEFAULT_BITRATE_BPS)
    }

    /// A stream of datagrams with the contract's own shape: one frame each,
    /// sequence and sample counter advancing exactly as the packetiser does it.
    /// The payloads are not real Opus, because nothing under test decodes them
    /// — what the buffer orders is bytes and a timestamp.
    struct Stream {
        datagrams: Vec<Vec<u8>>,
    }

    impl Stream {
        fn new(count: usize) -> Stream {
            let mut packetizer = OpusPacketizer::with_start(
                Ssrc(0x0au32),
                SequenceNumber(1_000),
                RtpTimestamp(500_000),
            );
            let samples = config().frame_samples() as u32;
            let datagrams = (0..count)
                .map(|index| {
                    // A payload that names its own frame, so a test can prove
                    // which frame the sink was handed.
                    let payload = [0xa0u8, index as u8, (index >> 8) as u8];
                    packetizer
                        .next(&payload, samples)
                        .expect("a three byte frame packetises")
                        .to_vec()
                })
                .collect();
            Stream { datagrams }
        }

        fn datagram(&self, index: usize) -> &[u8] {
            &self.datagrams[index]
        }
    }

    /// Pushes datagram `index` of `stream` at `at`.
    fn push(buffer: &mut JitterBuffer, stream: &Stream, index: usize, at: Timestamp) -> Admission {
        let packet = parse_opus_packet(stream.datagram(index)).expect("a packet it just wrote");
        buffer.push(&packet, at)
    }

    /// Pulls one frame and reports which frame index it was, or how it was
    /// concealed.
    fn pull(buffer: &mut JitterBuffer) -> Result<usize, Pull> {
        let mut into = [0u8; MAX_OPUS_PAYLOAD];
        let pulled = buffer.pull(&mut into);
        match pulled.outcome {
            Pull::Frame(len) => {
                assert_eq!(len, 3, "the test stream writes three byte payloads");
                assert_eq!(into[0], 0xa0);
                Ok(usize::from(into[1]) | usize::from(into[2]) << 8)
            }
            other => Err(other),
        }
    }

    fn buffer(target_ms: u64) -> JitterBuffer {
        JitterBuffer::new(config(), Nanos::from_millis(target_ms))
    }

    fn at() -> Timestamp {
        Timestamp::from_nanos(1_000_000_000)
    }

    #[test]
    fn the_default_target_is_two_frames_and_the_ceiling_is_six() {
        // The plan's baseline, stated so that a change to either constant has
        // to be a change to this line as well.
        let buffer = buffer(10);
        assert_eq!(buffer.target_frames(), 2);
        assert_eq!(buffer.target(), Nanos::from_millis(10));
        assert_eq!(buffer.ceiling_frames(), 6);
        assert_eq!(buffer.ceiling(), Nanos::from_millis(30));
        assert!(buffer.slots() > buffer.ceiling_frames());
    }

    #[test]
    fn a_stream_arriving_on_time_holds_exactly_the_target() {
        // What the occupancy figures in a healthy run should read. A frame
        // arrives, the sink pulls, and what is left over is the audio the
        // buffer is holding back — which is the target and nothing more, or the
        // run is quietly costing the listener latency nobody asked for.
        let stream = Stream::new(60);
        let mut buffer = buffer(10);
        let mut into = [0u8; MAX_OPUS_PAYLOAD];

        // The frames that arrive during the target, before playout begins.
        for index in 0..3 {
            push(&mut buffer, &stream, index, at());
        }
        for index in 3..60 {
            let pulled = buffer.pull(&mut into);
            assert!(matches!(pulled.outcome, Pull::Frame(_)));
            assert_eq!(
                pulled.occupancy,
                buffer.target_frames(),
                "held {} frames against a target of {}",
                pulled.occupancy,
                buffer.target_frames()
            );
            push(&mut buffer, &stream, index, at());
        }
    }

    #[test]
    fn a_frame_that_misses_its_deadline_is_concealed_and_the_stream_continues_in_order() {
        // The contract clause that separates audio from video: the gap is
        // filled and the stream carries on from where it was, rather than
        // jumping to whatever is newest.
        let stream = Stream::new(4);
        let mut buffer = buffer(10);
        for index in [0, 1, 3] {
            assert_eq!(
                push(&mut buffer, &stream, index, at()),
                Admission::Queued { reordered: false }
            );
        }

        assert_eq!(pull(&mut buffer), Ok(0));
        assert_eq!(pull(&mut buffer), Ok(1));
        assert_eq!(pull(&mut buffer), Err(Pull::Conceal));
        assert_eq!(pull(&mut buffer), Ok(3));

        let counts = buffer.counts();
        assert_eq!(counts.played, 3);
        assert_eq!(counts.concealed, 1);
        // A gap with audio on both sides of it is not an underrun: the buffer
        // still held frame three while frame two was being concealed.
        assert_eq!(counts.underruns, 0);
    }

    #[test]
    fn a_frame_arriving_after_its_concealment_is_discarded_and_counted_late() {
        let stream = Stream::new(3);
        let mut buffer = buffer(10);
        push(&mut buffer, &stream, 0, at());
        push(&mut buffer, &stream, 2, at());

        assert_eq!(pull(&mut buffer), Ok(0));
        assert_eq!(pull(&mut buffer), Err(Pull::Conceal));

        // Frame one turns up now, one period after the moment it described.
        assert_eq!(push(&mut buffer, &stream, 1, at()), Admission::Late);
        assert_eq!(buffer.counts().late, 1);

        // And it is never played: the next pull is frame two, in order.
        assert_eq!(pull(&mut buffer), Ok(2));
        assert_eq!(buffer.counts().played, 2);
    }

    #[test]
    fn a_duplicate_changes_nothing() {
        let stream = Stream::new(2);
        let mut buffer = buffer(10);
        push(&mut buffer, &stream, 0, at());
        push(&mut buffer, &stream, 1, at());
        let before = buffer.occupancy();

        assert_eq!(push(&mut buffer, &stream, 1, at()), Admission::Duplicate);
        assert_eq!(buffer.occupancy(), before);

        let counts = buffer.counts();
        assert_eq!(counts.duplicate, 1);
        assert_eq!(counts.late, 0);
        assert_eq!(counts.reordered, 0);
        assert_eq!(counts.overrun_frames, 0);

        // And the stream plays exactly once through, not twice.
        assert_eq!(pull(&mut buffer), Ok(0));
        assert_eq!(pull(&mut buffer), Ok(1));
        assert_eq!(pull(&mut buffer), Err(Pull::Underrun));
    }

    #[test]
    fn reordering_inside_the_window_decodes_normally() {
        // Arrival order 0, 2, 1, 3 — all within the target. Every frame is
        // played, in stream order, and nothing is late or concealed. The
        // counter moves because the buffer noticed, not because anything is
        // wrong.
        let stream = Stream::new(4);
        let mut buffer = buffer(10);
        for index in [0, 2, 1, 3] {
            let admission = push(&mut buffer, &stream, index, at());
            assert_eq!(
                admission,
                Admission::Queued {
                    reordered: index == 1
                }
            );
        }

        for expected in 0..4 {
            assert_eq!(pull(&mut buffer), Ok(expected));
        }
        let counts = buffer.counts();
        assert_eq!(counts.reordered, 1);
        assert_eq!(counts.played, 4);
        assert_eq!(counts.concealed, 0);
        assert_eq!(counts.late, 0);
        assert_eq!(counts.continuity_hole(), 0);
    }

    #[test]
    fn a_burst_beyond_the_ceiling_is_bounded_by_discarding_and_occupancy_returns_to_the_target() {
        // A stall releasing everything it held at once, with the sink not
        // having pulled: without a ceiling the buffer would keep all of it and
        // owe the listener that much latency for the rest of the run.
        let stream = Stream::new(12);
        let mut buffer = buffer(10);
        for index in 0..12 {
            push(&mut buffer, &stream, index, at());
        }

        let counts = buffer.counts();
        assert!(counts.overruns > 0, "the ceiling never fired");
        assert_eq!(counts.overrun_frames, 10, "{counts:?}");
        assert_eq!(
            buffer.occupancy(),
            buffer.target_frames(),
            "occupancy did not come back to the target"
        );

        // What is left is the newest audio, and it plays in order.
        assert_eq!(pull(&mut buffer), Ok(10));
        assert_eq!(pull(&mut buffer), Ok(11));
    }

    #[test]
    fn the_ceiling_holds_however_long_the_burst_is() {
        // The property, rather than one arithmetic case: occupancy never
        // exceeds the ceiling however much arrives between two pulls.
        let stream = Stream::new(400);
        let mut buffer = buffer(10);
        for index in 0..400 {
            push(&mut buffer, &stream, index, at());
            assert!(
                buffer.occupancy() <= buffer.ceiling_frames(),
                "occupancy {} passed the ceiling {}",
                buffer.occupancy(),
                buffer.ceiling_frames()
            );
        }
        // Four hundred frames of audio arrived while the sink was not looking;
        // the buffer kept a target's worth of the newest and gave up the rest,
        // which is the whole of what bounding it in time means.
        assert!(
            buffer.counts().overrun_frames > 380,
            "{:?}",
            buffer.counts()
        );
    }

    #[test]
    fn an_underrun_hands_the_sink_concealment_rather_than_nothing() {
        // Nothing has ever arrived beyond the first frame. The sink still gets
        // a frame period of audio every time it asks, because a render callback
        // handed no samples produces a click.
        let stream = Stream::new(1);
        let mut buffer = buffer(10);
        push(&mut buffer, &stream, 0, at());
        assert_eq!(pull(&mut buffer), Ok(0));

        for _ in 0..5 {
            assert_eq!(pull(&mut buffer), Err(Pull::Underrun));
        }
        let counts = buffer.counts();
        assert_eq!(counts.underruns, 5);
        // Every underrun is also a frame the concealer produced: the sink was
        // never handed nothing.
        assert_eq!(counts.concealed, 5);
    }

    #[test]
    fn continuity_counts_a_concealed_frame_as_played_and_an_underrun_as_a_hole() {
        let stream = Stream::new(4);
        let mut buffer = buffer(10);
        let frame = u64::from(buffer.frame_samples());

        // A gap with the stream still running: concealed, and continuous.
        for index in [0, 1, 3] {
            push(&mut buffer, &stream, index, at());
        }
        for _ in 0..4 {
            let _ = pull(&mut buffer);
        }
        let bridged = buffer.counts();
        assert_eq!(bridged.expected_samples, 4 * frame);
        assert_eq!(bridged.played_samples, 4 * frame);
        assert_eq!(bridged.continuity_hole(), 0);

        // Two periods with nothing at all behind them: the stream did not
        // produce that audio, and the account says so.
        assert_eq!(pull(&mut buffer), Err(Pull::Underrun));
        assert_eq!(pull(&mut buffer), Err(Pull::Underrun));
        let starved = buffer.counts();
        assert_eq!(starved.expected_samples, 6 * frame);
        assert_eq!(starved.played_samples, 4 * frame);
        assert_eq!(starved.continuity_hole(), 2 * frame);
    }

    #[test]
    fn frames_given_up_to_hold_the_ceiling_are_holes_too() {
        // Not asked for by name, but it follows from the same definition and
        // would otherwise be the one way a frame can vanish without the
        // continuity counter noticing.
        let stream = Stream::new(12);
        let mut buffer = buffer(10);
        for index in 0..12 {
            push(&mut buffer, &stream, index, at());
        }
        assert_eq!(pull(&mut buffer), Ok(10));
        assert_eq!(pull(&mut buffer), Ok(11));

        let counts = buffer.counts();
        let frame = u64::from(buffer.frame_samples());
        assert_eq!(counts.expected_samples, 12 * frame);
        assert_eq!(counts.played_samples, 2 * frame);
        assert_eq!(counts.continuity_hole(), 10 * frame);
    }

    #[test]
    fn the_deadline_comes_from_the_timestamp_and_not_from_the_arrival() {
        // Two buffers fed the same stream, one with every arrival an eternity
        // apart and one with them all at once. The playout order and the
        // playout start are identical, because neither is a function of when a
        // datagram turned up.
        let stream = Stream::new(3);
        let mut steady = buffer(10);
        let mut jittery = buffer(10);
        for index in 0..3 {
            push(&mut steady, &stream, index, at());
            push(
                &mut jittery,
                &stream,
                index,
                at().add(Nanos::from_millis(37 * index as u64)),
            );
        }

        assert_eq!(steady.playout_start(), jittery.playout_start());
        for expected in 0..3 {
            assert_eq!(pull(&mut steady), Ok(expected));
            assert_eq!(pull(&mut jittery), Ok(expected));
        }
        assert_eq!(steady.counts(), jittery.counts());
    }

    #[test]
    fn playout_starts_one_target_after_the_first_arrival() {
        let stream = Stream::new(1);
        let mut buffer = buffer(10);
        push(&mut buffer, &stream, 0, at());
        assert_eq!(
            buffer.playout_start(),
            Some(at().add(Nanos::from_millis(10)))
        );
    }

    #[test]
    fn a_timestamp_off_the_frame_grid_is_named_rather_than_silently_concealed() {
        // A sender at another frame duration. Every one of its frames would sit
        // between two playout positions and never be played, and the run would
        // look like total loss unless the cause has a name.
        let stream = Stream::new(2);
        let mut buffer = buffer(10);
        push(&mut buffer, &stream, 0, at());

        let mut packetizer = OpusPacketizer::with_start(
            Ssrc(0x0au32),
            SequenceNumber(9_000),
            RtpTimestamp(500_000 + 120),
        );
        let datagram = packetizer
            .next(&[1, 2, 3], 240)
            .expect("packetises")
            .to_vec();
        let packet = parse_opus_packet(&datagram).expect("valid");
        assert_eq!(buffer.push(&packet, at()), Admission::OffGrid);
        assert_eq!(buffer.counts().off_grid, 1);
    }

    #[test]
    fn the_buffer_drains_only_once_everything_seen_has_gone_past() {
        let stream = Stream::new(2);
        let mut buffer = buffer(10);
        assert!(!buffer.drained(), "nothing has arrived to drain");
        push(&mut buffer, &stream, 0, at());
        push(&mut buffer, &stream, 1, at());
        assert!(!buffer.drained());
        assert_eq!(pull(&mut buffer), Ok(0));
        assert!(!buffer.drained());
        assert_eq!(pull(&mut buffer), Ok(1));
        assert!(buffer.drained());
    }
}
