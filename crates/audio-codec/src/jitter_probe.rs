//! The receiving path run end to end, with no audio hardware in it: UDP in,
//! RTP reordering, a bounded jitter buffer, Opus decode with concealment, and
//! a synthetic sink.
//!
//! Nothing plays. CoreAudio belongs to the next phase and does not appear here,
//! deliberately: putting a device in this run would make every number a
//! statement about that device's callback behaviour, and the question this
//! phase asks — how deep does the buffer have to be, and what does it cost when
//! the link misbehaves — has to be answered before there is any point choosing
//! a device period.
//!
//! What stands in for the device is a sink that pulls on a clock at the frame
//! cadence, the way a render callback will. That is what makes the occupancy
//! figures mean anything. A sink that drained as fast as datagrams arrived
//! would report an occupancy of nearly zero however bad the link was, because
//! it would be measuring the network's delivery rate rather than the buffer's
//! depth. And the interval the sink actually achieved is reported rather than
//! the one it asked for, because a sink that cannot keep its own cadence is
//! measuring itself.
//!
//! All three threads here ask the system for a deadline rather than for a priority, and
//! the report says which policy each of them got. Without one, a clean loopback arm run
//! while the machine was compiling something reported 658 packets past their moment and
//! 658 underruns against a relay that had been told to break nothing. None of it was
//! the buffer's. A thread that spends the last three milliseconds of every five
//! spinning out its deadline is exactly the CPU-bound work a loaded scheduler sheds,
//! and when the sink is shed for longer than the buffer's ceiling the buffer skips
//! forward to recover the latency, the cursor lands ahead of a stream that was
//! arriving perfectly well, and everything still in flight is counted late. A
//! quality-of-service class was tried against this on the render path and left the
//! failure unexplained; `THREAD_TIME_CONSTRAINT_POLICY` states the period the work
//! actually has, and under it the sink held 5000 microseconds between pulls at both
//! the median and the 99th percentile with the machine at load average 23.
//!
//! The receiving thread was expected not to need one, and the measurement said
//! otherwise. The argument for leaving it alone is good as far as it goes: it keeps no
//! schedule of its own, it blocks in `recv_from`, the kernel holds its datagrams while
//! it is away, and what its absence adds is delivery jitter, which is the quantity the
//! target exists to absorb. What the argument leaves out is that the target is the
//! entire budget and it is ten milliseconds. With the sink and the sender promoted and
//! the receiver left alone, a loopback run with no relay and no injected fault still
//! reported thirteen packets past their moment at load average 23; with the receiver
//! promoted too, the same run at load average 30 reported none at all. So it does have
//! a period - one datagram per frame, which is the cadence the stream arrives at - and
//! it asks in those terms.
//!
//! Both halves run in this process and the sender uses a socket of its own,
//! bound to an ephemeral port rather than to the receiving one. That is what
//! lets the whole thing run through `tools/udp-fault`: the relay decides
//! direction by comparing the sender's address against the address it forwards
//! to, so a sender that sent from the receiving port would look to it like the
//! reply direction and its datagrams would go nowhere. With a separate sender
//! the chain is sender to relay to receiver, all on this machine, which is how
//! the fault arms are driven.
//!
//! The decoded audio is analysed for the contract tone at the end, through the
//! same [`analyse`] the other probes use. This is not decoration. A count of
//! frames played cannot tell a path that carries audio from one that plays
//! concealment forever — the concealer produces plausible samples from an empty
//! stream indefinitely — and this project has read a healthy-looking count as
//! success five times. 997 Hz on the left and 1997 Hz on the right is the only
//! statement in the report that cannot be produced by a path carrying nothing.

use core::fmt;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_audio_capture::analysis::hertz;
use lanplay_audio_capture::{Percentiles, Samples, ToneReport, analyse};
use lanplay_telemetry::{Nanos, ScheduledAs, Timestamp, wait_until};
use lanplay_tone_source::tone::{CONTRACT, Tone};
use lanplay_transport::{
    MAX_OPUS_PAYLOAD, MAX_UDP_PAYLOAD, OpusPacketizeError, OpusPacketizer, Ssrc, parse_opus_packet,
    random_u32,
};
use parking_lot::Mutex;

use crate::config::{CodecConfig, FrameDuration};
use crate::decoder::OpusDecoder;
use crate::encoder::OpusEncoder;
use crate::error::CodecError;
use crate::jitter::{Counts, JitterBuffer, Pull};
use crate::probe::{ANALYSIS_FRAMES, ANALYSIS_SKIP_FRAMES, decoded_format};

/// How long the receive loop blocks before asking whether it should stop.
const RECV_TIMEOUT: Duration = Duration::from_millis(100);

/// How long the receiver keeps reading after the sink has finished.
///
/// Long enough to outlast the longest stall the fault relay is asked for in
/// this phase, because the datagrams a stall releases are exactly the ones
/// whose lateness the run is trying to count. Stopping when the sink stops
/// would report them as never having arrived, which is a different fault with a
/// different remedy.
const DRAIN_GRACE: Duration = Duration::from_millis(400);

/// How far the decoded tone may sit from the contract before the audio is no
/// longer the audio that was sent. The detector resolves 2 Hz over its window,
/// so this is beyond what the measurement can distinguish and still hundreds of
/// times narrower than the gap between the two contract tones.
const TONE_TOLERANCE_HZ: f64 = 5.0;

/// How long the sink waits for a first packet before giving up on the run.
///
/// Bounded because a sink whose stream never starts would otherwise sit out the
/// entire window having measured nothing, and the report would describe an
/// experiment that never began.
const FIRST_PACKET_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Options {
    pub bind: SocketAddr,
    /// Where the datagrams go: the receiving port for a direct run, or the
    /// fault relay's listening port for a run behind one.
    pub send_to: SocketAddr,
    pub seconds: f64,
    pub frame: FrameDuration,
    /// Audio the buffer aims to hold. Quantised to whole frames, and reported
    /// as what it became.
    ///
    /// An experimental variable rather than a constant: the plan's baseline is
    /// 10 ms, about two 5 ms frames, and the whole point of this phase is to
    /// find out what the number should be under each fault.
    pub target: Nanos,
}

#[derive(Debug)]
pub enum ProbeError {
    Codec(CodecError),
    Packetize(OpusPacketizeError),
    /// A socket call failed, named, because an error number without its
    /// callsite is a number nobody can act on.
    Io {
        call: &'static str,
        error: io::Error,
    },
}

impl From<CodecError> for ProbeError {
    fn from(error: CodecError) -> Self {
        ProbeError::Codec(error)
    }
}

impl From<OpusPacketizeError> for ProbeError {
    fn from(error: OpusPacketizeError) -> Self {
        ProbeError::Packetize(error)
    }
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProbeError::Codec(error) => write!(f, "{error}"),
            ProbeError::Packetize(error) => write!(f, "{error}"),
            ProbeError::Io { call, error } => write!(f, "{call} failed: {error}"),
        }
    }
}

impl core::error::Error for ProbeError {}

fn io(call: &'static str) -> impl FnOnce(io::Error) -> ProbeError {
    move |error| ProbeError::Io { call, error }
}

/// The frame period: the cadence the sender emits at, the receiver takes delivery
/// at and the sink pulls at, and so the period all three deadlines are stated in.
fn frame_period(config: &CodecConfig) -> Nanos {
    Nanos(u64::from(config.frame.millis()) * 1_000_000)
}

/// What the sink consumed, and what it cost.
#[derive(Debug)]
pub struct SinkReport {
    /// Occupancy the sink found at each pull, in microseconds of audio.
    pub occupancy_us: Option<Percentiles>,
    /// Interval between consecutive pulls, as achieved rather than as asked
    /// for.
    pub interval_us: Option<Percentiles>,
    /// Time in `opus_decode_float` for frames that arrived, concealment
    /// excluded so that a lossy run's decode figure still describes decoding.
    pub decode_us: Option<Percentiles>,
    /// Time in the concealer, kept separate for the same reason.
    pub conceal_us: Option<Percentiles>,
    pub decode_failures: u64,
    pub tone: ToneReport,
    /// Set when the stream never started, so a report of nothing says why.
    pub never_started: bool,
    /// What the system granted the sink thread, which decides whether the rest
    /// of this report describes the buffer or describes the scheduler.
    pub scheduled_as: ScheduledAs,
}

/// What the sending half put on the wire.
#[derive(Debug)]
pub struct SendReport {
    pub packets: u64,
    pub send_failures: u64,
    /// And what it granted the sender, for the same reason from the other end:
    /// a sender that missed its own cadence delivers audio late, and late audio
    /// is indistinguishable in the counters from a network that delayed it.
    pub scheduled_as: ScheduledAs,
}

#[derive(Debug)]
pub struct Measurement {
    pub config: CodecConfig,
    pub bind: SocketAddr,
    pub send_to: SocketAddr,
    /// The target after quantisation to whole frames.
    pub target: Nanos,
    pub ceiling: Nanos,
    pub slots: usize,
    pub send: SendReport,
    pub counts: Counts,
    /// What the receiving thread was granted. It sits here rather than in a
    /// report of its own because the receiver counts nothing - the buffer does
    /// its accounting - but it has a deadline, so it has this.
    pub receiver_scheduled_as: ScheduledAs,
    pub sink: SinkReport,
}

impl Measurement {
    pub fn frame_samples(&self) -> u64 {
        self.config.frame_samples() as u64
    }

    /// Whether the decoded audio is the audio that was sent.
    pub fn tone_is_the_contract(&self) -> bool {
        let left = hertz(self.sink.tone.left);
        let right = hertz(self.sink.tone.right);
        (left - CONTRACT.left_hz).abs() <= TONE_TOLERANCE_HZ
            && (right - CONTRACT.right_hz).abs() <= TONE_TOLERANCE_HZ
    }

    /// Whether every thread that paces got the deadline it asked for.
    ///
    /// Reported rather than made a defect. A refusal does not make the counters
    /// wrong, it makes them a statement about the scheduler as well as about the
    /// buffer, and the reader who has to know that is the harness above this one.
    pub fn deadlines_were_granted(&self) -> bool {
        self.sink.scheduled_as.is_real_time()
            && self.send.scheduled_as.is_real_time()
            && self.receiver_scheduled_as.is_real_time()
    }

    /// A reason the run's numbers cannot be believed.
    ///
    /// Loss, lateness, concealment and underruns are not defects: they are the
    /// measurements this phase exists to take, and a probe that failed on them
    /// would be refusing to report its own result. What is a defect is a run
    /// that measured nothing, or one whose output is not the audio that went in
    /// — because those two are what a broken path and a working one have in
    /// common when only the counters are read.
    pub fn defect(&self) -> Option<Defect> {
        if self.sink.never_started {
            return Some(Defect::NothingArrived);
        }
        if self.counts.received == 0 {
            return Some(Defect::NothingArrived);
        }
        if self.counts.played == 0 {
            return Some(Defect::NothingPlayed);
        }
        if self.sink.decode_failures > 0 {
            return Some(Defect::DecodeFailed(self.sink.decode_failures));
        }
        if self.counts.off_grid > 0 {
            return Some(Defect::OffGrid(self.counts.off_grid));
        }
        if !self.tone_is_the_contract() {
            return Some(Defect::NotTheTone {
                left: hertz(self.sink.tone.left),
                right: hertz(self.sink.tone.right),
            });
        }
        if !self.sink.tone.distinct() {
            return Some(Defect::ChannelsNotDistinct);
        }
        None
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Defect {
    NothingArrived,
    /// Every frame the sink consumed came from the concealer. The counters
    /// would look almost identical to a working run, which is exactly why this
    /// has a name.
    NothingPlayed,
    DecodeFailed(u64),
    OffGrid(u64),
    NotTheTone {
        left: f64,
        right: f64,
    },
    ChannelsNotDistinct,
}

impl fmt::Display for Defect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Defect::NothingArrived => write!(
                f,
                "no packets arrived, so every figure above describes an experiment that did not \
                 happen; check that the relay or the peer is forwarding to --bind"
            ),
            Defect::NothingPlayed => write!(
                f,
                "every frame the sink consumed came from the concealer, so the counters describe \
                 a buffer keeping a sink alive rather than a path carrying audio"
            ),
            Defect::DecodeFailed(count) => write!(
                f,
                "{count} packets arrived in time and would not decode; the payload format and the \
                 decoder disagree, which no amount of buffering repairs"
            ),
            Defect::OffGrid(count) => write!(
                f,
                "{count} packets carried a timestamp that is not a whole number of frames from \
                 the start of the stream, so the sender is running at another frame duration"
            ),
            Defect::NotTheTone { left, right } => write!(
                f,
                "the decoded audio reads {left:.1} Hz left and {right:.1} Hz right, not the \
                 contract's {:.0} and {:.0}; the path delivered something, and it was not this \
                 stream",
                CONTRACT.left_hz, CONTRACT.right_hz
            ),
            Defect::ChannelsNotDistinct => write!(
                f,
                "both channels read the same frequency, which is what one channel copied twice \
                 looks like"
            ),
        }
    }
}

/// What the sink and the receiver share.
///
/// The buffer is behind a lock because the two halves run on their own threads
/// on purpose: a single thread would have to interleave a socket read with the
/// sink's schedule, and a read timeout short enough not to disturb a 5 ms
/// cadence would turn the loop into a spin. The lock is held for a copy of
/// eighty-odd bytes at either end and for nothing else — decoding happens
/// outside it — so the two threads meet for about as long as it takes to touch
/// two cache lines, four hundred times a second.
struct Shared {
    buffer: Mutex<JitterBuffer>,
    /// When the first frame is due, in clock nanoseconds, or zero while the
    /// stream has not started. Published by the receiver so the sink knows when
    /// its schedule begins.
    playout_at: AtomicU64,
    /// The sending half has emitted its last datagram.
    sent_all: AtomicBool,
    /// The receiver should stop reading.
    stop: AtomicBool,
}

pub fn run(options: Options) -> Result<Measurement, ProbeError> {
    let config = CodecConfig::contract(options.frame, CodecConfig::DEFAULT_BITRATE_BPS);
    let frame_samples = config.frame_samples();
    let total_frames = (options.seconds * f64::from(config.sample_rate)).round() as u64;
    let packets = total_frames / frame_samples as u64;
    if packets == 0 {
        return Err(ProbeError::Codec(CodecError::NothingToEncode {
            frames: total_frames,
            frame_samples,
        }));
    }

    let receiving = UdpSocket::bind(options.bind).map_err(io("bind"))?;
    receiving
        .set_read_timeout(Some(RECV_TIMEOUT))
        .map_err(io("set_read_timeout"))?;
    // A socket of its own for the sender, on an ephemeral port. The fault relay
    // tells the two directions apart by address, so a sender sharing the
    // receiving port would be taken for the far end and relayed nowhere.
    let sending = UdpSocket::bind(SocketAddr::new(options.bind.ip(), 0)).map_err(io("bind"))?;

    let buffer = JitterBuffer::new(config, options.target);
    let target = buffer.target();
    let ceiling = buffer.ceiling();
    let slots = buffer.slots();
    let shared = Arc::new(Shared {
        buffer: Mutex::new(buffer),
        playout_at: AtomicU64::new(0),
        sent_all: AtomicBool::new(false),
        stop: AtomicBool::new(false),
    });

    // An SSRC of its own, drawn independently of the video stream's, so a
    // capture holding both can never attribute an audio packet to the picture.
    let ssrc = Ssrc(random_u32());
    let period = frame_period(&config);

    thread::scope(|scope| {
        let receiver = {
            let shared = Arc::clone(&shared);
            scope.spawn(move || receive(&receiving, &shared, ssrc, period))
        };
        let sink = {
            let shared = Arc::clone(&shared);
            let config = &config;
            scope.spawn(move || drain(&shared, config, packets))
        };

        // The sender gets a thread rather than running on the caller's, because it asks
        // for a scheduling deadline and the caller outlives the run: a thread left under
        // a time-constraint policy while it formats a report holds a reservation on the
        // machine that nothing needs.
        let sender = {
            let shared = Arc::clone(&shared);
            let sending = &sending;
            let config = &config;
            scope.spawn(move || {
                let report = transmit(sending, options.send_to, config, ssrc, packets);
                // Whatever the outcome. A send that failed halfway is still a stream
                // that has stopped, and the sink has to be told so it can finish.
                shared.sent_all.store(true, Ordering::Relaxed);
                report
            })
        };

        let send = sender
            .join()
            .expect("the sender returns its failures instead of panicking");
        let sink = sink
            .join()
            .expect("the sink reports its own failures instead of panicking");
        // The receiver outlives the sink so that datagrams a stall was holding
        // are still read and counted late. They are the point of the fault
        // arms: a packet that arrives after its moment is a different finding
        // from one that never arrives, and stopping here would merge them.
        thread::sleep(DRAIN_GRACE);
        shared.stop.store(true, Ordering::Relaxed);
        let receiver_scheduled_as = receiver
            .join()
            .expect("the receive thread reports its own failures instead of panicking");

        Ok(Measurement {
            config,
            bind: options.bind,
            send_to: options.send_to,
            target,
            ceiling,
            slots,
            send: send?,
            counts: shared.buffer.lock().counts(),
            receiver_scheduled_as,
            sink,
        })
    })
}

/// Encodes the contract tone, packetises it and paces it onto the socket.
///
/// Every deadline is absolute, computed from the start of the run rather than
/// by sleeping a frame period in a loop: a relative sleep accumulates its own
/// overshoot, and over thirty seconds that drift is larger than the frame this
/// run is built around. None of it reaches the timestamps, which count samples
/// and would be identical if the pacing were terrible — which is exactly the
/// property the receiving end depends on to derive a deadline.
///
/// The absolute schedule is why this thread asks for a deadline before it sends
/// anything. It stands in for a machine whose audio clock is running whatever
/// this one is doing, and a frame it emits late arrives late, is discarded past
/// its moment and is counted against a network that did nothing wrong.
fn transmit(
    socket: &UdpSocket,
    target: SocketAddr,
    config: &CodecConfig,
    ssrc: Ssrc,
    packets: u64,
) -> Result<SendReport, ProbeError> {
    let mut encoder = OpusEncoder::new(*config)?;
    let mut packetizer = OpusPacketizer::new(ssrc);
    let mut tone = Tone::new(CONTRACT);
    let mut pcm = vec![0f32; config.frame_interleaved()];
    let samples = config.frame_samples() as u32;

    let period = frame_period(config);
    let mut report = SendReport {
        packets: 0,
        send_failures: 0,
        scheduled_as: ScheduledAs::request(period.get()),
    };
    let started = Timestamp::now();

    for index in 0..packets {
        wait_until(started.add(Nanos(period.get() * index)));

        tone.fill_stereo(&mut pcm);
        let frame = encoder.encode(&pcm)?;
        let datagram = packetizer.next(frame, samples)?;
        match socket.send_to(datagram, target) {
            Ok(_) => report.packets += 1,
            Err(_) => report.send_failures += 1,
        }
    }
    Ok(report)
}

/// Reads datagrams and offers each one to the buffer.
///
/// It does no decoding and takes no timing decisions. Everything it knows about
/// a packet is in the packet; when the frame it carries is due is the buffer's
/// arithmetic, from the timestamp, and the arrival instant is passed on only so
/// that the very first packet can anchor the stream.
///
/// It asks for a deadline all the same, and the reasoning that said it would not
/// need one was wrong. The argument was that a receiver keeps no schedule: it
/// blocks in `recv_from`, the kernel holds its datagrams while it is away, and
/// the delay it adds is delivery jitter, which is what the target absorbs. What
/// that argument leaves out is that the target is the whole budget and it is ten
/// milliseconds. With the sink and the sender on deadlines and this thread left
/// at ordinary priority, a loopback run with no relay in it and no fault injected
/// reported thirteen packets past their moment at load average 23; with this
/// thread promoted too, the same run at load average 30 reported none. So it has
/// a period after all - one datagram per frame, the cadence the stream arrives at
/// - and it is asked for in those terms.
fn receive(socket: &UdpSocket, shared: &Shared, expect: Ssrc, period: Nanos) -> ScheduledAs {
    let scheduled_as = ScheduledAs::request(period.get());
    let mut datagram = [0u8; MAX_UDP_PAYLOAD];
    loop {
        let length = match socket.recv_from(&mut datagram) {
            Ok((length, _from)) => length,
            Err(error) => {
                let timed_out = matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                );
                if !timed_out || shared.stop.load(Ordering::Relaxed) {
                    return scheduled_as;
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
        if packet.ssrc != expect {
            continue;
        }

        let mut buffer = shared.buffer.lock();
        let first = buffer.playout_start().is_none();
        buffer.push(&packet, at);
        if first && let Some(playout) = buffer.playout_start() {
            // Published after the push, so the sink can never start pulling
            // against a buffer that has not yet been anchored.
            shared
                .playout_at
                .store(playout.as_nanos(), Ordering::Release);
        }
    }
}

/// The synthetic sink: pulls one frame every frame period and decodes or
/// conceals whatever it is handed.
///
/// The schedule is absolute, from the first frame's deadline, so a pull that
/// runs late does not push the next one later still. That matters more here
/// than in the sender: the cursor advances one frame per pull, so a sink whose
/// schedule drifted would move every deadline in the stream with it and the
/// buffer would be absorbing the sink's own jitter instead of the network's.
///
/// Which is why the deadline is asked for here, before the stream has started
/// and while there is still nothing to be late for. The period is the frame
/// period, because that is the cycle a pull has to keep.
fn drain(shared: &Shared, config: &CodecConfig, packets: u64) -> SinkReport {
    let period = frame_period(config);
    let mut report = SinkReport {
        occupancy_us: None,
        interval_us: None,
        decode_us: None,
        conceal_us: None,
        decode_failures: 0,
        tone: ToneReport::empty(),
        never_started: false,
        scheduled_as: ScheduledAs::request(period.get()),
    };

    let mut decoder = match OpusDecoder::new(*config) {
        Ok(decoder) => decoder,
        Err(_) => {
            report.never_started = true;
            return report;
        }
    };

    let Some(playout) = await_playout(shared) else {
        report.never_started = true;
        return report;
    };

    let frame_us = u64::from(config.frame.millis()) * 1_000;
    let format = decoded_format(config);
    // Sized and allocated once, so filling it never puts a reallocation between
    // two pulls.
    let mut window = Vec::with_capacity(ANALYSIS_FRAMES * format.frame_bytes());
    let mut skipped = 0usize;

    // Room for the whole run plus the frames the buffer is behind by; the
    // percentiles must describe every pull rather than a prefix of them.
    let capacity = packets as usize + 64;
    let mut occupancy_us = Samples::with_capacity(capacity);
    let mut interval_us = Samples::with_capacity(capacity);
    let mut decode_us = Samples::with_capacity(capacity);
    let mut conceal_us = Samples::with_capacity(capacity);

    let mut payload = [0u8; MAX_OPUS_PAYLOAD];
    let mut previous: Option<Timestamp> = None;
    let mut index = 0u64;

    loop {
        wait_until(playout.add(Nanos(period.get() * index)));
        index += 1;

        // Asked before pulling, because a sink that stops the moment the last
        // frame has been played never records an underrun the path did not
        // cause. Everything the stream ever sent has by then been played,
        // concealed or discarded.
        if shared.sent_all.load(Ordering::Relaxed) && shared.buffer.lock().drained() {
            break;
        }

        let at = Timestamp::now();
        if let Some(earlier) = previous {
            interval_us.record(at.saturating_since(earlier).get() / 1_000);
        }
        previous = Some(at);

        let pulled = {
            let mut buffer = shared.buffer.lock();
            buffer.pull(&mut payload)
        };
        occupancy_us.record(pulled.occupancy as u64 * frame_us);

        let started = Timestamp::now();
        let decoded = match pulled.outcome {
            Pull::Frame(len) => {
                let decoded = decoder.decode(&payload[..len]);
                decode_us.record(Timestamp::now().saturating_since(started).get() / 1_000);
                decoded
            }
            // Both conceal, and the sink is handed samples either way: a render
            // callback given nothing produces a click, and this phase must not
            // invent a failure the next phase will not have.
            Pull::Conceal | Pull::Underrun => {
                let concealed = decoder.conceal();
                conceal_us.record(Timestamp::now().saturating_since(started).get() / 1_000);
                concealed
            }
        };

        match decoded {
            Ok(pcm) => {
                // Concealed samples go into the window with the rest, because
                // the window is what the listener heard. A path that concealed
                // everything therefore reads as the near-silence the concealer
                // decays to, which is the point.
                if skipped < ANALYSIS_SKIP_FRAMES {
                    skipped += pcm.len() / config.channels as usize;
                } else if window.len() < window.capacity() {
                    for sample in pcm {
                        window.extend_from_slice(&sample.to_le_bytes());
                    }
                }
            }
            Err(_) => report.decode_failures += 1,
        }
    }

    report.occupancy_us = occupancy_us.percentiles();
    report.interval_us = interval_us.percentiles();
    report.decode_us = decode_us.percentiles();
    report.conceal_us = conceal_us.percentiles();
    report.tone = analyse(&format, &window);
    report
}

/// Waits for the receiver to publish the first frame's deadline.
fn await_playout(shared: &Shared) -> Option<Timestamp> {
    let deadline = Timestamp::now().add(Nanos(FIRST_PACKET_WAIT.as_nanos() as u64));
    loop {
        let published = shared.playout_at.load(Ordering::Acquire);
        if published != 0 {
            return Some(Timestamp::from_nanos(published));
        }
        if Timestamp::now() >= deadline {
            return None;
        }
        // A poll rather than a condition variable: this runs once, before the
        // schedule starts, and a millisecond of latency on the very first frame
        // is absorbed by the target.
        thread::sleep(Duration::from_millis(1));
    }
}

const NOTHING: Percentiles = Percentiles {
    count: 0,
    min: 0,
    p50: 0,
    p95: 0,
    p99: 0,
    max: 0,
};

fn millis(micros: u64) -> f64 {
    micros as f64 / 1_000.0
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let counts = self.counts;
        let occupancy = self.sink.occupancy_us.unwrap_or(NOTHING);
        let interval = self.sink.interval_us.unwrap_or(NOTHING);
        let decode = self.sink.decode_us.unwrap_or(NOTHING);
        let conceal = self.sink.conceal_us.unwrap_or(NOTHING);

        writeln!(
            f,
            "target ms {}",
            self.target.as_millis_f64().round() as u64
        )?;
        // Ahead of every counter below, because the counters mean one thing under a
        // deadline and another without one, and a reader who takes the numbers before
        // reading this has already been misled.
        writeln!(f, "sink scheduled as {}", self.sink.scheduled_as)?;
        writeln!(f, "sender scheduled as {}", self.send.scheduled_as)?;
        writeln!(f, "receiver scheduled as {}", self.receiver_scheduled_as)?;
        writeln!(
            f,
            "every deadline granted {}",
            yes_no(self.deadlines_were_granted())
        )?;
        writeln!(f, "packets received {}", counts.received)?;
        writeln!(f, "packets late {}", counts.late)?;
        writeln!(f, "packets duplicate {}", counts.duplicate)?;
        writeln!(f, "packets reordered {}", counts.reordered)?;
        writeln!(f, "frames played {}", counts.played)?;
        writeln!(f, "frames concealed {}", counts.concealed)?;
        writeln!(
            f,
            "occupancy ms p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}",
            millis(occupancy.p50),
            millis(occupancy.p95),
            millis(occupancy.p99),
            millis(occupancy.max)
        )?;
        writeln!(f, "underruns {}", counts.underruns)?;
        writeln!(
            f,
            "overruns {} dropping {} frames",
            counts.overruns, counts.overrun_frames
        )?;
        writeln!(f, "decode us p50 {} p99 {}", decode.p50, decode.p99)?;
        writeln!(
            f,
            "sink interval us p50 {} p99 {}",
            interval.p50, interval.p99
        )?;
        writeln!(
            f,
            "continuity expected {} played {}",
            counts.expected_samples, counts.played_samples
        )?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(self.sink.tone.left),
            hertz(self.sink.tone.right)
        )?;

        // Everything below is for a person reading the run rather than for the
        // harness parsing it, in the order somebody asking "why those numbers"
        // would want it.
        writeln!(
            f,
            "tone channels distinct {}",
            yes_no(self.sink.tone.distinct())
        )?;
        writeln!(
            f,
            "continuity hole {} samples over {} frame periods",
            counts.continuity_hole(),
            counts.expected_samples / self.frame_samples().max(1)
        )?;
        writeln!(f, "packets sent {}", self.send.packets)?;
        writeln!(f, "send failures {}", self.send.send_failures)?;
        writeln!(f, "packets off grid {}", counts.off_grid)?;
        writeln!(f, "packets oversize {}", counts.oversize)?;
        writeln!(f, "decode failures {}", self.sink.decode_failures)?;
        writeln!(
            f,
            "ceiling ms {} over {} slots",
            self.ceiling.as_millis_f64().round() as u64,
            self.slots
        )?;
        writeln!(
            f,
            "occupancy ms min {:.1} count {}",
            millis(occupancy.min),
            occupancy.count
        )?;
        writeln!(
            f,
            "sink interval us min {} max {} count {}",
            interval.min, interval.max, interval.count
        )?;
        writeln!(f, "conceal us p50 {} p99 {}", conceal.p50, conceal.p99)?;
        writeln!(f, "frame ms {}", self.config.frame.millis())?;
        writeln!(f, "bind {} send to {}", self.bind, self.send_to)?;
        writeln!(
            f,
            "tone resolution {:.2} hz over {} frames",
            self.sink.tone.resolution_hz, self.sink.tone.analysed_frames
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jitter::Counts;

    fn measurement(counts: Counts, tone: ToneReport, decode_failures: u64) -> Measurement {
        let config = CodecConfig::contract(FrameDuration::Ms5, CodecConfig::DEFAULT_BITRATE_BPS);
        Measurement {
            config,
            bind: "127.0.0.1:5010".parse().expect("literal"),
            send_to: "127.0.0.1:5010".parse().expect("literal"),
            target: Nanos::from_millis(10),
            ceiling: Nanos::from_millis(30),
            slots: 8,
            send: SendReport {
                packets: 1_000,
                send_failures: 0,
                scheduled_as: granted(),
            },
            counts,
            receiver_scheduled_as: granted(),
            sink: SinkReport {
                occupancy_us: None,
                interval_us: None,
                decode_us: None,
                conceal_us: None,
                decode_failures,
                tone,
                never_started: false,
                scheduled_as: granted(),
            },
        }
    }

    /// The deadline these fixtures are written as having been granted. A run that was
    /// refused one is a different report, and the test that cares says so itself.
    fn granted() -> ScheduledAs {
        ScheduledAs::TimeConstraint {
            period_ns: 5_000_000,
            computation_ns: 625_000,
            constraint_ns: 2_500_000,
        }
    }

    fn contract_tone() -> ToneReport {
        ToneReport {
            left: Some(lanplay_audio_capture::goertzel::Tone {
                frequency: CONTRACT.left_hz,
                level_dbfs: -20.0,
            }),
            right: Some(lanplay_audio_capture::goertzel::Tone {
                frequency: CONTRACT.right_hz,
                level_dbfs: -20.0,
            }),
            resolution_hz: 2.0,
            analysed_frames: 24_000,
        }
    }

    fn healthy() -> Counts {
        Counts {
            received: 1_000,
            played: 1_000,
            expected_samples: 240_000,
            played_samples: 240_000,
            ..Counts::default()
        }
    }

    #[test]
    fn a_run_that_played_only_concealment_is_a_defect_however_good_the_counters_look() {
        // The shape of mistake this phase is built against: every frame period
        // served, nothing missing, and no audio in it at all.
        let counts = Counts {
            received: 0,
            played: 0,
            concealed: 1_000,
            underruns: 1_000,
            expected_samples: 240_000,
            played_samples: 0,
            ..Counts::default()
        };
        let run = measurement(counts, ToneReport::empty(), 0);
        assert_eq!(run.defect(), Some(Defect::NothingArrived));

        // And with packets arriving but none of them playable, the name changes
        // rather than the verdict.
        let counts = Counts {
            received: 1_000,
            late: 1_000,
            ..counts
        };
        let run = measurement(counts, ToneReport::empty(), 0);
        assert_eq!(run.defect(), Some(Defect::NothingPlayed));
    }

    #[test]
    fn loss_and_concealment_are_measurements_rather_than_defects() {
        // A run behind a lossy relay has to report its numbers and succeed. A
        // probe that failed on loss would be refusing to produce the figure it
        // exists to produce.
        let counts = Counts {
            received: 950,
            late: 12,
            played: 938,
            concealed: 62,
            underruns: 3,
            expected_samples: 240_000,
            played_samples: 240_000,
            ..Counts::default()
        };
        assert_eq!(measurement(counts, contract_tone(), 0).defect(), None);
    }

    #[test]
    fn audio_that_is_not_the_contract_tone_is_a_defect() {
        let mut tone = contract_tone();
        tone.left = Some(lanplay_audio_capture::goertzel::Tone {
            frequency: 440.0,
            level_dbfs: -20.0,
        });
        match measurement(healthy(), tone, 0).defect() {
            Some(Defect::NotTheTone { left, .. }) => assert_eq!(left, 440.0),
            other => panic!("440 Hz on the left was accepted as the contract tone: {other:?}"),
        }
    }

    #[test]
    fn a_healthy_run_has_no_defect() {
        assert_eq!(measurement(healthy(), contract_tone(), 0).defect(), None);
    }

    #[test]
    fn the_keyed_lines_are_the_ones_the_harness_reads() {
        // The wording is an interface. Checking it here means a change to it
        // fails a test rather than a harness.
        let counts = Counts {
            received: 1_000,
            late: 4,
            duplicate: 2,
            reordered: 7,
            played: 996,
            concealed: 6,
            underruns: 1,
            overruns: 1,
            overrun_frames: 5,
            expected_samples: 240_480,
            played_samples: 239_040,
            ..Counts::default()
        };
        let printed = measurement(counts, contract_tone(), 0).to_string();
        let keyed: Vec<&str> = printed.lines().take(19).collect();
        assert_eq!(
            keyed,
            vec![
                "target ms 10",
                "sink scheduled as time constraint, period 5.000 ms computation 0.625 ms \
                 constraint 2.500 ms",
                "sender scheduled as time constraint, period 5.000 ms computation 0.625 ms \
                 constraint 2.500 ms",
                "receiver scheduled as time constraint, period 5.000 ms computation 0.625 ms \
                 constraint 2.500 ms",
                "every deadline granted yes",
                "packets received 1000",
                "packets late 4",
                "packets duplicate 2",
                "packets reordered 7",
                "frames played 996",
                "frames concealed 6",
                "occupancy ms p50 0.0 p95 0.0 p99 0.0 max 0.0",
                "underruns 1",
                "overruns 1 dropping 5 frames",
                "decode us p50 0 p99 0",
                "sink interval us p50 0 p99 0",
                "continuity expected 240480 played 239040",
                "tone left 997.0 right 1997.0",
                "tone channels distinct yes",
            ]
        );
    }

    #[test]
    fn a_run_that_was_refused_its_deadline_says_so_where_the_numbers_are_read() {
        // The whole reason the policy is in the report. A run taken at ordinary priority
        // measures the scheduler as much as the buffer, and nothing else in these lines
        // would let a reader tell the two apart afterwards.
        let mut run = measurement(healthy(), contract_tone(), 0);
        run.receiver_scheduled_as =
            ScheduledAs::Default("thread_policy_set returned 46".to_owned());
        let printed = run.to_string();
        assert!(
            printed
                .contains("receiver scheduled as default priority: thread_policy_set returned 46"),
            "{printed}"
        );
        assert!(printed.contains("every deadline granted no"), "{printed}");
    }
}
