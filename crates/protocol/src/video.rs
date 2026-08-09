use core::fmt;

use serde::{Deserialize, Serialize};

/// Video codecs the pipeline can negotiate.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    Hevc,
    Av1,
}

impl VideoCodec {
    /// The `CMVideoCodecType` / QuickTime four-character code, which is also
    /// how VideoToolbox identifies the codec.
    pub const fn four_cc(self) -> u32 {
        match self {
            VideoCodec::H264 => u32::from_be_bytes(*b"avc1"),
            VideoCodec::Hevc => u32::from_be_bytes(*b"hvc1"),
            VideoCodec::Av1 => u32::from_be_bytes(*b"av01"),
        }
    }
}

impl fmt::Display for VideoCodec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::Hevc => "HEVC",
            VideoCodec::Av1 => "AV1",
        })
    }
}

/// A concrete video timing: pixel dimensions plus refresh rate.
///
/// Refresh is stored in millihertz so that rates such as 119.88 Hz survive the
/// wire without floats.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    pub refresh_mhz: u32,
}

impl VideoMode {
    pub const fn new(width: u32, height: u32, refresh_mhz: u32) -> Self {
        VideoMode {
            width,
            height,
            refresh_mhz,
        }
    }

    /// Builds a mode from a refresh rate in hertz, rounding to the nearest millihertz.
    pub fn from_hz(width: u32, height: u32, refresh_hz: f64) -> Self {
        VideoMode::new(width, height, (refresh_hz * 1000.0).round() as u32)
    }

    pub fn refresh_hz(self) -> f64 {
        f64::from(self.refresh_mhz) / 1000.0
    }

    /// Pixels per second the whole pipeline must sustain for this mode.
    ///
    /// 1080p120 is ~249 Mpx/s; 2560x1600@120 is ~492 Mpx/s. The ratio between
    /// those two numbers is the honest measure of how much harder a mode is.
    pub fn pixel_rate(self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * u64::from(self.refresh_mhz) / 1000
    }
}

impl fmt::Display for VideoMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{}", self.width, self.height, self.refresh_hz())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_rate_matches_hand_computation() {
        assert_eq!(
            VideoMode::new(1920, 1080, 120_000).pixel_rate(),
            248_832_000
        );
        assert_eq!(
            VideoMode::new(2560, 1600, 120_000).pixel_rate(),
            491_520_000
        );
    }

    #[test]
    fn fractional_refresh_survives_the_round_trip() {
        let mode = VideoMode::from_hz(1920, 1080, 119.88);
        assert_eq!(mode.refresh_mhz, 119_880);
        assert_eq!(mode.to_string(), "1920x1080@119.88");
    }
}
