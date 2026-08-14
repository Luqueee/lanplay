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

    /// Produces one frame from libopus's own concealer, for a frame that did
    /// not arrive.
    ///
    /// This is `opus_decode` with a null packet, which is precisely what the
    /// codec provides for a lost frame: it extrapolates from the state the real
    /// frames left behind, so the waveform continues instead of stopping.
    /// Zero-filled silence was rejected as the alternative. A step to zero and
    /// back is a click, and a click is louder and more noticeable than the
    /// few milliseconds of audio it stands in for.
    ///
    /// It runs through the same decoder as every real frame because it has to.
    /// The concealer reads the decoder state and updates it, so the next real
    /// packet decodes against a decoder that knows a gap went by; concealing on
    /// a second decoder would leave this one believing the stream was never
    /// interrupted, and the frame after the gap would be reconstructed from a
    /// history that did not happen.
    pub fn conceal(&mut self) -> Result<&[f32], CodecError> {
        let expected = self.config.frame_samples();
        // An empty slice is how the wrapper spells a null packet; the frame
        // size comes from the buffer's length, which is exactly one frame.
        let returned = self
            .decoder
            .decode_float(&[], &mut self.pcm, false)
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

    /// Root mean square of a frame, as a fraction of full scale.
    fn rms(pcm: &[f32]) -> f64 {
        let sum: f64 = pcm.iter().map(|sample| f64::from(*sample).powi(2)).sum();
        (sum / pcm.len() as f64).sqrt()
    }

    /// Runs the tone through the codec for `frames`, leaving the decoder with
    /// the state a real stream would have given it.
    fn warmed(config: CodecConfig, frames: usize) -> (OpusDecoder, f64) {
        let mut encoder = OpusEncoder::new(config).expect("encoder");
        let mut decoder = OpusDecoder::new(config).expect("decoder");
        let mut tone = lanplay_tone_source::tone::Tone::new(lanplay_tone_source::tone::CONTRACT);
        let mut pcm = vec![0f32; config.frame_interleaved()];
        let mut last = 0.0;
        for _ in 0..frames {
            tone.fill_stereo(&mut pcm);
            let packet = encoder.encode(&pcm).expect("encode").to_vec();
            last = rms(decoder.decode(&packet).expect("decode"));
        }
        (decoder, last)
    }

    #[test]
    fn concealment_returns_one_whole_frame() {
        let config = contract(FrameDuration::Ms5);
        let (mut decoder, _) = warmed(config, 8);
        let concealed = decoder.conceal().expect("conceal");
        assert_eq!(concealed.len(), config.frame_interleaved());
    }

    #[test]
    fn concealment_continues_the_waveform_rather_than_returning_silence() {
        // The clause the whole receiving path rests on. Zero-filled silence
        // would pass any frame count and any length check and be a click, so
        // the thing worth asserting is that there is audio in it and that it is
        // at roughly the level of the audio it stands in for.
        let config = contract(FrameDuration::Ms5);
        let (mut decoder, playing) = warmed(config, 40);
        let first = rms(decoder.conceal().expect("conceal")).max(f64::MIN_POSITIVE);

        assert!(
            first > playing / 4.0,
            "the concealer returned {first:.6} against {playing:.6} of real audio, which is \
             nearer silence than continuation"
        );
    }

    #[test]
    fn a_concealed_frame_leaves_the_decoder_able_to_decode_the_next_real_one() {
        // Concealment runs through the same decoder as every real frame, so a
        // gap must not leave it in a state the frame after the gap cannot be
        // decoded from.
        let config = contract(FrameDuration::Ms5);
        let (mut decoder, playing) = warmed(config, 20);
        for _ in 0..3 {
            decoder.conceal().expect("conceal");
        }

        let mut encoder = OpusEncoder::new(config).expect("encoder");
        let mut tone = lanplay_tone_source::tone::Tone::new(lanplay_tone_source::tone::CONTRACT);
        let mut pcm = vec![0f32; config.frame_interleaved()];
        let mut recovered = 0.0;
        for _ in 0..8 {
            tone.fill_stereo(&mut pcm);
            let packet = encoder.encode(&pcm).expect("encode").to_vec();
            recovered = rms(decoder.decode(&packet).expect("decode after a gap"));
        }
        assert!(
            recovered > playing / 2.0,
            "after a three frame gap the stream came back at {recovered:.6} against {playing:.6}"
        );
    }
}
