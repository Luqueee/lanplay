use core::fmt;

use opus::ErrorCode;

/// Everything that can go wrong between a buffer of samples and a packet, and
/// back.
///
/// Every variant is a refusal rather than a repair. A codec that padded a short
/// buffer, or that accepted a packet whose duration was not the one configured,
/// would keep running and produce audio nobody could account for; the whole
/// reason this phase exists is to establish what the codec does with correct
/// input, which cannot be said by a component that quietly fixes incorrect
/// input.
///
/// Variants that wrap an [`ErrorCode`] keep libopus's own code and the name of
/// the call that produced it, because a failure inside a C library is otherwise
/// indistinguishable from a failure in the wrapper around it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodecError {
    /// `opus_encoder_create` or `opus_decoder_create` refused.
    Creation {
        function: &'static str,
        code: ErrorCode,
    },
    /// A CTL refused. The encoder is configured entirely through these, so a
    /// CTL that fails silently would leave a codec running at settings nobody
    /// chose.
    Property {
        name: &'static str,
        code: ErrorCode,
    },
    /// Opus runs at five rates and nothing else.
    UnsupportedSampleRate(u32),
    /// The single-stream API is mono or stereo. Anything wider needs the
    /// multistream encoder, which is a different object with a different
    /// mapping, not a wider version of this one.
    UnsupportedChannels(u16),
    /// The buffer handed in was not exactly one frame.
    ///
    /// Exact equality rather than a minimum, because `opus_encode_float` infers
    /// the frame duration from the length it is given. A buffer holding two
    /// frames' worth would encode happily as one frame of twice the duration,
    /// and every byte count and latency measured afterwards would describe a
    /// frame size nobody asked for.
    FrameLength {
        submitted: usize,
        expected: usize,
    },
    /// A zero-length packet. libopus reads that as packet loss and runs its
    /// concealer, which would fabricate audio and report it as decoded. This
    /// phase has no loss in it, so a packet with no bytes is a defect upstream.
    EmptyPacket,
    Encode(ErrorCode),
    Decode(ErrorCode),
    /// The decoder returned a different number of samples than the frame
    /// duration calls for. Opus is lossy in amplitude and exact in length, so
    /// this cannot happen to a packet this encoder produced, and hiding it
    /// would remove the only cheap check that the two halves are still talking
    /// about the same stream.
    DecodedLength {
        returned: usize,
        expected: usize,
    },
    /// The run asked for less audio than a single frame.
    NothingToEncode {
        frames: u64,
        frame_samples: usize,
    },
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::Creation { function, code } => {
                write!(f, "{function} failed: {}", code.description())
            }
            CodecError::Property { name, code } => {
                write!(f, "could not set {name}: {}", code.description())
            }
            CodecError::UnsupportedSampleRate(rate) => write!(
                f,
                "Opus runs at 8000, 12000, 16000, 24000 or 48000 Hz; {rate} Hz would need a \
                 resampler, and this phase deliberately has none"
            ),
            CodecError::UnsupportedChannels(channels) => write!(
                f,
                "the single-stream Opus encoder is mono or stereo; {channels} channels needs the \
                 multistream API and a channel mapping"
            ),
            CodecError::FrameLength {
                submitted,
                expected,
            } => write!(
                f,
                "expected exactly one frame of {expected} interleaved samples, got {submitted}"
            ),
            CodecError::EmptyPacket => write!(
                f,
                "a zero-length packet asks libopus to conceal a loss and invent audio; there is no \
                 loss in this phase to conceal"
            ),
            CodecError::Encode(code) => {
                write!(f, "opus_encode_float failed: {}", code.description())
            }
            CodecError::Decode(code) => {
                write!(f, "opus_decode_float failed: {}", code.description())
            }
            CodecError::DecodedLength { returned, expected } => write!(
                f,
                "the packet decoded to {returned} samples per channel, not the {expected} the frame \
                 duration calls for"
            ),
            CodecError::NothingToEncode {
                frames,
                frame_samples,
            } => write!(
                f,
                "{frames} frames of audio is less than one {frame_samples} frame; there would be \
                 nothing to measure"
            ),
        }
    }
}

impl core::error::Error for CodecError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_creation_failure_names_the_call_and_what_opus_said() {
        let message = CodecError::Creation {
            function: "opus_encoder_create",
            code: ErrorCode::BadArg,
        }
        .to_string();
        assert!(message.contains("opus_encoder_create"), "{message}");
        assert!(
            message.contains(ErrorCode::BadArg.description()),
            "{message}"
        );
    }

    #[test]
    fn a_frame_length_refusal_reports_both_lengths() {
        let message = CodecError::FrameLength {
            submitted: 479,
            expected: 480,
        }
        .to_string();
        assert!(message.contains("479"), "{message}");
        assert!(message.contains("480"), "{message}");
    }

    #[test]
    fn the_empty_packet_refusal_says_it_is_about_concealment() {
        let message = CodecError::EmptyPacket.to_string();
        assert!(message.contains("conceal"), "{message}");
    }

    #[test]
    fn an_unsupported_rate_is_named_rather_than_rounded() {
        let message = CodecError::UnsupportedSampleRate(44_100).to_string();
        assert!(message.contains("44100"), "{message}");
        assert!(message.contains("48000"), "{message}");
    }
}
