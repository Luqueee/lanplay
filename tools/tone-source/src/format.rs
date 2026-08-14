//! What an endpoint says it accepts, and whether the tone can be rendered into
//! it untouched.
//!
//! The reason this is a type of its own rather than a `WAVEFORMATEX` passed
//! around is that the whole point of the run is to report the format verbatim,
//! including on the path where the format is refused. A refusal that printed
//! only "wrong format" would throw away the finding the phase exists to
//! establish.
//!
//! The rendering matches the line the capture side prints, `<rate> Hz
//! <channels> ch <bits> bit <float|int>`, so the two halves of the batch can be
//! compared by eye without either being reformatted.

use core::fmt;

use crate::tone::ToneSpec;

/// How samples are encoded. `Other` exists so a format this program does not
/// recognise is reported as itself instead of being rounded to the nearer of
/// two guesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sample {
    Float,
    Int,
    Other(u16),
}

impl fmt::Display for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sample::Float => write!(f, "float"),
            Sample::Int => write!(f, "int"),
            Sample::Other(tag) => write!(f, "format tag 0x{tag:04X}"),
        }
    }
}

/// One endpoint's mix format, in the terms that decide whether rendering it
/// needs a converter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MixFormat {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub sample: Sample,
    /// Bytes per frame, as the endpoint reports it. Kept because the render
    /// loop indexes the device buffer by it, and a value that disagrees with
    /// `channels * bits / 8` would mean the buffer is laid out some other way.
    pub block_align: u16,
    /// Which speaker each channel drives, when the format is extensible enough
    /// to say. `None` for a plain `WAVEFORMATEX`, which carries no mask.
    pub channel_mask: Option<u32>,
}

/// `SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT`, the only mask under which the
/// contract's left and right channels mean what they say.
const FRONT_PAIR: u32 = 0x3;

impl MixFormat {
    /// Whether the tone can be written into this format without conversion.
    ///
    /// Bit depth and encoding are part of the answer, not just the rate: the
    /// generator produces `f32`, and anything else would need a conversion this
    /// program refuses to perform silently.
    pub fn carries(&self, spec: &ToneSpec) -> bool {
        self.rate == spec.sample_rate
            && self.channels == spec.channels
            && self.bits == 32
            && self.sample == Sample::Float
            && u32::from(self.block_align) == u32::from(spec.channels) * 4
    }

    /// How the mask reads, for the report.
    pub fn channel_mask_note(&self) -> &'static str {
        match self.channel_mask {
            Some(FRONT_PAIR) => "front left, front right",
            Some(_) => "not the front stereo pair",
            None => "none reported",
        }
    }
}

impl fmt::Display for MixFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Hz {} ch {} bit {}",
            self.rate, self.channels, self.bits, self.sample
        )
    }
}

#[cfg(windows)]
mod win {
    use windows::Win32::Media::Audio::{WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE};
    use windows::Win32::Media::KernelStreaming::{
        KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE,
    };
    use windows::Win32::Media::Multimedia::{
        KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT,
    };

    use super::{MixFormat, Sample};

    impl MixFormat {
        /// Reads what `IAudioClient::GetMixFormat` handed back.
        ///
        /// A shared-mode mix format is `WAVE_FORMAT_EXTENSIBLE` on every
        /// Windows worth supporting, and then the encoding lives in `SubFormat`
        /// rather than in `wFormatTag`; reading only the tag would report every
        /// modern endpoint as an unknown format. The plain-tag case is still
        /// handled, because an endpoint is allowed to answer that way and being
        /// wrong about it here would look like a device problem.
        ///
        /// # Safety
        ///
        /// `format` must point at a `WAVEFORMATEX` whose trailing `cbSize`
        /// bytes are readable, which is what `GetMixFormat` allocates.
        pub unsafe fn read(format: *const WAVEFORMATEX) -> MixFormat {
            // SAFETY: the caller guarantees the pointer and its extra bytes;
            // the extensible fields are only read once `cbSize` says they are
            // there, which is the same test the OS applies.
            unsafe {
                let base = &*format;
                let extensible = base.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16
                    && usize::from(base.cbSize)
                        >= size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>();

                let (sample, channel_mask) = if extensible {
                    // `WAVEFORMATEXTENSIBLE` is byte-packed, so its GUID sits at
                    // whatever offset the header left it at and cannot be
                    // borrowed to be compared. Read through raw pointers into
                    // aligned locals instead.
                    let full = format as *const WAVEFORMATEXTENSIBLE;
                    let sub_format = (&raw const (*full).SubFormat).read_unaligned();
                    let mask = (&raw const (*full).dwChannelMask).read_unaligned();
                    let sample = if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                        Sample::Float
                    } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
                        Sample::Int
                    } else {
                        Sample::Other(WAVE_FORMAT_EXTENSIBLE as u16)
                    };
                    (sample, Some(mask))
                } else {
                    let sample = match u32::from(base.wFormatTag) {
                        WAVE_FORMAT_IEEE_FLOAT => Sample::Float,
                        WAVE_FORMAT_PCM => Sample::Int,
                        _ => Sample::Other(base.wFormatTag),
                    };
                    (sample, None)
                };

                MixFormat {
                    rate: base.nSamplesPerSec,
                    channels: base.nChannels,
                    bits: base.wBitsPerSample,
                    sample,
                    block_align: base.nBlockAlign,
                    channel_mask,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tone::CONTRACT;

    fn stereo_float() -> MixFormat {
        MixFormat {
            rate: 48_000,
            channels: 2,
            bits: 32,
            sample: Sample::Float,
            block_align: 8,
            channel_mask: Some(FRONT_PAIR),
        }
    }

    #[test]
    fn renders_the_line_the_capture_side_prints() {
        assert_eq!(stereo_float().to_string(), "48000 Hz 2 ch 32 bit float");
        assert_eq!(
            MixFormat {
                bits: 16,
                sample: Sample::Int,
                block_align: 4,
                ..stereo_float()
            }
            .to_string(),
            "48000 Hz 2 ch 16 bit int"
        );
        assert_eq!(
            MixFormat {
                sample: Sample::Other(0x0006),
                ..stereo_float()
            }
            .to_string(),
            "48000 Hz 2 ch 32 bit format tag 0x0006"
        );
    }

    /// Every one of these would need a converter, which is the thing this
    /// program refuses to do behind the operator's back.
    #[test]
    fn refuses_everything_that_is_not_the_tone_exactly() {
        assert!(stereo_float().carries(&CONTRACT));

        for wrong in [
            MixFormat {
                rate: 44_100,
                ..stereo_float()
            },
            MixFormat {
                channels: 6,
                block_align: 24,
                ..stereo_float()
            },
            MixFormat {
                bits: 16,
                sample: Sample::Int,
                block_align: 4,
                ..stereo_float()
            },
            MixFormat {
                sample: Sample::Int,
                ..stereo_float()
            },
            // Right on every field a report would print, and still laid out
            // some other way in memory.
            MixFormat {
                block_align: 6,
                ..stereo_float()
            },
        ] {
            assert!(!wrong.carries(&CONTRACT), "{wrong} was accepted");
        }
    }

    #[test]
    fn names_the_channel_mask_it_found() {
        assert_eq!(
            stereo_float().channel_mask_note(),
            "front left, front right"
        );
        assert_eq!(
            MixFormat {
                channel_mask: Some(0x30),
                ..stereo_float()
            }
            .channel_mask_note(),
            "not the front stereo pair"
        );
        assert_eq!(
            MixFormat {
                channel_mask: None,
                ..stereo_float()
            }
            .channel_mask_note(),
            "none reported"
        );
    }
}
