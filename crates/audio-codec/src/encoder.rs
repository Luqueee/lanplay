//! The encoding half: interleaved float samples in, one packet out.
//!
//! Split from the decoder so that the host and the client can each own the half
//! they need. Nothing here knows how a packet gets anywhere, and nothing here
//! keeps a queue: one call takes exactly one frame and returns exactly one
//! packet, which is what lets the caller time the call and have the number mean
//! the encoder rather than the encoder plus whatever it was waiting for.
//!
//! Everything the encoder does is decided in [`OpusEncoder::new`] and read back
//! immediately afterwards. Reading it back is the point: libopus CTLs are
//! advisory in the sense that an unsupported one returns a code rather than
//! changing anything, so a configuration that is only ever written is a
//! configuration nobody has checked.

use crate::config::{CodecConfig, MAX_PACKET_BYTES, SAMPLE_RATES};
use crate::error::CodecError;
use crate::ffi::{Application, Bandwidth, Channels, Encoder, ErrorCode, FrameSize};

/// What the encoder says it is doing, read out of the encoder rather than
/// copied from what it was told.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EncoderSettings {
    pub application: &'static str,
    pub bitrate_bps: i32,
    pub vbr: bool,
    pub vbr_constrained: bool,
    pub dtx: bool,
    pub inband_fec: bool,
    pub complexity: i32,
    /// Samples of algorithmic delay the codec adds, which is the part of
    /// end-to-end latency that no amount of scheduling can remove.
    pub lookahead: i32,
}

/// One Opus encoder and the single packet buffer it writes into.
pub struct OpusEncoder {
    encoder: Encoder,
    config: CodecConfig,
    settings: EncoderSettings,
    /// Owned up front and never resized. The encode call runs where a device is
    /// waiting, and a `Vec` grown inside it would put a heap allocation on the
    /// path whose timing is the measurement.
    packet: Box<[u8]>,
}

impl OpusEncoder {
    pub fn new(config: CodecConfig) -> Result<OpusEncoder, CodecError> {
        let channels = channels(config.channels)?;
        if !SAMPLE_RATES.contains(&config.sample_rate) {
            return Err(CodecError::UnsupportedSampleRate(config.sample_rate));
        }

        // Interactive audio, so neither of the other two applications fits.
        // `Audio` is documented as being for broadcast and high fidelity, where
        // the decoded result should be as close as possible to the input, and
        // it spends both bits and delay buying that. `Voip` optimises for
        // speech intelligibility, and game audio is music, effects and
        // ambience, none of which is speech. `LowDelay` is
        // OPUS_APPLICATION_RESTRICTED_LOWDELAY, which drops the SILK layer and
        // with it SILK's 6.5 ms lookahead; against a 5 ms frame budget that
        // lookahead would be larger than the frame.
        let mut encoder = Encoder::new(config.sample_rate, channels, Application::LowDelay)
            .map_err(|code| CodecError::Creation {
                function: "opus_encoder_create",
                code,
            })?;

        // Fullband, set rather than inherited. The endpoint mixes at 48 kHz and
        // the whole point of taking audio off a game is the parts of it that
        // live above 8 kHz, so letting the encoder narrow the bandpass on its
        // own would trade away the thing being carried to hit a target that is
        // not binding here anyway. Both CTLs are set because the maximum is
        // what the encoder's own bandwidth decision is clamped to, and the
        // other is that decision overridden.
        encoder
            .set_max_bandwidth(Bandwidth::FULLBAND)
            .map_err(property("OPUS_SET_MAX_BANDWIDTH"))?;
        encoder
            .set_bandwidth(Bandwidth::FULLBAND)
            .map_err(property("OPUS_SET_BANDWIDTH"))?;

        encoder
            .set_bitrate(config.bitrate_bps)
            .map_err(property("OPUS_SET_BITRATE"))?;

        // Variable rate, constrained. Both are libopus's own defaults and both
        // are set here anyway, so that the report describes a configuration
        // rather than an inheritance. The constraint is the one that matters
        // for a real-time link, and `opus_encoder.c` says why where it picks
        // it: "Makes constrained VBR the default (safer for real-time use)".
        // Its documented effect in `opus_defines.h` is that constrained VBR
        // "creates a maximum of one frame of buffering delay assuming a
        // transport with a serialization speed of the nominal bitrate", which
        // is exactly the property a link shared with a video stream needs.
        //
        // Unconstrained VBR was measured rather than assumed away, because the
        // gap between the requested rate and the produced one is the finding
        // this phase exists to produce. `celt_encoder.c` lets an unconstrained
        // frame run all the way to its ceiling — "Don't allow more than
        // doubling the rate", `target = IMIN(2*base_target, target)` — and the
        // tonality boost a few lines above it takes the contract tone very
        // nearly there: 232.4 kbps measured against a 128 kbps target at 5 ms,
        // a factor of 1.82. Constrained VBR damps the same excursion to 0.67
        // of it and lands at 129.7 kbps. Broadband noise through the identical
        // unconstrained encoder comes out at 118.9 kbps, below target, which
        // is what says the excess is the signal being tonal rather than
        // per-packet overhead: overhead would grow with the packet rate, and
        // `celt_encoder.c` in fact subtracts an allowance for it.
        encoder.set_vbr(true).map_err(property("OPUS_SET_VBR"))?;
        encoder
            .set_vbr_constraint(true)
            .map_err(property("OPUS_SET_VBR_CONSTRAINT"))?;

        // Discontinuous transmission off, so the encoder emits a packet for
        // every frame including silent ones. With DTX on it emits a two-byte
        // packet or none at all during silence, and a run that counted those
        // would report a bitrate about the silence rather than about the codec.
        // The recovery behaviour DTX interacts with belongs to a later phase
        // that has a network in it.
        encoder.set_dtx(false).map_err(property("OPUS_SET_DTX"))?;

        // In-band FEC off, for the same reason. FEC spends bits of the current
        // packet re-encoding the previous one at lower quality, and those bits
        // would land in the byte counts this phase reports as if they were the
        // cost of carrying audio. There is no loss here for them to recover,
        // and measuring the codec means measuring the codec rather than its
        // recovery features.
        encoder
            .set_inband_fec(false)
            .map_err(property("OPUS_SET_INBAND_FEC"))?;

        // One call, one frame, whatever length the buffer implies. This is the
        // default, and it is stated because the packet accounting depends on
        // it: any other setting lets the encoder split a buffer into several
        // shorter frames and emit them in one packet, and then a packet count
        // and a frame count stop being the same measurement.
        encoder
            .set_expert_frame_duration(FrameSize::ARG)
            .map_err(property("OPUS_SET_EXPERT_FRAME_DURATION"))?;

        let settings = EncoderSettings {
            application: application_name(
                encoder
                    .get_application()
                    .map_err(property("OPUS_GET_APPLICATION"))?,
            ),
            bitrate_bps: encoder
                .get_bitrate()
                .map_err(property("OPUS_GET_BITRATE"))?,
            vbr: encoder.get_vbr().map_err(property("OPUS_GET_VBR"))?,
            vbr_constrained: encoder
                .get_vbr_constraint()
                .map_err(property("OPUS_GET_VBR_CONSTRAINT"))?,
            dtx: encoder.get_dtx().map_err(property("OPUS_GET_DTX"))?,
            inband_fec: encoder
                .get_inband_fec()
                .map_err(property("OPUS_GET_INBAND_FEC"))?,
            complexity: encoder
                .get_complexity()
                .map_err(property("OPUS_GET_COMPLEXITY"))?,
            lookahead: encoder
                .get_lookahead()
                .map_err(property("OPUS_GET_LOOKAHEAD"))?,
        };

        Ok(OpusEncoder {
            encoder,
            config,
            settings,
            packet: vec![0u8; MAX_PACKET_BYTES].into_boxed_slice(),
        })
    }

    pub fn config(&self) -> &CodecConfig {
        &self.config
    }

    pub fn settings(&self) -> &EncoderSettings {
        &self.settings
    }

    /// Interleaved samples one call expects, which is also the only length it
    /// accepts.
    pub fn frame_interleaved(&self) -> usize {
        self.config.frame_interleaved()
    }

    /// Encodes exactly one frame and answers the packet, borrowed from the
    /// buffer this encoder owns.
    ///
    /// Borrowed rather than returned by value so that a caller who only wants
    /// to measure the packet never copies it, and a caller who wants to keep it
    /// has to say so. Nothing is allocated, nothing is logged and nothing is
    /// locked.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8], CodecError> {
        let expected = self.config.frame_interleaved();
        if pcm.len() != expected {
            return Err(CodecError::FrameLength {
                submitted: pcm.len(),
                expected,
            });
        }
        let written = self
            .encoder
            .encode_float(pcm, &mut self.packet)
            .map_err(CodecError::Encode)?;
        Ok(&self.packet[..written])
    }
}

fn channels(count: u16) -> Result<Channels, CodecError> {
    match count {
        1 => Ok(Channels::Mono),
        2 => Ok(Channels::Stereo),
        other => Err(CodecError::UnsupportedChannels(other)),
    }
}

fn application_name(application: Application) -> &'static str {
    match application {
        Application::Voip => "OPUS_APPLICATION_VOIP",
        Application::Audio => "OPUS_APPLICATION_AUDIO",
        Application::LowDelay => "OPUS_APPLICATION_RESTRICTED_LOWDELAY",
        // Unreachable for the three modes this crate sets, and named rather
        // than folded into one of them so that an encoder in a mode nobody
        // asked for says so in the report instead of being described as the one
        // it was meant to be in.
        Application::Unnamed(_) => "an application this encoder did not ask for",
    }
}

/// Turns a CTL failure into ours while naming the CTL, because an Opus error
/// code without the request that produced it is a number nobody can act on.
fn property(name: &'static str) -> impl FnOnce(ErrorCode) -> CodecError {
    move |code| CodecError::Property { name, code }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FrameDuration;

    fn contract(frame: FrameDuration) -> CodecConfig {
        CodecConfig::contract(frame, CodecConfig::DEFAULT_BITRATE_BPS)
    }

    #[test]
    fn the_encoder_reports_the_settings_it_was_given() {
        let encoder = OpusEncoder::new(contract(FrameDuration::Ms5)).expect("encoder");
        let settings = *encoder.settings();
        assert_eq!(settings.application, "OPUS_APPLICATION_RESTRICTED_LOWDELAY");
        assert_eq!(settings.bitrate_bps, 128_000);
        assert!(settings.vbr);
        assert!(settings.vbr_constrained);
        assert!(!settings.dtx, "discontinuous transmission must be off");
        assert!(!settings.inband_fec, "in-band FEC must be off");
        assert!(settings.lookahead > 0, "a codec with no delay is a defect");
    }

    #[test]
    fn a_rate_opus_cannot_run_at_is_refused_rather_than_resampled() {
        let mut config = contract(FrameDuration::Ms10);
        config.sample_rate = 44_100;
        assert_eq!(
            OpusEncoder::new(config).err(),
            Some(CodecError::UnsupportedSampleRate(44_100))
        );
    }

    #[test]
    fn more_channels_than_the_single_stream_api_has_is_refused() {
        let mut config = contract(FrameDuration::Ms10);
        config.channels = 6;
        assert_eq!(
            OpusEncoder::new(config).err(),
            Some(CodecError::UnsupportedChannels(6))
        );
    }

    #[test]
    fn an_encoder_built_on_one_thread_encodes_on_another() {
        // The encoder state is `Send` and deliberately not `Sync`, which is the
        // pair of claims the host's shape depends on: it will be built where the
        // capture callback runs and used where the sender does, and it must
        // never be reachable from both at once. Nothing in this phase crosses
        // that boundary yet, so without this the claim would be checked for the
        // first time by whoever needed it.
        let config = contract(FrameDuration::Ms5);
        let mut encoder = OpusEncoder::new(config).expect("encoder");
        let pcm = vec![0f32; config.frame_interleaved()];

        let encoded = std::thread::spawn(move || encoder.encode(&pcm).map(<[u8]>::len))
            .join()
            .expect("the thread the encoder was moved to");
        assert!(
            encoded.expect("encode") > 0,
            "a frame that produced no bytes would pass a test that only moved the encoder"
        );
    }
}
