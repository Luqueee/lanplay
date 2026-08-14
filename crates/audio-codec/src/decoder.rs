//! The decoding half: one packet in, one frame of interleaved float samples
//! out.
//!
//! The mirror of the encoder and deliberately not its partner: nothing here
//! holds a reference to an encoder, and the two can be constructed in different
//! processes on different machines as long as they were handed the same
//! [`CodecConfig`]. That is the whole reason the configuration is a value.
//!
//! The output buffer is sized to the configured frame and no larger, which
//! makes any packet of another duration fail rather than decode. That is
//! intentional. libopus will happily decode a 20 ms packet into a stream whose
//! frames are 5 ms, and a receiver that accepted it would be quietly running at
//! a cadence its jitter buffer was never sized for. Here the buffer is the
//! check: `opus_decode_float` refuses with a buffer-too-small code, which
//! arrives as [`CodecError::Decode`] naming exactly that.

use opus::{Channels, Decoder};

use crate::config::{CodecConfig, SAMPLE_RATES};
use crate::error::CodecError;

/// One Opus decoder and the single frame buffer it writes into.
pub struct OpusDecoder {
    decoder: Decoder,
    config: CodecConfig,
    /// Owned up front and never resized, for the same reason the encoder's
    /// packet buffer is: this runs on a path where a renderer is waiting.
    pcm: Box<[f32]>,
}

impl OpusDecoder {
    pub fn new(config: CodecConfig) -> Result<OpusDecoder, CodecError> {
        let channels = match config.channels {
            1 => Channels::Mono,
            2 => Channels::Stereo,
            other => return Err(CodecError::UnsupportedChannels(other)),
        };
        if !SAMPLE_RATES.contains(&config.sample_rate) {
            return Err(CodecError::UnsupportedSampleRate(config.sample_rate));
        }

        let decoder =
            Decoder::new(config.sample_rate, channels).map_err(|error| CodecError::Creation {
                function: error.function(),
                code: error.code(),
            })?;

        Ok(OpusDecoder {
            decoder,
            config,
            pcm: vec![0f32; config.frame_interleaved()].into_boxed_slice(),
        })
    }

    pub fn config(&self) -> &CodecConfig {
        &self.config
    }

    /// Interleaved samples every successful decode returns.
    pub fn frame_interleaved(&self) -> usize {
        self.config.frame_interleaved()
    }

    /// Decodes one packet into exactly one frame, borrowed from the buffer this
    /// decoder owns.
    ///
    /// The length is checked rather than trusted. Opus is lossy in amplitude
    /// and exact in length, so a frame count that disagrees with the frame
    /// duration is a defect somewhere upstream, and a decoder that returned the
    /// short buffer anyway would hand a renderer a gap it had no way to notice.
    pub fn decode(&mut self, packet: &[u8]) -> Result<&[f32], CodecError> {
        if packet.is_empty() {
            return Err(CodecError::EmptyPacket);
        }
        let expected = self.config.frame_samples();
        let returned = self
            .decoder
            .decode_float(packet, &mut self.pcm, false)
            .map_err(|error| CodecError::Decode(error.code()))?;
        if returned != expected {
            return Err(CodecError::DecodedLength { returned, expected });
        }
        Ok(&self.pcm[..returned * self.config.channels as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FrameDuration;
    use crate::encoder::OpusEncoder;

    fn contract(frame: FrameDuration) -> CodecConfig {
        CodecConfig::contract(frame, CodecConfig::DEFAULT_BITRATE_BPS)
    }

    /// One frame of the contract tone, generated the same way the probe does.
    fn frame(config: &CodecConfig) -> Vec<f32> {
        let mut tone = lanplay_tone_source::tone::Tone::new(lanplay_tone_source::tone::CONTRACT);
        let mut pcm = vec![0f32; config.frame_interleaved()];
        tone.fill_stereo(&mut pcm);
        pcm
    }

    #[test]
    fn an_empty_packet_is_refused_rather_than_concealed() {
        let mut decoder = OpusDecoder::new(contract(FrameDuration::Ms5)).expect("decoder");
        assert_eq!(decoder.decode(&[]).err(), Some(CodecError::EmptyPacket));
    }

    #[test]
    fn a_packet_of_the_wrong_duration_is_refused() {
        let config = contract(FrameDuration::Ms20);
        let mut encoder = OpusEncoder::new(config).expect("encoder");
        let packet = encoder.encode(&frame(&config)).expect("encode").to_vec();

        // Same rate, same channels, a quarter of the duration: the packet is
        // perfectly valid Opus and still must not be accepted here.
        let mut decoder = OpusDecoder::new(contract(FrameDuration::Ms5)).expect("decoder");
        match decoder.decode(&packet) {
            Err(CodecError::Decode(code)) => {
                assert_eq!(code, opus::ErrorCode::BufferTooSmall)
            }
            other => panic!("a 20 ms packet was accepted by a 5 ms decoder: {other:?}"),
        }
    }
}
