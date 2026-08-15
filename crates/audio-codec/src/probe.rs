//! One measured run of the codec, and the exact lines the harness reads.
//!
//! The run pushes the batch's contract tone through the encoder and straight
//! back through the decoder, timing the two halves separately. Separately is
//! the whole point: the question this phase asks is whether the encoder is
//! irrelevant against a 5 ms frame budget, and a single number covering both
//! directions cannot answer it, because a cheap encoder behind an expensive
//! decoder and the reverse produce the same sum.
//!
//! What is deliberately absent, because a later phase owns each of them: no
//! RTP, no socket, no jitter buffer, no resampler, no capture. The tone is
//! generated in memory at the rate the endpoint already mixes at, so nothing in
//! this file converts anything, and every microsecond it reports is the codec's.
//!
//! The report is a plain value with a `Display` rather than a set of `println!`
//! calls, so the wording the harness parses can be checked by a test instead of
//! by running the codec and reading the terminal.
//!
//! And the decoded audio is analysed rather than assumed. A byte count and a
//! frame count agree just as happily when the decoder returns silence, and this
//! project has read that agreement as success three times. The frequencies
//! printed at the bottom of the keyed block come out of the samples that came
//! out of libopus.

use core::fmt;
use std::time::Instant;

use lanplay_audio_capture::analysis::hertz;
use lanplay_audio_capture::format::{
    MixFormat, RawExtensible, RawWaveFormat, SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_EXTENSIBLE,
};
use lanplay_audio_capture::{Percentiles, Samples, ToneReport, analyse};
use lanplay_tone_source::tone::{CONTRACT, Tone};

use crate::config::{CodecConfig, FrameDuration};
use crate::decoder::OpusDecoder;
use crate::encoder::{EncoderSettings, OpusEncoder};
use crate::error::CodecError;
use crate::ffi;

/// Frames skipped before the analysis window opens.
///
/// A hundred milliseconds, which is comfortably past the codec's own lookahead
/// and the first few frames where the encoder is still settling. Measuring the
/// ramp would drag the reported frequency towards whatever the transient
/// contains and make a correct codec look slightly wrong.
pub const ANALYSIS_SKIP_FRAMES: usize = 4_800;

/// Frames the analysis window holds.
///
/// Half a second, which puts the bin spacing at 2 Hz — four hundred times finer
/// than the gap between the two contract tones, and fine enough that the
/// parabolic refinement in the detector lands well inside a hertz. Analysing
/// the whole run instead would cost quadratic time for no more certainty: the
/// detector scans the band at the window's own bin spacing, so doubling the
/// window doubles both the number of bins and the cost of each.
pub const ANALYSIS_FRAMES: usize = 24_000;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Options {
    pub frame: FrameDuration,
    pub seconds: f64,
    pub bitrate_kbps: u32,
}

/// What one run amounted to.
#[derive(Clone, Debug)]
pub struct Measurement {
    pub config: CodecConfig,
    pub settings: EncoderSettings,
    pub libopus: &'static str,
    /// Frames of one channel handed to the encoder.
    pub frames_submitted: u64,
    /// Frames of one channel the decoder gave back. Reported next to the line
    /// above rather than asserted against it, because a probe that panicked on
    /// a mismatch would print nothing at all about the run that found it.
    pub frames_returned: u64,
    pub packets: u64,
    pub encode_us: Option<Percentiles>,
    pub decode_us: Option<Percentiles>,
    pub packet_bytes: Option<Percentiles>,
    pub total_packet_bytes: u64,
    pub tone: ToneReport,
}

impl Measurement {
    /// The rate the packets actually came out at, from the bytes that were
    /// produced rather than from the target that was requested.
    pub fn effective_kbps(&self) -> f64 {
        let seconds = self.packets as f64 * self.config.frame.seconds();
        if seconds == 0.0 {
            return 0.0;
        }
        self.total_packet_bytes as f64 * 8.0 / seconds / 1_000.0
    }
}

pub fn run(options: Options) -> Result<Measurement, CodecError> {
    let config = CodecConfig::contract(
        options.frame,
        i32::try_from(options.bitrate_kbps).unwrap_or(i32::MAX / 1_000) * 1_000,
    );
    let frame_samples = config.frame_samples();
    let total_frames = (options.seconds * f64::from(config.sample_rate)).round() as u64;
    let packets = total_frames / frame_samples as u64;
    if packets == 0 {
        return Err(CodecError::NothingToEncode {
            frames: total_frames,
            frame_samples,
        });
    }

    let mut encoder = OpusEncoder::new(config)?;
    let mut decoder = OpusDecoder::new(config)?;

    // The generator's contract is the codec's: 48000 Hz stereo. Nothing here
    // reconciles the two, because there is nothing to reconcile and code that
    // could reconcile them would be a resampler this phase must not contain.
    let mut tone = Tone::new(CONTRACT);

    let mut pcm = vec![0f32; config.frame_interleaved()];
    let mut encode_us = Samples::with_capacity(packets as usize);
    let mut decode_us = Samples::with_capacity(packets as usize);
    let mut packet_bytes = Samples::with_capacity(packets as usize);

    // The analysis window, kept as bytes because that is what the existing
    // detector reads. Sized and allocated once so that filling it costs a copy
    // and never a reallocation between two timed calls.
    let window_format = decoded_format(&config);
    let mut window = Vec::with_capacity(ANALYSIS_FRAMES * window_format.frame_bytes());

    let mut frames_submitted = 0u64;
    let mut frames_returned = 0u64;
    let mut total_packet_bytes = 0u64;
    let mut skipped = 0usize;

    for _ in 0..packets {
        tone.fill_stereo(&mut pcm);

        let started = Instant::now();
        let packet = encoder.encode(&pcm)?;
        let encoded = started.elapsed();
        let bytes = packet.len();

        let started = Instant::now();
        let decoded = decoder.decode(packet)?;
        let decoding = started.elapsed();

        frames_submitted += frame_samples as u64;
        frames_returned += (decoded.len() / config.channels as usize) as u64;
        total_packet_bytes += bytes as u64;
        encode_us.record(encoded.as_micros() as u64);
        decode_us.record(decoding.as_micros() as u64);
        packet_bytes.record(bytes as u64);

        if skipped < ANALYSIS_SKIP_FRAMES {
            skipped += frame_samples;
        } else if window.len() < window.capacity() {
            for sample in decoded {
                window.extend_from_slice(&sample.to_le_bytes());
            }
        }
    }

    Ok(Measurement {
        config,
        settings: *encoder.settings(),
        libopus: ffi::version(),
        frames_submitted,
        frames_returned,
        packets,
        encode_us: encode_us.percentiles(),
        decode_us: decode_us.percentiles(),
        packet_bytes: packet_bytes.percentiles(),
        total_packet_bytes,
        tone: analyse(&window_format, &window),
    })
}

/// The decoder's output described the way the mix-format decoder expects it.
///
/// Interleaved 32-bit float at the configured rate, which is the same shape the
/// lab's render endpoint mixes at down to the channel mask. Going through
/// [`analyse`] rather than calling the filter directly is what keeps the band
/// limits, the silence floor and the peak refinement the same judgement the
/// capture side already makes; the byte buffer is the price of not making that
/// judgement twice.
///
/// Shared with the RTP probe and with the Mac's receiver, both of which analyse
/// decoded audio that arrived over a socket rather than straight out of the
/// encoder: three descriptions of the same decoder output could drift apart,
/// and then three probes would be quoting frequencies measured under different
/// rules.
pub fn decoded_format(config: &CodecConfig) -> MixFormat {
    let bytes_per_sample = 4u16;
    let block_align = config.channels * bytes_per_sample;
    MixFormat::from_raw(&RawWaveFormat {
        format_tag: WAVE_FORMAT_EXTENSIBLE,
        channels: config.channels,
        samples_per_sec: config.sample_rate,
        avg_bytes_per_sec: config.sample_rate * u32::from(block_align),
        block_align,
        bits_per_sample: bytes_per_sample * 8,
        extensible: Some(RawExtensible {
            valid_bits: bytes_per_sample * 8,
            // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT, which is what the
            // endpoint measured in the previous phase reports.
            channel_mask: 0x3,
            subformat: SUBTYPE_IEEE_FLOAT,
        }),
    })
    .expect("a 32-bit float interleaved format is one the decoder accepts")
}

const NOTHING: Percentiles = Percentiles {
    count: 0,
    min: 0,
    p50: 0,
    p95: 0,
    p99: 0,
    max: 0,
};

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encode = self.encode_us.unwrap_or(NOTHING);
        let decode = self.decode_us.unwrap_or(NOTHING);
        let bytes = self.packet_bytes.unwrap_or(NOTHING);

        writeln!(f, "frame duration {}", self.config.frame.millis())?;
        writeln!(f, "frames submitted {}", self.frames_submitted)?;
        writeln!(f, "frames returned {}", self.frames_returned)?;
        writeln!(f, "packets {}", self.packets)?;
        writeln!(
            f,
            "encode us p50 {} p95 {} p99 {} max {}",
            encode.p50, encode.p95, encode.p99, encode.max
        )?;
        writeln!(
            f,
            "decode us p50 {} p95 {} p99 {} max {}",
            decode.p50, decode.p95, decode.p99, decode.max
        )?;
        writeln!(
            f,
            "packet bytes p50 {} p95 {} p99 {} max {}",
            bytes.p50, bytes.p95, bytes.p99, bytes.max
        )?;
        writeln!(f, "effective kbps {:.1}", self.effective_kbps())?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(self.tone.left),
            hertz(self.tone.right)
        )?;
        writeln!(f, "tone channels distinct {}", yes_no(self.tone.distinct()))?;

        // Everything below is for a person reading the run rather than for the
        // harness parsing it, in the order somebody asking "why that bitrate"
        // would want it.
        writeln!(f, "requested kbps {}", self.config.bitrate_bps / 1_000)?;
        writeln!(f, "encoder reports bitrate {}", self.settings.bitrate_bps)?;
        writeln!(f, "application {}", self.settings.application)?;
        writeln!(
            f,
            "vbr {} constrained {}",
            yes_no(self.settings.vbr),
            yes_no(self.settings.vbr_constrained)
        )?;
        writeln!(
            f,
            "dtx {} inband fec {}",
            yes_no(self.settings.dtx),
            yes_no(self.settings.inband_fec)
        )?;
        writeln!(f, "complexity {}", self.settings.complexity)?;
        writeln!(f, "lookahead samples {}", self.settings.lookahead)?;
        writeln!(
            f,
            "frame samples per channel {}",
            self.config.frame_samples()
        )?;
        writeln!(f, "packet bytes min {} count {}", bytes.min, bytes.count)?;
        writeln!(
            f,
            "tone resolution {:.2} hz over {} frames",
            self.tone.resolution_hz, self.tone.analysed_frames
        )?;
        writeln!(f, "libopus {}", self.libopus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurement() -> Measurement {
        Measurement {
            config: CodecConfig::contract(FrameDuration::Ms5, 128_000),
            settings: EncoderSettings {
                application: "OPUS_APPLICATION_RESTRICTED_LOWDELAY",
                bitrate_bps: 128_000,
                vbr: true,
                vbr_constrained: true,
                dtx: false,
                inband_fec: false,
                complexity: 9,
                lookahead: 120,
            },
            libopus: "libopus 1.4",
            frames_submitted: 240_000,
            frames_returned: 240_000,
            packets: 1_000,
            encode_us: Some(Percentiles {
                count: 1_000,
                min: 18,
                p50: 21,
                p95: 26,
                p99: 31,
                max: 88,
            }),
            decode_us: Some(Percentiles {
                count: 1_000,
                min: 5,
                p50: 6,
                p95: 8,
                p99: 9,
                max: 40,
            }),
            packet_bytes: Some(Percentiles {
                count: 1_000,
                min: 120,
                p50: 126,
                p95: 128,
                p99: 129,
                max: 131,
            }),
            total_packet_bytes: 126_000,
            tone: ToneReport::empty(),
        }
    }

    fn keyed(text: &str) -> Vec<&str> {
        text.lines().take(10).collect()
    }

    #[test]
    fn the_keyed_lines_are_the_ones_the_harness_parses() {
        let printed = measurement().to_string();
        assert_eq!(
            keyed(&printed),
            vec![
                "frame duration 5",
                "frames submitted 240000",
                "frames returned 240000",
                "packets 1000",
                "encode us p50 21 p95 26 p99 31 max 88",
                "decode us p50 6 p95 8 p99 9 max 40",
                "packet bytes p50 126 p95 128 p99 129 max 131",
                "effective kbps 201.6",
                "tone left 0.0 right 0.0",
                "tone channels distinct no",
            ]
        );
    }

    #[test]
    fn the_effective_rate_comes_from_the_bytes_and_not_from_the_target() {
        let measured = measurement();
        // A thousand 5 ms packets is five seconds; 126000 bytes over five
        // seconds is 201.6 kbps whatever OPUS_SET_BITRATE was told.
        assert!((measured.effective_kbps() - 201.6).abs() < 0.05);
        assert_eq!(measured.config.bitrate_bps, 128_000);
    }

    #[test]
    fn a_run_shorter_than_one_frame_is_refused_rather_than_padded() {
        let error = run(Options {
            frame: FrameDuration::Ms10,
            seconds: 0.001,
            bitrate_kbps: 128,
        })
        .expect_err("48 frames is not a 480 frame frame");
        assert_eq!(
            error,
            CodecError::NothingToEncode {
                frames: 48,
                frame_samples: 480,
            }
        );
    }
}
