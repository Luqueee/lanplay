//! The whole receiving half, from a datagram to a speaker: UDP in, RTP
//! ordering, a bounded jitter buffer, Opus decode with concealment, a bounded
//! PCM ring, and the HAL's own callback taking frames out of it.
//!
//! Every part of this was measured on its own first, and none of it is new
//! here. The packetiser and its parser are `lanplay-transport`'s, the buffer
//! and the decoder are `lanplay-audio-codec`'s, the ring and the callback are
//! this crate's, and the deadline and the clock are `lanplay-telemetry`'s. What
//! is new is the join, and the join is where the interesting failures live:
//! three threads with three different notions of when now is, and one of them
//! is a crystal on the far side of a radio.
//!
//! # Three clocks, and what stands between them
//!
//! The sender's audio clock decides when frames exist. This machine's
//! monotonic clock decides when the producer wakes. The output device's crystal
//! decides when audio leaves. All three run at nominally 48000 Hz and none of
//! them agrees: the host's endpoint was measured at −15 ppm and this Mac's
//! output at +5 ppm, which is 20 ppm apart, or about 12 ms of drift over ten
//! minutes.
//!
//! Nothing here corrects for that, deliberately. Rate matching is A7's subject
//! and this phase has to measure the drift rather than hide it, so the two
//! buffers absorb what they can and the counters report what they could not.
//!
//! ```text
//! socket -> jitter buffer -> producer -> PCM ring -> IOProc -> device
//!            (stream time)   (this Mac's clock)      (the device's crystal)
//! ```
//!
//! The jitter buffer absorbs delivery jitter between the first two, on
//! deadlines derived from RTP timestamps and never from arrival times. The ring
//! absorbs the phase difference between the last two, which is real and
//! permanent even with perfect clocks: the producer deposits 240 frames every
//! 5 ms and the device takes 256 frames every 5.333 ms, so occupancy sawtooths
//! by up to a frame and a buffer whatever else is happening.
//!
//! # The ring's prime, and where it actually settles
//!
//! The device is not started until the producer has put `ring_prime_frames`
//! into the ring. That is latency, paid once and stated, and without it the
//! first callback would arrive at an empty ring and every callback after it
//! would find the ring in the same state, because a producer that cannot run
//! faster than the stream can never catch up. Starting the device first and
//! letting it underrun until audio exists is the only alternative, and it
//! trades a bounded, silent, one-off latency for callbacks of audible silence.
//!
//! The prime is aimed at one IO buffer plus one Opus frame, which is the floor
//! rather than the resting place. `AudioDeviceStart` takes time to get the
//! device going and the producer keeps depositing throughout, so the ring
//! settles at the prime plus whatever that took - eight milliseconds on this
//! machine, and a property of the device rather than of this code. What that
//! costs is reported as the settled occupancy, which is the number to read as
//! the ring's contribution to end-to-end latency; the prime is only where it
//! started from.
//!
//! The rest of the ring is headroom, and the two directions are not
//! interchangeable. The tone probe next door holds its ring nearly full,
//! because its producer is demand-driven and can top up whenever it notices
//! room, so the useful margin is entirely below. This producer has no such
//! freedom: its rate is the sender's rate, it cannot produce a frame early,
//! and its occupancy walks from wherever it starts. A walk needs room in both
//! directions, and the direction it walks is the 20 ppm this phase is
//! forbidden to correct.
//!
//! # What the counters mean, and the one that decides the phase
//!
//! Continuity is the whole point, and it is stated in samples rather than in
//! frames or packets because samples are what a listener loses. See
//! [`Continuity`] for the arithmetic and for why an underrun and a concealment
//! are counted differently.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lanplay_audio_capture::{Percentiles, Samples, ToneReport, analyse};
use lanplay_audio_codec::config::CodecConfig;
use lanplay_audio_codec::decoder::OpusDecoder;
use lanplay_audio_codec::error::CodecError;
use lanplay_audio_codec::jitter::{Admission, Counts, JitterBuffer, Pull};
use lanplay_audio_codec::probe::{ANALYSIS_FRAMES, ANALYSIS_SKIP_FRAMES, decoded_format};
use lanplay_telemetry::{Nanos, ScheduledAs, Timestamp, wait_until};
use lanplay_transport::{
    MAX_OPUS_PAYLOAD, MAX_UDP_PAYLOAD, RtpTimestamp, SequenceNumber, Ssrc, parse_opus_packet,
};
use objc2_core_audio::{AudioConvertHostTimeToNanos, AudioGetCurrentHostTime};
use parking_lot::Mutex;

use crate::device::{self, Error as DeviceError};
use crate::format::OutputFormat;
use crate::occupancy::WindowOccupancy;
use crate::ring::PcmRing;
use crate::stream::{RenderState, Stream};

/// How long the receive loop blocks before asking whether it should stop.
const RECV_TIMEOUT: Duration = Duration::from_millis(100);

/// How long the producer waits for the ring to be primed before giving up.
///
/// Generous against the frames it needs — half a ring is a few milliseconds of
/// pulling — because a prime that has not completed in a second means the
/// stream stopped, and a run should say that rather than push it into the first
/// callback.
const PRIME_TIMEOUT: Duration = Duration::from_secs(2);

/// Shortest nap anything in here takes while waiting on another thread.
const MINIMUM_NAP: Duration = Duration::from_micros(250);

/// What the run was asked to do.
#[derive(Clone, Debug)]
pub struct ReceiveOptions {
    pub bind: SocketAddr,
    /// Which output device to render through, by the name CoreAudio gives it,
    /// or the system default when nothing is named. A gate names one: the
    /// default is a system-wide setting that changes when a pair of headphones
    /// reconnects, and a ten-minute measurement that depends on it discovers so
    /// after it has started.
    pub device: Option<String>,
    /// Seconds of audio to account for, counted from the first datagram rather
    /// than from process start. A window that opened before the stream existed
    /// would spend its first seconds measuring the silence before the run, and
    /// the harness that starts this end cannot know when the far end will
    /// appear.
    pub seconds: f64,
    pub config: CodecConfig,
    /// Audio the jitter buffer aims to hold. Quantised to whole frames.
    pub target: Nanos,
    /// Frames per IO cycle to ask the device for. A request, not a setting.
    pub buffer_frames: u32,
    /// Ring capacity as a multiple of the granted IO buffer.
    pub ring_multiple: u32,
    /// How long to wait for a first datagram before declaring that nothing
    /// arrived.
    pub first_packet_wait: Duration,
    /// How often to close a counter window. Ten seconds is what the plan asks
    /// for: a figure over the whole run cannot show a fault that started
    /// halfway through it.
    pub window: Duration,
}

#[derive(Debug)]
pub enum ReceiveError {
    Device(DeviceError),
    Codec(CodecError),
    Io {
        call: &'static str,
        error: io::Error,
    },
    /// The device cannot be fed this stream without a converter, which this
    /// phase must not quietly insert.
    Mismatch(String),
}

impl core::fmt::Display for ReceiveError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReceiveError::Device(error) => write!(f, "{error}"),
            ReceiveError::Codec(error) => write!(f, "{error}"),
            ReceiveError::Io { call, error } => write!(f, "{call} failed: {error}"),
            ReceiveError::Mismatch(why) => write!(f, "{why}"),
        }
    }
}

impl core::error::Error for ReceiveError {}

impl From<DeviceError> for ReceiveError {
    fn from(error: DeviceError) -> Self {
        ReceiveError::Device(error)
    }
}

impl From<CodecError> for ReceiveError {
    fn from(error: CodecError) -> Self {
        ReceiveError::Codec(error)
    }
}

fn io(call: &'static str) -> impl FnOnce(io::Error) -> ReceiveError {
    move |error| ReceiveError::Io { call, error }
}

/// Loss, as the sequence numbers themselves state it.
///
/// A within-stream figure and never a difference of two machines' counts. The
/// sender's total and this receiver's total are taken over two spans on two
/// clocks that do not share an epoch, and subtracting them is the same defect
/// that produced a 150 ppm reading elsewhere in this project wearing different
/// clothes. What is subtracted here is the number of distinct sequence numbers
/// that arrived from the span the sequence numbers themselves describe.
#[derive(Clone, Copy, Default, Debug)]
pub struct Loss {
    base: Option<SequenceNumber>,
    /// The furthest sequence number seen, carried into a space wider than sixteen bits.
    ///
    /// A sixteen-bit distance can only say where a packet sits within half the sequence
    /// space, so `distance_from` turns negative once the stream is more than 32767 packets
    /// past `base` and a span taken from it saturates at 32768. At the wire's 200 packets a
    /// second that is 163.8 seconds: every sixty-second arm was right and every ten-minute
    /// arm reported 32768 expected against the 120000 it actually sent. Two runs on two
    /// different radios printing the same denominator to the digit is what gave it away.
    ///
    /// Counting the wraps is what RFC 3550 does for exactly this, and the count belongs here
    /// rather than at the call site because a receiver that has to remember to widen a
    /// number it was handed will one day forget.
    cycles: u64,
    highest: Option<SequenceNumber>,
    span: u64,
    unique: u64,
}

impl Loss {
    /// Records one arrival that was not a duplicate.
    pub fn arrived(&mut self, sequence: SequenceNumber) {
        self.unique += 1;
        let (Some(base), Some(highest)) = (self.base, self.highest) else {
            self.base = Some(sequence);
            self.highest = Some(sequence);
            self.span = 1;
            return;
        };
        // Forward of the furthest seen: advance, and count a wrap when the raw number went
        // backwards while the distance says forward. A reordered packet is behind the
        // furthest and moves nothing, which is what keeps a late arrival from inventing a
        // cycle.
        let step = sequence.distance_from(highest);
        if step > 0 {
            if sequence.0 < highest.0 {
                self.cycles += u64::from(u16::MAX) + 1;
            }
            self.highest = Some(sequence);
            self.span = (self.cycles + u64::from(sequence.0)).saturating_sub(u64::from(base.0)) + 1;
        }
    }

    /// Packets the numbering says should have been in the span.
    pub fn expected(&self) -> u64 {
        self.span
    }

    pub fn unique(&self) -> u64 {
        self.unique
    }

    /// Packets inside the span that never turned up.
    ///
    /// Saturating rather than signed: more unique numbers than the span holds
    /// is arithmetically impossible, and a negative loss printed as evidence of
    /// it would be less use than a zero and a count that does not add up.
    pub fn lost(&self) -> u64 {
        self.span.saturating_sub(self.unique)
    }
}

/// The continuity accounting, which is what decides this phase.
///
/// # How it is computed
///
/// `expected` is the jitter buffer's `expected_samples`: per-channel samples
/// the playout cursor travelled, one frame's worth per pull, plus every
/// position it skipped over to hold its ceiling. It is the length of the
/// stream as the stream itself describes it, and nothing about this machine's
/// behaviour can shorten it.
///
/// `played` is the buffer's `played_samples` less the samples the ring refused.
/// The buffer credits a decoded frame and a gap concealment alike and credits
/// neither an underrun nor a frame it skipped; the ring's refusals are then
/// taken off, because a frame the producer generated and the ring had no room
/// for reached nobody.
///
/// `hole` is the difference, and zero is the only good value.
///
/// # Why an underrun and a concealment are counted differently
///
/// Both hand the sink samples nobody sent, and there the resemblance stops.
///
/// A concealment bridges a gap with real audio on both sides of it. The
/// concealer extrapolates from the frames that did arrive, the waveform
/// continues, and a listener hears a few milliseconds of slightly wrong audio
/// rather than a click. The stream was carried across that moment, so the
/// moment counts as played.
///
/// An underrun has nothing behind it. The buffer was empty, so the concealer is
/// running on stale state and inventing, and what comes out decays towards
/// silence however long it goes on. Crediting it would make the two runs this
/// phase most needs to tell apart identical in the counters: a path carrying
/// audio, and a path carrying nothing at all while a concealer keeps a sink
/// alive. That confusion is exactly how a run with zero underruns can have
/// carried nothing, which is the sentence the plan puts the whole phase on.
///
/// The same distinction is why the device's own shortfall is reported beside
/// this rather than inside it. A callback the ring could not fill got silence,
/// which is audible and is counted as [`Render::underruns`] — but it is counted
/// in the device's cycles on the device's clock, and folding a count taken on
/// one crystal into a count taken on another is how a subtraction stops meaning
/// anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Continuity {
    pub expected: u64,
    pub played: u64,
}

impl Continuity {
    /// What the buffer and the ring together made of one run.
    pub fn of(counts: Counts, refused_frames: u64) -> Continuity {
        Continuity {
            expected: counts.expected_samples,
            played: counts.played_samples.saturating_sub(refused_frames),
        }
    }

    pub fn hole(&self) -> u64 {
        self.expected.saturating_sub(self.played)
    }

    pub fn unbroken(&self) -> bool {
        self.expected > 0 && self.hole() == 0
    }
}

/// What the producer thread did and what the system let it do.
#[derive(Debug)]
pub struct Producer {
    /// Ahead of every counter, because a counter means one thing under a
    /// deadline and another without one.
    pub scheduled_as: ScheduledAs,
    pub pulls: u64,
    /// Interval between consecutive pulls, as achieved rather than as asked
    /// for. A producer that cannot keep its own cadence is measuring itself.
    pub interval_us: Option<Percentiles>,
    /// Occupancy the buffer reported after serving each pull, in microseconds
    /// of audio.
    pub occupancy_us: Option<Percentiles>,
    /// Time in `opus_decode_float` for frames that arrived, concealment
    /// excluded so that a lossy run's decode figure still describes decoding.
    pub decode_us: Option<Percentiles>,
    pub conceal_us: Option<Percentiles>,
    pub decode_failures: u64,
    /// Frames the producer generated and the ring had no room for.
    pub refused_frames: u64,
    pub tone: ToneReport,
}

/// What the device did.
#[derive(Debug)]
pub struct Render {
    pub device: String,
    /// Named or inherited. Beside the name because a figure whose device
    /// nobody can identify is not reproducible, and the reader who has to know
    /// which of the two it was is the one comparing this run against another.
    pub chosen: device::Chosen,
    pub format: OutputFormat,
    pub buffer_frames: u32,
    pub ring_frames: usize,
    pub ring_prime_frames: usize,
    pub callbacks: u64,
    pub odd_cycles: u64,
    pub interval_us: Option<Percentiles>,
    pub occupancy_frames: Option<Percentiles>,
    /// Cycles the ring could not fill, each one a whole buffer of silence sent
    /// to a device in place of audio.
    pub underruns: u64,
    pub underrun_frames: u64,
    pub overruns: u64,
    pub overrun_frames: u64,
    pub frames_consumed: u64,
    /// Between the first and last cycles' own timestamps, so it is the device's
    /// clock and not this program's.
    pub span_seconds: f64,
    /// How long `AudioDeviceStart` took to produce a first cycle.
    ///
    /// Reported because it is the ring's resting occupancy: the producer runs
    /// throughout it and the device consumes nothing, so the ring ends up
    /// holding the prime plus this. It is a property of the device and not of
    /// the link, and reading the ring's occupancy without it invites the wrong
    /// conclusion about where the latency went.
    pub start_latency_ms: f64,
    pub samples_dropped: u64,
}

/// One ten-second window's worth of the counters, as deltas.
///
/// The plan asks for these rather than for run totals because a fault that
/// started halfway through a ten-minute run is invisible in a figure taken over
/// the whole of it.
#[derive(Clone, Copy, Debug)]
pub struct WindowRow {
    pub seconds: f64,
    pub rtp_received: u64,
    pub rtp_lost: u64,
    pub plc_frames: u64,
    pub frames_played: u64,
    pub jitter_underruns: u64,
    pub render_callbacks: u64,
    pub render_underruns: u64,
    pub render_overruns: u64,
    pub expected_samples: u64,
    pub played_samples: u64,
    /// Occupancy the buffer reported after serving each pull inside this
    /// window, in microseconds of audio, and absent when no pull happened.
    ///
    /// This window's rather than the run's so far, which for a distribution
    /// means a store that empties at the boundary rather than a difference of
    /// two running ones: see [`crate::occupancy`]. It is the figure that says
    /// what state the buffer was in when the hole beside it appeared - empty,
    /// at its ceiling, or holding its target while frames arrived too late to
    /// be in it.
    pub occupancy_us: Option<Percentiles>,
}

impl WindowRow {
    pub fn hole(&self) -> u64 {
        self.expected_samples.saturating_sub(self.played_samples)
    }
}

/// Everything one run of the receiver established.
#[derive(Debug)]
pub struct Receipt {
    pub config: CodecConfig,
    pub bind: SocketAddr,
    pub ssrc: Option<Ssrc>,
    pub target: Nanos,
    pub ceiling: Nanos,
    pub slots: usize,
    /// What the receiving thread was granted. It has no counters of its own —
    /// the buffer does that accounting — but it has a deadline, so it has this.
    pub receiver_scheduled_as: ScheduledAs,
    /// How far past its own moment each frame arrived, biased by
    /// [`unbias_micros`]'s offset and positive when late.
    ///
    /// The distribution rather than the count, because the count says a budget
    /// was exceeded and the shape says by what. A median already past zero is
    /// a fixed cost every frame pays; a healthy median under a long tail is a
    /// link that stutters; and the two want opposite remedies.
    pub arrival_delay_us: Option<Percentiles>,
    pub counts: Counts,
    pub loss: Loss,
    /// Datagrams that parsed as Opus and carried another SSRC. Not this
    /// stream, and named rather than merely dropped so that two senders on one
    /// port is a finding instead of a mystery.
    pub foreign_ssrc: u64,
    pub producer: Producer,
    pub render: Render,
    pub windows: Vec<WindowRow>,
    /// Between the first datagram and the end of the accounting window.
    pub span_seconds: f64,
}

impl Receipt {
    pub fn continuity(&self) -> Continuity {
        Continuity::of(self.counts, self.producer.refused_frames)
    }

    pub fn frame_samples(&self) -> u64 {
        self.config.frame_samples() as u64
    }

    /// Whether every thread that paces got the deadline it asked for.
    ///
    /// Reported rather than made a failure. A refusal does not make the
    /// counters wrong, it makes them a statement about the scheduler as well as
    /// about the path, and the reader who has to know that is the harness above
    /// this one.
    pub fn deadlines_were_granted(&self) -> bool {
        self.producer.scheduled_as.is_real_time() && self.receiver_scheduled_as.is_real_time()
    }

    /// The worst ten seconds of the run, by continuity.
    ///
    /// A run's total hole can be small and still be one window that lost
    /// everything, and that window is the one worth looking at.
    pub fn worst_window_hole(&self) -> u64 {
        self.windows.iter().map(WindowRow::hole).max().unwrap_or(0)
    }
}

/// What the receiver and the producer share.
///
/// The buffer is behind a lock because the two run on their own threads on
/// purpose: one thread would have to interleave a socket read with the
/// producer's schedule, and a read timeout short enough not to disturb a 5 ms
/// cadence would turn the loop into a spin. The lock is held for a copy of
/// eighty-odd bytes at either end and for nothing else — decoding happens
/// outside it — so the two threads meet for about as long as it takes to touch
/// two cache lines.
struct Shared {
    buffer: Mutex<JitterBuffer>,
    loss: Mutex<Loss>,
    /// When the first frame is due, in clock nanoseconds, or zero while the
    /// stream has not started. Published by the receiver so the producer knows
    /// when its schedule begins.
    playout_at: AtomicU64,
    /// The SSRC the first datagram carried, plus one so that zero can mean
    /// nothing yet: an SSRC of zero is a legal SSRC.
    ssrc: AtomicU64,
    foreign_ssrc: AtomicU64,
    /// Frames the producer generated and the ring refused, published so a
    /// window can be closed while the run is going.
    refused_frames: AtomicU64,
    /// What the buffer held after each pull, for the window that is open now.
    ///
    /// Here rather than in the producer's own stores because a window is closed
    /// by another thread while the run is going, and the producer's stores are
    /// read once at the end. The producer writes and the watcher empties; see
    /// [`crate::occupancy`] for why a distribution has to be emptied rather
    /// than differenced.
    occupancy: WindowOccupancy,
    /// The producer has pulled its last frame. The device is stopped on this
    /// rather than on a duration of the watcher's own, because the producer's
    /// schedule is derived from the playout anchor and the watcher's is not:
    /// a device left running past the last deposit drains the ring and turns
    /// the tail of every run into underruns this run caused rather than found.
    done: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    /// When the first frame is due, once a packet has anchored the stream.
    fn playout_start_ns(&self) -> Option<u64> {
        match self.playout_at.load(Ordering::Acquire) {
            0 => None,
            published => Some(published),
        }
    }
}

/// Runs the receiving path for the requested time and reports what arrived.
pub fn receive(options: ReceiveOptions) -> Result<Receipt, ReceiveError> {
    let config = options.config;

    let (device, chosen) = device::output_device(options.device.as_deref())?;
    let name = device::device_name(device)?;
    // The name alone is not enough in a refusal. The whole reason a run refuses
    // for a format is usually that it inherited an endpoint nobody chose, and a
    // message that says only which device it was leaves its reader to work out
    // where the device came from.
    let attributed = format!("{name} ({chosen})");
    let streams = device::output_streams(device)?;
    let stream = match streams.as_slice() {
        [] => {
            return Err(ReceiveError::Mismatch(format!(
                "{attributed} has no output stream, so there is nothing to render into"
            )));
        }
        [single] => *single,
        many => {
            return Err(ReceiveError::Mismatch(format!(
                "{attributed} has {} output streams; this receiver feeds a single-stream device",
                many.len()
            )));
        }
    };

    device::request_buffer_frame_size(device, options.buffer_frames).ok();
    let buffer_frames = device::buffer_frame_size(device)?;
    if buffer_frames == 0 {
        return Err(ReceiveError::Mismatch(format!(
            "{attributed} reports an IO buffer of no frames, so no cycle would ever ask for audio"
        )));
    }

    let format = device::virtual_format(stream)?;
    if !format.is_writable() {
        return Err(ReceiveError::Mismatch(format!(
            "{attributed} mixes at {format}; this receiver writes 32-bit float and will not \
             convert"
        )));
    }
    // Refused rather than resampled. A converter here would sit on the one path
    // in this project with a hard deadline, and every figure in the report
    // would become a statement about the converter rather than about the link.
    // The plan's contract is that both ends are at 48000 Hz stereo, and a
    // machine where that is untrue is a finding to state and not to paper over.
    if format.sample_rate != config.sample_rate || format.channels != config.channels {
        return Err(ReceiveError::Mismatch(format!(
            "{attributed} mixes at {format} and the stream is {} Hz {} ch; this receiver will not \
             resample, because a converter on this path would make every figure below a \
             statement about the converter",
            config.sample_rate, config.channels
        )));
    }

    let channels = usize::from(format.channels);
    let ring_frames = buffer_frames as usize * options.ring_multiple as usize;
    let ring = Arc::new(PcmRing::new(ring_frames, channels));
    // One IO buffer, because the device's first cycle takes a whole one, plus
    // one Opus frame, because the producer deposits in whole frames and may
    // wake a frame late. That is the floor and not the resting place: the
    // producer keeps depositing while `AudioDeviceStart` is getting the device
    // going, so the ring settles at the prime plus whatever that took. On this
    // machine it took eight milliseconds, which is 384 frames, and aiming at
    // half the ring put the resting place at 896 of 1024 - close enough to the
    // top that a 240 frame deposit was refused six times in twenty seconds.
    // Aiming at the floor instead lets the device's own start latency decide
    // the resting place, which is the honest thing to report, and leaves the
    // headroom above it for the drift A7 is about.
    let ring_prime_frames = buffer_frames as usize + config.frame_samples();

    let socket = UdpSocket::bind(options.bind).map_err(io("bind"))?;
    socket
        .set_read_timeout(Some(RECV_TIMEOUT))
        .map_err(io("set_read_timeout"))?;

    let buffer = JitterBuffer::new(config, options.target);
    let target = buffer.target();
    let ceiling = buffer.ceiling();
    let slots = buffer.slots();
    let shared = Arc::new(Shared {
        buffer: Mutex::new(buffer),
        loss: Mutex::new(Loss::default()),
        playout_at: AtomicU64::new(0),
        ssrc: AtomicU64::new(0),
        foreign_ssrc: AtomicU64::new(0),
        refused_frames: AtomicU64::new(0),
        // Sized from the buffer's own slot count, so the histogram spans every
        // occupancy the buffer is able to report and its percentiles are exact
        // rather than sampled.
        occupancy: WindowOccupancy::new(slots, u64::from(config.frame.millis()) * 1_000),
        done: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });

    let period = Nanos(u64::from(config.frame.millis()) * 1_000_000);
    // Room for every pull the run should make, plus a quarter, plus a floor for
    // a very short run. Sized before any thread starts, because neither the
    // producer nor the callback may allocate afterwards.
    let expected_pulls = (options.seconds / config.frame.seconds()) as usize;
    let store = expected_pulls + expected_pulls / 4 + 1_024;
    let expected_cycles =
        (options.seconds * f64::from(format.sample_rate) / f64::from(buffer_frames)) as usize;
    let render_store = expected_cycles + expected_cycles / 4 + 1_024;
    let state = Box::new(RenderState::new(Arc::clone(&ring), channels, render_store));

    type Outcome = (
        Producer,
        ScheduledAs,
        Option<Percentiles>,
        Vec<WindowRow>,
        f64,
        u64,
    );
    let outcome = thread::scope(|scope| -> Result<Outcome, ReceiveError> {
        let receiver = {
            let shared = Arc::clone(&shared);
            let socket = &socket;
            let sample_rate = config.sample_rate;
            scope.spawn(move || listen(socket, &shared, period, sample_rate, store))
        };

        // Spawned before the anchor exists rather than after it, and the first
        // runs showed why. Asking for the deadline, building the decoder and
        // allocating four distributions took about thirty milliseconds, and a
        // producer that started them after the first packet arrived was thirty
        // milliseconds late to a schedule that begins at the anchor. It caught
        // up, because the schedule is absolute - but the buffer had eight
        // frames in it by then against a ceiling of six, so it skipped forward
        // and gave up five frames of a stream that had arrived perfectly well.
        // Every one of those was a hole in the continuity accounting charged to
        // a link that had done nothing. Warmed up first, the thread is already
        // sitting in its wait when the anchor is published.
        let producer = {
            let shared = Arc::clone(&shared);
            let ring = Arc::clone(&ring);
            scope.spawn(move || {
                let produced = produce(
                    &shared,
                    &ring,
                    config,
                    options.first_packet_wait,
                    options.seconds,
                    store,
                    channels,
                );
                shared.done.store(true, Ordering::Release);
                produced
            })
        };

        // The wait is bounded: a run whose stream never started must say so
        // rather than spend its whole window measuring an empty socket.
        if await_playout(&shared, options.first_packet_wait).is_none() {
            shared.stop.store(true, Ordering::Relaxed);
            let (scheduled_as, _) = receiver.join().expect("the receive thread does not panic");
            producer.join().expect("the producer thread does not panic");
            return Err(ReceiveError::Mismatch(format!(
                "nothing arrived on {} within {:.0} s, so there is no run to report; the \
                 scheduler gave the receive thread {scheduled_as}",
                options.bind,
                options.first_packet_wait.as_secs_f64()
            )));
        }
        let began = Timestamp::now();

        // The device starts once the ring holds its prime. Started before, its
        // first cycles would drain a ring the producer cannot fill faster than
        // the stream arrives, and every one of them would be an underrun this
        // run caused rather than found.
        let prime_by = Instant::now() + PRIME_TIMEOUT;
        while ring.occupancy_frames() < ring_prime_frames && !shared.done.load(Ordering::Acquire) {
            if Instant::now() >= prime_by {
                break;
            }
            thread::sleep(MINIMUM_NAP);
        }

        // Read on the HAL's own clock, immediately before the start call, so
        // that subtracting it from the first cycle's timestamp is two readings
        // of one clock rather than a comparison of two.
        //
        // What it buys is the only explanation the ring's occupancy has. The
        // producer keeps depositing while the device is getting going, so the
        // ring settles at the prime plus whatever that took, and on this
        // machine it is most of the ring's contribution to end-to-end latency
        // and has nothing to do with the network.
        // SAFETY: no arguments and no failure mode.
        let start_call = unsafe { AudioGetCurrentHostTime() };
        let windows = {
            let mut audio = Stream::new(device, &state)?;
            audio.start()?;
            let windows = watch(&shared, &ring, &state, &options);
            // Inside the block, so it happens before the device is torn down
            // rather than after. `AudioDeviceStop` and
            // `AudioDeviceDestroyIOProcID` took about thirty milliseconds on
            // this machine, and a receiver still admitting packets across them
            // pushed six frames into a buffer nothing was pulling from: the
            // ceiling fired, five frames were given up, and 1200 samples of
            // hole were charged to a link that had already finished. The run's
            // window shuts with its last pull.
            shared.stop.store(true, Ordering::Relaxed);
            windows
        };
        let span_seconds = Timestamp::now().saturating_since(began).as_secs_f64();
        let (receiver_scheduled_as, arrival_delay_us) =
            receiver.join().expect("the receive thread does not panic");
        let produced = producer.join().expect("the producer thread does not panic");
        Ok((
            produced,
            receiver_scheduled_as,
            arrival_delay_us,
            windows,
            span_seconds,
            start_call,
        ))
    });

    let (producer, receiver_scheduled_as, arrival_delay_us, windows, span_seconds, start_call) =
        outcome?;
    // The stream borrowed the state for exactly as long as the IOProc existed,
    // and consuming it here is the proof that the borrow has ended.
    let rendered = state.finish();

    let ssrc = match shared.ssrc.load(Ordering::Relaxed) {
        0 => None,
        raw => Some(Ssrc((raw - 1) as u32)),
    };

    Ok(Receipt {
        config,
        bind: options.bind,
        ssrc,
        target,
        ceiling,
        slots,
        receiver_scheduled_as,
        arrival_delay_us,
        counts: shared.buffer.lock().counts(),
        loss: *shared.loss.lock(),
        foreign_ssrc: shared.foreign_ssrc.load(Ordering::Relaxed),
        producer,
        render: Render {
            device: name,
            chosen,
            format,
            buffer_frames,
            ring_frames,
            ring_prime_frames,
            callbacks: rendered.callbacks,
            odd_cycles: rendered.odd_cycles,
            interval_us: rendered.interval_us,
            occupancy_frames: rendered.occupancy_frames,
            underruns: ring.underruns(),
            underrun_frames: ring.underrun_frames(),
            overruns: ring.overruns(),
            overrun_frames: ring.overrun_frames(),
            frames_consumed: ring.consumed(),
            span_seconds: rendered.span_seconds,
            // Zero when the device never ran a cycle, which is the same thing
            // the callback count already says and is better than an interval
            // measured against a timestamp nobody wrote.
            // SAFETY: a pure arithmetic conversion with no pointers in it.
            start_latency_ms: unsafe {
                AudioConvertHostTimeToNanos(rendered.first_host_time.saturating_sub(start_call))
            } as f64
                / 1e6,
            samples_dropped: rendered.samples_dropped,
        },
        windows,
        span_seconds,
    })
}

/// Reads datagrams and offers each one to the buffer.
///
/// It decodes nothing and takes no timing decisions. Everything it knows about
/// a packet is in the packet; when the frame it carries is due is the buffer's
/// arithmetic, from the timestamp, and the arrival instant is passed on only so
/// that the very first packet can anchor the stream.
///
/// It asks for a deadline all the same. The argument for leaving a receiver
/// alone is that it keeps no schedule — it blocks in `recv_from`, the kernel
/// holds its datagrams while it is away, and the delay it adds is delivery
/// jitter, which is what the target absorbs. What that argument leaves out is
/// that the target is the whole budget and it is ten milliseconds. The phase
/// before this one measured thirteen packets past their moment on a clean
/// loopback run with this thread left at ordinary priority, and none at all
/// with it promoted. So it has a period after all: one datagram per frame,
/// which is the cadence the stream arrives at.
///
/// The SSRC is learned rather than agreed. The sender is another process on
/// another machine and draws its own; the first datagram that parses decides,
/// and every later one from anywhere else is counted and dropped.
fn listen(
    socket: &UdpSocket,
    shared: &Shared,
    period: Nanos,
    sample_rate: u32,
    store: usize,
) -> (ScheduledAs, Option<Percentiles>) {
    let scheduled_as = ScheduledAs::request(period.get());
    let mut datagram = [0u8; MAX_UDP_PAYLOAD];
    // Where the stream's clock met this one, kept here rather than asked of the
    // buffer so that the delay below is computed from the same two numbers for
    // every frame of the run.
    let mut anchor: Option<(RtpTimestamp, u64)> = None;
    let mut delay_us = Samples::with_capacity(store);
    loop {
        // Checked before every read and not only after a timeout. On the first
        // run this loop only ever tested the flag when the socket went quiet,
        // so a sender that outlived the accounting window kept it going for six
        // seconds past the last pull: every datagram in those six seconds
        // pushed the buffer past its ceiling with no pull to match, and 1195
        // frames were skipped and charged to a run that had already ended.
        if shared.stop.load(Ordering::Relaxed) {
            return (scheduled_as, delay_us.percentiles());
        }
        let length = match socket.recv_from(&mut datagram) {
            Ok((length, _from)) => length,
            Err(error) => {
                let timed_out = matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                );
                if !timed_out {
                    return (scheduled_as, delay_us.percentiles());
                }
                continue;
            }
        };
        let at = Timestamp::now();
        let Ok(packet) = parse_opus_packet(&datagram[..length]) else {
            // Anything that is not this stream's payload format is not this
            // stream. Counted nowhere on purpose: the buffer accounts for
            // audio, and a foreign datagram on the port is a question for the
            // packet-level probe that already answers it.
            continue;
        };

        let expected = shared.ssrc.load(Ordering::Relaxed);
        if expected == 0 {
            shared
                .ssrc
                .store(u64::from(packet.ssrc.0) + 1, Ordering::Relaxed);
        } else if expected != u64::from(packet.ssrc.0) + 1 {
            shared.foreign_ssrc.fetch_add(1, Ordering::Relaxed);
            continue;
        }

        let admission = {
            let mut buffer = shared.buffer.lock();
            let first = buffer.playout_start().is_none();
            let admission = buffer.push(&packet, at);
            if first && let Some(playout) = buffer.playout_start() {
                // Published after the push, so the producer can never start
                // pulling against a buffer that has not yet been anchored.
                shared
                    .playout_at
                    .store(playout.as_nanos(), Ordering::Release);
            }
            admission
        };

        // How far past its own moment the frame turned up, in microseconds,
        // positive when late. The moment is the anchor's playout plus the
        // frame's distance from the anchor in the stream's own samples, which
        // is exactly the arithmetic the buffer's cursor performs, so this is
        // the quantity the Late decision is made on rather than a proxy for it.
        //
        // It is what tells a budget that does not fit from a link that
        // stutters. A distribution tight around one value is a fixed cost every
        // frame pays and points at whatever is spending it; a distribution with
        // a long upper tail and a healthy median is the radio holding
        // datagrams; and a median already past zero with no tail at all would
        // be an anchor taken on a frame that arrived early, which is this
        // receiver's fault and nobody else's.
        if admission != Admission::Duplicate {
            shared.loss.lock().arrived(packet.sequence);
            if let Some((anchor_rtp, playout_ns)) = anchor {
                let ticks = i128::from(packet.timestamp.distance_from(anchor_rtp));
                let moment =
                    i128::from(playout_ns) + ticks * 1_000_000_000 / i128::from(sample_rate);
                let late_us = (i128::from(at.as_nanos()) - moment) / 1_000;
                delay_us.record(bias_micros(late_us as i64));
            } else if let Some(playout) = shared.playout_start_ns() {
                anchor = Some((packet.timestamp, playout));
            }
        }
    }
}

/// Microseconds either side of a moment, shifted so a distribution of them can
/// be kept in the same fixed-capacity store as everything else.
///
/// The shift is arithmetic and not a loss of information: it is monotonic, so
/// the order statistics of the shifted values are the shifted order statistics,
/// and [`unbias_micros`] takes it back off at the point of reporting. A signed
/// store would be a second implementation of `Samples` for one caller.
pub const DELAY_BIAS_US: i64 = 10_000_000;

fn bias_micros(micros: i64) -> u64 {
    (micros + DELAY_BIAS_US).clamp(0, 2 * DELAY_BIAS_US) as u64
}

pub fn unbias_micros(biased: u64) -> i64 {
    biased as i64 - DELAY_BIAS_US
}

/// Pulls one frame every frame period, decodes or conceals it, and puts the
/// samples in the ring.
///
/// The schedule is absolute, from the first frame's deadline, so a pull that
/// runs late does not push the next one later still. That matters more here
/// than anywhere else in the path: the buffer's cursor advances one frame per
/// pull, so a producer whose schedule drifted would move every deadline in the
/// stream with it and the buffer would be absorbing this thread's jitter
/// instead of the network's.
///
/// Which is why the deadline is asked for first, and why everything else this
/// thread needs — the decoder, the analysis window, the four distributions —
/// is built before it waits for the anchor rather than after. The period is
/// the frame period, because that is the cycle a pull has to keep. A
/// quality-of-service band was tried in the phase before this one and gave
/// twelve underruns in five minutes where a time constraint gave none, so the
/// band is not an option that is still open.
///
/// Nothing in the loop allocates, and the only lock it takes is the buffer's,
/// held for one payload copy.
#[allow(clippy::too_many_arguments)]
fn produce(
    shared: &Shared,
    ring: &PcmRing,
    config: CodecConfig,
    patience: Duration,
    seconds: f64,
    store: usize,
    channels: usize,
) -> Producer {
    let period = Nanos(u64::from(config.frame.millis()) * 1_000_000);
    let mut report = Producer {
        scheduled_as: ScheduledAs::request(period.get()),
        pulls: 0,
        interval_us: None,
        occupancy_us: None,
        decode_us: None,
        conceal_us: None,
        decode_failures: 0,
        refused_frames: 0,
        tone: ToneReport::empty(),
    };
    eprintln!(
        "audio-e2e-receiver: producer scheduled as {}",
        report.scheduled_as
    );
    if !report.scheduled_as.is_real_time() {
        eprintln!(
            "audio-e2e-receiver: the producer did not get a deadline, so the underruns below \
             are the scheduler's and not the path's"
        );
    }

    let mut decoder = match OpusDecoder::new(config) {
        Ok(decoder) => decoder,
        Err(error) => {
            eprintln!("audio-e2e-receiver: the decoder would not start: {error}");
            return report;
        }
    };

    let frame_us = u64::from(config.frame.millis()) * 1_000;
    let window_format = decoded_format(&config);
    let mut analysis = Vec::with_capacity(ANALYSIS_FRAMES * window_format.frame_bytes());
    let mut skipped = 0usize;

    let mut interval_us = Samples::with_capacity(store);
    let mut occupancy_us = Samples::with_capacity(store);
    let mut decode_us = Samples::with_capacity(store);
    let mut conceal_us = Samples::with_capacity(store);

    // Warmed up: everything above happens before the anchor is looked for, so
    // that this thread is already sitting in its wait when the first packet
    // publishes one and is not thirty milliseconds behind a schedule that has
    // already begun.
    let Some(playout) = await_playout(shared, patience) else {
        return report;
    };

    let mut payload = [0u8; MAX_OPUS_PAYLOAD];
    let mut previous: Option<Timestamp> = None;
    let mut index = 0u64;
    let pulls = (seconds / config.frame.seconds()).round() as u64;

    while index < pulls {
        wait_until(playout.add(Nanos(period.get() * index)));
        index += 1;

        let at = Timestamp::now();
        if let Some(earlier) = previous {
            interval_us.record(at.saturating_since(earlier).get() / 1_000);
        }
        previous = Some(at);

        let pulled = {
            let mut buffer = shared.buffer.lock();
            buffer.pull(&mut payload)
        };
        // Twice, into two stores with two lifetimes: the run's, sorted once at
        // the end, and the window's, emptied by the watcher every ten seconds.
        // The second is not derivable from the first, because a percentile taken
        // over a run cannot be split back into the windows that made it.
        occupancy_us.record(pulled.occupancy as u64 * frame_us);
        shared.occupancy.record(pulled.occupancy);

        let started = Timestamp::now();
        let decoded = match pulled.outcome {
            Pull::Frame(len) => {
                let decoded = decoder.decode(&payload[..len]);
                decode_us.record(Timestamp::now().saturating_since(started).get() / 1_000);
                decoded
            }
            // Both conceal, and the ring is fed either way: a callback handed
            // nothing produces a click, which is louder than the few
            // milliseconds it stands in for. What separates them is the
            // accounting, not the audio.
            Pull::Conceal | Pull::Underrun => {
                let concealed = decoder.conceal();
                conceal_us.record(Timestamp::now().saturating_since(started).get() / 1_000);
                concealed
            }
        };

        match decoded {
            Ok(pcm) => {
                let frames = pcm.len() / channels;
                let mut offset = 0usize;
                let filled = ring.fill(frames, &mut |_, run| {
                    run.copy_from_slice(&pcm[offset..offset + run.len()]);
                    offset += run.len();
                });
                report.refused_frames += filled.refused as u64;
                if filled.refused > 0 {
                    shared
                        .refused_frames
                        .store(report.refused_frames, Ordering::Relaxed);
                }

                // Concealed samples go into the analysis window with the rest,
                // because the window is what the listener heard. A path that
                // concealed everything therefore reads as the near-silence the
                // concealer decays to, which is the point of analysing at all.
                if skipped < ANALYSIS_SKIP_FRAMES {
                    skipped += frames;
                } else if analysis.len() < analysis.capacity() {
                    for sample in pcm {
                        analysis.extend_from_slice(&sample.to_le_bytes());
                    }
                }
            }
            Err(_) => report.decode_failures += 1,
        }
    }

    // Published here rather than after the figures below, and the first run
    // showed why: sorting four distributions and running a Goertzel over half
    // a second of audio took 277 ms, and for every one of those milliseconds
    // the device was still draining a ring nobody was filling. The tail of the
    // run was fifty-two callbacks of silence that the path had not caused, and
    // the last window read as a total loss. The pulling is what the rest of
    // the run waits on; the arithmetic afterwards is this thread's own.
    shared.done.store(true, Ordering::Release);

    report.pulls = index;
    report.interval_us = interval_us.percentiles();
    report.occupancy_us = occupancy_us.percentiles();
    report.decode_us = decode_us.percentiles();
    report.conceal_us = conceal_us.percentiles();
    report.tone = analyse(&window_format, &analysis);
    report
}

/// Waits for the receiver to publish the first frame's deadline.
fn await_playout(shared: &Shared, patience: Duration) -> Option<Timestamp> {
    let deadline = Instant::now() + patience;
    loop {
        let published = shared.playout_at.load(Ordering::Acquire);
        if published != 0 {
            return Some(Timestamp::from_nanos(published));
        }
        if Instant::now() >= deadline {
            return None;
        }
        // A poll rather than a condition variable: this runs once, before the
        // schedule starts, and a millisecond of latency on the very first frame
        // is absorbed by the target.
        thread::sleep(Duration::from_millis(1));
    }
}

/// Closes a counter window every `window` seconds until the producer stops.
///
/// It runs on the caller's thread, which is the one thread here with no
/// deadline to keep, so sampling costs the path nothing. Every count is a delta
/// against the previous window rather than a running total, because a total
/// cannot show a fault that started halfway through it, and a ten-minute run
/// reported as one number is a ten-minute run nobody can read.
///
/// Occupancy is the one figure that cannot be had that way, since a percentile
/// does not subtract, so what closing a window does for it is empty the store
/// the producer has been filling. Same intent and the other mechanism.
///
/// It ends when the producer says it has finished rather than after a duration
/// of its own, because the two schedules do not share a start: the producer's
/// comes from the playout anchor and this one from whenever the device was
/// primed. Sleeping out its own clock would leave the device draining a ring
/// nobody is filling, and the tail of every run would be underruns.
fn watch(
    shared: &Shared,
    ring: &PcmRing,
    state: &RenderState,
    options: &ReceiveOptions,
) -> Vec<WindowRow> {
    let mut rows =
        Vec::with_capacity((options.seconds / options.window.as_secs_f64()) as usize + 2);
    // The snapshot the drain fills is allocated here, before the first window,
    // so that closing one costs an atomic swap per bucket and nothing else.
    let mut occupancy = shared.occupancy.reader();
    // What the producer recorded while the ring was being primed belongs to no
    // window at all, so it is thrown away rather than charged to the one about
    // to open.
    let _ = occupancy.take();
    let mut previous = Sample::read(shared, ring, state);
    let mut previous_at = Instant::now();

    // Woken far more often than a window is long, so the run ends within a
    // few milliseconds of the producer's last pull rather than within a
    // window of it.
    let tick = Duration::from_millis(5).min(options.window);
    while !shared.done.load(Ordering::Acquire) {
        thread::sleep(tick);
        if previous_at.elapsed() < options.window {
            continue;
        }
        let now = Instant::now();
        let current = Sample::read(shared, ring, state);
        rows.push(current.since(&previous, now.duration_since(previous_at), occupancy.take()));
        previous = current;
        previous_at = now;
    }

    // Whatever is left over closes a window of its own rather than being
    // dropped. A partial window states its own span, so a rate computed from
    // it is computed over the interval it was counted in.
    let now = Instant::now();
    let span = now.duration_since(previous_at);
    if span > Duration::ZERO {
        let current = Sample::read(shared, ring, state);
        rows.push(current.since(&previous, span, occupancy.take()));
    }
    rows
}

/// One reading of every counter, for differencing against the next.
#[derive(Clone, Copy)]
struct Sample {
    counts: Counts,
    lost: u64,
    refused: u64,
    callbacks: u64,
    underruns: u64,
    overruns: u64,
}

impl Sample {
    fn read(shared: &Shared, ring: &PcmRing, state: &RenderState) -> Sample {
        Sample {
            counts: shared.buffer.lock().counts(),
            lost: shared.loss.lock().lost(),
            refused: shared.refused_frames.load(Ordering::Relaxed),
            callbacks: state.callbacks(),
            underruns: ring.underruns(),
            overruns: ring.overruns(),
        }
    }

    fn since(
        &self,
        earlier: &Sample,
        span: Duration,
        occupancy_us: Option<Percentiles>,
    ) -> WindowRow {
        let played = self.counts.played_samples.saturating_sub(self.refused);
        let played_before = earlier
            .counts
            .played_samples
            .saturating_sub(earlier.refused);
        WindowRow {
            seconds: span.as_secs_f64(),
            rtp_received: self.counts.received - earlier.counts.received,
            rtp_lost: self.lost.saturating_sub(earlier.lost),
            plc_frames: self.counts.concealed - earlier.counts.concealed,
            frames_played: self.counts.played - earlier.counts.played,
            jitter_underruns: self.counts.underruns - earlier.counts.underruns,
            render_callbacks: self.callbacks.saturating_sub(earlier.callbacks),
            render_underruns: self.underruns.saturating_sub(earlier.underruns),
            render_overruns: self.overruns.saturating_sub(earlier.overruns),
            expected_samples: self.counts.expected_samples - earlier.counts.expected_samples,
            played_samples: played.saturating_sub(played_before),
            // Handed in rather than differenced, because it arrives from a store
            // that was emptied at this boundary and so is already this window's
            // and only this window's.
            occupancy_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(expected: u64, played: u64) -> Counts {
        Counts {
            expected_samples: expected,
            played_samples: played,
            ..Counts::default()
        }
    }

    /// The shape of a run that worked: every sample the cursor travelled
    /// reached the device, and the accounting says so.
    #[test]
    fn a_run_that_carried_everything_has_no_hole() {
        let continuity = Continuity::of(counts(288_000, 288_000), 0);
        assert_eq!(continuity.hole(), 0);
        assert!(continuity.unbroken());
    }

    /// The failure the whole phase turns on. Gap concealment is credited, so a
    /// run that lost packets and concealed them is still continuous; an
    /// underrun is not, so a run whose buffer emptied is not.
    #[test]
    fn concealment_is_played_and_an_underrun_is_not() {
        // A thousand frames of 240 samples. Fifty were concealed across gaps
        // and the buffer never emptied, so every position was carried.
        let concealed = Continuity::of(counts(240_000, 240_000), 0);
        assert_eq!(concealed.hole(), 0);

        // The same run with fifty of those positions found by an empty buffer
        // instead: the concealer still ran, and the accounting refuses to
        // credit what it produced.
        let starved = Continuity::of(counts(240_000, 228_000), 0);
        assert_eq!(starved.hole(), 12_000);
        assert!(!starved.unbroken());
    }

    /// A frame the producer generated and the ring had no room for reached
    /// nobody, so it comes back off a total the jitter buffer had already
    /// credited.
    #[test]
    fn frames_the_ring_refused_are_not_played() {
        let continuity = Continuity::of(counts(240_000, 240_000), 480);
        assert_eq!(continuity.played, 239_520);
        assert_eq!(continuity.hole(), 480);
    }

    /// The defect this project has read as success five times: nothing
    /// happened, so nothing is missing, and a bare comparison of two zeroes
    /// says the run was perfect.
    #[test]
    fn a_run_that_carried_nothing_is_not_unbroken() {
        let nothing = Continuity::of(counts(0, 0), 0);
        assert_eq!(nothing.hole(), 0);
        assert!(
            !nothing.unbroken(),
            "a hole of zero over an expectation of zero is an absence and not a result"
        );
    }

    /// Loss is the span the sequence numbers describe less the distinct numbers
    /// inside it, so a duplicate cannot conceal a loss and a reordered packet
    /// is not one.
    #[test]
    fn loss_is_the_span_less_what_arrived() {
        let mut loss = Loss::default();
        for number in [100u16, 101, 102, 105, 103] {
            loss.arrived(SequenceNumber(number));
        }
        assert_eq!(loss.expected(), 6, "100 to 105 inclusive");
        assert_eq!(loss.unique(), 5);
        assert_eq!(loss.lost(), 1, "104 never arrived");
    }

    /// A stream that crosses the sixteen-bit wrap is one interval, not two.
    #[test]
    fn loss_survives_the_sequence_wrap() {
        let mut loss = Loss::default();
        for number in [65_534u16, 65_535, 0, 1] {
            loss.arrived(SequenceNumber(number));
        }
        assert_eq!(loss.expected(), 4);
        assert_eq!(loss.lost(), 0);
    }

    /// The ten-minute run, which the four-packet test above passed without covering.
    ///
    /// A span of 120000 packets is what six hundred seconds at the wire's 200 a second comes
    /// to, and it crosses the sixteen-bit space nearly twice. The short test crosses the wrap
    /// too, and passes, because four packets never leave the half of the space a signed
    /// sixteen-bit distance can describe - which is why it stood beside a counter that
    /// saturated at 32768 for two committed ten-minute runs without objecting.
    #[test]
    fn a_ten_minute_span_is_not_capped_at_half_the_sequence_space() {
        let mut loss = Loss::default();
        let packets: u64 = 120_000;
        for step in 0..packets {
            loss.arrived(SequenceNumber(((step + 40_000) % 65_536) as u16));
        }
        assert_eq!(
            loss.expected(),
            packets,
            "a span longer than half the sequence space must still be its own length"
        );
        assert_eq!(loss.unique(), packets);
        assert_eq!(loss.lost(), 0);
    }

    /// A packet behind the furthest seen must not be read as a wrap.
    ///
    /// Reordering is the case that separates counting cycles from guessing them: 65535
    /// arriving after 1 has gone past is a late packet, and a counter that added a cycle for
    /// it would claim the stream had run another 65536 packets in the gap.
    #[test]
    fn a_reordered_packet_across_the_wrap_invents_no_cycle() {
        let mut loss = Loss::default();
        for number in [65_533u16, 65_534, 0, 1, 65_535] {
            loss.arrived(SequenceNumber(number));
        }
        assert_eq!(
            loss.expected(),
            5,
            "65533 to 1 inclusive, and 65535 was late"
        );
        assert_eq!(loss.lost(), 0);
    }

    /// A window is a difference, so a counter that did not move contributes
    /// nothing and one that did contributes exactly what it moved by.
    #[test]
    fn a_window_reports_what_changed_inside_it() {
        let earlier = Sample {
            counts: counts(240_000, 239_000),
            lost: 3,
            refused: 0,
            callbacks: 1_000,
            underruns: 1,
            overruns: 0,
        };
        let later = Sample {
            counts: counts(720_000, 718_000),
            lost: 7,
            refused: 240,
            callbacks: 2_875,
            underruns: 1,
            overruns: 2,
        };
        let row = later.since(&earlier, Duration::from_secs(10), None);
        assert_eq!(row.expected_samples, 480_000);
        assert_eq!(row.played_samples, 478_760);
        assert_eq!(row.hole(), 1_240);
        assert_eq!(row.rtp_lost, 4);
        assert_eq!(row.render_callbacks, 1_875);
        assert_eq!(row.render_underruns, 0);
        assert_eq!(row.render_overruns, 2);
        assert_eq!(
            row.occupancy_us, None,
            "a window with no pull in it has no occupancy, which is not an occupancy of zero"
        );
    }

    /// The two stores the producer writes to, working the way a run needs them
    /// to: the buffer holds two frames while the first window is open and six
    /// while the second is, and the rows have to say so rather than both
    /// reporting the run's mixture of the two.
    ///
    /// This is the shape of the defect the per-window figure exists to rule out.
    /// A running store would give both windows the same median, and every other
    /// number in both rows would still be right.
    #[test]
    fn a_window_carries_the_occupancy_measured_inside_it() {
        let histogram = WindowOccupancy::new(8, 5_000);
        let mut reader = histogram.reader();
        let sample = Sample {
            counts: counts(240_000, 240_000),
            lost: 0,
            refused: 0,
            callbacks: 1_000,
            underruns: 0,
            overruns: 0,
        };

        for _ in 0..2_000 {
            histogram.record(2);
        }
        let first = sample.since(&sample, Duration::from_secs(10), reader.take());
        assert_eq!(first.occupancy_us.map(|held| held.p50), Some(10_000));

        for _ in 0..2_000 {
            histogram.record(6);
        }
        let second = sample.since(&sample, Duration::from_secs(10), reader.take());
        assert_eq!(second.occupancy_us.map(|held| held.p50), Some(30_000));
        assert_eq!(second.occupancy_us.map(|held| held.min), Some(30_000));
    }
}
