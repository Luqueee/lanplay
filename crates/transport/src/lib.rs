//! Media transport: RTP over UDP, and the H.264 payload format that goes in
//! it.
//!
//! Scope is deliberately narrow. RFC 3550 headers, the RFC 8285 extension that
//! carries a [`lanplay_protocol::FrameId`], and the two RFC 6184 packetisation
//! modes a low-latency screen stream actually needs: a whole NAL unit in one
//! packet, or one NAL unit split across FU-A fragments. No STAP, no MTAP, no
//! interleaving, no RTCP, no retransmission.
//!
//! The two rules that shape everything here:
//!
//! * an access unit is a set of NAL units sharing one timestamp, and only the
//!   last packet of the last NAL carries the marker bit. A picture encoded as
//!   ten slices is one frame, not ten;
//! * nothing waits. A frame that cannot be reassembled is dropped and counted,
//!   because holding the pipeline for it costs every frame behind it.

pub mod control;
pub mod h264;
pub mod reassembly;
pub mod rtp;
pub mod stats;

pub use rtp::{
    FRAME_ID_EXTENSION_ID, H264_CLOCK_RATE, H264_PAYLOAD_TYPE, HEADER_OVERHEAD, MAX_UDP_PAYLOAD,
    RtpClock, RtpError, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc, parse_packet,
    random_u32, random_u64, write_packet,
};

pub use h264::{MINIMUM_MTU, NAL_LENGTH_SIZE, PacketizeError, PacketizedAu, Packetizer};
pub use reassembly::{Depacketizer, DepacketizerConfig, MAX_REORDER_WINDOW};
pub use stats::{RxStats, TxStats};

pub use control::{
    CONTROL_MAGIC, CONTROL_VERSION, ControlClient, ControlError, ControlFrame, ControlMessage,
    ControlServer, ControlSession, FRAME_HEADER_LEN, MAX_CONTROL_PAYLOAD, MAX_SESSIONS,
    PROTOCOL_VERSION, SessionToken, UdpBinding,
};
