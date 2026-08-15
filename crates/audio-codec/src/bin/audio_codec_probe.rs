use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Instant, SystemTime};

use clap::Parser;
use lanplay_audio_codec::FrameDuration;
use lanplay_audio_codec::envelope;
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

    /// Also write the gate envelope here, for `xtask verdict` to decide on.
    ///
    /// Additional rather than instead of: the keyed block on stdout is what a
    /// person reads when a gate fails, and the document is what an evaluator
    /// reads, and neither audience is served by being handed the other's form.
    #[arg(long, value_name = "PATH")]
    envelope: Option<PathBuf>,

    /// The commit the envelope records, stated by whoever invoked this rather
    /// than read from git: a probe that shells out to git prints a wrong hash
    /// in a checkout that is not a repository, and a wrong provenance is worse
    /// than an absent one.
    #[arg(long, value_name = "HASH")]
    commit: Option<String>,

    /// Feed the encoder the contract tone with its two channels exchanged.
    ///
    /// The gate's negative control. The run is otherwise identical and the
    /// document it writes states the same criteria, so the two frequency
    /// criteria must disagree with it and the rest must not: an arm that also
    /// lost its packets or its sample count would be evidence about the
    /// harness rather than about the criteria.
    #[arg(long)]
    swap_tone_channels: bool,
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

    let options = Options {
        frame,
        seconds: args.seconds,
        bitrate_kbps: args.bitrate_kbps,
        swap_tone_channels: args.swap_tone_channels,
    };
    // The wall clock says when, and the monotonic clock says how long. Taking
    // the span from two readings of the wall clock would fold anything that
    // adjusted it into the measurement.
    let started = SystemTime::now();
    let began = Instant::now();
    match probe::run(options) {
        Ok(measurement) => {
            let span_s = began.elapsed().as_secs_f64();
            print!("{measurement}");
            if let Some(path) = &args.envelope {
                let document = envelope::document(
                    &measurement,
                    options,
                    started,
                    span_s,
                    args.commit.as_deref(),
                );
                if let Err(error) = fs::write(path, document) {
                    // Louder than the frame counts below, because a gate whose
                    // evaluator has no document to read cannot decide anything
                    // at all, and a silent absence there reads as a gate that
                    // was never run.
                    eprintln!(
                        "audio-codec-probe: could not write the envelope to {}: {error}",
                        path.display()
                    );
                    return ExitCode::FAILURE;
                }
            }
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
