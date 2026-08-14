//! How the codec is set up, and why every choice was made the way it was.
//!
//! The configuration is a plain value rather than a builder because there is
//! nothing optional about it: a stream whose two ends disagree about rate,
//! channel count or frame duration does not degrade, it decodes to noise or
//! refuses outright. Making every field mandatory means the host and the client
//! can be handed the same literal and neither can inherit a default the other
//! did not.
//!
//! Frame duration is an enumeration rather than a number of milliseconds
//! because Opus permits exactly six of them and nothing else. A `u32`
//! millisecond field would let a caller construct a 7 ms encoder that fails at
//! the first `opus_encode` with a bad-argument code, a very long way from the
//! line that chose the seven.

/// Frame durations this codec will run at, named in whole milliseconds.
///
/// Opus also permits 2.5 ms, which is deliberately absent. It cannot be named
/// by the whole-millisecond flag the harness passes, and the render endpoint
/// measured in the previous phase has a 3.000 ms minimum device period, so a
/// 2.5 ms frame could never line up with a packet the audio engine actually
/// delivers.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FrameDuration {
    Ms5,
    Ms10,
    Ms20,
    Ms40,
    Ms60,
}

impl FrameDuration {
    pub const ALL: [FrameDuration; 5] = [
        FrameDuration::Ms5,
        FrameDuration::Ms10,
        FrameDuration::Ms20,
        FrameDuration::Ms40,
        FrameDuration::Ms60,
    ];

    pub fn from_millis(millis: u32) -> Option<FrameDuration> {
        FrameDuration::ALL
            .into_iter()
            .find(|duration| duration.millis() == millis)
    }

    pub fn millis(self) -> u32 {
        match self {
            FrameDuration::Ms5 => 5,
            FrameDuration::Ms10 => 10,
            FrameDuration::Ms20 => 20,
            FrameDuration::Ms40 => 40,
            FrameDuration::Ms60 => 60,
        }
    }

    pub fn seconds(self) -> f64 {
        f64::from(self.millis()) / 1_000.0
    }

    /// Samples of one channel in one frame at the given rate.
    ///
    /// Exact for every rate Opus accepts, because all five of them divide by a
    /// thousand into a whole number of samples per millisecond.
    pub fn samples_per_channel(self, sample_rate: u32) -> usize {
        sample_rate as usize * self.millis() as usize / 1_000
    }
}

/// Sample rates Opus accepts, from `opus_encoder_create`'s documentation.
pub const SAMPLE_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];

/// Bytes reserved for one encoded packet.
///
/// This is libopus's own recommendation rather than an estimate: `opus.h` says
/// of the encode call that "max_packet is the maximum number of bytes that can
/// be written in the packet (4000 bytes is recommended). Do not use max_packet
/// to control VBR target bitrate, instead use the OPUS_SET_BITRATE CTL."
///
/// The tight bound for this configuration is smaller. Every packet produced
/// here holds exactly one frame, so the same header documents the size that
/// cannot be exceeded where it describes the repacketizer's output: "at least
/// 1276 for a single frame, or for multiple frames, 1277*(end-begin)". The
/// larger figure is used anyway because it costs four kilobytes once, at
/// construction, and because a buffer sized to the tight bound would turn any
/// future decision to emit multi-frame packets into a truncation rather than
/// into a compile error.
pub const MAX_PACKET_BYTES: usize = 4_000;

/// Everything the encoder and the decoder must agree on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CodecConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub frame: FrameDuration,
    /// The target handed to `OPUS_SET_BITRATE`, in bits per second. A target,
    /// not a promise: what the encoder actually produces is measured and
    /// reported separately, because the two turned out not to be the same
    /// number.
    pub bitrate_bps: i32,
}

impl CodecConfig {
    /// What the plan asks for: the mix format the lab's render endpoint
    /// already produces, so nothing between capture and encode resamples or
    /// folds a channel.
    pub const CONTRACT_SAMPLE_RATE: u32 = 48_000;
    pub const CONTRACT_CHANNELS: u16 = 2;
    pub const DEFAULT_BITRATE_BPS: i32 = 128_000;

    pub fn contract(frame: FrameDuration, bitrate_bps: i32) -> CodecConfig {
        CodecConfig {
            sample_rate: CodecConfig::CONTRACT_SAMPLE_RATE,
            channels: CodecConfig::CONTRACT_CHANNELS,
            frame,
            bitrate_bps,
        }
    }

    /// Samples of one channel in one frame.
    pub fn frame_samples(&self) -> usize {
        self.frame.samples_per_channel(self.sample_rate)
    }

    /// Interleaved samples in one frame, which is the exact length of every
    /// buffer handed to the encoder and returned by the decoder.
    pub fn frame_interleaved(&self) -> usize {
        self.frame_samples() * self.channels as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_durations_round_trip_through_milliseconds() {
        for duration in FrameDuration::ALL {
            assert_eq!(
                FrameDuration::from_millis(duration.millis()),
                Some(duration)
            );
        }
    }

    #[test]
    fn a_duration_opus_does_not_permit_has_no_variant() {
        for millis in [0, 1, 2, 3, 4, 6, 7, 15, 30, 50, 120] {
            assert_eq!(FrameDuration::from_millis(millis), None, "{millis} ms");
        }
    }

    #[test]
    fn frame_lengths_are_the_documented_sample_counts_at_48_khz() {
        let expected = [240, 480, 960, 1920, 2880];
        for (duration, samples) in FrameDuration::ALL.into_iter().zip(expected) {
            assert_eq!(duration.samples_per_channel(48_000), samples);
        }
    }

    #[test]
    fn every_rate_opus_accepts_gives_a_whole_number_of_samples() {
        for rate in SAMPLE_RATES {
            for duration in FrameDuration::ALL {
                let samples = duration.samples_per_channel(rate);
                assert_eq!(
                    samples as f64,
                    f64::from(rate) * duration.seconds(),
                    "{rate} Hz at {} ms",
                    duration.millis()
                );
            }
        }
    }

    #[test]
    fn a_stereo_frame_is_twice_its_per_channel_length() {
        let config = CodecConfig::contract(FrameDuration::Ms5, 128_000);
        assert_eq!(config.frame_samples(), 240);
        assert_eq!(config.frame_interleaved(), 480);
    }
}
