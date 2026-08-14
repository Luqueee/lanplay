//! Turning a window of captured frames into a statement about each channel.
//!
//! The tone source puts a different frequency in each channel on purpose, so
//! this is where channel order and channel independence are proved. Two
//! channels carrying the same frequency would be consistent with a capture that
//! read one channel twice, with a mix that folded to mono somewhere, and with a
//! detector measuring its own scratch buffer; a left that reads 997 and a right
//! that reads 1997 is consistent with none of those.
//!
//! Only the analysis window is deinterleaved, not the run. A tenth of a second
//! of a steady tone says everything a minute of it would, and copying a minute
//! into two float buffers to find that out would be a lot of memory spent on
//! the same answer.

use crate::format::MixFormat;
use crate::goertzel::{Tone, dominant};

/// Quietest peak that counts as content. An endpoint playing nothing produces
/// exact zeroes, so this only has to sit below anything audible and above the
/// arithmetic noise of the filter itself.
const FLOOR_DBFS: f64 = -80.0;

/// Bottom of the band searched. Below this is where a DC offset and mains hum
/// live, and neither is the thing being looked for.
const BAND_LOW_HZ: f64 = 20.0;

/// Top of the band searched, clamped to the Nyquist frequency of whatever the
/// mix format turns out to be.
const BAND_HIGH_HZ: f64 = 8_000.0;

/// What each channel was carrying.
#[derive(Clone, Debug, PartialEq)]
pub struct ToneReport {
    pub left: Option<Tone>,
    pub right: Option<Tone>,
    /// Bin spacing of the analysis window, which is the resolution every
    /// frequency below is quoted at.
    pub resolution_hz: f64,
    pub analysed_frames: usize,
}

impl ToneReport {
    /// Nothing to analyse: either no window was collected or the format had no
    /// channels to read.
    pub fn empty() -> Self {
        ToneReport {
            left: None,
            right: None,
            resolution_hz: 0.0,
            analysed_frames: 0,
        }
    }

    /// Whether the two channels carry different content.
    ///
    /// Two frequencies closer together than a couple of bins are not
    /// distinguishable by this window, so they are reported as the same rather
    /// than as a difference the measurement cannot support.
    pub fn distinct(&self) -> bool {
        match (self.left, self.right) {
            (Some(left), Some(right)) => {
                (left.frequency - right.frequency).abs() > 2.0 * self.resolution_hz
            }
            _ => false,
        }
    }
}

/// The frequency of a tone that may not be there.
///
/// A channel with no content reports zero hertz rather than being left out of
/// the report, because the line it appears on is one the gate parses and a
/// missing field would read as a parse failure rather than as silence. The
/// silence itself is stated on its own line.
pub fn hertz(tone: Option<Tone>) -> f64 {
    tone.map_or(0.0, |found| found.frequency)
}

/// Measures the dominant frequency of the first two channels of a window of
/// interleaved frames.
pub fn analyse(format: &MixFormat, frames: &[u8]) -> ToneReport {
    let frame_bytes = format.frame_bytes();
    if frame_bytes == 0 || format.channels == 0 {
        return ToneReport::empty();
    }
    let count = frames.len() / frame_bytes;
    if count == 0 {
        return ToneReport::empty();
    }

    let high = BAND_HIGH_HZ.min(f64::from(format.sample_rate) / 2.0 * 0.9);
    let mut channel = Vec::with_capacity(count);
    let mut measure = |index: u16| -> Option<Tone> {
        if index >= format.channels {
            return None;
        }
        channel.clear();
        for frame in frames.chunks_exact(frame_bytes) {
            channel.push(format.decode(frame, index)?);
        }
        dominant(&channel, format.sample_rate, BAND_LOW_HZ, high, FLOOR_DBFS)
    };

    let left = measure(0);
    let right = measure(1);
    ToneReport {
        left,
        right,
        resolution_hz: f64::from(format.sample_rate) / count as f64,
        analysed_frames: count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{RawExtensible, RawWaveFormat, SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_EXTENSIBLE};
    use core::f64::consts::PI;

    fn stereo_float(rate: u32) -> MixFormat {
        MixFormat::from_raw(&RawWaveFormat {
            format_tag: WAVE_FORMAT_EXTENSIBLE,
            channels: 2,
            samples_per_sec: rate,
            avg_bytes_per_sec: rate * 8,
            block_align: 8,
            bits_per_sample: 32,
            extensible: Some(RawExtensible {
                valid_bits: 32,
                channel_mask: 3,
                subformat: SUBTYPE_IEEE_FLOAT,
            }),
        })
        .expect("a describable format")
    }

    fn interleaved(rate: u32, left_hz: f64, right_hz: f64, frames: usize) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(frames * 8);
        for n in 0..frames {
            let phase = 2.0 * PI * n as f64 / f64::from(rate);
            let left = (0.5 * (phase * left_hz).sin()) as f32;
            let right = (0.5 * (phase * right_hz).sin()) as f32;
            bytes.extend_from_slice(&left.to_le_bytes());
            bytes.extend_from_slice(&right.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn the_contract_tones_are_read_off_the_right_channels() {
        let format = stereo_float(48_000);
        let report = analyse(&format, &interleaved(48_000, 997.0, 1997.0, 4_800));
        let left = report.left.expect("the left channel carries a tone");
        let right = report.right.expect("the right channel carries a tone");
        assert!(
            (left.frequency - 997.0).abs() < 1.0,
            "left {}",
            left.frequency
        );
        assert!(
            (right.frequency - 1997.0).abs() < 1.0,
            "right {}",
            right.frequency
        );
        assert!(report.distinct());
        assert_eq!(report.analysed_frames, 4_800);
        assert!((report.resolution_hz - 10.0).abs() < 1e-9);
    }

    /// The failure this exists to catch: a capture that read one channel into
    /// both would look exactly like a working one to a frame counter.
    #[test]
    fn two_channels_carrying_the_same_tone_are_not_distinct() {
        let format = stereo_float(48_000);
        let report = analyse(&format, &interleaved(48_000, 997.0, 997.0, 4_800));
        assert!(report.left.is_some());
        assert!(report.right.is_some());
        assert!(!report.distinct());
    }

    #[test]
    fn silence_reports_no_tone_on_either_channel() {
        let format = stereo_float(48_000);
        let report = analyse(&format, &vec![0u8; 4_800 * 8]);
        assert_eq!(report.left, None);
        assert_eq!(report.right, None);
        assert!(!report.distinct());
        assert_eq!(report.analysed_frames, 4_800);
    }

    #[test]
    fn one_silent_channel_is_not_a_distinct_pair() {
        let format = stereo_float(48_000);
        let report = analyse(&format, &interleaved(48_000, 997.0, 0.0, 4_800));
        assert!(report.left.is_some());
        assert_eq!(report.right, None);
        assert!(!report.distinct());
    }

    #[test]
    fn an_empty_window_analyses_to_nothing() {
        let format = stereo_float(48_000);
        let report = analyse(&format, &[]);
        assert_eq!(report, ToneReport::empty());
    }

    #[test]
    fn a_rate_that_is_not_the_contracts_is_still_analysed() {
        let format = stereo_float(44_100);
        let report = analyse(&format, &interleaved(44_100, 997.0, 1997.0, 4_410));
        let left = report.left.expect("the left channel carries a tone");
        assert!(
            (left.frequency - 997.0).abs() < 1.5,
            "left {}",
            left.frequency
        );
    }
}
