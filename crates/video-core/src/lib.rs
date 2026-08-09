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

pub use access_unit::{
    AccessUnitSource, EncodedAccessUnit, ParameterSets, PixelFormat, VideoDecoder, VideoTimestamp,
};
