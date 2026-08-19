//! Opus over RTP over UDP, and every packet accounted for.
//!
//! The previous phase measured the codec with nothing between the encoder and
//! the decoder. This one puts a socket there and asks what the socket does to
//! the stream.
//!
//! Why the probe lives beside the codec rather than beside the packetiser: the
//! packetiser takes encoded bytes and a sample count, so `lanplay-transport`
//! carries no dependency on libopus and can still be cross-checked for Windows
//! from a machine with no C toolchain. A program that needs both halves has to
//! sit on the side that already vendors the C.
//!
//! It runs two ways, and the difference is not a convenience.
//!
//! With `--send-to` it sends and receives in one process. That is what makes
//! byte-for-byte verification possible: the sender keeps a digest of every
//! payload and the receiver claims it, so a payload whose bytes changed is
//! caught. Two processes could only compare digests by sending them, which would
//! put the verification on the same link it is trying to measure.
//!
//! With `--receive-only` there is no sender here at all, and the peer is another
//! machine. Then the digest ledger is not merely empty, it is meaningless, and
//! the report says so rather than printing zero verified: a zero there would
//! read as total corruption when what it means is that the question does not
//! apply. What stands in for it is the tone. A receiver that decodes what
//! arrived and finds 997 Hz on the left and 1997 Hz on the right has proved the
//! path carried real audio, which is the property the digests were standing in
//! for. Everything else — sequence gaps, timestamp deltas, reordering,
//! duplicates, datagram sizes, arrival intervals — is a property of the arriving
//! stream and needs no sender in this process to be true.
//!
//! That second mode exists because of something measured rather than assumed:
//! sending to this machine's own routable address does not put the traffic on
//! the air. A datagram addressed to a local interface never reaches the driver —
//! the interface counters say so, 1000 packets sent to 192.168.1.108 leaving
//! 1016 on lo0 and nothing above background on en0, against 1091 on en0 for the
//! same 1000 sent to the router. A run against its own address measures the
//! loopback path a second time with a different address on it, and reports the
//! zero loss such a path deserves. A loss figure that belongs to the radio needs
//! two machines, one sending and one receiving.
//!
//! Nothing here conceals a gap. A missing sequence number is reported and the
//! decoder is simply not fed, so the tone measured at the end is the tone that
//! actually arrived. That seam is deliberately empty: a concealer put here would
//! make the loss figure smaller and the audio no better, and the next phase
//! cannot size a jitter buffer against a number that has already been improved.
//!
//! Corruption and absence are counted apart, because they are different faults.
//! A datagram arrives whole or does not arrive at all — a UDP checksum failure
//! is a drop, not a delivery — so a payload whose bytes differ from what was
//! sent is a defect in this code, while a payload that never came is the link's.
//! A single counter covering both would let the first hide inside the second.
//!
//! And the audio is decoded and measured rather than assumed to be audio. A
//! packet count and a byte count agree just as happily when every payload is
//! plausible rubbish, and this project has read that agreement as success four
//! times.

use core::fmt;
use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_audio_capture::analysis::hertz;
use lanplay_audio_capture::{Percentiles, Samples, ToneReport, analyse};
use lanplay_telemetry::{Nanos, Timestamp, wait_until};
use lanplay_tone_source::tone::{CONTRACT, Tone};
use lanplay_transport::{
    MAX_UDP_PAYLOAD, OPUS_PAYLOAD_TYPE, OpusPacketizeError, OpusPacketizer, OpusParseError,
    RtpTimestamp, SequenceNumber, Ssrc, parse_opus_packet, random_u32,
};
use sha2::{Digest, Sha256};

use crate::config::{CodecConfig, FrameDuration};
use crate::decoder::OpusDecoder;
use crate::encoder::OpusEncoder;
use crate::error::CodecError;
use crate::probe::{ANALYSIS_FRAMES, ANALYSIS_SKIP_FRAMES, decoded_format};

/// Payloads kept while their packet is in flight, following the video path's
/// ledger.
///
/// A thousand packets is five seconds at 5 ms, which is several hundred times
/// any delay this link can impose. The bound is what stops verification from
/// becoming a leak when the far end goes quiet.
const VERIFY_WINDOW: usize = 1_024;

/// Sequence numbers the duplicate check remembers.
///
/// Twenty seconds at 5 ms. A copy of a packet arriving later than that would be
/// counted as a fresh arrival, which is a limit worth stating: on a LAN a
/// duplicate comes from a retransmitting driver or a bridged interface and
/// follows within milliseconds.
const SEEN_WINDOW: usize = 4_096;

/// How long the receive loop blocks before asking whether it should stop.
const RECV_TIMEOUT: Duration = Duration::from_millis(100);

/// How long the receiver keeps reading after the sender in this process has
/// finished.
///
/// Sized for a link, not for loopback: the last datagrams may still be in the
/// kernel or in the air when the send loop ends, and stopping the moment it does
/// would report them as lost.
const DRAIN_GRACE: Duration = Duration::from_millis(300);

/// Silence that ends a receive-only run.
///
/// Two seconds is four hundred frame periods, so no jitter or retry a LAN can
/// impose looks like this. It exists because a receiver waiting on a peer cannot
/// know when the peer stopped: without it the run would sit out its whole window
/// after the last packet, and an operator would learn nothing for the wait. A
/// stall long enough to trip it is reported, so a run that ended early says so
/// rather than quietly describing a prefix of the stream.
const IDLE_STOP: Duration = Duration::from_secs(2);

/// How far the decoded tone may sit from the contract before the audio is no
/// longer the audio that was sent.
///
/// The detector's own bin spacing over the analysis window is 2 Hz, so this is
/// beyond what the measurement can resolve and still four hundred times narrower
/// than the gap between the two contract tones. Nothing that is the tone lands
/// outside it, and nothing that is not lands inside.
pub const TONE_TOLERANCE_HZ: f64 = 5.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Options {
    pub bind: SocketAddr,
    /// Where the packets go, or `None` to listen for a peer and send nothing.
    ///
    /// One field rather than an address beside a flag, so a run cannot claim to
    /// be receive-only and send at the same time: the two would be different
    /// experiments described by one report.
    pub send_to: Option<SocketAddr>,
    pub seconds: f64,
    pub frame: FrameDuration,
}

#[derive(Debug)]
pub enum ProbeError {
    Codec(CodecError),
    /// The packetiser refused. Reaching this means the codec produced something
    /// the payload format cannot carry, which is a finding rather than a hiccup.
    Packetize(OpusPacketizeError),
    /// A socket call failed, named, because an error number without its callsite
    /// is a number nobody can act on.
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

fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// What the sender put on the wire, so the receiver can say whether the bytes
/// that came back are the bytes that went out.
///
/// The same shape as the video path's `VerifyLedger`: the sender records a
/// digest under the stream's own identifier, the receiver claims it once, and
/// `None` means the entry has aged out rather than that anything disagreed. One
/// deviation, and it is deliberate — the video ledger drains everything older
/// than the entry it matched, because access units complete in order and a
/// skipped one is genuinely lost. A reordered datagram is a normal event on a
/// radio, so here the matched entry is removed and the ones in front of it are
/// left for the packets that may still be behind it.
pub struct VerifyLedger {
    entries: parking_lot::Mutex<VecDeque<(SequenceNumber, [u8; 32])>>,
}

impl VerifyLedger {
    fn new() -> Arc<Self> {
        Arc::new(VerifyLedger {
            entries: parking_lot::Mutex::new(VecDeque::with_capacity(VERIFY_WINDOW)),
        })
    }

    /// Called before the datagram is handed to the socket, so a payload can
    /// never arrive before the digest it will be checked against.
    fn record(&self, sequence: SequenceNumber, payload: &[u8]) {
        let mut entries = self.entries.lock();
        if entries.len() == VERIFY_WINDOW {
            entries.pop_front();
        }
        entries.push_back((sequence, digest(payload)));
    }

    /// Whether this payload is the one that was sent under this sequence
    /// number, or `None` when the original has aged out.
    fn check(&self, sequence: SequenceNumber, payload: &[u8]) -> Option<bool> {
        let mut entries = self.entries.lock();
        let position = entries.iter().position(|(seq, _)| *seq == sequence)?;
        let (_, expected) = entries.remove(position)?;
        Some(digest(payload) == expected)
    }
}

/// What comparing received bytes against sent bytes amounted to.
///
/// An enumeration rather than three counters, because across two machines there
/// is nothing to compare against and every count would be zero. Zero verified
/// and zero mismatched is indistinguishable from a path that corrupted
/// everything, and this is exactly the shape of mistake the project keeps
/// making: a number that reads as a finding when it is the absence of one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verification {
    /// Both halves in one process, so every arrival was compared against the
    /// digest of the payload that was sent under its sequence number.
    Digests {
        verified: u64,
        mismatched: u64,
        /// Arrivals whose sent payload had aged out of the ledger.
        unverifiable: u64,
    },
    /// The sender is another machine. The tone at the bottom of the report is
    /// what proves the path carried real audio instead.
    NotApplicable,
}

impl Verification {
    fn record(&mut self, outcome: Option<bool>) {
        if let Verification::Digests {
            verified,
            mismatched,
            unverifiable,
        } = self
        {
            match outcome {
                Some(true) => *verified += 1,
                Some(false) => *mismatched += 1,
                None => *unverifiable += 1,
            }
        }
    }
}

/// What the sending half did.
#[derive(Debug)]
pub struct SendReport {
    pub packets: u64,
    /// Datagram bytes handed to the socket. IP and UDP headers are not counted,
    /// because the packetiser is what this phase is measuring and 28 bytes of
    /// kernel header per packet is not its doing.
    pub wire_bytes: u64,
    pub largest_datagram: usize,
    pub send_us: Option<Percentiles>,
    /// Datagrams the socket refused. Counted rather than fatal: a run that
    /// aborted here would print nothing about the run that found it.
    pub send_failures: u64,
}

/// What arrived, and what the stream's own arithmetic said about it.
#[derive(Debug)]
pub struct ReceiveReport {
    /// The stream's synchronisation source: the sender's own when it runs here,
    /// and otherwise the first one seen, so a stray sender is counted as foreign
    /// rather than mixed into the measurement.
    pub ssrc: Option<Ssrc>,
    /// Distinct packets of this stream that arrived. A duplicate is the same
    /// packet twice and is not counted here: letting it would allow a chatty
    /// link to hide a packet it had lost.
    pub packets: u64,
    pub bytes: u64,
    pub largest_datagram: usize,
    /// Consecutive pairs whose timestamps differed by exactly the sample count
    /// their sequence numbers call for, out of the pairs examined. The
    /// normalisation by sequence distance is what makes this a statement about
    /// the sender's arithmetic rather than about the link: two frames of loss
    /// between a pair means two frames of samples, and a link cannot change
    /// that.
    pub timestamp_pairs: u64,
    pub timestamp_exact: u64,
    pub sequence_gaps: u64,
    pub packets_missing: u64,
    /// Arrivals older than the highest sequence number already seen. Counted
    /// apart from the gaps they fill, because a gap that a late packet closes
    /// still happened and a receiver with no buffer still saw it.
    pub reordered: u64,
    pub duplicates: u64,
    pub verification: Verification,
    /// Datagrams that were not RTP at all.
    pub not_rtp: u64,
    /// RTP carrying another stream's payload type, which on a socket of its own
    /// means a stray sender rather than something to demultiplex.
    pub wrong_payload_type: u64,
    pub empty_payload: u64,
    /// RTP of the right type from an SSRC that is not this stream's.
    pub foreign_ssrc: u64,
    pub frames_decoded: u64,
    pub decode_failures: u64,
    pub arrival_us: Option<Percentiles>,
    pub tone: ToneReport,
    /// Whether the run ended because the stream went silent for [`IDLE_STOP`]
    /// rather than because its window closed.
    pub ended_on_silence: bool,
    /// A receive call that failed for a reason other than its timeout, kept so
    /// that a truncated run says why it was truncated.
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Measurement {
    pub config: CodecConfig,
    pub bind: SocketAddr,
    /// Absent in a receive-only run, which is also why [`Measurement::send`] is.
    pub send_to: Option<SocketAddr>,
    pub send: Option<SendReport>,
    pub receive: ReceiveReport,
}

impl Measurement {
    /// The stream's SSRC: this process's when it sent, and the peer's when it
    /// only listened.
    pub fn ssrc(&self) -> Option<Ssrc> {
        self.receive.ssrc
    }

    /// Packets that went out of this process and never came back, or `None` when
    /// nothing went out of it. A receiver cannot subtract what it never sent;
    /// what it can say about loss is the sequence total.
    pub fn lost(&self) -> Option<u64> {
        let send = self.send.as_ref()?;
        Some(send.packets.saturating_sub(self.receive.packets))
    }

    pub fn loss_percent(&self) -> Option<f64> {
        let send = self.send.as_ref()?;
        if send.packets == 0 {
            return Some(0.0);
        }
        Some(self.lost()? as f64 * 100.0 / send.packets as f64)
    }

    /// The rate the stream cost on the wire, from the datagrams themselves
    /// rather than from the bitrate that was requested: sent when this process
    /// sent them, and otherwise received.
    pub fn effective_kbps(&self) -> f64 {
        let (packets, bytes) = match &self.send {
            Some(send) => (send.packets, send.wire_bytes),
            None => (self.receive.packets, self.receive.bytes),
        };
        let seconds = packets as f64 * self.config.frame.seconds();
        if seconds == 0.0 {
            return 0.0;
        }
        bytes as f64 * 8.0 / seconds / 1_000.0
    }

    /// Whether the decoded audio is the tone that was sent.
    ///
    /// The only proof of content a receive-only run has, so it is the criterion
    /// there. Both channels must be right and they must differ, because two
    /// channels reading the same frequency is what a folded mix or a buffer read
    /// twice looks like.
    pub fn tone_is_the_contract(&self) -> bool {
        let tone = &self.receive.tone;
        let close = |measured: f64, expected: f64| (measured - expected).abs() <= TONE_TOLERANCE_HZ;
        tone.distinct()
            && close(hertz(tone.left), CONTRACT.left_hz)
            && close(hertz(tone.right), CONTRACT.right_hz)
    }

    /// Why this run is not a measurement, or `None` when it is one.
    ///
    /// Loss is never the answer. It is the number this phase exists to produce,
    /// and a probe that failed on it would be refusing to report its own
    /// measurement. Everything below is instead a reason the numbers cannot be
    /// believed, named so that a run which measured nothing says which nothing
    /// it measured: an instrument that sits quiet for thirty seconds and then
    /// prints zeroes is the shape this project keeps having to delete.
    ///
    /// What counts differs by mode in exactly one place. With both halves here, a
    /// payload whose bytes changed is a defect; with a peer, there are no digests
    /// to disagree, and the tone takes over the job of saying that what arrived
    /// was the audio that was sent.
    pub fn defect(&self) -> Option<Defect> {
        if let Some(error) = &self.receive.error {
            return Some(Defect::ReceiveFailed(error.clone()));
        }
        if self.send.as_ref().is_some_and(|send| send.packets == 0) {
            return Some(Defect::NothingSent);
        }
        if self.receive.packets == 0 {
            return Some(Defect::NothingArrived {
                listening_only: self.send.is_none(),
            });
        }
        if self.receive.timestamp_exact != self.receive.timestamp_pairs {
            return Some(Defect::TimestampIsNotASampleCounter {
                exact: self.receive.timestamp_exact,
                pairs: self.receive.timestamp_pairs,
            });
        }
        if let Verification::Digests {
            mismatched,
            unverifiable,
            ..
        } = self.receive.verification
            && (mismatched > 0 || unverifiable > 0)
        {
            return Some(Defect::PayloadChanged {
                mismatched,
                unverifiable,
            });
        }
        if self.receive.decode_failures > 0 {
            return Some(Defect::UndecodablePayload {
                failures: self.receive.decode_failures,
            });
        }
        if self.send.is_none() && !self.tone_is_the_contract() {
            return Some(Defect::ToneIsNotTheContract {
                left: hertz(self.receive.tone.left),
                right: hertz(self.receive.tone.right),
            });
        }
        None
    }

    pub fn sound(&self) -> bool {
        self.defect().is_none()
    }
}

/// A reason the run's numbers cannot be believed.
///
/// Named rather than a bare failure, because the two ways a run measures nothing
/// look identical in the numbers and are opposite in what they ask of whoever
/// ran it: a sending run that received nothing back has a path to fix, and a
/// listening run that received nothing was pointed at a peer that never sent.
#[derive(Clone, PartialEq, Debug)]
pub enum Defect {
    /// The send loop produced no datagram at all.
    NothingSent,
    NothingArrived {
        listening_only: bool,
    },
    /// The one thing the phase turns on: a timestamp that is not a sample
    /// counter leaves a receiver unable to tell a late packet from a packet
    /// describing a later moment.
    TimestampIsNotASampleCounter {
        exact: u64,
        pairs: u64,
    },
    /// Bytes that arrived and differed, or arrived too late to compare. Never
    /// the link: a datagram arrives whole or does not arrive.
    PayloadChanged {
        mismatched: u64,
        unverifiable: u64,
    },
    UndecodablePayload {
        failures: u64,
    },
    /// A peer run whose decoded audio is not the tone that was sent, which is
    /// the only content check such a run has.
    ToneIsNotTheContract {
        left: f64,
        right: f64,
    },
    ReceiveFailed(String),
}

impl fmt::Display for Defect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Defect::NothingSent => {
                f.write_str("no datagram was sent, so every figure below is an absence")
            }
            Defect::NothingArrived {
                listening_only: true,
            } => f.write_str(
                "no packet of this stream arrived: this end listened and no peer sent, and a run \
                 of zeroes is not a measurement of a link",
            ),
            Defect::NothingArrived {
                listening_only: false,
            } => f.write_str("packets were sent and none came back"),
            Defect::TimestampIsNotASampleCounter { exact, pairs } => write!(
                f,
                "{exact} of {pairs} timestamp deltas counted samples exactly; the timestamp is a \
                 sample counter and no link can change it"
            ),
            Defect::PayloadChanged {
                mismatched,
                unverifiable,
            } => write!(
                f,
                "{mismatched} payloads differed from what was sent and {unverifiable} arrived too \
                 late to compare; a datagram arrives whole or not at all, so this is ours"
            ),
            Defect::UndecodablePayload { failures } => {
                write!(f, "{failures} payloads would not decode as Opus")
            }
            Defect::ToneIsNotTheContract { left, right } => write!(
                f,
                "the decoded tone is {left:.1} / {right:.1} Hz against {:.0} / {:.0}, so nothing \
                 proves the path carried the audio that was sent",
                CONTRACT.left_hz, CONTRACT.right_hz
            ),
            Defect::ReceiveFailed(error) => write!(f, "the receive loop failed: {error}"),
        }
    }
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

    let socket = UdpSocket::bind(options.bind).map_err(io("bind"))?;
    socket
        .set_read_timeout(Some(RECV_TIMEOUT))
        .map_err(io("set_read_timeout"))?;
    // One socket, cloned rather than a second binding, so a run that sends can
    // send to the very port it listens on.
    let receiving = socket.try_clone().map_err(io("try_clone"))?;

    // An SSRC of its own, drawn independently of the video stream's, so a
    // capture holding both can never attribute an audio packet to the picture.
    // A receive-only run has none of its own and adopts the peer's.
    let ssrc = options.send_to.map(|_| Ssrc(random_u32()));
    let ledger = options.send_to.map(|_| VerifyLedger::new());
    let stop = Arc::new(AtomicBool::new(false));

    thread::scope(|scope| {
        let receiver = {
            let ledger = ledger.clone();
            let stop = Arc::clone(&stop);
            let config = &config;
            scope
                .spawn(move || receive(&receiving, config, ssrc, ledger.as_deref(), &stop, packets))
        };

        let send = match (options.send_to, ledger.as_deref()) {
            (Some(target), Some(ledger)) => {
                let report = transmit(
                    &socket,
                    target,
                    &config,
                    ssrc.expect("a sending run drew an SSRC"),
                    packets,
                    ledger,
                );
                thread::sleep(DRAIN_GRACE);
                Some(report)
            }
            // Nothing to send, so the window is the only thing that bounds the
            // wait for a peer that may never appear.
            _ => {
                wait_until(Timestamp::now().add(Nanos::from_millis_f64(options.seconds * 1_000.0)));
                None
            }
        };
        stop.store(true, Ordering::Relaxed);
        let receive = receiver
            .join()
            .expect("the receive thread reports its own failures instead of panicking");

        Ok(Measurement {
            config,
            bind: options.bind,
            send_to: options.send_to,
            send: send.transpose()?,
            receive,
        })
    })
}

/// Encodes the contract tone, packetises it and paces it onto the socket.
///
/// Every deadline is absolute, computed from the start of the run rather than by
/// sleeping a frame period in a loop: a relative sleep accumulates its own
/// overshoot, and over thirty seconds that drift is larger than the interval
/// being measured. None of it reaches the timestamps, which count samples and
/// would be identical if the pacing were terrible.
fn transmit(
    socket: &UdpSocket,
    target: SocketAddr,
    config: &CodecConfig,
    ssrc: Ssrc,
    packets: u64,
    ledger: &VerifyLedger,
) -> Result<SendReport, ProbeError> {
    let mut encoder = OpusEncoder::new(*config)?;
    let mut packetizer = OpusPacketizer::new(ssrc);
    // The generator's contract is the codec's, 48000 Hz stereo, so nothing here
    // converts anything.
    let mut tone = Tone::new(CONTRACT);
    let mut pcm = vec![0f32; config.frame_interleaved()];
    let mut send_us = Samples::with_capacity(packets as usize);
    let samples = config.frame_samples() as u32;

    let period = Nanos(u64::from(config.frame.millis()) * 1_000_000);
    let started = Timestamp::now();
    let mut report = SendReport {
        packets: 0,
        wire_bytes: 0,
        largest_datagram: 0,
        send_us: None,
        send_failures: 0,
    };

    for index in 0..packets {
        wait_until(started.add(Nanos(period.get() * index)));

        tone.fill_stereo(&mut pcm);
        let frame = encoder.encode(&pcm)?;
        let sequence = packetizer.next_sequence();
        // Recorded before the datagram exists, let alone leaves, so the receiver
        // can never look up a payload the ledger has not heard of yet.
        ledger.record(sequence, frame);
        let datagram = packetizer.next(frame, samples)?;

        let at = Timestamp::now();
        match socket.send_to(datagram, target) {
            Ok(_) => {
                // The send call only: encoding was measured in the previous
                // phase, and mixing the two would hide whichever is cheaper.
                send_us.record(Timestamp::now().saturating_since(at).get() / 1_000);
                report.packets += 1;
                report.wire_bytes += datagram.len() as u64;
                report.largest_datagram = report.largest_datagram.max(datagram.len());
            }
            Err(_) => report.send_failures += 1,
        }
    }

    report.send_us = send_us.percentiles();
    Ok(report)
}

/// Reads the stream back, accounts for it, and decodes what arrived.
///
/// Packets are decoded in arrival order and nothing waits for a straggler. That
/// is not an oversight: a jitter buffer belongs to the next phase, and putting
/// one here would mean the tone at the bottom of the report described audio this
/// path cannot actually deliver yet.
///
/// `expect` is the sender's SSRC when it runs in this process. Without one the
/// first stream seen is adopted, which is the only thing a receiver can do and
/// still tell a second sender apart from the one it is measuring.
fn receive(
    socket: &UdpSocket,
    config: &CodecConfig,
    expect: Option<Ssrc>,
    ledger: Option<&VerifyLedger>,
    stop: &AtomicBool,
    expected_packets: u64,
) -> ReceiveReport {
    // No SSRC to expect means no sender in this process, which is the same
    // condition as having a peer whose stopping this end can only infer.
    let stop_on_silence = expect.is_none();
    let mut report = ReceiveReport {
        ssrc: expect,
        packets: 0,
        bytes: 0,
        largest_datagram: 0,
        timestamp_pairs: 0,
        timestamp_exact: 0,
        sequence_gaps: 0,
        packets_missing: 0,
        reordered: 0,
        duplicates: 0,
        verification: match ledger {
            Some(_) => Verification::Digests {
                verified: 0,
                mismatched: 0,
                unverifiable: 0,
            },
            None => Verification::NotApplicable,
        },
        not_rtp: 0,
        wrong_payload_type: 0,
        empty_payload: 0,
        foreign_ssrc: 0,
        frames_decoded: 0,
        decode_failures: 0,
        arrival_us: None,
        tone: ToneReport::empty(),
        ended_on_silence: false,
        error: None,
    };

    let mut decoder = match OpusDecoder::new(*config) {
        Ok(decoder) => decoder,
        Err(error) => {
            report.error = Some(error.to_string());
            return report;
        }
    };

    let samples = config.frame_samples() as i64;
    let format = decoded_format(config);
    // Sized and allocated once, so filling it never puts a reallocation between
    // two arrivals.
    let mut window = Vec::with_capacity(ANALYSIS_FRAMES * format.frame_bytes());
    let mut skipped = 0usize;

    let mut datagram = [0u8; MAX_UDP_PAYLOAD];
    let mut seen = SeenWindow::new();
    let mut arrival_us = Samples::with_capacity(expected_packets as usize + 64);
    let mut previous_arrival: Option<Timestamp> = None;
    // The last packet that moved the stream forwards. A reordered arrival must
    // not become the reference, or the pair after it would be measured against
    // a packet that is behind both of them.
    let mut previous: Option<(SequenceNumber, RtpTimestamp)> = None;

    loop {
        let (length, _from) = match socket.recv_from(&mut datagram) {
            Ok(received) => received,
            Err(error) => {
                let timed_out = matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                );
                if !timed_out {
                    report.error = Some(error.to_string());
                    break;
                }
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                // A peer that has stopped will not stop again, so waiting out
                // the rest of the window would tell an operator nothing.
                if stop_on_silence
                    && let Some(last) = previous_arrival
                    && Timestamp::now().saturating_since(last).as_duration() >= IDLE_STOP
                {
                    report.ended_on_silence = true;
                    break;
                }
                continue;
            }
        };

        let at = Timestamp::now();
        if let Some(earlier) = previous_arrival {
            arrival_us.record(at.saturating_since(earlier).get() / 1_000);
        }
        previous_arrival = Some(at);
        report.bytes += length as u64;
        report.largest_datagram = report.largest_datagram.max(length);

        let packet = match parse_opus_packet(&datagram[..length]) {
            Ok(packet) => packet,
            Err(OpusParseError::WrongPayloadType { .. }) => {
                report.wrong_payload_type += 1;
                continue;
            }
            Err(OpusParseError::EmptyPayload) => {
                report.empty_payload += 1;
                continue;
            }
            Err(OpusParseError::Rtp(_)) => {
                report.not_rtp += 1;
                continue;
            }
        };
        match report.ssrc {
            Some(ssrc) if ssrc != packet.ssrc => {
                report.foreign_ssrc += 1;
                continue;
            }
            Some(_) => {}
            None => report.ssrc = Some(packet.ssrc),
        }
        if !seen.arrived(packet.sequence) {
            report.duplicates += 1;
            continue;
        }

        report.packets += 1;
        if let Some(ledger) = ledger {
            report
                .verification
                .record(ledger.check(packet.sequence, packet.payload));
        }

        match previous {
            Some((last_sequence, last_timestamp)) => {
                let distance = packet.sequence.distance_from(last_sequence);
                if distance >= 1 {
                    report.timestamp_pairs += 1;
                    if packet.timestamp.distance_from(last_timestamp)
                        == i64::from(distance) * samples
                    {
                        report.timestamp_exact += 1;
                    }
                    if distance > 1 {
                        report.sequence_gaps += 1;
                        report.packets_missing += distance as u64 - 1;
                    }
                    previous = Some((packet.sequence, packet.timestamp));
                } else {
                    // Older than the furthest the stream has got, and not a
                    // duplicate, since those were counted above.
                    report.reordered += 1;
                }
            }
            None => previous = Some((packet.sequence, packet.timestamp)),
        }

        match decoder.decode(packet.payload) {
            Ok(pcm) => {
                report.frames_decoded += 1;
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

    report.arrival_us = arrival_us.percentiles();
    report.tone = analyse(&format, &window);
    report
}

/// Sequence numbers already counted as arrivals.
///
/// A fixed ring indexed by the sequence number itself, which is what makes the
/// check a single comparison and no allocation: consecutive sequence numbers
/// land in consecutive slots, so a slot is only reused once a whole window has
/// gone by.
struct SeenWindow {
    slots: Box<[Option<SequenceNumber>; SEEN_WINDOW]>,
}

impl SeenWindow {
    fn new() -> Self {
        SeenWindow {
            slots: Box::new([None; SEEN_WINDOW]),
        }
    }

    /// Whether this is the first sighting of the packet. Records it either way.
    fn arrived(&mut self, sequence: SequenceNumber) -> bool {
        let slot = usize::from(sequence.0) % SEEN_WINDOW;
        if self.slots[slot] == Some(sequence) {
            return false;
        }
        self.slots[slot] = Some(sequence);
        true
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

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let send = self
            .send
            .as_ref()
            .and_then(|send| send.send_us)
            .unwrap_or(NOTHING);
        let arrival = self.receive.arrival_us.unwrap_or(NOTHING);
        let sent = self.send.as_ref().map_or(0, |send| send.packets);
        let wire_bytes = self.send.as_ref().map_or(0, |send| send.wire_bytes);

        match self.send_to {
            Some(target) => writeln!(f, "mode send and receive {} to {target}", self.bind)?,
            None => writeln!(f, "mode receive only on {}", self.bind)?,
        }
        match self.ssrc() {
            Some(ssrc) => writeln!(f, "ssrc {ssrc}")?,
            // No stream was seen, so there is no synchronisation source to name.
            None => writeln!(f, "ssrc none")?,
        }
        writeln!(f, "payload type {OPUS_PAYLOAD_TYPE}")?;
        writeln!(f, "packets sent {sent}")?;
        writeln!(f, "packets received {}", self.receive.packets)?;
        writeln!(f, "bytes on the wire {wire_bytes}")?;
        writeln!(
            f,
            "timestamp delta exact {} of {}",
            self.receive.timestamp_exact, self.receive.timestamp_pairs
        )?;
        writeln!(
            f,
            "sequence gaps {} totalling {}",
            self.receive.sequence_gaps, self.receive.packets_missing
        )?;
        writeln!(f, "reordered {}", self.receive.reordered)?;
        writeln!(f, "duplicates {}", self.receive.duplicates)?;
        match self.receive.verification {
            Verification::Digests { verified, .. } => {
                writeln!(f, "payload verified {verified} of {}", self.receive.packets)?
            }
            // Deliberately not a number: zero of N would read as a path that
            // corrupted everything, and what it means is that the sender's bytes
            // are on another machine. The tone below is the check that replaces
            // it.
            Verification::NotApplicable => writeln!(
                f,
                "payload verified not applicable across machines, the tone stands in"
            )?,
        }
        writeln!(f, "largest datagram {}", self.receive.largest_datagram)?;
        writeln!(
            f,
            "send us p50 {} p95 {} p99 {} max {}",
            send.p50, send.p95, send.p99, send.max
        )?;
        writeln!(
            f,
            "arrival interval us p50 {} p95 {} p99 {} max {}",
            arrival.p50, arrival.p95, arrival.p99, arrival.max
        )?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(self.receive.tone.left),
            hertz(self.receive.tone.right)
        )?;

        // Everything below is for a person reading the run rather than for the
        // harness parsing it. Corruption, absence and rubbish each get their own
        // line, because a single line covering them would be the counter this
        // module exists not to keep.
        match (self.lost(), self.loss_percent()) {
            (Some(lost), Some(percent)) => {
                writeln!(f, "packets lost {lost} of {sent} at {percent:.3} %")?
            }
            // A receiver cannot subtract what it never sent. What it can say
            // about loss is on the sequence gaps line above.
            _ => writeln!(
                f,
                "packets lost unknown here, the sender is another machine; the sequence \
                 total above is what this end can see"
            )?,
        }
        match self.receive.verification {
            Verification::Digests {
                mismatched,
                unverifiable,
                ..
            } => writeln!(
                f,
                "payload mismatched {mismatched} unverifiable {unverifiable}"
            )?,
            Verification::NotApplicable => writeln!(
                f,
                "payload comparison needs one process holding both halves, and this run holds one"
            )?,
        }
        writeln!(
            f,
            "frames decoded {} decode failures {}",
            self.receive.frames_decoded, self.receive.decode_failures
        )?;
        writeln!(
            f,
            "datagrams refused: not rtp {} wrong type {} empty {} foreign ssrc {}",
            self.receive.not_rtp,
            self.receive.wrong_payload_type,
            self.receive.empty_payload,
            self.receive.foreign_ssrc
        )?;
        if let Some(send) = &self.send {
            writeln!(f, "send failures {}", send.send_failures)?;
            writeln!(f, "largest datagram sent {}", send.largest_datagram)?;
        }
        writeln!(f, "bytes received {}", self.receive.bytes)?;
        writeln!(f, "effective kbps {:.1}", self.effective_kbps())?;
        writeln!(f, "frame ms {}", self.config.frame.millis())?;
        writeln!(
            f,
            "frame samples per channel {}",
            self.config.frame_samples()
        )?;
        writeln!(
            f,
            "tone is the contract {}",
            yes_no(self.tone_is_the_contract())
        )?;
        writeln!(
            f,
            "tone channels distinct {}",
            yes_no(self.receive.tone.distinct())
        )?;
        writeln!(
            f,
            "tone resolution {:.2} hz over {} frames",
            self.receive.tone.resolution_hz, self.receive.tone.analysed_frames
        )?;
        writeln!(
            f,
            "ended on silence {}",
            yes_no(self.receive.ended_on_silence)
        )?;
        match &self.receive.error {
            Some(error) => writeln!(f, "receive error {error}"),
            None => writeln!(f, "receive error none"),
        }
    }
}

#[cfg(test)]
mod tests {
    use lanplay_audio_capture::goertzel::Tone as DetectedTone;

    use super::*;

    fn contract_tone() -> ToneReport {
        ToneReport {
            left: Some(DetectedTone {
                frequency: CONTRACT.left_hz,
                level_dbfs: CONTRACT.level_dbfs,
            }),
            right: Some(DetectedTone {
                frequency: CONTRACT.right_hz,
                level_dbfs: CONTRACT.level_dbfs,
            }),
            resolution_hz: 2.0,
            analysed_frames: ANALYSIS_FRAMES,
        }
    }

    fn received(verification: Verification) -> ReceiveReport {
        ReceiveReport {
            ssrc: Some(Ssrc(0x0A0B_0C0D)),
            packets: 5_999,
            bytes: 557_907,
            largest_datagram: 93,
            timestamp_pairs: 5_998,
            timestamp_exact: 5_998,
            sequence_gaps: 1,
            packets_missing: 1,
            reordered: 0,
            duplicates: 0,
            verification,
            not_rtp: 0,
            wrong_payload_type: 0,
            empty_payload: 0,
            foreign_ssrc: 0,
            frames_decoded: 5_999,
            decode_failures: 0,
            arrival_us: None,
            tone: contract_tone(),
            ended_on_silence: false,
            error: None,
        }
    }

    fn sending() -> Measurement {
        Measurement {
            config: CodecConfig::contract(FrameDuration::Ms5, 128_000),
            bind: "0.0.0.0:5008".parse().expect("address"),
            send_to: Some("127.0.0.1:5008".parse().expect("address")),
            send: Some(SendReport {
                packets: 6_000,
                wire_bytes: 558_000,
                largest_datagram: 93,
                send_us: None,
                send_failures: 0,
            }),
            receive: received(Verification::Digests {
                verified: 5_999,
                mismatched: 0,
                unverifiable: 0,
            }),
        }
    }

    fn listening() -> Measurement {
        Measurement {
            config: CodecConfig::contract(FrameDuration::Ms5, 128_000),
            bind: "0.0.0.0:5008".parse().expect("address"),
            send_to: None,
            send: None,
            receive: received(Verification::NotApplicable),
        }
    }

    /// The wording the harness parses, checked here rather than by running a
    /// socket and reading the terminal.
    #[test]
    fn the_report_keys_are_the_ones_the_gate_reads() {
        let text = sending().to_string();
        for line in [
            "ssrc 0a0b0c0d",
            "payload type 111",
            "packets sent 6000",
            "packets received 5999",
            "bytes on the wire 558000",
            "timestamp delta exact 5998 of 5998",
            "sequence gaps 1 totalling 1",
            "reordered 0",
            "duplicates 0",
            "payload verified 5999 of 5999",
            "largest datagram 93",
            "send us p50 0 p95 0 p99 0 max 0",
            "arrival interval us p50 0 p95 0 p99 0 max 0",
            "tone left 997.0 right 1997.0",
        ] {
            assert!(
                text.lines().any(|written| written == line),
                "missing line {line:?} in\n{text}"
            );
        }
    }

    /// A receive-only run must not report a number where it has no comparison
    /// to make: zero verified would read as a path that corrupted everything.
    #[test]
    fn a_peer_run_says_verification_does_not_apply_instead_of_zero() {
        let text = listening().to_string();
        assert!(
            text.contains("payload verified not applicable across machines"),
            "{text}"
        );
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with("payload verified 0")),
            "{text}"
        );
        // Everything the arriving stream can say for itself still says it.
        for line in [
            "mode receive only on 0.0.0.0:5008",
            "packets sent 0",
            "packets received 5999",
            "timestamp delta exact 5998 of 5998",
            "sequence gaps 1 totalling 1",
            "largest datagram 93",
            "tone left 997.0 right 1997.0",
        ] {
            assert!(
                text.lines().any(|written| written == line),
                "missing line {line:?} in\n{text}"
            );
        }
    }

    /// Loss is a measurement, corruption is a defect, and the two must not
    /// reach the same verdict.
    #[test]
    fn a_lost_packet_is_not_a_defect_and_a_changed_byte_is() {
        let mut run = sending();
        assert_eq!(
            run.defect(),
            None,
            "one lost packet is what this phase measures"
        );

        run.receive.verification = Verification::Digests {
            verified: 5_998,
            mismatched: 1,
            unverifiable: 0,
        };
        assert_eq!(
            run.defect(),
            Some(Defect::PayloadChanged {
                mismatched: 1,
                unverifiable: 0
            })
        );

        let mut skewed = sending();
        skewed.receive.timestamp_exact -= 1;
        assert_eq!(
            skewed.defect(),
            Some(Defect::TimestampIsNotASampleCounter {
                exact: 5_997,
                pairs: 5_998
            }),
            "a timestamp that is not a sample counter is the one thing the phase turns on"
        );
    }

    /// A run that measured nothing has to say which nothing it measured, because
    /// an instrument that prints zeroes and exits cleanly is one nobody can act
    /// on.
    #[test]
    fn a_run_that_measured_nothing_names_which_nothing() {
        let mut waited = listening();
        waited.receive.packets = 0;
        waited.receive.timestamp_pairs = 0;
        waited.receive.timestamp_exact = 0;
        waited.receive.tone = ToneReport::empty();
        let defect = waited
            .defect()
            .expect("a run of zeroes is not a measurement");
        assert_eq!(
            defect,
            Defect::NothingArrived {
                listening_only: true
            }
        );
        assert!(defect.to_string().contains("no peer sent"), "{defect}");

        // The same emptiness from the sending side asks a different question of
        // whoever ran it, so it does not share the wording.
        let mut unanswered = sending();
        unanswered.receive.packets = 0;
        assert_eq!(
            unanswered.defect(),
            Some(Defect::NothingArrived {
                listening_only: false
            })
        );
    }

    /// With no digests to compare, the tone is the only evidence that what
    /// arrived was the audio that was sent, so it is the criterion.
    #[test]
    fn a_peer_run_turns_on_the_tone_instead_of_the_digests() {
        assert!(listening().sound());

        let mut silent = listening();
        silent.receive.tone = ToneReport::empty();
        assert!(
            !silent.sound(),
            "a peer run that decoded silence has proved nothing about the path"
        );

        let mut folded = listening();
        folded.receive.tone.right = folded.receive.tone.left;
        assert!(
            !folded.sound(),
            "two channels reading one frequency is what a folded mix looks like"
        );

        // The same silence in a sending run is still caught, by the digests.
        let mut sent_silence = sending();
        sent_silence.receive.tone = ToneReport::empty();
        assert!(sent_silence.sound());
    }

    #[test]
    fn the_ledger_claims_a_payload_once_and_survives_reordering() {
        let ledger = VerifyLedger::new();
        ledger.record(SequenceNumber(10), b"first");
        ledger.record(SequenceNumber(11), b"second");
        ledger.record(SequenceNumber(12), b"third");

        // Arriving out of order must not discard the entries in front, or a
        // reordered packet would read as a payload nobody could verify.
        assert_eq!(ledger.check(SequenceNumber(12), b"third"), Some(true));
        assert_eq!(ledger.check(SequenceNumber(10), b"first"), Some(true));
        assert_eq!(ledger.check(SequenceNumber(11), b"changed"), Some(false));
        // Claimed once: a second sighting is a duplicate, not a fresh arrival.
        assert_eq!(ledger.check(SequenceNumber(10), b"first"), None);
    }

    #[test]
    fn the_duplicate_window_counts_a_copy_and_not_a_new_sequence() {
        let mut seen = SeenWindow::new();
        assert!(seen.arrived(SequenceNumber(1)));
        assert!(!seen.arrived(SequenceNumber(1)));
        assert!(seen.arrived(SequenceNumber(2)));
        // A wrap of the 16-bit sequence is a new packet, not a duplicate of the
        // one that sat in the slot a whole window ago.
        assert!(seen.arrived(SequenceNumber(65_535)));
        assert!(seen.arrived(SequenceNumber(0)));
        assert!(!seen.arrived(SequenceNumber(0)));
    }
}
