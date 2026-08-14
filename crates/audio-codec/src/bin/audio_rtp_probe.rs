use std::net::SocketAddr;
use std::process::ExitCode;

use clap::Parser;
use lanplay_audio_codec::FrameDuration;
use lanplay_audio_codec::rtp_probe::{self, Options};

/// Opus over RTP over UDP: encode the contract tone, packetise it, send it,
/// receive it, decode it, and account for every packet.
///
/// With `--send-to` both halves run here, which is what makes byte-for-byte
/// verification possible: holding the digest of each payload only works while
/// one process holds both ends, and two machines could compare digests only by
/// sending them over the link they are measuring.
///
/// With `--receive-only` this end sends nothing and the peer is another machine.
/// That is the only way to measure what the air loses, because a datagram
/// addressed to this machine's own routable address never reaches the driver.
/// Byte verification then reports that it cannot be done rather than reporting
/// zero, and the decoded tone takes over the job of proving that what arrived
/// was the audio that was sent.
#[derive(Parser, Debug)]
#[command(name = "audio-rtp-probe", version, about, long_about = None)]
struct Args {
    /// Where the packets go. The port is usually the one `--bind` listens on,
    /// so the run receives its own stream.
    #[arg(long)]
    send_to: Option<SocketAddr>,

    /// Send nothing and account for a peer's stream instead.
    #[arg(long, conflicts_with = "send_to")]
    receive_only: bool,

    /// Where the receiving half listens.
    #[arg(long, default_value = "0.0.0.0:5008")]
    bind: SocketAddr,

    /// Seconds of audio to send, or to listen for. A receive-only run also stops
    /// two seconds after the stream goes quiet, because a peer that has stopped
    /// will not stop again.
    #[arg(long, default_value_t = 30.0)]
    seconds: f64,

    /// Frame duration in milliseconds. Opus permits 5, 10, 20, 40 and 60.
    #[arg(long, default_value_t = 5)]
    frame_ms: u32,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let Some(frame) = FrameDuration::from_millis(args.frame_ms) else {
        eprintln!(
            "audio-rtp-probe: Opus frames are 5, 10, 20, 40 or 60 ms; {} ms is not one of them, \
             and rounding it would measure a frame duration nobody asked for.",
            args.frame_ms
        );
        return ExitCode::FAILURE;
    };

    // Refused rather than defaulted: a run with neither flag would have to guess
    // whether the operator meant loopback or a peer, and either guess produces a
    // report of an experiment nobody asked for.
    if args.send_to.is_none() && !args.receive_only {
        eprintln!(
            "audio-rtp-probe: give --send-to to send and receive here, or --receive-only to \
             account for a peer's stream. Which one it is decides whether the payload bytes \
             can be verified at all."
        );
        return ExitCode::FAILURE;
    }

    match rtp_probe::run(Options {
        bind: args.bind,
        send_to: args.send_to,
        seconds: args.seconds,
        frame,
    }) {
        Ok(measurement) => {
            print!("{measurement}");
            // Loss is not a failure here: it is the number the phase exists to
            // produce, and a probe that exited non-zero on it would be refusing
            // to report its own measurement. Anything that makes the numbers
            // unbelievable is, and it is named on stderr after the report rather
            // than instead of it, because the figures are what say where the
            // audio went.
            match measurement.defect() {
                None => ExitCode::SUCCESS,
                Some(defect) => {
                    eprintln!("audio-rtp-probe: {defect}");
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => {
            eprintln!("audio-rtp-probe: {error}");
            ExitCode::FAILURE
        }
    }
}
