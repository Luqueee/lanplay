//! What the endpoint says its mix is, read once and never converted.
//!
//! The whole point of this phase is to find out what WASAPI actually hands
//! over, so nothing here rounds a rate, folds a channel or promotes an integer
//! sample to a float. A format that is not the one the rest of the project
//! expected is a finding to be printed, and a later phase decides whether a
//! conversion is worth writing at all.
//!
//! Decoding a `WAVEFORMATEX` is separated from reading one out of memory
//! because the interesting half is the decision -- which tag means what, when
//! the extensible subformat overrides the tag, whether the declared block
//! alignment agrees with the channels and the sample size -- and that half has
//! to be exercisable on a machine with no audio endpoint and no Win32 at all.
//! [`RawWaveFormat`] is therefore an ordinary struct that a test can fill in,
//! and the pointer-reading lives with the capture code.

use core::fmt;

/// `WAVE_FORMAT_PCM`, from mmreg.h.
pub const WAVE_FORMAT_PCM: u16 = 1;

/// `WAVE_FORMAT_IEEE_FLOAT`, from mmreg.h.
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;

/// `WAVE_FORMAT_EXTENSIBLE`, from mmreg.h. The tag the audio engine almost
/// always uses, because a mix with a channel mask cannot be described without
/// it.
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// `KSDATAFORMAT_SUBTYPE_PCM`.
pub const SUBTYPE_PCM: u128 = 0x00000001_0000_0010_8000_00aa00389b71;

/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
pub const SUBTYPE_IEEE_FLOAT: u128 = 0x00000003_0000_0010_8000_00aa00389b71;

/// How a sample is encoded in its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleKind {
    Float,
    Int,
}

impl fmt::Display for SampleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SampleKind::Float => f.write_str("float"),
            SampleKind::Int => f.write_str("int"),
        }
    }
}

/// The tail a `WAVEFORMATEXTENSIBLE` adds to a `WAVEFORMATEX`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawExtensible {
    pub valid_bits: u16,
    pub channel_mask: u32,
    pub subformat: u128,
}

/// A `WAVEFORMATEX`, and its extensible tail when it had one, as plain fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawWaveFormat {
    pub format_tag: u16,
    pub channels: u16,
    pub samples_per_sec: u32,
    pub avg_bytes_per_sec: u32,
    pub block_align: u16,
    pub bits_per_sample: u16,
    pub extensible: Option<RawExtensible>,
}

/// Why a mix format could not be believed.
///
/// Every one of these is a refusal rather than a repair. A format this code
/// cannot describe exactly is a format whose samples it cannot decode exactly,
/// and a probe that guessed would report a frequency it had invented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatError {
    UnknownTag(u16),
    UnknownSubformat(u128),
    NoChannels,
    NoRate,
    OddSampleSize(u16),
    BlockAlign { declared: u16, implied: u16 },
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FormatError::UnknownTag(tag) => {
                write!(f, "unknown wave format tag {tag:#06x}")
            }
            FormatError::UnknownSubformat(guid) => {
                write!(f, "unknown extensible subformat {guid:#034x}")
            }
            FormatError::NoChannels => f.write_str("the mix format declares no channels"),
            FormatError::NoRate => f.write_str("the mix format declares no sample rate"),
            FormatError::OddSampleSize(bits) => {
                write!(f, "{bits} bits per sample is not a whole number of bytes")
            }
            FormatError::BlockAlign { declared, implied } => write!(
                f,
                "the mix format declares a block alignment of {declared} bytes where its \
                 channels and sample size imply {implied}"
            ),
        }
    }
}

/// The endpoint's mix format, in the terms the report and the decoder need.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixFormat {
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits in the container, which is what the report prints. A 24-in-32
    /// format says 32 here and 24 in `valid_bits`.
    pub bits_per_sample: u16,
    pub valid_bits: u16,
    pub block_align: u16,
    pub kind: SampleKind,
    /// The `SPEAKER_*` mask, or zero when the format was a plain
    /// `WAVEFORMATEX` and therefore said nothing about speaker positions.
    pub channel_mask: u32,
    pub subformat: u128,
    pub extensible: bool,
}

impl MixFormat {
    /// Decides what a `WAVEFORMATEX` means, refusing anything it cannot decode.
    pub fn from_raw(raw: &RawWaveFormat) -> Result<Self, FormatError> {
        if raw.channels == 0 {
            return Err(FormatError::NoChannels);
        }
        if raw.samples_per_sec == 0 {
            return Err(FormatError::NoRate);
        }
        if raw.bits_per_sample == 0 || raw.bits_per_sample % 8 != 0 {
            return Err(FormatError::OddSampleSize(raw.bits_per_sample));
        }

        // The subformat wins wherever there is one. An extensible format's
        // `wFormatTag` is always `WAVE_FORMAT_EXTENSIBLE`, so the tag alone
        // cannot tell float from integer and reading it first would decode
        // every extensible float mix as PCM.
        let (kind, subformat) = match raw.extensible {
            Some(extensible) => match extensible.subformat {
                SUBTYPE_IEEE_FLOAT => (SampleKind::Float, SUBTYPE_IEEE_FLOAT),
                SUBTYPE_PCM => (SampleKind::Int, SUBTYPE_PCM),
                other => return Err(FormatError::UnknownSubformat(other)),
            },
            None => match raw.format_tag {
                WAVE_FORMAT_IEEE_FLOAT => (SampleKind::Float, SUBTYPE_IEEE_FLOAT),
                WAVE_FORMAT_PCM => (SampleKind::Int, SUBTYPE_PCM),
                other => return Err(FormatError::UnknownTag(other)),
            },
        };

        let implied = raw.channels * (raw.bits_per_sample / 8);
        if raw.block_align != implied {
            return Err(FormatError::BlockAlign {
                declared: raw.block_align,
                implied,
            });
        }

        // A zero here means the driver left the field alone rather than that
        // no bits are valid, and the container size is then the honest answer.
        let valid_bits = match raw.extensible {
            Some(extensible) if extensible.valid_bits != 0 => extensible.valid_bits,
            _ => raw.bits_per_sample,
        };

        Ok(MixFormat {
            sample_rate: raw.samples_per_sec,
            channels: raw.channels,
            bits_per_sample: raw.bits_per_sample,
            valid_bits,
            block_align: raw.block_align,
            kind,
            channel_mask: raw.extensible.map_or(0, |e| e.channel_mask),
            subformat,
            extensible: raw.extensible.is_some(),
        })
    }

    pub fn sample_bytes(&self) -> usize {
        self.bits_per_sample as usize / 8
    }

    pub fn frame_bytes(&self) -> usize {
        self.block_align as usize
    }

    /// Whether this is the shape the rest of the project has been assuming.
    /// Not a requirement, only something the report says out loud.
    pub fn matches_contract(&self) -> bool {
        self.sample_rate == 48_000 && self.channels == 2
    }

    /// One channel of one frame, as a number between -1 and 1.
    ///
    /// This is a reading, not a conversion: nothing captured is stored in this
    /// form, and it exists so the tone detector has something to measure. An
    /// integer sample is left-justified in its container whatever
    /// `wValidBitsPerSample` says, which is why the scale below is the
    /// container's and not the valid width's.
    pub fn decode(&self, frame: &[u8], channel: u16) -> Option<f32> {
        if channel >= self.channels {
            return None;
        }
        let width = self.sample_bytes();
        let start = channel as usize * width;
        let bytes = frame.get(start..start + width)?;
        Some(match (self.kind, width) {
            (SampleKind::Float, 4) => f32::from_le_bytes(bytes.try_into().ok()?),
            (SampleKind::Float, 8) => f64::from_le_bytes(bytes.try_into().ok()?) as f32,
            (SampleKind::Int, 2) => i16::from_le_bytes(bytes.try_into().ok()?) as f32 / 32_768.0,
            (SampleKind::Int, 3) => {
                // Assembled unsigned and then reinterpreted, because a
                // negative 24-bit sample has its top byte above 0x7F and
                // shifting that into place as a signed integer overflows.
                let raw = ((bytes[0] as u32) << 8)
                    | ((bytes[1] as u32) << 16)
                    | ((bytes[2] as u32) << 24);
                raw as i32 as f32 / 2_147_483_648.0
            }
            (SampleKind::Int, 4) => {
                i32::from_le_bytes(bytes.try_into().ok()?) as f32 / 2_147_483_648.0
            }
            _ => return None,
        })
    }
}

impl fmt::Display for MixFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Hz {} ch {} bit {}",
            self.sample_rate, self.channels, self.bits_per_sample, self.kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extensible_float() -> RawWaveFormat {
        RawWaveFormat {
            format_tag: WAVE_FORMAT_EXTENSIBLE,
            channels: 2,
            samples_per_sec: 48_000,
            avg_bytes_per_sec: 48_000 * 8,
            block_align: 8,
            bits_per_sample: 32,
            extensible: Some(RawExtensible {
                valid_bits: 32,
                channel_mask: 3,
                subformat: SUBTYPE_IEEE_FLOAT,
            }),
        }
    }

    #[test]
    fn extensible_float_reports_the_contract_shape() {
        let format = MixFormat::from_raw(&extensible_float()).expect("a describable format");
        assert_eq!(format.to_string(), "48000 Hz 2 ch 32 bit float");
        assert!(format.matches_contract());
        assert_eq!(format.frame_bytes(), 8);
    }

    #[test]
    fn the_subformat_beats_the_tag() {
        // An extensible PCM format carries the same tag as an extensible float
        // one, so a decoder that read the tag would call this float.
        let mut raw = extensible_float();
        raw.bits_per_sample = 16;
        raw.block_align = 4;
        raw.extensible = Some(RawExtensible {
            valid_bits: 16,
            channel_mask: 3,
            subformat: SUBTYPE_PCM,
        });
        let format = MixFormat::from_raw(&raw).expect("a describable format");
        assert_eq!(format.kind, SampleKind::Int);
        assert_eq!(format.to_string(), "48000 Hz 2 ch 16 bit int");
    }

    #[test]
    fn a_plain_wave_format_still_decodes() {
        let raw = RawWaveFormat {
            format_tag: WAVE_FORMAT_PCM,
            channels: 1,
            samples_per_sec: 44_100,
            avg_bytes_per_sec: 88_200,
            block_align: 2,
            bits_per_sample: 16,
            extensible: None,
        };
        let format = MixFormat::from_raw(&raw).expect("a describable format");
        assert_eq!(format.to_string(), "44100 Hz 1 ch 16 bit int");
        assert!(!format.matches_contract());
        assert_eq!(format.channel_mask, 0);
    }

    #[test]
    fn an_unknown_subformat_is_refused() {
        let mut raw = extensible_float();
        raw.extensible = Some(RawExtensible {
            valid_bits: 32,
            channel_mask: 3,
            subformat: 0xdead_beef,
        });
        assert_eq!(
            MixFormat::from_raw(&raw),
            Err(FormatError::UnknownSubformat(0xdead_beef))
        );
    }

    #[test]
    fn a_block_alignment_that_disagrees_is_refused() {
        let mut raw = extensible_float();
        raw.block_align = 4;
        assert_eq!(
            MixFormat::from_raw(&raw),
            Err(FormatError::BlockAlign {
                declared: 4,
                implied: 8
            })
        );
    }

    #[test]
    fn a_zero_valid_bits_field_falls_back_to_the_container() {
        let mut raw = extensible_float();
        raw.extensible = Some(RawExtensible {
            valid_bits: 0,
            channel_mask: 3,
            subformat: SUBTYPE_IEEE_FLOAT,
        });
        assert_eq!(
            MixFormat::from_raw(&raw)
                .expect("a describable format")
                .valid_bits,
            32
        );
    }

    #[test]
    fn float_frames_decode_per_channel() {
        let format = MixFormat::from_raw(&extensible_float()).expect("a describable format");
        let mut frame = [0u8; 8];
        frame[..4].copy_from_slice(&0.5f32.to_le_bytes());
        frame[4..].copy_from_slice(&(-0.25f32).to_le_bytes());
        assert_eq!(format.decode(&frame, 0), Some(0.5));
        assert_eq!(format.decode(&frame, 1), Some(-0.25));
        assert_eq!(format.decode(&frame, 2), None);
    }

    #[test]
    fn sixteen_bit_frames_decode_to_full_scale() {
        let mut raw = extensible_float();
        raw.bits_per_sample = 16;
        raw.block_align = 4;
        raw.extensible = Some(RawExtensible {
            valid_bits: 16,
            channel_mask: 3,
            subformat: SUBTYPE_PCM,
        });
        let format = MixFormat::from_raw(&raw).expect("a describable format");
        let mut frame = [0u8; 4];
        frame[..2].copy_from_slice(&(-32_768i16).to_le_bytes());
        frame[2..].copy_from_slice(&16_384i16.to_le_bytes());
        assert_eq!(format.decode(&frame, 0), Some(-1.0));
        assert_eq!(format.decode(&frame, 1), Some(0.5));
    }

    #[test]
    fn twenty_four_bit_frames_decode_left_justified() {
        let raw = RawWaveFormat {
            format_tag: WAVE_FORMAT_EXTENSIBLE,
            channels: 2,
            samples_per_sec: 48_000,
            avg_bytes_per_sec: 48_000 * 6,
            block_align: 6,
            bits_per_sample: 24,
            extensible: Some(RawExtensible {
                valid_bits: 24,
                channel_mask: 3,
                subformat: SUBTYPE_PCM,
            }),
        };
        let format = MixFormat::from_raw(&raw).expect("a describable format");
        // 0x400000 is a quarter of the 24-bit range.
        let frame = [0x00, 0x00, 0x40, 0x00, 0x00, 0xC0];
        assert_eq!(format.decode(&frame, 0), Some(0.5));
        assert_eq!(format.decode(&frame, 1), Some(-0.5));
    }

    /// The two subformat GUIDs and the three tags above are written out as
    /// integers so that a machine without the Windows SDK can decode a format
    /// a Windows machine reported. That is only safe while they really are the
    /// SDK's values, which is what this asserts wherever the SDK exists.
    #[cfg(windows)]
    #[test]
    fn the_local_constants_are_the_sdk_constants() {
        use windows::Win32::Media::Audio::WAVE_FORMAT_PCM as SDK_PCM;
        use windows::Win32::Media::KernelStreaming::{
            KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE as SDK_EXTENSIBLE,
        };
        use windows::Win32::Media::Multimedia::{
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT as SDK_FLOAT,
        };

        assert_eq!(SUBTYPE_PCM, KSDATAFORMAT_SUBTYPE_PCM.to_u128());
        assert_eq!(
            SUBTYPE_IEEE_FLOAT,
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.to_u128()
        );
        assert_eq!(u32::from(WAVE_FORMAT_EXTENSIBLE), SDK_EXTENSIBLE);
        assert_eq!(u32::from(WAVE_FORMAT_PCM), SDK_PCM);
        assert_eq!(u32::from(WAVE_FORMAT_IEEE_FLOAT), SDK_FLOAT);
    }
}
