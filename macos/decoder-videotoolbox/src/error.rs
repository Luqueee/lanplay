use core::fmt;

/// Everything that can go wrong between a coded access unit and a pixel
/// buffer.
///
/// Every variant that wraps an `OSStatus` keeps the raw value: VideoToolbox
/// status codes are the only evidence available when a decode fails on a
/// device we cannot attach a debugger to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecoderError {
    /// A H.264 format description needs at least one SPS and one PPS.
    MissingParameterSets {
        sps: usize,
        pps: usize,
    },
    /// AVCC length prefixes are 1, 2 or 4 bytes wide; nothing else is legal.
    UnsupportedNalLengthSize(u8),
    FormatDescription(i32),
    /// The parameter sets describe a different picture size than the caller
    /// declared. Silently accepting it would hand the renderer textures of
    /// the wrong size.
    DimensionMismatch {
        expected: (u32, u32),
        actual: (u32, u32),
    },
    /// The session could not be created. `require_hardware` records whether
    /// the request insisted on a hardware decoder, because that is by far the
    /// most common reason for a refusal.
    SessionCreation {
        status: i32,
        require_hardware: bool,
    },
    /// The session was created but reports a software decoder, and the
    /// configuration asked for hardware. Falling back silently would make
    /// every latency number measured afterwards a lie.
    SoftwareDecoder,
    Property {
        key: &'static str,
        status: i32,
    },
    BlockBuffer(i32),
    SampleBuffer(i32),
    DecodeFrame(i32),
    WaitForFrames(i32),
}

impl fmt::Display for DecoderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            DecoderError::MissingParameterSets { sps, pps } => write!(
                f,
                "H.264 format description needs at least one SPS and one PPS, got {sps} SPS and {pps} PPS"
            ),
            DecoderError::UnsupportedNalLengthSize(size) => {
                write!(
                    f,
                    "unsupported AVCC NAL length size {size}, expected 1, 2 or 4"
                )
            }
            DecoderError::DimensionMismatch {
                expected: (ew, eh),
                actual: (aw, ah),
            } => write!(
                f,
                "configuration declares {ew}x{eh} but the parameter sets describe {aw}x{ah}"
            ),
            DecoderError::FormatDescription(status) => {
                write!(
                    f,
                    "CMVideoFormatDescriptionCreateFromH264ParameterSets failed with status {status}"
                )
            }
            DecoderError::SessionCreation {
                status,
                require_hardware: true,
            } => write!(
                f,
                "VTDecompressionSessionCreate failed with status {status} while requiring a hardware decoder"
            ),
            DecoderError::SessionCreation {
                status,
                require_hardware: false,
            } => write!(
                f,
                "VTDecompressionSessionCreate failed with status {status}"
            ),
            DecoderError::SoftwareDecoder => f.write_str(
                "hardware decoding was required but the session reports a software decoder",
            ),
            DecoderError::Property { key, status } => {
                write!(f, "VideoToolbox property {key} failed with status {status}")
            }
            DecoderError::BlockBuffer(status) => {
                write!(f, "CMBlockBuffer creation failed with status {status}")
            }
            DecoderError::SampleBuffer(status) => {
                write!(f, "CMSampleBuffer creation failed with status {status}")
            }
            DecoderError::DecodeFrame(status) => {
                write!(
                    f,
                    "VTDecompressionSessionDecodeFrame failed with status {status}"
                )
            }
            DecoderError::WaitForFrames(status) => write!(
                f,
                "VTDecompressionSessionWaitForAsynchronousFrames failed with status {status}"
            ),
        }
    }
}

impl core::error::Error for DecoderError {}

#[cfg(test)]
mod tests {
    use super::DecoderError;

    #[test]
    fn hardware_requirement_is_named_in_the_session_error() {
        let required = DecoderError::SessionCreation {
            status: -12_913,
            require_hardware: true,
        };
        let optional = DecoderError::SessionCreation {
            status: -12_913,
            require_hardware: false,
        };
        assert_eq!(
            required.to_string(),
            "VTDecompressionSessionCreate failed with status -12913 while requiring a hardware decoder"
        );
        assert_eq!(
            optional.to_string(),
            "VTDecompressionSessionCreate failed with status -12913"
        );
    }

    #[test]
    fn software_fallback_error_says_what_was_required_and_what_happened() {
        let text = DecoderError::SoftwareDecoder.to_string();
        assert!(text.contains("hardware"), "{text}");
        assert!(text.contains("software"), "{text}");
    }

    #[test]
    fn status_codes_survive_into_the_message() {
        assert_eq!(
            DecoderError::DecodeFrame(-8_969).to_string(),
            "VTDecompressionSessionDecodeFrame failed with status -8969"
        );
        assert_eq!(
            DecoderError::Property {
                key: "RealTime",
                status: -12_900,
            }
            .to_string(),
            "VideoToolbox property RealTime failed with status -12900"
        );
    }

    #[test]
    fn parameter_set_error_reports_both_counts() {
        assert_eq!(
            DecoderError::MissingParameterSets { sps: 1, pps: 0 }.to_string(),
            "H.264 format description needs at least one SPS and one PPS, got 1 SPS and 0 PPS"
        );
    }
}
