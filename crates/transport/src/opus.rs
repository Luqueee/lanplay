//! RFC 7587: one Opus frame in one RTP datagram, and nothing else.
//!
//! The previous phase measured what the encoder produces at the settings this
//! project runs: 81 bytes per 5 ms frame at 128 kbps, p50 and p99 alike. An
//! 81-byte payload cannot approach an MTU, so there is no fragmentation to
//! design here and no reassembly state to keep — which is why the receiving
//! side is a function rather than a type. The video path needs a depacketiser
//! that remembers things because a picture spans datagrams; a frame of audio
//! that arrives has arrived whole.
//!
//! The timestamp is a sample counter, and that is a property of
//! [`OpusPacketizer`] rather than of whoever calls it. RFC 7587 section 4.1
//! fixes the clock at 48000 Hz for every Opus mode and every sampling rate, and
//! each packet advances it by the frame's per-channel sample count. The video
//! packetiser derives its timestamp from a presentation time because a picture's
//! identity is the moment it was captured; an audio timestamp derived the same
//! way would carry the sender's scheduling jitter into the stream's own notion
//! of time, and a receiver would then be unable to tell a late packet from a
//! packet describing a later moment. A caller who could hand in a clock reading
//! would eventually hand in a clock reading, so [`OpusPacketizer::next`] takes a
//! sample count and keeps the counter itself.
//!
//! No header extension. Video carries a frame id in one because a fragment has
//! to say which picture it belongs to; here the sequence number and the
//! timestamp identify a frame completely, and sixteen bytes of extension against
//! an eighty-one byte payload would be a fifth of the stream spent restating
//! what is already known.
//!
//! Every check refuses rather than repairs, and each refusal has its own name. A
//! receiver that quietly accepted a video packet on the audio socket would hand
//! H.264 bytes to a decoder that would either fail or produce noise, and neither
//! outcome names the socket the packet arrived on.
//!
//! What is deliberately absent, because this phase measures loss instead of
//! hiding it: no jitter buffer, no concealment, no retransmission, no
//! forward error correction. There is no seam left half-built for any of them
//! either. A gap in the sequence numbers is a gap a receiver reports, and the
//! decoder is simply not fed.

use core::fmt;

use crate::rtp::{
    FIXED_HEADER_LEN, MAX_UDP_PAYLOAD, RtpError, RtpHeader, RtpTimestamp, SequenceNumber, Ssrc,
    parse_packet, random_u32, write_packet,
};

/// RFC 7587 section 4.1: the RTP timestamp for Opus runs at 48000 Hz whatever
/// the encoder's own sampling rate is.
pub const OPUS_CLOCK_RATE: u32 = 48_000;

/// Dynamic payload type for the audio stream.
///
/// 111 is what every WebRTC endpoint uses for Opus, so a capture of this stream
/// is readable by ordinary tools without being told anything. Video already
/// holds 96.
pub const OPUS_PAYLOAD_TYPE: u8 = 111;

/// Per-channel samples in the six frame durations Opus can code, at the 48 kHz
/// clock RFC 7587 fixes: 2.5, 5, 10, 20, 40 and 60 ms.
///
/// A packet holding several frames would advance the timestamp by a multiple of
/// one of these, and is not produced here: the encoder is configured for one
/// frame per packet, and accepting a multiple would mean accepting a duration no
/// decoder in this project is sized for.
pub const FRAME_SAMPLE_COUNTS: [u32; 6] = [120, 240, 480, 960, 1920, 2880];

/// Opus bytes one datagram can carry.
///
/// The RTP header comes out of the datagram budget rather than sitting on top of
/// it. libopus documents 1276 bytes as the ceiling for a single frame, which is
/// larger than this: a frame that big cannot be sent, and the packetiser says so
/// instead of truncating it.
pub const MAX_OPUS_PAYLOAD: usize = MAX_UDP_PAYLOAD - FIXED_HEADER_LEN;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpusPacketizeError {
    /// A frame with no bytes. libopus reads a zero-length packet as packet loss
    /// and runs its concealer, so sending one would ask the far end to
    /// fabricate audio and then report it as decoded.
    EmptyFrame,
    /// More Opus than a datagram holds.
    PayloadTooLarge { bytes: usize, capacity: usize },
    /// A sample count Opus cannot have produced. Advancing the timestamp by it
    /// would put the stream's clock somewhere no decoder could follow, and the
    /// caller that computed it is the one that needs to hear about it.
    NotAFrameLength { samples: u32 },
}

impl fmt::Display for OpusPacketizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpusPacketizeError::EmptyFrame => {
                f.write_str("an Opus frame of no bytes reads as packet loss at the far end")
            }
            OpusPacketizeError::PayloadTooLarge { bytes, capacity } => {
                write!(f, "{bytes} bytes of Opus, and a datagram holds {capacity}")
            }
            OpusPacketizeError::NotAFrameLength { samples } => write!(
                f,
                "{samples} samples per channel is not an Opus frame at {OPUS_CLOCK_RATE} Hz; \
                 the frame durations are 2.5, 5, 10, 20, 40 and 60 ms"
            ),
        }
    }
}

impl core::error::Error for OpusPacketizeError {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpusParseError {
    /// The datagram is not RTP at all.
    Rtp(RtpError),
    /// RTP, but another stream's. The audio and video streams run on separate
    /// sockets, so this is a misconfiguration or a stray sender rather than
    /// something to demultiplex.
    WrongPayloadType { found: u8 },
    /// A header with nothing behind it. Feeding it to libopus would invoke the
    /// concealer, which is the one thing this phase must not do.
    EmptyPayload,
}

impl From<RtpError> for OpusParseError {
    fn from(error: RtpError) -> Self {
        OpusParseError::Rtp(error)
    }
}

impl fmt::Display for OpusParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpusParseError::Rtp(error) => write!(f, "{error}"),
            OpusParseError::WrongPayloadType { found } => write!(
                f,
                "payload type {found} on the audio stream, which carries {OPUS_PAYLOAD_TYPE}"
            ),
            OpusParseError::EmptyPayload => {
                f.write_str("an RTP header with no Opus behind it reads as packet loss")
            }
        }
    }
}

impl core::error::Error for OpusParseError {}

/// Turns encoded Opus frames into RTP datagrams.
///
/// Takes bytes and a sample count rather than an encoder, so this crate stays
/// free of libopus and can still be cross-checked for Windows from a machine
/// with no C toolchain.
pub struct OpusPacketizer {
    ssrc: Ssrc,
    sequence: SequenceNumber,
    /// The sample counter. Only [`OpusPacketizer::next`] moves it, and only by
    /// the sample count of the frame it just wrote.
    timestamp: RtpTimestamp,
    /// Boxed so moving a packetiser moves a pointer rather than the datagram.
    packet: Box<[u8; MAX_UDP_PAYLOAD]>,
}

impl OpusPacketizer {
    /// Starts at a random sequence number and a random timestamp, as RFC 3550
    /// requires of both.
    pub fn new(ssrc: Ssrc) -> Self {
        OpusPacketizer::with_start(
            ssrc,
            SequenceNumber(random_u32() as u16),
            RtpTimestamp(random_u32()),
        )
    }

    /// Same, from a chosen starting point. Tests need to sit on a wrap
    /// deliberately rather than one run in sixty-five thousand.
    pub fn with_start(ssrc: Ssrc, sequence: SequenceNumber, timestamp: RtpTimestamp) -> Self {
        OpusPacketizer {
            ssrc,
            sequence,
            timestamp,
            packet: Box::new([0; MAX_UDP_PAYLOAD]),
        }
    }

    pub fn ssrc(&self) -> Ssrc {
        self.ssrc
    }

    pub fn next_sequence(&self) -> SequenceNumber {
        self.sequence
    }

    pub fn next_timestamp(&self) -> RtpTimestamp {
        self.timestamp
    }

    /// Writes one datagram for one encoded frame and advances the counters.
    ///
    /// The slice borrows the buffer this packetiser owns and is valid until the
    /// next call, which is the same bargain the video packetiser makes: a frame
    /// costs a copy into the packet behind its header and no allocation at all.
    ///
    /// The marker bit is never set. RFC 3551 section 4.1 gives it to the first
    /// packet of a talkspurt, and this stream has exactly one: discontinuous
    /// transmission is off, so the sender emits a frame every frame period from
    /// the first to the last and there is no silence for a talkspurt to begin
    /// after.
    pub fn next(
        &mut self,
        frame: &[u8],
        samples_per_channel: u32,
    ) -> Result<&[u8], OpusPacketizeError> {
        if frame.is_empty() {
            return Err(OpusPacketizeError::EmptyFrame);
        }
        if frame.len() > MAX_OPUS_PAYLOAD {
            return Err(OpusPacketizeError::PayloadTooLarge {
                bytes: frame.len(),
                capacity: MAX_OPUS_PAYLOAD,
            });
        }
        if !FRAME_SAMPLE_COUNTS.contains(&samples_per_channel) {
            return Err(OpusPacketizeError::NotAFrameLength {
                samples: samples_per_channel,
            });
        }

        let header = RtpHeader {
            marker: false,
            payload_type: OPUS_PAYLOAD_TYPE,
            sequence: self.sequence,
            timestamp: self.timestamp,
            ssrc: self.ssrc,
            frame_id: None,
        };
        let length = write_packet(&header, frame, self.packet.as_mut_slice())
            .expect("a payload bounded by MAX_OPUS_PAYLOAD leaves room for the fixed header");

        self.sequence = self.sequence.next();
        // Wrapping, because a 32-bit sample counter at 48 kHz turns over every
        // twenty-five hours and a session may outlive that.
        self.timestamp = RtpTimestamp(self.timestamp.0.wrapping_add(samples_per_channel));
        Ok(&self.packet[..length])
    }
}

/// One frame of Opus, borrowed from the datagram it arrived in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OpusPacket<'a> {
    pub ssrc: Ssrc,
    pub sequence: SequenceNumber,
    pub timestamp: RtpTimestamp,
    /// Reported rather than ignored so that a sender using discontinuous
    /// transmission is visible instead of merely puzzling.
    pub marker: bool,
    pub payload: &'a [u8],
}

/// Reads one datagram back.
///
/// A header extension is tolerated and skipped, as RFC 8285 requires of a
/// receiver that does not know an element: this packetiser writes none, but
/// refusing one would make the stream unreadable by any sender that added, say,
/// an audio level.
pub fn parse_opus_packet(bytes: &[u8]) -> Result<OpusPacket<'_>, OpusParseError> {
    let packet = parse_packet(bytes)?;
    if packet.header.payload_type != OPUS_PAYLOAD_TYPE {
        return Err(OpusParseError::WrongPayloadType {
            found: packet.header.payload_type,
        });
    }
    if packet.payload.is_empty() {
        return Err(OpusParseError::EmptyPayload);
    }
    Ok(OpusPacket {
        ssrc: packet.header.ssrc,
        sequence: packet.header.sequence,
        timestamp: packet.header.timestamp,
        marker: packet.header.marker,
        payload: packet.payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp::H264_PAYLOAD_TYPE;

    /// The 5 ms frame this project runs, in per-channel samples.
    const CONTRACT_SAMPLES: u32 = 240;

    /// An 81-byte frame, which is what the encoder measured at 128 kbps
    /// actually produces, with a body that differs from any other frame's.
    fn frame(seed: u8) -> Vec<u8> {
        (0..81u8)
            .map(|index| index.wrapping_mul(37).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn timestamp_counts_samples_and_wraps_at_thirty_two_bits() {
        // Started so that the wrap falls a hundred frames in, and run long
        // enough afterwards that a counter which reset or stalled at the wrap
        // could not stay in step.
        let start = u32::MAX - 99 * CONTRACT_SAMPLES;
        let mut tx = OpusPacketizer::with_start(Ssrc(1), SequenceNumber(0), RtpTimestamp(start));

        let mut expected = start;
        let mut wrapped = false;
        for index in 0..200_000u32 {
            let datagram = tx
                .next(&frame(index as u8), CONTRACT_SAMPLES)
                .expect("sent");
            let packet = parse_opus_packet(datagram).expect("parses");
            assert_eq!(packet.timestamp, RtpTimestamp(expected), "frame {index}");

            let next = expected.wrapping_add(CONTRACT_SAMPLES);
            wrapped |= next < expected;
            expected = next;
        }

        assert!(
            wrapped,
            "the run must cross the 32-bit wrap to prove anything"
        );
        assert_eq!(tx.next_timestamp(), RtpTimestamp(expected));
    }

    #[test]
    fn sequence_wraps_at_sixteen_bits() {
        let mut tx = OpusPacketizer::with_start(Ssrc(2), SequenceNumber(65_530), RtpTimestamp(0));
        let mut seen = Vec::new();
        for index in 0..8u8 {
            let datagram = tx.next(&frame(index), CONTRACT_SAMPLES).expect("sent");
            seen.push(parse_opus_packet(datagram).expect("parses").sequence.0);
        }
        assert_eq!(
            seen,
            vec![65_530, 65_531, 65_532, 65_533, 65_534, 65_535, 0, 1]
        );
        // The wrap is an increment of one, not a jump backwards of 65535.
        assert_eq!(
            SequenceNumber(0).distance_from(SequenceNumber(65_535)),
            1,
            "a receiver reading the wrap as a gap would report 65535 lost packets"
        );
    }

    #[test]
    fn a_payload_too_large_for_a_datagram_is_refused() {
        let mut tx = OpusPacketizer::new(Ssrc(3));
        let before = (tx.next_sequence(), tx.next_timestamp());
        let oversized = vec![0u8; MAX_OPUS_PAYLOAD + 1];
        assert_eq!(
            tx.next(&oversized, CONTRACT_SAMPLES),
            Err(OpusPacketizeError::PayloadTooLarge {
                bytes: MAX_OPUS_PAYLOAD + 1,
                capacity: MAX_OPUS_PAYLOAD,
            })
        );
        assert_eq!(
            tx.next(&[], CONTRACT_SAMPLES),
            Err(OpusPacketizeError::EmptyFrame)
        );
        // Neither refusal moved the stream on. One that had would leave a hole
        // in the sequence that a receiver could only read as a lost packet.
        assert_eq!((tx.next_sequence(), tx.next_timestamp()), before);

        // The largest payload that does fit still fits.
        let largest = vec![7u8; MAX_OPUS_PAYLOAD];
        let datagram = tx.next(&largest, CONTRACT_SAMPLES).expect("sent");
        assert_eq!(datagram.len(), MAX_UDP_PAYLOAD);
    }

    #[test]
    fn a_sample_count_that_is_not_an_opus_frame_is_refused() {
        let mut tx = OpusPacketizer::new(Ssrc(4));
        let body = frame(0);
        // 241 is a mistyped 240; 480 000 is a clock reading in microseconds
        // that a caller might mistake for a sample count, which is the misuse
        // this signature exists to prevent.
        for samples in [0, 1, 239, 241, 320, 2_881, 480_000] {
            assert_eq!(
                tx.next(&body, samples),
                Err(OpusPacketizeError::NotAFrameLength { samples }),
                "{samples} samples"
            );
        }
        for samples in FRAME_SAMPLE_COUNTS {
            tx.next(&body, samples)
                .expect("every Opus frame length is accepted");
        }
    }

    #[test]
    fn a_packet_from_another_stream_is_refused() {
        let header = RtpHeader {
            marker: true,
            payload_type: H264_PAYLOAD_TYPE,
            sequence: SequenceNumber(9),
            timestamp: RtpTimestamp(90_000),
            ssrc: Ssrc(5),
            frame_id: None,
        };
        let mut datagram = [0u8; MAX_UDP_PAYLOAD];
        let length = write_packet(&header, &frame(1), &mut datagram).expect("written");
        assert_eq!(
            parse_opus_packet(&datagram[..length]),
            Err(OpusParseError::WrongPayloadType {
                found: H264_PAYLOAD_TYPE
            })
        );

        // A header with no payload is the other packet that must not reach a
        // decoder, because libopus would conceal it and report audio.
        let audio = RtpHeader {
            payload_type: OPUS_PAYLOAD_TYPE,
            ..header
        };
        let length = write_packet(&audio, &[], &mut datagram).expect("written");
        assert_eq!(
            parse_opus_packet(&datagram[..length]),
            Err(OpusParseError::EmptyPayload)
        );

        assert!(matches!(
            parse_opus_packet(&datagram[..4]),
            Err(OpusParseError::Rtp(RtpError::TooShort { len: 4 }))
        ));
    }

    #[test]
    fn a_round_trip_preserves_every_field() {
        let ssrc = Ssrc(0xDEAD_BEEF);
        let mut tx =
            OpusPacketizer::with_start(ssrc, SequenceNumber(4_242), RtpTimestamp(0x1234_5678));
        let body = frame(11);
        let datagram = tx.next(&body, CONTRACT_SAMPLES).expect("sent");

        // No extension and no padding: the whole datagram is twelve bytes of
        // header and the frame, which is the property that makes the extension
        // decision visible rather than merely stated.
        assert_eq!(datagram.len(), FIXED_HEADER_LEN + body.len());

        let packet = parse_opus_packet(datagram).expect("parses");
        assert_eq!(packet.ssrc, ssrc);
        assert_eq!(packet.sequence, SequenceNumber(4_242));
        assert_eq!(packet.timestamp, RtpTimestamp(0x1234_5678));
        assert!(!packet.marker);
        assert_eq!(packet.payload, &body[..]);
        let first_timestamp = packet.timestamp;

        let second = tx.next(&body, CONTRACT_SAMPLES).expect("sent");
        let second = parse_opus_packet(second).expect("parses");
        assert_eq!(second.sequence, SequenceNumber(4_243));
        assert_eq!(
            second.timestamp.distance_from(first_timestamp),
            i64::from(CONTRACT_SAMPLES)
        );
        assert_eq!(second.ssrc, ssrc);
    }
}
