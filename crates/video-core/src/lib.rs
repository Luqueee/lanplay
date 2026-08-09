//! Media plumbing between whatever produces coded frames and whatever decodes
//! them.
//!
//! Three consumers on day one: the fixture source, the VideoToolbox decoder,
//! and the harness that joins them. The RTP depacketiser will be the fourth,
//! and the whole point of these types is that swapping `FixtureSource` for
//! `RtpSource` must not touch the decoder.
//!
//! Nothing here is platform specific.

mod access_unit;
mod annexb;
mod fixture;

pub use access_unit::{
    AccessUnitSource, EncodedAccessUnit, ParameterSets, PixelFormat, VideoDecoder, VideoTimestamp,
};
pub use annexb::{AnnexBError, NalUnitType, RawAccessUnit, parse_stream, split_annex_b, to_avcc};
pub use fixture::{
    FixtureError, FixturePattern, FixtureReport, FixtureSource, FixtureSpec, ensure_fixture,
    verify_no_frame_reordering,
};
