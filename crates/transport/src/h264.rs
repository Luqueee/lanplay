//! RFC 6184 packetisation mode 1, and nothing else.
//!
//! Two packet types carry a screen stream: a NAL unit that fits in one
//! datagram goes out whole, and one that does not is split into FU-A
//! fragments. STAP-A would save a few headers on the tiny SEI and delimiter
//! NALs, but it also invites a decoder to wait for a bundle, and interleaved
//! modes exist for networks that reorder on purpose. Neither belongs on a LAN
//! that is trying to be invisible.
//!
//! Two invariants the rest of the pipeline depends on:
//!
//! * every packet of one access unit carries the *same* RTP timestamp,
//!   computed once from the media clock. A picture encoded as ten slices is
//!   one frame with ten NAL units, and our fixtures really do contain those;
//! * the marker bit is set on the last packet of the last NAL unit and
//!   nowhere else. It is the only frame boundary the receiver can trust.

use core::fmt;

use lanplay_video_core::{EncodedAccessUnit, avcc_nal_units};

use crate::rtp::{
    HEADER_OVERHEAD, MAX_UDP_PAYLOAD, RtpClock, RtpHeader, RtpTimestamp, SequenceNumber, Ssrc,
    random_u32, write_packet,
};

/// AVCC prefix width used everywhere in this project.
pub const NAL_LENGTH_SIZE: u8 = 4;

/// RFC 6184 fragmentation unit without a DON.
const FU_A_TYPE: u8 = 28;
/// FU indicator plus FU header.
const FU_HEADER_LEN: usize = 2;
const FU_START: u8 = 0x80;
const FU_END: u8 = 0x40;
const NAL_TYPE_MASK: u8 = 0x1F;
/// F and NRI, copied from the original NAL header onto the FU indicator.
const NAL_REF_MASK: u8 = 0xE0;

/// Smallest MTU that can carry a stream.
///
/// A single-NAL packet only needs one payload byte, but an MTU that leaves no
/// room past the two FU bytes would make a fragmented NAL emit packets
/// forever without consuming input. The floor is set by the case that can
/// stall, not by the case that cannot.
pub const MINIMUM_MTU: usize = HEADER_OVERHEAD + FU_HEADER_LEN + 1;

/// What one access unit cost on the wire.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct PacketizedAu {
    pub packets: u32,
    pub bytes: u64,
    pub single_nal: u32,
    pub fu_a: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PacketizeError {
    EmptyAccessUnit,
    /// The AVCC length prefixes do not tile the buffer exactly. Guessing a NAL
    /// boundary here would put a plausible-looking corrupt frame on the wire.
    MalformedAvcc,
    MtuTooSmall {
        mtu: usize,
        minimum: usize,
    },
}

impl fmt::Display for PacketizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacketizeError::EmptyAccessUnit => f.write_str("access unit carries no bytes"),
            PacketizeError::MalformedAvcc => {
                f.write_str("AVCC length prefixes do not describe the buffer")
            }
            PacketizeError::MtuTooSmall { mtu, minimum } => {
                write!(f, "mtu {mtu} is below the {minimum} byte minimum")
            }
        }
    }
}

impl core::error::Error for PacketizeError {}

/// Turns access units into RTP datagrams.
///
/// Owns the one datagram buffer it needs, so a steady-state frame costs no
/// allocation at all: NAL bytes are copied straight from the encoder's access
/// unit into the packet behind its header.
pub struct Packetizer {
    ssrc: Ssrc,
    clock: RtpClock,
    payload_type: u8,
    mtu: usize,
    sequence: SequenceNumber,
    /// Boxed so moving a `Packetizer` moves a pointer, not 1200 bytes.
    packet: Box<[u8; MAX_UDP_PAYLOAD]>,
}

impl Packetizer {
    /// Starts at a random sequence number, as RFC 3550 requires.
    pub fn new(ssrc: Ssrc, clock: RtpClock, payload_type: u8, mtu: usize) -> Self {
        Self::with_sequence(
            ssrc,
            clock,
            payload_type,
            mtu,
            SequenceNumber(random_u32() as u16),
        )
    }

    /// Same, with a caller-chosen starting sequence number. Tests need to sit
    /// on the wrap point deliberately rather than one run in 65536.
    pub fn with_sequence(
        ssrc: Ssrc,
        clock: RtpClock,
        payload_type: u8,
        mtu: usize,
        sequence: SequenceNumber,
    ) -> Self {
        Packetizer {
            ssrc,
            clock,
            payload_type,
            // A datagram larger than the buffer cannot be built, and an MTU
            // above it would only be a lie about what went out.
            mtu: mtu.min(MAX_UDP_PAYLOAD),
            sequence,
            packet: Box::new([0; MAX_UDP_PAYLOAD]),
        }
    }

    pub fn ssrc(&self) -> Ssrc {
        self.ssrc
    }

    pub fn next_sequence(&self) -> SequenceNumber {
        self.sequence
    }

    /// Effective MTU, after clamping to what one datagram can hold.
    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// Emits every RTP datagram for one access unit, in order. The slice
    /// handed to `emit` is valid only until the next call.
    pub fn packetize(
        &mut self,
        access_unit: &EncodedAccessUnit,
        mut emit: impl FnMut(&[u8]),
    ) -> Result<PacketizedAu, PacketizeError> {
        if self.mtu < MINIMUM_MTU {
            return Err(PacketizeError::MtuTooSmall {
                mtu: self.mtu,
                minimum: MINIMUM_MTU,
            });
        }
        if access_unit.data.is_empty() {
            return Err(PacketizeError::EmptyAccessUnit);
        }
        // Counted up front: the marker bit belongs to the last packet of the
        // last NAL, which cannot be recognised while streaming forwards.
        let nal_count = count_nal_units(&access_unit.data)?;

        let timestamp = self.clock.timestamp(access_unit.pts);
        let single_capacity = self.mtu - HEADER_OVERHEAD;
        let fragment_capacity = single_capacity - FU_HEADER_LEN;
        let mut report = PacketizedAu::default();

        for (index, nal) in avcc_nal_units(&access_unit.data, NAL_LENGTH_SIZE).enumerate() {
            let last_nal = index + 1 == nal_count;

            if nal.len() <= single_capacity {
                let head = self.write_header(last_nal, timestamp, access_unit);
                self.packet[head..head + nal.len()].copy_from_slice(nal);
                let total = head + nal.len();
                emit(&self.packet[..total]);
                report.packets += 1;
                report.single_nal += 1;
                report.bytes += total as u64;
                continue;
            }

            // The original header byte is not repeated in the payload: its F
            // and NRI bits live on the FU indicator, its type on the FU
            // header, and the receiver rebuilds it from the two.
            let indicator = (nal[0] & NAL_REF_MASK) | FU_A_TYPE;
            let nal_type = nal[0] & NAL_TYPE_MASK;
            let body = &nal[1..];
            let fragments = body.len().div_ceil(fragment_capacity);

            for (fragment, chunk) in body.chunks(fragment_capacity).enumerate() {
                let first = fragment == 0;
                let last = fragment + 1 == fragments;
                let head = self.write_header(last_nal && last, timestamp, access_unit);
                self.packet[head] = indicator;
                self.packet[head + 1] =
                    (u8::from(first) * FU_START) | (u8::from(last) * FU_END) | nal_type;
                self.packet[head + FU_HEADER_LEN..head + FU_HEADER_LEN + chunk.len()]
                    .copy_from_slice(chunk);
                let total = head + FU_HEADER_LEN + chunk.len();
                emit(&self.packet[..total]);
                report.packets += 1;
                report.fu_a += 1;
                report.bytes += total as u64;
            }
        }

        Ok(report)
    }

    /// Writes the fixed header and extension, returning where the payload
    /// starts, and consumes one sequence number.
    fn write_header(
        &mut self,
        marker: bool,
        timestamp: RtpTimestamp,
        access_unit: &EncodedAccessUnit,
    ) -> usize {
        let header = RtpHeader {
            marker,
            payload_type: self.payload_type,
            sequence: self.sequence,
            timestamp,
            ssrc: self.ssrc,
            // Twelve bytes per packet buys unambiguous packet-to-frame
            // attribution in the telemetry, which is the whole point of this
            // phase.
            frame_id: Some(access_unit.id),
        };
        self.sequence = self.sequence.next();
        write_packet(&header, &[], self.packet.as_mut_slice())
            .expect("a bare header always fits a MAX_UDP_PAYLOAD buffer")
    }
}

/// Verifies that the AVCC prefixes tile `data` exactly, and counts the NALs.
///
/// `avcc_nal_units` stops silently at a truncated prefix, so the only way to
/// tell a well-formed buffer from a truncated one is to check that the walk
/// consumed everything.
fn count_nal_units(data: &[u8]) -> Result<usize, PacketizeError> {
    let mut count = 0usize;
    let mut consumed = 0usize;
    for nal in avcc_nal_units(data, NAL_LENGTH_SIZE) {
        consumed += usize::from(NAL_LENGTH_SIZE) + nal.len();
        count += 1;
    }
    if count == 0 || consumed != data.len() {
        return Err(PacketizeError::MalformedAvcc);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use lanplay_protocol::FrameId;
    use lanplay_video_core::{VideoTimestamp, to_avcc};

    use super::*;
    use crate::rtp::{H264_CLOCK_RATE, H264_PAYLOAD_TYPE, parse_packet};

    fn access_unit(nals: &[Vec<u8>]) -> EncodedAccessUnit {
        EncodedAccessUnit {
            id: FrameId::new(7),
            pts: VideoTimestamp::new(3, 120),
            is_idr: false,
            data: to_avcc(nals.iter().map(|nal| nal.as_slice()), NAL_LENGTH_SIZE),
        }
    }

    fn packetizer(mtu: usize) -> Packetizer {
        Packetizer::with_sequence(
            Ssrc(0x1234_5678),
            RtpClock::new(H264_CLOCK_RATE, 1000),
            H264_PAYLOAD_TYPE,
            mtu,
            SequenceNumber(10),
        )
    }

    fn collect(packetizer: &mut Packetizer, unit: &EncodedAccessUnit) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        packetizer
            .packetize(unit, |datagram| out.push(datagram.to_vec()))
            .expect("packetises");
        out
    }

    #[test]
    fn one_small_nal_is_one_unfragmented_packet() {
        let unit = access_unit(&[vec![0x65; 64]]);
        let mut packetizer = packetizer(MAX_UDP_PAYLOAD);
        let packets = collect(&mut packetizer, &unit);

        assert_eq!(packets.len(), 1);
        let parsed = parse_packet(&packets[0]).expect("valid");
        assert!(parsed.header.marker);
        assert_eq!(parsed.payload, &[0x65; 64]);
        assert_eq!(parsed.header.frame_id, Some(FrameId::new(7)));
    }

    #[test]
    fn every_packet_of_an_access_unit_shares_one_timestamp() {
        let unit = access_unit(&[vec![0x41; 4000], vec![0x41; 32], vec![0x41; 9000]]);
        let mut packetizer = packetizer(MAX_UDP_PAYLOAD);
        let packets = collect(&mut packetizer, &unit);

        let timestamps: Vec<_> = packets
            .iter()
            .map(|p| parse_packet(p).expect("valid").header.timestamp)
            .collect();
        assert!(timestamps.windows(2).all(|w| w[0] == w[1]));
        let markers = packets
            .iter()
            .filter(|p| parse_packet(p).expect("valid").header.marker)
            .count();
        assert_eq!(markers, 1);
        assert!(
            parse_packet(packets.last().expect("packets"))
                .expect("valid")
                .header
                .marker
        );
    }

    #[test]
    fn fu_a_fragments_carry_start_and_end_exactly_once() {
        let unit = access_unit(&[vec![0x65; 8000]]);
        let mut packetizer = packetizer(MAX_UDP_PAYLOAD);
        let packets = collect(&mut packetizer, &unit);
        assert!(packets.len() > 1);

        let mut starts = 0;
        let mut ends = 0;
        for packet in &packets {
            let payload = parse_packet(packet).expect("valid").payload;
            assert_eq!(payload[0] & NAL_TYPE_MASK, FU_A_TYPE);
            assert_eq!(payload[0] & NAL_REF_MASK, 0x60);
            assert_eq!(payload[1] & NAL_TYPE_MASK, 5);
            starts += usize::from(payload[1] & FU_START != 0);
            ends += usize::from(payload[1] & FU_END != 0);
        }
        assert_eq!((starts, ends), (1, 1));
    }

    #[test]
    fn no_packet_exceeds_the_mtu() {
        let unit = access_unit(&[vec![0x41; 50_000]]);
        for mtu in [MINIMUM_MTU, 200, 576, MAX_UDP_PAYLOAD] {
            let mut packetizer = Packetizer::with_sequence(
                Ssrc(1),
                RtpClock::new(H264_CLOCK_RATE, 0),
                H264_PAYLOAD_TYPE,
                mtu,
                SequenceNumber(0),
            );
            let packets = collect(&mut packetizer, &unit);
            assert!(packets.iter().all(|p| p.len() <= mtu), "mtu {mtu}");
        }
    }

    #[test]
    fn sequence_numbers_advance_by_one_and_wrap() {
        let unit = access_unit(&[vec![0x41; 6000]]);
        let mut packetizer = Packetizer::with_sequence(
            Ssrc(1),
            RtpClock::new(H264_CLOCK_RATE, 0),
            H264_PAYLOAD_TYPE,
            MAX_UDP_PAYLOAD,
            SequenceNumber(65534),
        );
        let packets = collect(&mut packetizer, &unit);
        let sequences: Vec<u16> = packets
            .iter()
            .map(|p| parse_packet(p).expect("valid").header.sequence.0)
            .collect();
        assert_eq!(&sequences[..3], &[65534, 65535, 0]);
        assert_eq!(packetizer.next_sequence().0, sequences.len() as u16 - 2);
    }

    #[test]
    fn degenerate_inputs_are_rejected_not_guessed() {
        let mut ok = packetizer(MAX_UDP_PAYLOAD);
        let mut empty = access_unit(&[vec![0x41; 8]]);
        empty.data.clear();
        assert_eq!(
            ok.packetize(&empty, |_| {}),
            Err(PacketizeError::EmptyAccessUnit)
        );

        let mut truncated = access_unit(&[vec![0x41; 8]]);
        truncated.data.truncate(9);
        assert_eq!(
            ok.packetize(&truncated, |_| {}),
            Err(PacketizeError::MalformedAvcc)
        );

        let mut tiny = packetizer(MINIMUM_MTU - 1);
        let unit = access_unit(&[vec![0x41; 8]]);
        assert_eq!(
            tiny.packetize(&unit, |_| {}),
            Err(PacketizeError::MtuTooSmall {
                mtu: MINIMUM_MTU - 1,
                minimum: MINIMUM_MTU,
            })
        );
    }
}
