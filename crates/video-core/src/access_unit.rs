use lanplay_protocol::FrameId;

/// A point on the media timeline, in `timescale` units per second.
///
/// Mirrors `CMTime` so it can be handed to CoreMedia without a lossy float
/// round trip. It is *not* a wall-clock instant: it identifies a frame's
/// position in the stream, nothing more. Presentation scheduling deliberately
/// does not use it (a remote-desktop client shows the newest frame it has, it
/// does not play back a timeline).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct VideoTimestamp {
    pub value: i64,
    pub timescale: u32,
}

impl VideoTimestamp {
    pub const fn new(value: i64, timescale: u32) -> Self {
        VideoTimestamp { value, timescale }
    }

    /// Timestamp of frame `index` in a stream of `fps_numerator / fps_denominator`
    /// frames per second, expressed exactly: 120 fps becomes 1/120 s ticks,
    /// 119.88 fps becomes 1001/120000 s ticks.
    pub const fn from_frame_index(index: u64, fps_numerator: u32, fps_denominator: u32) -> Self {
        VideoTimestamp {
            value: (index * fps_denominator as u64) as i64,
            timescale: fps_numerator,
        }
    }

    pub fn as_secs_f64(self) -> f64 {
        if self.timescale == 0 {
            return 0.0;
        }
        self.value as f64 / f64::from(self.timescale)
    }
}

/// SPS and PPS NAL units, without start codes.
///
/// These build the `CMVideoFormatDescription`; `nal_length_size` must match
/// the prefix width used by [`EncodedAccessUnit::data`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ParameterSets {
    pub sps: Vec<Vec<u8>>,
    pub pps: Vec<Vec<u8>>,
    pub nal_length_size: u8,
}

/// One complete coded frame: everything the decoder needs, and nothing it has
/// to wait for.
///
/// `data` is **AVCC**, i.e. each NAL unit prefixed by a big-endian length of
/// [`ParameterSets::nal_length_size`] bytes. VideoToolbox rejects Annex-B
/// start codes in a sample buffer, so the conversion happens once, in
/// [`crate::to_avcc`], at the boundary where bytes enter the pipeline.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EncodedAccessUnit {
    pub id: FrameId,
    pub pts: VideoTimestamp,
    pub is_idr: bool,
    pub data: Vec<u8>,
}

impl EncodedAccessUnit {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

/// Pixel layouts the decoder is allowed to hand to the renderer.
///
/// Deliberately short: anything that would need a CPU conversion pass is not
/// on the list.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PixelFormat {
    /// Bi-planar Y + interleaved CbCr, 8 bit, video range (16-235).
    Nv12VideoRange,
    /// Bi-planar Y + interleaved CbCr, 8 bit, full range (0-255).
    Nv12FullRange,
}

impl PixelFormat {
    /// The `CVPixelBufferRef` four-character code.
    pub const fn four_cc(self) -> u32 {
        match self {
            // '420v'
            PixelFormat::Nv12VideoRange => u32::from_be_bytes(*b"420v"),
            // '420f'
            PixelFormat::Nv12FullRange => u32::from_be_bytes(*b"420f"),
        }
    }

    pub const fn plane_count(self) -> usize {
        2
    }
}

/// Where access units come from: a fixture file today, an RTP depacketiser
/// later.
pub trait AccessUnitSource {
    /// Parameter sets for the stream. Available before the first access unit,
    /// because the decoder session cannot be created without them.
    fn parameter_sets(&self) -> &ParameterSets;

    /// The next access unit, or `None` at end of stream.
    fn next_access_unit(&mut self) -> Option<EncodedAccessUnit>;
}

/// A decoder that accepts access units and reports finished frames out of
/// band.
///
/// There is no `poll` and no returned frame: hardware decoders are
/// asynchronous, and forcing a synchronous shape on them would add exactly
/// the latency this project exists to remove. Implementations take an output
/// callback at construction.
pub trait VideoDecoder {
    type Error;

    /// Hands one access unit to the decoder. Returns once the frame is
    /// accepted, not once it is decoded.
    fn submit(&mut self, access_unit: &EncodedAccessUnit) -> Result<(), Self::Error>;

    /// Waits for every submitted frame to come out.
    fn flush(&mut self) -> Result<(), Self::Error>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_index_maps_to_an_exact_timestamp() {
        let at_120 = VideoTimestamp::from_frame_index(120, 120, 1);
        assert_eq!(at_120.as_secs_f64(), 1.0);

        // 119.88 fps: 1001 ticks of a 120000 Hz timescale per frame.
        let ntsc = VideoTimestamp::from_frame_index(1, 120_000, 1001);
        assert_eq!(ntsc, VideoTimestamp::new(1001, 120_000));
    }

    #[test]
    fn pixel_formats_use_the_corevideo_codes() {
        assert_eq!(PixelFormat::Nv12VideoRange.four_cc(), 0x3432_3076);
        assert_eq!(PixelFormat::Nv12FullRange.four_cc(), 0x3432_3066);
    }
}
