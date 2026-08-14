use std::process::ExitCode;

use clap::Parser;
use lanplay_audio_codec::FrameDuration;
use lanplay_audio_codec::probe::{self, Options};

/// Opus in isolation: encode the contract tone, decode it back, and report what
/// it cost and what came out.
///
/// Nothing is captured and nothing is sent. The tone — 48000 Hz stereo, 997 Hz
/// left, 1997 Hz right, at -20 dBFS — is generated in memory at the rate the
/// lab's render endpoint already mixes at, so the run measures the codec and
/// not a conversion in front of it. Encode and decode are timed separately,
/// because the question is whether the encoder is irrelevant against a 5 ms
/// frame budget and a combined number cannot answer that.
#[derive(Parser, Debug)]
#[command(name = "audio-codec-probe", version, about, long_about = None)]
struct Args {
    /// Frame duration in milliseconds. Opus permits 5, 10, 20, 40 and 60.
    #[arg(long, default_value_t = 5)]
    frame_ms: u32,

    /// Seconds of audio to push through, per frame duration.
    #[arg(long, default_value_t = 5.0)]
    seconds: f64,

    /// Target handed to OPUS_SET_BITRATE. What comes out is measured, not
    /// assumed, and the two are printed next to each other.
    #[arg(long, default_value_t = 128)]
    bitrate_kbps: u32,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let Some(frame) = FrameDuration::from_millis(args.frame_ms) else {
        eprintln!(
            "audio-codec-probe: Opus frames are 5, 10, 20, 40 or 60 ms; {} ms is not one of them, \
             and rounding it would measure a frame duration nobody asked for.",
            args.frame_ms
        );
        return ExitCode::FAILURE;
    };

    match probe::run(Options {
        frame,
        seconds: args.seconds,
        bitrate_kbps: args.bitrate_kbps,
    }) {
        Ok(measurement) => {
            print!("{measurement}");
            // A run whose frame counts disagree has produced a report worth
            // reading and an exit code worth failing on, in that order: the
            // numbers are what say where the audio went.
            if measurement.frames_submitted == measurement.frames_returned {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("audio-codec-probe: {error}");
            ExitCode::FAILURE
        }
    }
}
