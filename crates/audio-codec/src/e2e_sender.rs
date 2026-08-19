//! The host half of the first Windows to Mac audio: loopback, Opus, RTP, UDP.
//!
//! A1, A2 and A3 each measured one stage with nothing else attached, and each
//! left a probe behind. This is the join, and it adds no mechanism of its own:
//! the loopback session is [`lanplay_audio_capture::Loopback`], the encoder is
//! [`OpusEncoder`] at the settings A2 fixed, and the datagrams come out of
//! [`OpusPacketizer`], which owns the sample counter the RTP timestamp is.
//!
//! There is no accumulator, and its absence is the point. A1 measured the
//! endpoint delivering exactly 480 frames in every packet, which at 48 kHz is
//! exactly two 5 ms Opus frames, so a packet is **split** into two frames and
//! two datagrams and nothing is ever left over. A buffer that can never hold
//! anything is a buffer whose emptiness nobody checks, and the first packet
//! shape that broke the assumption would be carried silently in it. What stands
//! in its place is [`Split`], which states the whole frames a packet contains
//! and the frames it could not fill, and a residue is counted rather than
//! stored. Over a run it must be zero, and a run where it is not has found
//! something about the endpoint that A1 did not.
//!
//! Why the sender counts what it counts. A sender that reports what it sent
//! cannot be compared against a receiver at all: a receiver reporting 11 940
//! datagrams against a sender's 12 000 has established that 60 went missing
//! between the two, and nothing whatsoever about whether the endpoint delivered
//! the audio those 12 000 were made from. So every stage is counted separately
//! at its own boundary -- packets and frames off the endpoint by device
//! position, frames into the encoder, datagrams the socket accepted -- and the
//! two counts that must agree exactly are stated as their own numbers: samples
//! captured against samples encoded. That identity is the split's proof, and it
//! is also the sender's half of the continuity accounting A6 is decided on,
//! because a sender whose captured and encoded sample counts have drifted apart
//! has already lost audio before the radio was involved, and a receiver has no
//! way to tell that apart from loss on the air.
//!
//! Deliberately absent, each because a later phase owns it: no rate matching and
//! no drift correction, so the drift between the two clocks is measured here
//! rather than hidden; no FEC, no NACK, no retransmission, because the plan is
//! explicit that the loss figure arrives before anything is built to conceal it;
//! and no pacing of the two datagrams a packet becomes. The capture cadence is
//! part of what A6 measures, so the pair leaves as a pair.

use core::fmt;
use std::net::{SocketAddr, UdpSocket};

use lanplay_audio_capture::accounting::{
    Accounting, Drift, Packet, Percentiles, Rate, Samples, Totals,
};
use lanplay_audio_capture::analysis::{ToneReport, analyse, hertz};
use lanplay_audio_capture::format::{MixFormat, SampleKind};
use lanplay_audio_capture::report::Wakeup;
use lanplay_audio_capture::scheduling::Scheduling;
use lanplay_transport::{OpusPacketizeError, OpusPacketizer, RtpTimestamp, Ssrc, random_u32};

use crate::config::{CodecConfig, FrameDuration};
use crate::encoder::{EncoderSettings, OpusEncoder};
use crate::error::CodecError;
use crate::probe::ANALYSIS_FRAMES;

/// The frame duration A2 fixed, and not a parameter.
///
/// Every other duration Opus offers either exceeds the audio budget or fails to
/// divide the endpoint's packet: a 10 ms frame is one packet exactly and would
/// make the split arithmetic vanish along with the property it proves, and 20 ms
/// and above cannot be sent inside the deadline at all.
pub const FRAME: FrameDuration = FrameDuration::Ms5;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Options {
    /// Where the datagrams go, which for a real measurement is another machine:
    /// a datagram addressed to this machine's own routable address never
    /// reaches the driver, and A3 measured that rather than assuming it.
    pub send_to: SocketAddr,
    pub bind: SocketAddr,
    pub seconds: f64,
    pub bitrate_kbps: u32,
}

/// How a captured packet divides into whole Opus frames.
///
/// Frames rather than samples, because a frame is what the encoder takes and a
/// residue in samples would have to be divided again by whoever read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Split {
    /// Whole Opus frames the packet contains.
    pub frames: usize,
    /// Frames at the end of the packet that do not fill another Opus frame.
    ///
    /// Zero on this endpoint, every packet. It is counted rather than carried
    /// forward because carrying it forward is what an accumulator does, and an
    /// accumulator hides the very finding this number exists to report.
    pub residue: usize,
}

/// Divides a captured packet into the frames the encoder can take.
pub fn split(packet_frames: usize, frame_samples: usize) -> Split {
    if frame_samples == 0 {
        return Split {
            frames: 0,
            residue: packet_frames,
        };
    }
    Split {
        frames: packet_frames / frame_samples,
        residue: packet_frames % frame_samples,
    }
}

/// Why a run could not be set up, or could not be believed once it was.
#[derive(Debug)]
pub enum SenderError {
    Codec(CodecError),
    Packetize(OpusPacketizeError),
    Io {
        call: &'static str,
        error: std::io::Error,
    },
    /// The endpoint mixes something the encoder cannot be handed as it stands.
    ///
    /// A refusal and not a conversion. A1 measured this endpoint at 48 kHz
    /// stereo 32-bit float, which is exactly what Opus takes here, so a
    /// resampler on this path would be code written against a problem the
    /// machine does not have -- and a run that quietly converted would report
    /// the conversion's latency as the codec's.
    Format {
        endpoint: MixFormat,
        wanted: CodecConfig,
    },
    /// There is no WASAPI endpoint to capture, because this is not Windows.
    NoEndpoint(&'static str),
    #[cfg(windows)]
    Capture(lanplay_audio_capture::CaptureError),
}

impl From<CodecError> for SenderError {
    fn from(error: CodecError) -> Self {
        SenderError::Codec(error)
    }
}

impl From<OpusPacketizeError> for SenderError {
    fn from(error: OpusPacketizeError) -> Self {
        SenderError::Packetize(error)
    }
}

#[cfg(windows)]
impl From<lanplay_audio_capture::CaptureError> for SenderError {
    fn from(error: lanplay_audio_capture::CaptureError) -> Self {
        SenderError::Capture(error)
    }
}

impl fmt::Display for SenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SenderError::Codec(error) => write!(f, "codec: {error}"),
            SenderError::Packetize(error) => write!(f, "packetiser: {error}"),
            SenderError::Io { call, error } => write!(f, "{call}: {error}"),
            SenderError::Format { endpoint, wanted } => write!(
                f,
                "the endpoint mixes {endpoint}, and this sender carries {} Hz {} ch 32 bit float \
                 without converting anything",
                wanted.sample_rate, wanted.channels
            ),
            SenderError::NoEndpoint(os) => write!(
                f,
                "loopback capture needs a WASAPI render endpoint and this is {os}"
            ),
            #[cfg(windows)]
            SenderError::Capture(error) => write!(f, "capture: {error}"),
        }
    }
}

impl core::error::Error for SenderError {}

/// Only the run needs this, and only Windows has a run.
#[cfg(windows)]
fn io(call: &'static str) -> impl FnOnce(std::io::Error) -> SenderError {
    move |error| SenderError::Io { call, error }
}

/// Counters taken at the boundary each of them describes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    /// Frames the encoder produced a packet for.
    pub frames_encoded: u64,
    /// Datagrams the socket accepted.
    pub datagrams_sent: u64,
    /// Bytes those datagrams carried, RTP header included, which is the load on
    /// the radio rather than the load on the codec.
    pub datagram_bytes: u64,
    pub residue_frames: u64,
    /// Packets whose bytes could not be read as the floats the endpoint said
    /// they are.
    ///
    /// Counted rather than worked around: the packet is skipped, so its frames
    /// are captured and not encoded, and the sample identity below breaks by
    /// exactly that many. A silent substitution here would keep the identity
    /// intact and put unmeasured silence on the wire, which is the shape of
    /// mistake this project keeps finding.
    pub unreadable_packets: u64,
    pub encode_failures: u64,
    pub send_failures: u64,
    /// Times the packetiser advanced its timestamp, and times the advance was
    /// exactly the frame's sample count.
    ///
    /// Measured rather than trusted. The advance is the one property of this
    /// stream a receiver cannot recover from anything else it sees: a timestamp
    /// that drifted with the sender's scheduling would leave it unable to tell
    /// a late packet from a packet describing a later moment.
    pub timestamp_steps: u64,
    pub timestamp_steps_exact: u64,
}

/// Everything the sending path produced, once the stream has stopped.
#[derive(Clone, Debug)]
pub struct Carried {
    pub totals: Totals,
    /// The endpoint's own rate, from the device position and the performance
    /// counter every packet carries together.
    ///
    /// The sender's half of A7.1, and it is the whole of what this end can say
    /// about drift: a rate is measurable on one machine against that machine's
    /// own monotonic clock, and nothing here is ever compared with a reading
    /// taken on the Mac. What the two ends' figures are worth together is the
    /// samples-produced against samples-consumed invariant, which is counted in
    /// samples at the far end and needs no clock at all.
    pub drift: Drift,
    pub counts: Counts,
    pub packet_frames: Option<Percentiles>,
    pub encode_us: Option<Percentiles>,
    pub send_us: Option<Percentiles>,
    pub packet_bytes: Option<Percentiles>,
    pub tone: ToneReport,
    pub first_encode_error: Option<String>,
    pub first_send_error: Option<String>,
    /// Measurements the fixed-size sample stores had no room for, so that a
    /// distribution can never quietly describe a prefix of the run.
    pub samples_dropped: u64,
}

/// The capture, encode and send path, with every store it writes into sized
/// before the stream starts.
///
/// Platform independent on purpose, and not as a courtesy to a test: the split,
/// the encode, the packetisation and the send are the parts that can be wrong
/// arithmetically, and every one of them can be exercised by handing this
/// synthetic packets on a machine with no audio endpoint. What is left needing
/// Windows is the endpoint itself.
pub struct Carrier<'a> {
    socket: &'a UdpSocket,
    target: SocketAddr,
    format: MixFormat,
    frame_interleaved: usize,
    frame_samples: u32,
    encoder: OpusEncoder,
    packetizer: OpusPacketizer,
    /// The sample counter the packetiser started at, kept because the counter
    /// itself moves and this value is the run's identity rather than its state.
    ///
    /// It is the one thing a receiver cannot work out for itself. RFC 3550
    /// requires the timestamp to start at a random value, so the far end sees
    /// consecutive frames stepping by a frame's samples and can partition them
    /// into the two residue classes a 480-frame packet produces, but not tell
    /// which class is a packet's first frame. This is that bit, and it is a bare
    /// integer: no clock is read to obtain it and none is crossed by using it.
    rtp_base: RtpTimestamp,
    account: Accounting,
    /// One frame of zeroes, reused for every frame of every silent packet.
    ///
    /// A silent packet's contents are undefined by the API's own account and are
    /// to be read as silence, so encoding the buffer as it stands would encode
    /// whatever the engine last left in the ring.
    silence: Vec<f32>,
    packet_frames: Samples,
    encode_us: Samples,
    send_us: Samples,
    packet_bytes: Samples,
    /// A window of captured bytes kept for the tone detector.
    ///
    /// A packet count and a datagram count agree just as happily when the
    /// endpoint was playing nothing, and A6 is decided on whether real audio
    /// arrived continuously, so the sender says what it captured was audio
    /// rather than leaving the far end to infer it from a byte rate.
    analysis: Vec<u8>,
    counts: Counts,
    first_encode_error: Option<String>,
    first_send_error: Option<String>,
}

impl<'a> Carrier<'a> {
    /// Builds the path and takes every allocation it will ever make.
    ///
    /// `expected_frames` sizes the distributions. Overflow is counted rather
    /// than grown into, because a `Vec` that reallocated inside the loop would
    /// put a heap allocation on the path whose timing is the measurement.
    pub fn new(
        socket: &'a UdpSocket,
        target: SocketAddr,
        config: CodecConfig,
        format: MixFormat,
        expected_frames: usize,
    ) -> Result<Carrier<'a>, SenderError> {
        if format.sample_rate != config.sample_rate
            || format.channels != config.channels
            || format.kind != SampleKind::Float
            || format.sample_bytes() != size_of::<f32>()
        {
            return Err(SenderError::Format {
                endpoint: format,
                wanted: config,
            });
        }

        let frame_interleaved = config.frame_interleaved();
        let capacity = expected_frames + 1_024;
        // An SSRC of its own, drawn independently of the video stream's, so that
        // a capture holding both can never attribute an audio packet to the
        // picture.
        let packetizer = OpusPacketizer::new(Ssrc(random_u32()));
        // Read before a datagram has moved it, because the packetiser's counter
        // is the only place the base exists: a value taken after the first send
        // would be the second frame's timestamp and would label the two residue
        // classes the wrong way round for the whole run.
        let rtp_base = packetizer.next_timestamp();
        Ok(Carrier {
            socket,
            target,
            format,
            frame_interleaved,
            frame_samples: config.frame_samples() as u32,
            encoder: OpusEncoder::new(config)?,
            packetizer,
            rtp_base,
            // The endpoint's own rate, which the check above has already agreed
            // with the codec's, so the drift it accumulates is a deviation from
            // what this device claims rather than from a constant written here.
            account: Accounting::new(f64::from(format.sample_rate)),
            silence: vec![0f32; frame_interleaved],
            packet_frames: Samples::with_capacity(capacity),
            encode_us: Samples::with_capacity(capacity),
            send_us: Samples::with_capacity(capacity),
            packet_bytes: Samples::with_capacity(capacity),
            analysis: Vec::with_capacity(ANALYSIS_FRAMES * format.frame_bytes()),
            counts: Counts::default(),
            first_encode_error: None,
            first_send_error: None,
        })
    }

    pub fn ssrc(&self) -> Ssrc {
        self.packetizer.ssrc()
    }

    /// The sample counter this run's first datagram carried.
    ///
    /// The far end needs it to say which of a captured packet's two frames a
    /// datagram held, and it needs it from here because nothing on the wire says
    /// so: the timestamps fix the pairing and the random base hides the phase.
    pub fn rtp_base(&self) -> RtpTimestamp {
        self.rtp_base
    }

    /// What the encoder says it is doing, read out of the encoder rather than
    /// copied from what it was told.
    pub fn encoder_settings(&self) -> &EncoderSettings {
        self.encoder.settings()
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    /// Splits one captured packet, encodes each frame and sends each datagram.
    ///
    /// Allocates nothing, logs nothing and takes no lock: it runs where the
    /// endpoint is already filling the next packet, and every store it writes
    /// into was sized before the stream started.
    pub fn carry(&mut self, described: &Packet, bytes: &[u8], now: impl Fn() -> u64) {
        self.account.record(described);
        self.packet_frames.record(u64::from(described.frames));

        // Silence first, because a silent packet's bytes say nothing about what
        // the host was playing and reading them as floats would be reading the
        // ring's leftovers.
        let source = if described.silent {
            None
        } else {
            match floats(bytes) {
                Some(samples) => Some(samples),
                None => {
                    self.counts.unreadable_packets += 1;
                    return;
                }
            }
        };

        // Silence contributes nothing to a frequency measurement except an
        // attenuation of whatever came before it, so the window is filled from
        // packets that carried something.
        if source.is_some() && self.analysis.len() < self.analysis.capacity() {
            let room = self.analysis.capacity() - self.analysis.len();
            self.analysis
                .extend_from_slice(&bytes[..bytes.len().min(room)]);
        }

        let divided = split(described.frames as usize, self.frame_samples as usize);
        self.counts.residue_frames += divided.residue as u64;

        for index in 0..divided.frames {
            let frame = match source {
                Some(samples) => {
                    let start = index * self.frame_interleaved;
                    &samples[start..start + self.frame_interleaved]
                }
                None => &self.silence[..],
            };

            let began = now();
            let encoded = match self.encoder.encode(frame) {
                Ok(encoded) => encoded,
                Err(error) => {
                    self.counts.encode_failures += 1;
                    if self.first_encode_error.is_none() {
                        self.first_encode_error = Some(error.to_string());
                    }
                    continue;
                }
            };
            self.encode_us.record(now().saturating_sub(began) / 1_000);
            self.counts.frames_encoded += 1;
            self.packet_bytes.record(encoded.len() as u64);

            let before = self.packetizer.next_timestamp();
            let datagram = match self.packetizer.next(encoded, self.frame_samples) {
                Ok(datagram) => datagram,
                Err(_) => {
                    // A payload the packetiser refuses is a packet the encoder
                    // should not have produced, and it is counted where a send
                    // failure is counted because from here they are the same
                    // event: a frame that was encoded and did not leave.
                    self.counts.send_failures += 1;
                    continue;
                }
            };
            let at = now();
            let sent = self.socket.send_to(datagram, self.target);
            // Read after the datagram has left, because the packetiser has
            // already moved its counter and the borrow of it ends with the
            // datagram. What is checked is the step it took, which is a
            // property of the stream and not of when it was looked at.
            let advance = self.packetizer.next_timestamp().0.wrapping_sub(before.0);
            self.counts.timestamp_steps += 1;
            if advance == self.frame_samples {
                self.counts.timestamp_steps_exact += 1;
            }
            match sent {
                Ok(bytes_sent) => {
                    // The send call alone. Encoding was timed above, and one
                    // number covering both would hide whichever is cheaper.
                    self.send_us.record(now().saturating_sub(at) / 1_000);
                    self.counts.datagrams_sent += 1;
                    self.counts.datagram_bytes += bytes_sent as u64;
                }
                Err(error) => {
                    self.counts.send_failures += 1;
                    if self.first_send_error.is_none() {
                        self.first_send_error = Some(error.to_string());
                    }
                }
            }
        }
    }

    /// Closes the accounting and measures the audio that was captured.
    pub fn finish(mut self) -> Carried {
        let samples_dropped = self.packet_frames.dropped()
            + self.encode_us.dropped()
            + self.send_us.dropped()
            + self.packet_bytes.dropped();
        Carried {
            totals: self.account.totals(),
            drift: self.account.drift(),
            counts: self.counts,
            packet_frames: self.packet_frames.percentiles(),
            encode_us: self.encode_us.percentiles(),
            send_us: self.send_us.percentiles(),
            packet_bytes: self.packet_bytes.percentiles(),
            tone: analyse(&self.format, &self.analysis),
            first_encode_error: self.first_encode_error,
            first_send_error: self.first_send_error,
            samples_dropped,
        }
    }
}

/// The engine's buffer as the interleaved floats it holds.
///
/// Not a conversion and not an endianness assumption: the audio engine wrote
/// `f32` values into this buffer at this machine's own byte order, and the mix
/// format was refused above unless it said so. The alignment is checked because
/// the language requires a `&[f32]` to be aligned, not because an endpoint has
/// ever handed back an odd address, and a buffer that failed the check is
/// reported rather than read.
fn floats(bytes: &[u8]) -> Option<&[f32]> {
    if !bytes.as_ptr().cast::<f32>().is_aligned() || !bytes.len().is_multiple_of(size_of::<f32>()) {
        return None;
    }
    // SAFETY: the pointer is aligned for `f32` as just checked, the length
    // divides exactly, and the borrow lives no longer than the bytes it came
    // from.
    Some(unsafe {
        core::slice::from_raw_parts(bytes.as_ptr().cast::<f32>(), bytes.len() / size_of::<f32>())
    })
}

/// One run of the sender, and the environment it ran in.
#[derive(Clone, Debug)]
pub struct Measurement {
    pub endpoint: String,
    pub format: MixFormat,
    pub default_period_ms: f64,
    pub minimum_period_ms: f64,
    pub buffer_frames: u32,
    pub wakeup: Wakeup,
    pub event_refused: Option<String>,
    /// What the one thread that carries audio was granted.
    ///
    /// One thread and not several, because the whole path from `GetBuffer` to
    /// `send_to` fits inside a device period and a queue between two threads
    /// would add a handoff whose delay would then have to be measured too. A
    /// figure below that was taken under default scheduling describes the
    /// scheduler and not this code, which is why this line sits above the
    /// counters rather than in a footnote under them.
    pub scheduling: Scheduling,
    pub config: CodecConfig,
    pub settings: EncoderSettings,
    pub libopus: &'static str,
    pub bind: SocketAddr,
    pub send_to: SocketAddr,
    pub ssrc: Ssrc,
    /// The RTP timestamp the run's first datagram carried, which is by
    /// construction the first Opus frame of the first captured packet.
    ///
    /// Stated because it is the only thing the receiving end cannot derive: a
    /// 480-frame packet becomes two datagrams 240 ticks apart, so the far end can
    /// partition a whole run into the two residue classes modulo 480 from the
    /// timestamps alone, and RFC 3550's random base then leaves it unable to say
    /// which class is a packet's first frame. That is one bit and it is the sign
    /// of A6.1's answer. Deciding it from arrival order instead would be reading
    /// the conclusion off the measurement.
    pub rtp_base: RtpTimestamp,
    pub carried: Carried,
    /// Packets `GetBuffer` refused, and what it said the first time.
    pub buffer_errors: u64,
    pub first_buffer_error: Option<String>,
    pub wakeup_timeouts: u64,
    pub wakeup_intervals_us: Option<Percentiles>,
    /// Seconds between the stream starting and stopping, on the monotonic
    /// clock, so every rate below is a count divided by the interval that count
    /// was taken over.
    pub span_s: f64,
}

impl Measurement {
    /// Frames of one channel the endpoint delivered.
    pub fn samples_captured(&self) -> u64 {
        self.carried.totals.frames
    }

    /// Frames of one channel the encoder was handed.
    pub fn samples_encoded(&self) -> u64 {
        self.carried.counts.frames_encoded * self.config.frame_samples() as u64
    }

    /// Stated as its own number rather than left to a reader to subtract,
    /// because a difference nobody named is a difference somebody computes
    /// twice and disagrees about once.
    pub fn sample_disagreement(&self) -> u64 {
        self.samples_captured().abs_diff(self.samples_encoded())
    }

    /// The capture endpoint's rate against nominal, or nothing when too few
    /// packets arrived to state one.
    ///
    /// A1 read this endpoint at -15 ppm and A7.1 exists because that figure and
    /// this Mac's +5 ppm together predict a buffer that empties, while the one
    /// that was measured filled. So it is derived from the run rather than
    /// carried forward: the whole point is to find out whether the old figure
    /// survives.
    pub fn source_rate(&self) -> Option<Rate> {
        self.carried.drift.rate()
    }

    pub fn capture_packets_per_s(&self) -> f64 {
        self.per_second(self.carried.totals.packets)
    }

    pub fn frames_encoded_per_s(&self) -> f64 {
        self.per_second(self.carried.counts.frames_encoded)
    }

    pub fn datagrams_per_s(&self) -> f64 {
        self.per_second(self.carried.counts.datagrams_sent)
    }

    /// What the stream costs the radio, header included.
    pub fn wire_kbps(&self) -> f64 {
        if self.span_s <= 0.0 {
            return 0.0;
        }
        self.carried.counts.datagram_bytes as f64 * 8.0 / self.span_s / 1_000.0
    }

    fn per_second(&self, count: u64) -> f64 {
        if self.span_s <= 0.0 {
            return 0.0;
        }
        count as f64 / self.span_s
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
        let counts = self.carried.counts;
        let totals = self.carried.totals;
        let encode = self.carried.encode_us.unwrap_or(NOTHING);
        let send = self.carried.send_us.unwrap_or(NOTHING);
        let bytes = self.carried.packet_bytes.unwrap_or(NOTHING);
        let packet_frames = self.carried.packet_frames.unwrap_or(NOTHING);
        let wakeups = self.wakeup_intervals_us.unwrap_or(NOTHING);

        writeln!(f, "scheduling {}", self.scheduling)?;
        writeln!(f, "endpoint {}", self.endpoint)?;
        writeln!(f, "mix format {}", self.format)?;
        writeln!(f, "sending to {} from {}", self.send_to, self.bind)?;
        writeln!(f, "ssrc {}", self.ssrc.0)?;
        writeln!(f, "rtp base {}", self.rtp_base.0)?;
        writeln!(f, "span {:.3} s", self.span_s)?;

        writeln!(f, "capture packets {}", totals.packets)?;
        writeln!(f, "capture frames {}", totals.frames)?;
        writeln!(
            f,
            "capture packets per s {:.2}",
            self.capture_packets_per_s()
        )?;
        writeln!(f, "frames encoded {}", counts.frames_encoded)?;
        writeln!(f, "frames encoded per s {:.2}", self.frames_encoded_per_s())?;
        writeln!(f, "datagrams sent {}", counts.datagrams_sent)?;
        writeln!(f, "datagram bytes {}", counts.datagram_bytes)?;
        writeln!(f, "samples captured {}", self.samples_captured())?;
        writeln!(f, "samples encoded {}", self.samples_encoded())?;
        writeln!(f, "sample disagreement {}", self.sample_disagreement())?;
        writeln!(f, "split residue frames {}", counts.residue_frames)?;
        writeln!(
            f,
            "encode us p50 {} p95 {} p99 {} max {}",
            encode.p50, encode.p95, encode.p99, encode.max
        )?;
        writeln!(
            f,
            "send us p50 {} p95 {} p99 {} max {}",
            send.p50, send.p95, send.p99, send.max
        )?;
        writeln!(
            f,
            "packet bytes p50 {} p95 {} p99 {} max {}",
            bytes.p50, bytes.p95, bytes.p99, bytes.max
        )?;
        writeln!(
            f,
            "position gaps {} totalling {} frames",
            totals.gaps, totals.gap_frames
        )?;
        writeln!(
            f,
            "position rewinds {} totalling {} frames",
            totals.rewinds, totals.rewind_frames
        )?;
        writeln!(
            f,
            "discontinuities {} in flight {}",
            totals.discontinuities,
            totals.discontinuities_in_flight()
        )?;
        writeln!(f, "silent packets {}", totals.silent_packets)?;
        // The endpoint's own rate, from the same position stream the gaps and
        // rewinds above are counted in. Absent rather than zero when too few
        // packets arrived to state one: 0.000 ppm is exactly what a correct
        // clock looks like, and printing it for a run that measured nothing is
        // the shape of report this project has read as success five times.
        match self.source_rate() {
            Some(rate) => {
                writeln!(
                    f,
                    "source ppm {:+.3} error {:.3} over {} readings and {:.3} s",
                    rate.fitted_ppm, rate.error_ppm, rate.readings, rate.seconds
                )?;
                writeln!(f, "source ppm endpoints {:+.3}", rate.endpoints_ppm)?;
                writeln!(f, "source position samples {:.0}", rate.samples)?;
                writeln!(
                    f,
                    "source counter scatter {:.2} samples estimates agree {}",
                    rate.scatter_samples,
                    yes_no(rate.estimates_agree())
                )?;
            }
            None => writeln!(
                f,
                "source ppm unavailable over {} readings",
                self.carried.drift.readings()
            )?,
        }
        writeln!(
            f,
            "source counter stalls {}",
            self.carried.drift.stalled()
        )?;
        writeln!(
            f,
            "timestamp steps {} exact {}",
            counts.timestamp_steps, counts.timestamp_steps_exact
        )?;
        writeln!(f, "send failures {}", counts.send_failures)?;
        writeln!(f, "encode failures {}", counts.encode_failures)?;
        writeln!(f, "unreadable packets {}", counts.unreadable_packets)?;
        writeln!(f, "buffer errors {}", self.buffer_errors)?;
        writeln!(f, "wakeup timeouts {}", self.wakeup_timeouts)?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(self.carried.tone.left),
            hertz(self.carried.tone.right)
        )?;
        writeln!(
            f,
            "tone channels distinct {}",
            yes_no(self.carried.tone.distinct())
        )?;

        // Everything below is for a person reading the run rather than for a
        // harness, in the order somebody asking how the numbers above came
        // about would want it.
        writeln!(f, "wire kbps {:.1}", self.wire_kbps())?;
        writeln!(f, "datagrams per s {:.2}", self.datagrams_per_s())?;
        writeln!(
            f,
            "packet frames p50 {} min {} max {}",
            packet_frames.p50, packet_frames.min, packet_frames.max
        )?;
        writeln!(
            f,
            "wakeup us p50 {} p95 {} p99 {} max {}",
            wakeups.p50, wakeups.p95, wakeups.p99, wakeups.max
        )?;
        writeln!(f, "wakeup {}", self.wakeup)?;
        if let Some(why) = &self.event_refused {
            writeln!(f, "event driven refused {why}")?;
        }
        writeln!(
            f,
            "device period default {:.3} ms minimum {:.3} ms",
            self.default_period_ms, self.minimum_period_ms
        )?;
        writeln!(f, "endpoint buffer frames {}", self.buffer_frames)?;
        writeln!(f, "frame ms {}", self.config.frame.millis())?;
        writeln!(
            f,
            "frame samples per channel {}",
            self.config.frame_samples()
        )?;
        writeln!(f, "requested kbps {}", self.config.bitrate_bps / 1_000)?;
        writeln!(f, "encoder reports bitrate {}", self.settings.bitrate_bps)?;
        writeln!(f, "application {}", self.settings.application)?;
        writeln!(
            f,
            "vbr {} constrained {} dtx {} inband fec {}",
            yes_no(self.settings.vbr),
            yes_no(self.settings.vbr_constrained),
            yes_no(self.settings.dtx),
            yes_no(self.settings.inband_fec)
        )?;
        writeln!(f, "lookahead samples {}", self.settings.lookahead)?;
        writeln!(
            f,
            "tone resolution {:.2} hz over {} frames",
            self.carried.tone.resolution_hz, self.carried.tone.analysed_frames
        )?;
        writeln!(f, "samples dropped {}", self.carried.samples_dropped)?;
        if let Some(error) = &self.first_buffer_error {
            writeln!(f, "first buffer error {error}")?;
        }
        if let Some(error) = &self.carried.first_encode_error {
            writeln!(f, "first encode error {error}")?;
        }
        if let Some(error) = &self.carried.first_send_error {
            writeln!(f, "first send error {error}")?;
        }
        writeln!(f, "libopus {}", self.libopus)
    }
}

/// Captures the endpoint mix for the requested time, encoding and sending as it
/// goes.
#[cfg(windows)]
pub fn run(options: &Options) -> Result<Measurement, SenderError> {
    use lanplay_audio_capture::{Loopback, ProAudio};
    use lanplay_telemetry::{Nanos, Timestamp};

    use crate::ffi;

    let config = CodecConfig::contract(FRAME, options.bitrate_kbps as i32 * 1_000);

    let mut loopback = Loopback::open()?;
    let format = loopback.format();

    let socket = UdpSocket::bind(options.bind).map_err(io("bind"))?;
    // Four times the frames a clean run produces, plus a floor, so that an
    // endpoint delivering unusually small packets still has its whole
    // distribution measured rather than a prefix of it.
    let expected_frames = (options.seconds / FRAME.seconds()).ceil() as usize * 4;
    let mut carrier = Carrier::new(&socket, options.send_to, config, format, expected_frames)?;
    let ssrc = carrier.ssrc();
    let rtp_base = carrier.rtp_base();

    let mut wakeup_intervals = Samples::with_capacity(expected_frames + 1_024);
    let mut wakeup_timeouts = 0u64;
    let mut buffer_errors = 0u64;
    let mut first_buffer_error: Option<String> = None;

    // Asked for before the stream starts, so that no packet is collected under
    // one scheduling and reported under another.
    let scheduled = ProAudio::join();
    let now = || Timestamp::now().as_nanos();

    loopback.start()?;
    let started = Timestamp::now();
    let deadline = started.add(Nanos::from_millis_f64(options.seconds * 1_000.0));
    let mut previous = started;

    while Timestamp::now() < deadline {
        if !loopback.wait() {
            wakeup_timeouts += 1;
        }
        let woke = Timestamp::now();
        wakeup_intervals.record(woke.saturating_since(previous).get() / 1_000);
        previous = woke;

        // Drained to empty each time round, because one signal can cover more
        // than one packet and a loop that took only the first would fall
        // steadily behind the endpoint.
        loop {
            match loopback.next(|described, bytes| carrier.carry(described, bytes, now)) {
                Ok(Some(())) => {}
                Ok(None) => break,
                Err(error) => {
                    buffer_errors += 1;
                    if first_buffer_error.is_none() {
                        first_buffer_error = Some(error.to_string());
                    }
                    break;
                }
            }
        }
    }

    let span_s = Timestamp::now().saturating_since(started).as_secs_f64();
    loopback.stop()?;
    let settings = *carrier.encoder_settings();

    Ok(Measurement {
        endpoint: loopback.endpoint().to_owned(),
        format,
        default_period_ms: loopback.default_period_ms(),
        minimum_period_ms: loopback.minimum_period_ms(),
        buffer_frames: loopback.buffer_frames(),
        wakeup: loopback.wakeup(),
        event_refused: loopback.event_refused().map(str::to_owned),
        scheduling: scheduled.granted().clone(),
        config,
        settings,
        libopus: ffi::version(),
        bind: socket.local_addr().map_err(io("local_addr"))?,
        send_to: options.send_to,
        ssrc,
        rtp_base,
        carried: carrier.finish(),
        buffer_errors,
        first_buffer_error,
        wakeup_timeouts,
        wakeup_intervals_us: wakeup_intervals.percentiles(),
        span_s,
    })
}

/// Off Windows there is no endpoint to capture, and a run that produced numbers
/// here would be describing a machine it is not on.
#[cfg(not(windows))]
pub fn run(_options: &Options) -> Result<Measurement, SenderError> {
    Err(SenderError::NoEndpoint(std::env::consts::OS))
}

#[cfg(test)]
mod tests {
    use lanplay_transport::parse_opus_packet;

    use super::*;

    /// The endpoint's own packet, from A1: 480 frames, every packet.
    const CAPTURED_FRAMES: u32 = 480;

    fn contract() -> CodecConfig {
        CodecConfig::contract(FRAME, CodecConfig::DEFAULT_BITRATE_BPS)
    }

    /// The mix format A1 measured, as the sender requires it.
    fn endpoint_format() -> MixFormat {
        MixFormat {
            sample_rate: 48_000,
            channels: 2,
            bits_per_sample: 32,
            valid_bits: 32,
            block_align: 8,
            kind: SampleKind::Float,
            channel_mask: 3,
            subformat: lanplay_audio_capture::format::SUBTYPE_IEEE_FLOAT,
            extensible: true,
        }
    }

    fn packet(position: u64, frames: u32) -> Packet {
        Packet {
            device_position: position,
            frames,
            qpc_100ns: position * 10_000 / 48,
            discontinuity: false,
            silent: false,
            timestamp_error: false,
        }
    }

    /// A packet of interleaved floats, as the engine would hand it over: two
    /// tones an octave and a bit apart so the two channels are never each
    /// other's.
    fn captured(frames: u32, from: u64) -> Vec<f32> {
        let mut samples = Vec::with_capacity(frames as usize * 2);
        for index in 0..frames as u64 {
            let time = (from + index) as f32 / 48_000.0;
            samples.push((core::f32::consts::TAU * 997.0 * time).sin() * 0.1);
            samples.push((core::f32::consts::TAU * 1997.0 * time).sin() * 0.1);
        }
        samples
    }

    fn as_bytes(samples: &[f32]) -> &[u8] {
        // SAFETY: reading a float buffer's own bytes, which is the shape the
        // engine hands over and is always a valid read of a `[f32]`.
        unsafe { core::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), size_of_val(samples)) }
    }

    /// The arithmetic the whole sender turns on: A1's packet is exactly two
    /// frames and leaves nothing behind, which is why there is no accumulator.
    #[test]
    fn the_endpoints_packet_is_exactly_two_frames() {
        let config = contract();
        assert_eq!(
            split(CAPTURED_FRAMES as usize, config.frame_samples()),
            Split {
                frames: 2,
                residue: 0
            }
        );
    }

    /// A packet that does not divide has to say so by the frame, because a
    /// residue is the finding and an accumulator would be the place it went
    /// unnoticed. 441 frames is a 44.1 kHz endpoint's 10 ms packet, which is
    /// the shape a different host would hand over.
    #[test]
    fn a_packet_that_does_not_divide_reports_its_residue() {
        assert_eq!(
            split(441, 240),
            Split {
                frames: 1,
                residue: 201
            }
        );
        assert_eq!(
            split(239, 240),
            Split {
                frames: 0,
                residue: 239
            }
        );
        // A frame duration of nothing divides nothing, and the whole packet is
        // the residue rather than a division by zero.
        assert_eq!(
            split(480, 0),
            Split {
                frames: 0,
                residue: 480
            }
        );
    }

    /// The identity A6 is decided on, over a run: every captured sample reaches
    /// the encoder, so the two counts agree exactly rather than approximately.
    #[test]
    fn every_captured_sample_is_encoded_and_sent() {
        let config = contract();
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let target = receiver.local_addr().expect("the bound address");
        let sender = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let mut carrier = Carrier::new(&sender, target, config, endpoint_format(), 64)
            .expect("the contract format is the one the sender takes");

        let packets = 10u32;
        for index in 0..packets {
            let position = u64::from(index) * u64::from(CAPTURED_FRAMES);
            let samples = captured(CAPTURED_FRAMES, position);
            carrier.carry(&packet(position, CAPTURED_FRAMES), as_bytes(&samples), || 0);
        }

        let counts = carrier.counts();
        let carried = carrier.finish();
        assert_eq!(counts.frames_encoded, u64::from(packets) * 2);
        assert_eq!(counts.datagrams_sent, counts.frames_encoded);
        assert_eq!(counts.residue_frames, 0);
        assert_eq!(counts.unreadable_packets, 0);
        assert_eq!(counts.encode_failures, 0);
        assert_eq!(counts.send_failures, 0);
        // The two counts the split is proved by: 4800 frames captured, and 20
        // frames of 240 samples encoded out of them.
        assert_eq!(carried.totals.frames, u64::from(packets * CAPTURED_FRAMES));
        assert_eq!(
            counts.frames_encoded * config.frame_samples() as u64,
            carried.totals.frames
        );
    }

    /// The timestamp is a sample counter, so it advances by the frame's
    /// per-channel sample count and by nothing else. Read off the datagrams
    /// rather than off the packetiser, because what a receiver acts on is what
    /// arrived.
    #[test]
    fn the_timestamp_advances_by_the_frames_samples() {
        let config = contract();
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let target = receiver.local_addr().expect("the bound address");
        let sender = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let mut carrier = Carrier::new(&sender, target, config, endpoint_format(), 64)
            .expect("the contract format is the one the sender takes");

        let samples = captured(CAPTURED_FRAMES, 0);
        carrier.carry(&packet(0, CAPTURED_FRAMES), as_bytes(&samples), || 0);
        let samples = captured(CAPTURED_FRAMES, u64::from(CAPTURED_FRAMES));
        carrier.carry(
            &packet(u64::from(CAPTURED_FRAMES), CAPTURED_FRAMES),
            as_bytes(&samples),
            || 0,
        );

        let mut datagram = [0u8; 1_500];
        let mut timestamps = Vec::new();
        let mut sequences = Vec::new();
        for _ in 0..4 {
            let length = receiver.recv(&mut datagram).expect("a datagram is waiting");
            let parsed = parse_opus_packet(&datagram[..length]).expect("one Opus frame per packet");
            timestamps.push(parsed.timestamp.0);
            sequences.push(parsed.sequence.0);
            assert_eq!(parsed.ssrc, carrier.ssrc());
        }

        let expected = config.frame_samples() as u32;
        for pair in timestamps.windows(2) {
            assert_eq!(
                pair[1].wrapping_sub(pair[0]),
                expected,
                "the timestamp counts samples: {timestamps:?}"
            );
        }
        for pair in sequences.windows(2) {
            assert_eq!(pair[1].wrapping_sub(pair[0]), 1, "{sequences:?}");
        }

        let counts = carrier.counts();
        assert_eq!(counts.timestamp_steps, 4);
        assert_eq!(counts.timestamp_steps_exact, counts.timestamp_steps);
    }

    /// A silent packet's bytes are undefined by the API's own account, so what
    /// goes into the encoder is zeroes and the frames still count: silence the
    /// host really played is audio the receiver has to keep playing through.
    #[test]
    fn a_silent_packet_is_encoded_as_silence_and_still_counted() {
        let config = contract();
        let receiver = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let target = receiver.local_addr().expect("the bound address");
        let sender = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let mut carrier = Carrier::new(&sender, target, config, endpoint_format(), 64)
            .expect("the contract format is the one the sender takes");

        let mut described = packet(0, CAPTURED_FRAMES);
        described.silent = true;
        // Bytes that are not silence at all, to prove the flag and not the
        // buffer decides what gets encoded.
        let samples = captured(CAPTURED_FRAMES, 0);
        carrier.carry(&described, as_bytes(&samples), || 0);

        let counts = carrier.counts();
        assert_eq!(counts.frames_encoded, 2);
        assert_eq!(counts.datagrams_sent, 2);
        let carried = carrier.finish();
        // Nothing from a silent packet reaches the tone window, so a run of
        // them reports no tone rather than the ring's leftovers.
        assert_eq!(carried.tone.analysed_frames, 0);
        assert_eq!(carried.totals.silent_packets, 1);
    }

    /// The refusal that keeps a conversion out of this path. An endpoint mixing
    /// something else is a finding for a later phase, not a resampler here.
    #[test]
    fn a_format_the_encoder_cannot_take_is_refused_rather_than_converted() {
        let config = contract();
        let sender = UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
        let target = sender.local_addr().expect("the bound address");

        let mut sixteen_bit = endpoint_format();
        sixteen_bit.kind = SampleKind::Int;
        sixteen_bit.bits_per_sample = 16;
        sixteen_bit.block_align = 4;
        assert!(matches!(
            Carrier::new(&sender, target, config, sixteen_bit, 8),
            Err(SenderError::Format { .. })
        ));

        let mut mono = endpoint_format();
        mono.channels = 1;
        mono.block_align = 4;
        assert!(matches!(
            Carrier::new(&sender, target, config, mono, 8),
            Err(SenderError::Format { .. })
        ));
    }

    /// Off Windows the run refuses instead of reporting, because the only thing
    /// worse than no measurement is one that describes another machine.
    #[test]
    #[cfg(not(windows))]
    fn there_is_no_run_without_an_endpoint() {
        let options = Options {
            send_to: "127.0.0.1:5012".parse().expect("a literal address"),
            bind: "0.0.0.0:0".parse().expect("a literal address"),
            seconds: 1.0,
            bitrate_kbps: 128,
        };
        let refused = run(&options).expect_err("there is no WASAPI endpoint here");
        assert!(refused.to_string().contains("WASAPI"), "{refused}");
    }
}
