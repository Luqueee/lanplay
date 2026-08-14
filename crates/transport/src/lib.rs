//! Media transport: RTP over UDP, and the two payload formats that go in it.
//!
//! Scope is deliberately narrow. RFC 3550 headers, the RFC 8285 extension that
//! carries a [`lanplay_protocol::FrameId`], the two RFC 6184 packetisation
//! modes a low-latency screen stream actually needs — a whole NAL unit in one
//! packet, or one NAL unit split across FU-A fragments — and RFC 7587 for
//! audio. No STAP, no MTAP, no interleaving, no RTCP, no retransmission.
//!
//! The two rules that shape the video path:
//!
//! * an access unit is a set of NAL units sharing one timestamp, and only the
//!   last packet of the last NAL carries the marker bit. A picture encoded as
//!   ten slices is one frame, not ten;
//! * nothing waits. A frame that cannot be reassembled is dropped and counted,
//!   because holding the pipeline for it costs every frame behind it.
//!
//! Audio is the simpler shape and a different one: one Opus frame is one
//! datagram, so [`opus`] has no fragmentation, no reassembly and no header
//! extension, and its timestamp counts samples rather than reading a clock.

pub mod control;
pub mod h264;
pub mod opus;
pub mod reassembly;
pub mod rtp;
pub mod stats;

pub use rtp::{
    FIXED_HEADER_LEN, FRAME_ID_EXTENSION_ID, H264_CLOCK_RATE, H264_PAYLOAD_TYPE, HEADER_OVERHEAD,
    MAX_UDP_PAYLOAD, RtpClock, RtpError, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc,
    parse_packet, random_u32, random_u64, write_packet,
};

pub use opus::{
    FRAME_SAMPLE_COUNTS, MAX_OPUS_PAYLOAD, OPUS_CLOCK_RATE, OPUS_PAYLOAD_TYPE, OpusPacket,
    OpusPacketizeError, OpusPacketizer, OpusParseError, parse_opus_packet,
};

pub use h264::{MINIMUM_MTU, NAL_LENGTH_SIZE, PacketizeError, PacketizedAu, Packetizer};
pub use reassembly::{Depacketizer, DepacketizerConfig, MAX_REORDER_WINDOW, ReorderWait};
pub use stats::{RxStats, TxStats};

pub use control::{
    CONTROL_MAGIC, CONTROL_VERSION, ControlClient, ControlError, ControlFrame, ControlMessage,
    ControlServer, ControlSession, FRAME_HEADER_LEN, MAX_CONTROL_PAYLOAD, MAX_SESSIONS,
    PROTOCOL_VERSION, SessionToken, UdpBinding,
};
