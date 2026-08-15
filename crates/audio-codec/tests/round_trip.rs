//! What has to be true of the codec before anything is built on top of it.
//!
//! Three of these defend a different half of the same claim, which is that the
//! packets carry the audio that went in. A byte count says a packet was
//! produced, a frame count says something came back, and neither can tell a
//! working codec from one returning silence; only the frequencies measured out
//! of the decoded samples can. This project has read the first two as evidence
//! for the third three times.
//!
//! The fourth defends the opposite property: that input the codec cannot
//! honestly handle is refused rather than quietly made to work. A wrapper that
//! padded a short buffer, or that accepted a buffer holding two frames and
//! encoded it as one longer one, would keep every count in this file agreeing
//! while measuring a frame duration nobody asked for.

use lanplay_audio_codec::probe::{self, Options};
use lanplay_audio_codec::{
    CodecConfig, CodecError, ErrorCode, FrameDuration, OpusDecoder, OpusEncoder,
};
use lanplay_tone_source::tone::{CONTRACT, Tone};

/// Long enough to fill the probe's analysis window past its warm-up skip.
const SECONDS: f64 = 1.0;

fn config(frame: FrameDuration) -> CodecConfig {
    CodecConfig::contract(frame, CodecConfig::DEFAULT_BITRATE_BPS)
}

fn one_frame(config: &CodecConfig) -> Vec<f32> {
    let mut tone = Tone::new(CONTRACT);
    let mut pcm = vec![0f32; config.frame_interleaved()];
    tone.fill_stereo(&mut pcm);
    pcm
}

#[test]
fn the_decoded_audio_is_the_contract_tone() {
    for frame in [FrameDuration::Ms5, FrameDuration::Ms10] {
        let measured = probe::run(Options {
            frame,
            seconds: SECONDS,
            bitrate_kbps: 128,
        })
        .expect("run");

        let left = measured
            .tone
            .left
            .expect("the left channel decoded to silence");
        let right = measured
            .tone
            .right
            .expect("the right channel decoded to silence");

        // A few hertz, against an analysis window whose own bin spacing is
        // 2 Hz. Anything looser would also pass for a decoder that returned the
        // wrong channel; anything tighter would be asserting the refinement
        // rather than the codec.
        assert!(
            (left.frequency - CONTRACT.left_hz).abs() < 3.0,
            "{} ms: left read {:.2} Hz, not {}",
            frame.millis(),
            left.frequency,
            CONTRACT.left_hz
        );
        assert!(
            (right.frequency - CONTRACT.right_hz).abs() < 3.0,
            "{} ms: right read {:.2} Hz, not {}",
            frame.millis(),
            right.frequency,
            CONTRACT.right_hz
        );
        assert!(
            measured.tone.distinct(),
            "{} ms: both channels read the same frequency, which is what one \
             channel decoded twice looks like",
            frame.millis()
        );
    }
}

#[test]
fn every_frame_submitted_comes_back() {
    for frame in [FrameDuration::Ms5, FrameDuration::Ms10] {
        let measured = probe::run(Options {
            frame,
            seconds: SECONDS,
            bitrate_kbps: 128,
        })
        .expect("run");

        assert_eq!(
            measured.frames_submitted,
            measured.frames_returned,
            "{} ms: {} frames went in and {} came back",
            frame.millis(),
            measured.frames_submitted,
            measured.frames_returned
        );
        assert_eq!(
            measured.frames_submitted,
            measured.packets * frame.samples_per_channel(48_000) as u64
        );
        assert!(measured.packets > 0);
        assert!(
            measured.total_packet_bytes > 0,
            "a run with no bytes in it would satisfy every count above"
        );
    }
}

#[test]
fn a_packet_decodes_to_exactly_one_frame() {
    for frame in FrameDuration::ALL {
        let config = config(frame);
        let mut encoder = OpusEncoder::new(config).expect("encoder");
        let mut decoder = OpusDecoder::new(config).expect("decoder");

        let packet = encoder
            .encode(&one_frame(&config))
            .expect("encode")
            .to_vec();
        assert!(!packet.is_empty());

        let decoded = decoder.decode(&packet).expect("decode");
        assert_eq!(
            decoded.len(),
            config.frame_interleaved(),
            "{} ms: a packet decoded to {} interleaved samples",
            frame.millis(),
            decoded.len()
        );
    }
}

#[test]
fn wrong_input_is_refused_rather_than_repaired() {
    let config = config(FrameDuration::Ms5);
    let mut encoder = OpusEncoder::new(config).expect("encoder");
    let mut decoder = OpusDecoder::new(config).expect("decoder");
    let frame = one_frame(&config);

    // One sample short. Padding it would encode a frame whose last sample the
    // caller never provided.
    assert_eq!(
        encoder.encode(&frame[..frame.len() - 1]).err(),
        Some(CodecError::FrameLength {
            submitted: frame.len() - 1,
            expected: frame.len(),
        })
    );

    // Two frames' worth, which is the dangerous one: 480 samples per channel at
    // 48 kHz is a legal Opus frame of 10 ms, so libopus would encode it without
    // complaint and every byte count afterwards would describe a frame duration
    // nobody configured.
    let doubled: Vec<f32> = frame.iter().chain(frame.iter()).copied().collect();
    assert_eq!(
        encoder.encode(&doubled).err(),
        Some(CodecError::FrameLength {
            submitted: frame.len() * 2,
            expected: frame.len(),
        })
    );

    // A packet with no bytes is libopus's way of asking for loss concealment,
    // which would invent audio and report it as decoded.
    assert_eq!(decoder.decode(&[]).err(), Some(CodecError::EmptyPacket));

    // A packet that is not one. The first byte of an Opus packet is the table
    // of contents, and 0xFF asks for a code 3 packet whose frame count byte is
    // missing.
    match decoder.decode(&[0xFF]) {
        Err(CodecError::Decode(code)) => assert_eq!(code, ErrorCode::InvalidPacket),
        other => panic!("a truncated packet was accepted: {other:?}"),
    }

    // And the codec still works afterwards, which is what makes the four above
    // refusals rather than damage.
    let packet = encoder.encode(&frame).expect("encode");
    assert_eq!(
        decoder.decode(packet).expect("decode").len(),
        config.frame_interleaved()
    );
}
