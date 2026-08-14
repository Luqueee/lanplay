//! RTP as far as this project needs it, and no further.
//!
//! RFC 3550 for the header, RFC 8285 for the one-byte header extension that
//! carries our [`FrameId`]. Nothing here allocates: packets are written into a
//! caller-owned buffer and parsed as borrowed slices.

use core::fmt;

use lanplay_protocol::FrameId;
use lanplay_video_core::VideoTimestamp;

/// Everything an RTP packet may occupy inside one datagram.
///
/// Chosen so that the *datagram* stays clear of the Ethernet MTU with room for
/// IPv6 and any tunnelling: 1200 + 40 (IPv6) + 8 (UDP) = 1248 bytes. The RTP
/// header and its extension come out of this budget, not on top of it.
pub const MAX_UDP_PAYLOAD: usize = 1200;

/// RFC 6184 fixes the H.264 RTP clock at 90 kHz.
pub const H264_CLOCK_RATE: u32 = 90_000;

/// Dynamic payload type for our H.264 stream.
pub const H264_PAYLOAD_TYPE: u8 = 96;

/// RFC 8285 one-byte extension profile.
const ONE_BYTE_PROFILE: u16 = 0xBEDE;
/// Extension element id carrying the frame id. Must be 1..=14.
pub const FRAME_ID_EXTENSION_ID: u8 = 1;

const VERSION: u8 = 2;
/// The fixed header every packet carries, before any extension. Public because
/// a payload format that writes no extension budgets against this rather than
/// against [`HEADER_OVERHEAD`], and must not restate the number to do it.
pub const FIXED_HEADER_LEN: usize = 12;
/// Profile and length words, then a 9-byte element padded to a word boundary.
const FRAME_ID_EXTENSION_LEN: usize = 4 + 12;

/// Header bytes every packet carries once the frame id extension is included.
pub const HEADER_OVERHEAD: usize = FIXED_HEADER_LEN + FRAME_ID_EXTENSION_LEN;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Ssrc(pub u32);

impl fmt::Display for Ssrc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:08x}", self.0)
    }
}

/// A 16-bit RTP sequence number, compared the only way a wrapping counter can
/// be compared.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SequenceNumber(pub u16);

impl SequenceNumber {
    #[inline]
    pub const fn next(self) -> Self {
        SequenceNumber(self.0.wrapping_add(1))
    }

    /// Signed distance from `earlier` to `self`, in the RFC 1982 sense: the
    /// wrapped difference reinterpreted as a signed 16-bit value. Positive
    /// means `self` is ahead.
    ///
    /// This is what makes 65535 -> 0 an increment of one rather than a jump of
    /// minus 65535, and a soak at 6000 packets per second crosses that point
    /// every eleven seconds.
    #[inline]
    pub const fn distance_from(self, earlier: SequenceNumber) -> i32 {
        (self.0.wrapping_sub(earlier.0) as i16) as i32
    }

    #[inline]
    pub const fn is_after(self, other: SequenceNumber) -> bool {
        self.distance_from(other) > 0
    }
}

impl fmt::Display for SequenceNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A 32-bit RTP timestamp. Wraps, and comparisons must respect that.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RtpTimestamp(pub u32);

impl RtpTimestamp {
    /// Signed distance in ticks, wrapping-aware.
    #[inline]
    pub const fn distance_from(self, earlier: RtpTimestamp) -> i64 {
        (self.0.wrapping_sub(earlier.0) as i32) as i64
    }
}

/// Converts media timestamps into RTP ticks.
///
/// Each timestamp is computed from the media clock in one exact rational step,
/// never by adding a per-frame increment. `90000 / 120` happens to be a whole
/// 750, but `90000 / 119.88` is not, and an incremental counter would drift by
/// a tick every few frames and by a whole frame period over a soak.
#[derive(Clone, Copy, Debug)]
pub struct RtpClock {
    rate: u32,
    base: u32,
}

impl RtpClock {
    /// `base` is the random starting offset RFC 3550 asks for.
    pub const fn new(rate: u32, base: u32) -> Self {
        RtpClock { rate, base }
    }

    pub const fn rate(&self) -> u32 {
        self.rate
    }

    /// Exact conversion, rounded to the nearest tick.
    pub fn timestamp(&self, pts: VideoTimestamp) -> RtpTimestamp {
        if pts.timescale == 0 {
            return RtpTimestamp(self.base);
        }
        let numerator = i128::from(pts.value) * i128::from(self.rate);
        let timescale = i128::from(pts.timescale);
        // Round half away from zero so successive frames cannot both round
        // down and lose a tick per frame.
        let ticks = if numerator >= 0 {
            (numerator + timescale / 2) / timescale
        } else {
            (numerator - timescale / 2) / timescale
        };
        RtpTimestamp(self.base.wrapping_add(ticks as u32))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RtpHeader {
    /// Set on the last packet of an access unit, and only there.
    pub marker: bool,
    pub payload_type: u8,
    pub sequence: SequenceNumber,
    pub timestamp: RtpTimestamp,
    pub ssrc: Ssrc,
    /// Carried in an RFC 8285 extension rather than inside the payload, so the
    /// bitstream stays a bitstream.
    pub frame_id: Option<FrameId>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RtpError {
    /// Fewer bytes than a fixed header.
    TooShort { len: usize },
    /// Version field is not 2.
    BadVersion { version: u8 },
    /// The header claims more CSRCs, padding or extension than the datagram holds.
    Truncated,
    /// Padding length byte is zero or larger than the remaining payload.
    BadPadding,
    /// The caller's buffer cannot hold the packet.
    BufferTooSmall { needed: usize, available: usize },
}

impl fmt::Display for RtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RtpError::TooShort { len } => write!(f, "packet is {len} bytes, needs at least 12"),
            RtpError::BadVersion { version } => write!(f, "RTP version {version}, expected 2"),
            RtpError::Truncated => f.write_str("header describes more bytes than the packet holds"),
            RtpError::BadPadding => f.write_str("invalid RTP padding length"),
            RtpError::BufferTooSmall { needed, available } => {
                write!(f, "need {needed} bytes, buffer holds {available}")
            }
        }
    }
}

impl core::error::Error for RtpError {}

/// A parsed packet borrowing the datagram it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RtpPacket<'a> {
    pub header: RtpHeader,
    pub payload: &'a [u8],
}

/// Writes a packet into `out` and returns its length.
pub fn write_packet(header: &RtpHeader, payload: &[u8], out: &mut [u8]) -> Result<usize, RtpError> {
    let extension_len = if header.frame_id.is_some() {
        FRAME_ID_EXTENSION_LEN
    } else {
        0
    };
    let needed = FIXED_HEADER_LEN + extension_len + payload.len();
    if out.len() < needed {
        return Err(RtpError::BufferTooSmall {
            needed,
            available: out.len(),
        });
    }

    out[0] = (VERSION << 6) | if header.frame_id.is_some() { 0x10 } else { 0 };
    out[1] = (u8::from(header.marker) << 7) | (header.payload_type & 0x7F);
    out[2..4].copy_from_slice(&header.sequence.0.to_be_bytes());
    out[4..8].copy_from_slice(&header.timestamp.0.to_be_bytes());
    out[8..12].copy_from_slice(&header.ssrc.0.to_be_bytes());

    let mut cursor = FIXED_HEADER_LEN;
    if let Some(frame) = header.frame_id {
        out[cursor..cursor + 2].copy_from_slice(&ONE_BYTE_PROFILE.to_be_bytes());
        // Length counts 32-bit words of extension data, not bytes.
        out[cursor + 2..cursor + 4].copy_from_slice(&3u16.to_be_bytes());
        // One-byte element header: id in the high nibble, length minus one in
        // the low nibble.
        out[cursor + 4] = (FRAME_ID_EXTENSION_ID << 4) | 7;
        out[cursor + 5..cursor + 13].copy_from_slice(&frame.get().to_be_bytes());
        // Pad the element out to the word boundary the length field promised.
        out[cursor + 13..cursor + 16].fill(0);
        cursor += FRAME_ID_EXTENSION_LEN;
    }

    out[cursor..cursor + payload.len()].copy_from_slice(payload);
    Ok(cursor + payload.len())
}

/// Parses a datagram. Never panics, never allocates.
pub fn parse_packet(bytes: &[u8]) -> Result<RtpPacket<'_>, RtpError> {
    if bytes.len() < FIXED_HEADER_LEN {
        return Err(RtpError::TooShort { len: bytes.len() });
    }
    let version = bytes[0] >> 6;
    if version != VERSION {
        return Err(RtpError::BadVersion { version });
    }
    let has_padding = bytes[0] & 0x20 != 0;
    let has_extension = bytes[0] & 0x10 != 0;
    let csrc_count = usize::from(bytes[0] & 0x0F);

    let mut cursor = FIXED_HEADER_LEN + csrc_count * 4;
    if bytes.len() < cursor {
        return Err(RtpError::Truncated);
    }

    let mut frame_id = None;
    if has_extension {
        if bytes.len() < cursor + 4 {
            return Err(RtpError::Truncated);
        }
        let profile = u16::from_be_bytes([bytes[cursor], bytes[cursor + 1]]);
        let words = usize::from(u16::from_be_bytes([bytes[cursor + 2], bytes[cursor + 3]]));
        let data_start = cursor + 4;
        let data_end = data_start + words * 4;
        if bytes.len() < data_end {
            return Err(RtpError::Truncated);
        }
        if profile == ONE_BYTE_PROFILE {
            frame_id = read_frame_id(&bytes[data_start..data_end]);
        }
        cursor = data_end;
    }

    let mut end = bytes.len();
    if has_padding {
        let pad = usize::from(bytes[end - 1]);
        if pad == 0 || pad > end - cursor {
            return Err(RtpError::BadPadding);
        }
        end -= pad;
    }

    Ok(RtpPacket {
        header: RtpHeader {
            marker: bytes[1] & 0x80 != 0,
            payload_type: bytes[1] & 0x7F,
            sequence: SequenceNumber(u16::from_be_bytes([bytes[2], bytes[3]])),
            timestamp: RtpTimestamp(u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])),
            ssrc: Ssrc(u32::from_be_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11],
            ])),
            frame_id,
        },
        payload: &bytes[cursor..end],
    })
}

/// Walks RFC 8285 one-byte elements looking for the frame id.
fn read_frame_id(data: &[u8]) -> Option<FrameId> {
    let mut index = 0;
    while index < data.len() {
        let byte = data[index];
        // Padding between elements.
        if byte == 0 {
            index += 1;
            continue;
        }
        let id = byte >> 4;
        let len = usize::from(byte & 0x0F) + 1;
        // 15 is reserved and terminates parsing.
        if id == 15 {
            return None;
        }
        let start = index + 1;
        let end = start + len;
        if end > data.len() {
            return None;
        }
        if id == FRAME_ID_EXTENSION_ID && len == 8 {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(&data[start..end]);
            return Some(FrameId::new(u64::from_be_bytes(raw)));
        }
        index = end;
    }
    None
}

/// Random starting values, as RFC 3550 requires: a session that always began
/// at zero would collide with a stale one and be trivially spoofable.
pub fn random_u32() -> u32 {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("system random source");
    u32::from_be_bytes(bytes)
}

pub fn random_u64() -> u64 {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).expect("system random source");
    u64::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> RtpHeader {
        RtpHeader {
            marker: false,
            payload_type: H264_PAYLOAD_TYPE,
            sequence: SequenceNumber(1000),
            timestamp: RtpTimestamp(90_000),
            ssrc: Ssrc(0xDEAD_BEEF),
            frame_id: Some(FrameId::new(42)),
        }
    }

    #[test]
    fn a_packet_survives_a_write_and_parse_round_trip() {
        let mut buffer = [0u8; MAX_UDP_PAYLOAD];
        let payload = b"the quick brown fox";
        let written = write_packet(&header(), payload, &mut buffer).expect("writes");
        assert_eq!(written, HEADER_OVERHEAD + payload.len());

        let parsed = parse_packet(&buffer[..written]).expect("parses");
        assert_eq!(parsed.header, header());
        assert_eq!(parsed.payload, payload);
    }

    #[test]
    fn the_marker_bit_survives_the_round_trip() {
        let mut buffer = [0u8; MAX_UDP_PAYLOAD];
        let marked = RtpHeader {
            marker: true,
            ..header()
        };
        let written = write_packet(&marked, b"x", &mut buffer).unwrap();
        assert!(parse_packet(&buffer[..written]).unwrap().header.marker);
    }

    #[test]
    fn a_packet_without_an_extension_reports_no_frame_id() {
        let mut buffer = [0u8; MAX_UDP_PAYLOAD];
        let plain = RtpHeader {
            frame_id: None,
            ..header()
        };
        let written = write_packet(&plain, b"x", &mut buffer).unwrap();
        assert_eq!(written, FIXED_HEADER_LEN + 1);
        assert_eq!(
            parse_packet(&buffer[..written]).unwrap().header.frame_id,
            None
        );
    }

    #[test]
    fn sequence_numbers_compare_across_the_wrap() {
        let before = SequenceNumber(65_534);
        let at = SequenceNumber(65_535);
        let after = SequenceNumber(0);
        let later = SequenceNumber(1);

        assert_eq!(at.distance_from(before), 1);
        assert_eq!(after.distance_from(at), 1);
        assert_eq!(later.distance_from(before), 3);
        assert_eq!(before.distance_from(later), -3);
        assert!(after.is_after(at));
        assert!(later.is_after(before));
        assert!(!before.is_after(later));
        assert_eq!(at.next(), after);
    }

    #[test]
    fn timestamps_compare_across_the_wrap() {
        let before = RtpTimestamp(u32::MAX - 100);
        let after = RtpTimestamp(650);
        assert_eq!(after.distance_from(before), 751);
        assert_eq!(before.distance_from(after), -751);
    }

    #[test]
    fn a_whole_number_of_ticks_per_frame_is_exact() {
        let clock = RtpClock::new(H264_CLOCK_RATE, 0);
        for index in 0..1_000u64 {
            let pts = VideoTimestamp::from_frame_index(index, 120, 1);
            assert_eq!(clock.timestamp(pts).0, index as u32 * 750);
        }
    }

    #[test]
    fn a_fractional_frame_rate_does_not_drift() {
        // 119.88 fps: 750.75 ticks per frame. An incremental counter would be
        // a whole frame period out after 4000 frames; an exact conversion is
        // never more than half a tick out.
        let clock = RtpClock::new(H264_CLOCK_RATE, 0);
        for index in [1u64, 4_000, 100_000] {
            let pts = VideoTimestamp::from_frame_index(index, 120_000, 1001);
            let expected = (index as f64 * 750.75).round() as u32;
            let actual = clock.timestamp(pts).0;
            assert!(
                actual.abs_diff(expected) <= 1,
                "frame {index}: {actual} against {expected}"
            );
        }
    }

    #[test]
    fn the_clock_base_is_an_offset_not_an_origin() {
        let clock = RtpClock::new(H264_CLOCK_RATE, u32::MAX - 10);
        let first = clock.timestamp(VideoTimestamp::from_frame_index(0, 120, 1));
        let second = clock.timestamp(VideoTimestamp::from_frame_index(1, 120, 1));
        assert_eq!(first.0, u32::MAX - 10);
        // Wraps rather than saturating.
        assert_eq!(second.distance_from(first), 750);
    }

    #[test]
    fn malformed_packets_are_rejected_without_panicking() {
        assert_eq!(parse_packet(&[]), Err(RtpError::TooShort { len: 0 }));
        assert_eq!(
            parse_packet(&[0u8; 11]),
            Err(RtpError::TooShort { len: 11 })
        );

        let mut wrong_version = [0u8; 12];
        wrong_version[0] = 0x40;
        assert_eq!(
            parse_packet(&wrong_version),
            Err(RtpError::BadVersion { version: 1 })
        );

        // Claims four CSRCs it does not carry.
        let mut lying_csrc = [0u8; 12];
        lying_csrc[0] = 0x84;
        assert_eq!(parse_packet(&lying_csrc), Err(RtpError::Truncated));

        // Claims an extension that runs off the end.
        let mut lying_extension = [0u8; 16];
        lying_extension[0] = 0x90;
        lying_extension[14] = 0xFF;
        assert_eq!(parse_packet(&lying_extension), Err(RtpError::Truncated));

        let mut bad_padding = [0u8; 13];
        bad_padding[0] = 0xA0;
        bad_padding[12] = 200;
        assert_eq!(parse_packet(&bad_padding), Err(RtpError::BadPadding));
    }

    #[test]
    fn every_truncation_of_a_valid_packet_is_rejected_cleanly() {
        let mut buffer = [0u8; MAX_UDP_PAYLOAD];
        let written = write_packet(&header(), &[7u8; 64], &mut buffer).unwrap();
        for length in 0..written {
            // Some prefixes are structurally valid shorter packets; what must
            // never happen is a panic or an out-of-bounds read.
            let _ = parse_packet(&buffer[..length]);
        }
    }

    #[test]
    fn an_unknown_extension_element_is_skipped_not_misread() {
        let mut buffer = [0u8; 64];
        buffer[0] = 0x90;
        buffer[1] = H264_PAYLOAD_TYPE;
        buffer[12..14].copy_from_slice(&ONE_BYTE_PROFILE.to_be_bytes());
        buffer[14..16].copy_from_slice(&3u16.to_be_bytes());
        // Element id 5, length 2, then our frame id element after it.
        buffer[16] = (5 << 4) | 1;
        buffer[17] = 0xAA;
        buffer[18] = 0xBB;
        buffer[19] = (FRAME_ID_EXTENSION_ID << 4) | 7;
        buffer[20..28].copy_from_slice(&99u64.to_be_bytes());
        let parsed = parse_packet(&buffer[..28]).expect("parses");
        assert_eq!(parsed.header.frame_id, Some(FrameId::new(99)));
    }

    #[test]
    fn a_buffer_that_cannot_hold_the_packet_is_an_error_not_a_truncation() {
        let mut tiny = [0u8; 20];
        assert_eq!(
            write_packet(&header(), &[0u8; 64], &mut tiny),
            Err(RtpError::BufferTooSmall {
                needed: HEADER_OVERHEAD + 64,
                available: 20
            })
        );
    }

    #[test]
    fn random_starting_values_are_not_constant() {
        // RFC 3550 requires unpredictable initial values; the weakest useful
        // check is that two draws differ.
        assert_ne!(random_u32(), random_u32());
        assert_ne!(random_u64(), random_u64());
    }
}
