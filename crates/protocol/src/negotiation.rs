use crate::{VideoCodec, VideoMode};

/// Capabilities advertised by one session endpoint.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CapabilitySet {
    pub codecs: Vec<VideoCodec>,
    pub modes: Vec<VideoMode>,
    pub audio_sample_rates: Vec<u32>,
    pub audio_channels: Vec<u16>,
    pub gamepad: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CapabilitySelection {
    pub codec: VideoCodec,
    pub mode: VideoMode,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
    pub gamepad: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NegotiationError {
    NoCommonCodec,
    NoCommonMode,
    NoCommonAudioSampleRate,
    NoCommonAudioChannels,
}

/// Selects only values both endpoints explicitly advertised.
///
/// H.264 wins over newer codecs when both are available because it is the
/// product baseline. Within that codec, the highest common mode and audio
/// format are selected deterministically; no fallback is invented.
pub fn negotiate(
    client: &CapabilitySet,
    host: &CapabilitySet,
) -> Result<CapabilitySelection, NegotiationError> {
    let codec = [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1]
        .into_iter()
        .find(|codec| client.codecs.contains(codec) && host.codecs.contains(codec))
        .ok_or(NegotiationError::NoCommonCodec)?;
    let mode = client
        .modes
        .iter()
        .copied()
        .filter(|mode| host.modes.contains(mode))
        .max_by_key(|mode| mode.pixel_rate())
        .ok_or(NegotiationError::NoCommonMode)?;
    let audio_sample_rate = client
        .audio_sample_rates
        .iter()
        .copied()
        .filter(|rate| host.audio_sample_rates.contains(rate))
        .max()
        .ok_or(NegotiationError::NoCommonAudioSampleRate)?;
    let audio_channels = client
        .audio_channels
        .iter()
        .copied()
        .filter(|channels| host.audio_channels.contains(channels))
        .max()
        .ok_or(NegotiationError::NoCommonAudioChannels)?;
    Ok(CapabilitySelection {
        codec,
        mode,
        audio_sample_rate,
        audio_channels,
        gamepad: client.gamepad && host.gamepad,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> CapabilitySet {
        CapabilitySet {
            codecs: vec![VideoCodec::Hevc, VideoCodec::H264],
            modes: vec![
                VideoMode::new(1280, 720, 60_000),
                VideoMode::new(1920, 1080, 120_000),
            ],
            audio_sample_rates: vec![44_100, 48_000],
            audio_channels: vec![1, 2],
            gamepad: true,
        }
    }

    #[test]
    fn selects_only_common_values_with_baseline_precedence() {
        let selection = negotiate(&endpoint(), &endpoint()).expect("common capabilities");
        assert_eq!(selection.codec, VideoCodec::H264);
        assert_eq!(selection.mode, VideoMode::new(1920, 1080, 120_000));
        assert_eq!(selection.audio_sample_rate, 48_000);
        assert_eq!(selection.audio_channels, 2);
        assert!(selection.gamepad);
    }

    #[test]
    fn unsupported_codec_does_not_fallback_to_an_unadvertised_value() {
        let mut host = endpoint();
        host.codecs = vec![VideoCodec::Av1];
        assert_eq!(
            negotiate(&endpoint(), &host),
            Err(NegotiationError::NoCommonCodec)
        );
    }

    #[test]
    fn gamepad_is_optional_but_video_and_audio_are_not() {
        let mut host = endpoint();
        host.gamepad = false;
        let selection = negotiate(&endpoint(), &host).expect("media remains negotiable");
        assert!(!selection.gamepad);
    }
}
