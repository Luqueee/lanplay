use std::net::SocketAddr;
use std::process::ExitCode;

use clap::Parser;
use lanplay_audio_codec::FrameDuration;
use lanplay_audio_codec::jitter_probe::{self, Options};
use lanplay_telemetry::Nanos;

/// The receiving audio path with no audio hardware in it: UDP in, RTP
/// reordering, a bounded jitter buffer, Opus decode with concealment, and a
/// synthetic sink that pulls on a clock the way a render callback will.
///
/// Both halves run here, and the sender uses a socket of its own. That is what
/// lets the run go through `tools/udp-fault`: point `--send-to` at the relay's
/// listening port and the relay's `--forward` at `--bind`, and the chain is
/// sender to relay to receiver, all on this machine.
///
/// Nothing plays. The proof that the path carried audio is the tone at the
/// bottom of the report — 997 Hz left, 1997 Hz right — because a count of
/// frames played cannot tell a working path from one playing concealment
/// forever.
#[derive(Parser, Debug)]
#[command(name = "audio-jitter-probe", version, about, long_about = None)]
struct Args {
    /// Where the receiving half listens.
    #[arg(long, default_value = "127.0.0.1:5010")]
    bind: SocketAddr,

    /// Where the packets go: the receiving port for a direct run, or the fault
    /// relay's listening port for a run behind one.
    #[arg(long, default_value = "127.0.0.1:5010")]
    send_to: SocketAddr,

    /// Seconds of audio to send.
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,

    /// Frame duration in milliseconds. Opus permits 5, 10, 20, 40 and 60.
    #[arg(long, default_value_t = 5)]
    frame_ms: u32,

    /// Audio the buffer aims to hold, in milliseconds.
    ///
    /// The plan's baseline is 10 ms, about two 5 ms frames, and it is an
    /// experimental variable rather than a constant: this flag is how the
    /// experiment is run. Quantised to whole frames, and the report says what
    /// it became.
    #[arg(long, default_value_t = 10)]
    target_ms: u64,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let Some(frame) = FrameDuration::from_millis(args.frame_ms) else {
        eprintln!(
            "audio-jitter-probe: Opus frames are 5, 10, 20, 40 or 60 ms; {} ms is not one of \
             them, and rounding it would measure a frame duration nobody asked for.",
            args.frame_ms
        );
        return ExitCode::FAILURE;
    };

    match jitter_probe::run(Options {
        bind: args.bind,
        send_to: args.send_to,
        seconds: args.seconds,
        frame,
        target: Nanos::from_millis(args.target_ms),
    }) {
        Ok(measurement) => {
            print!("{measurement}");
            // Loss, lateness, concealment and underruns are the measurements
            // this phase exists to take, so none of them fails the run. What
            // fails it is a report that cannot be believed, named on stderr
            // after the figures rather than instead of them.
            match measurement.defect() {
                None => ExitCode::SUCCESS,
                Some(defect) => {
                    eprintln!("audio-jitter-probe: {defect}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("audio-jitter-probe: {error}");
            ExitCode::FAILURE
        }
    }
}
