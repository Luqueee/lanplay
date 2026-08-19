use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Instant, SystemTime};

use clap::Parser;
use lanplay_audio_codec::FrameDuration;
use lanplay_audio_codec::rtp_envelope::{self, Arm};
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

    /// Also write the gate envelope here, for `xtask verdict` to decide on.
    ///
    /// Additional rather than instead of: the keyed block on stdout is what a
    /// person reads when a gate fails, and the document is what an evaluator
    /// reads, and neither audience is served by being handed the other's form.
    #[arg(long, value_name = "PATH", requires = "arm")]
    envelope: Option<PathBuf>,

    /// Which arm of the gate this run is, which decides what the document it
    /// writes is in a position to claim.
    ///
    /// Required with `--envelope` and refused without a default, because it is
    /// the one property of a run this process cannot observe: a receive-only
    /// run cannot tell a relay on 127.0.0.1 from a peer across the air, and
    /// whether a lost packet is a defect or the measurement turns on exactly
    /// that.
    #[arg(long, value_name = "ARM")]
    arm: Option<Arm>,

    /// The seed the faults on this path were injected with, for the arm that
    /// has any. A run whose faults nobody can reproduce is a run nobody can
    /// re-run, and re-running one is how the first negative control here was
    /// caught firing on a misdirected relay.
    #[arg(long, value_name = "N")]
    seed: Option<u64>,

    /// The commit the envelope records, stated by whoever invoked this rather
    /// than read from git: a probe that shells out to git prints a wrong hash
    /// in a checkout that is not a repository, and a wrong provenance is worse
    /// than an absent one.
    #[arg(long, value_name = "HASH")]
    commit: Option<String>,
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

    let options = Options {
        bind: args.bind,
        send_to: args.send_to,
        seconds: args.seconds,
        frame,
    };
    // The wall clock says when, and the monotonic clock says how long. Taking
    // the span from two readings of the wall clock would fold anything that
    // adjusted it into the measurement.
    let started = SystemTime::now();
    let began = Instant::now();
    match rtp_probe::run(options) {
        Ok(measurement) => {
            let span_s = began.elapsed().as_secs_f64();
            print!("{measurement}");
            // Written before the defect below is decided, because a run that
            // could not be believed is exactly the run whose numbers somebody
            // needs to read, and an evaluator with no document cannot say even
            // that much.
            if let (Some(path), Some(arm)) = (&args.envelope, args.arm) {
                let document = rtp_envelope::document(
                    &measurement,
                    options,
                    arm,
                    started,
                    span_s,
                    args.seed,
                    args.commit.as_deref(),
                );
                if let Err(error) = fs::write(path, document) {
                    // Louder than the defect below, because a gate whose
                    // evaluator has no document to read cannot decide anything
                    // at all, and a silent absence there reads as a gate that
                    // was never run.
                    eprintln!(
                        "audio-rtp-probe: could not write the envelope to {}: {error}",
                        path.display()
                    );
                    return ExitCode::FAILURE;
                }
            }
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
