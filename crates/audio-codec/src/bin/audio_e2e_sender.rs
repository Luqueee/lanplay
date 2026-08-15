use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::Parser;
use lanplay_audio_codec::e2e_envelope;
use lanplay_audio_codec::e2e_sender::{self, Options};

/// The host half of A6: WASAPI loopback to Opus to RTP to UDP, in one thread.
///
/// Each 480-frame packet the endpoint delivers is exactly two 5 ms Opus frames,
/// so it is split and sent as two datagrams whose timestamp advances by 240
/// samples. There is no accumulator between the capture and the encoder, because
/// A1 measured that there is never anything for one to hold.
///
/// Runs on the host and nowhere else: loopback needs a WASAPI render endpoint,
/// and it needs the interactive session, since the session an SSH login lands in
/// has no audio endpoints to enumerate.
#[derive(Parser, Debug)]
#[command(name = "audio-e2e-sender", version, about, long_about = None)]
struct Args {
    /// Where the datagrams go, which is the receiving machine's address and
    /// port. Not this machine's own: a datagram addressed to a local interface
    /// never reaches the driver, so a run sent there measures loopback twice.
    #[arg(long, value_name = "ADDR:PORT")]
    send_to: SocketAddr,

    /// Which local address and port to send from. Port zero lets the kernel
    /// choose, which is what a sender wants: nothing replies to this stream.
    #[arg(long, default_value = "0.0.0.0:0")]
    bind: SocketAddr,

    /// Seconds to capture, encode and send.
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,

    /// Target handed to OPUS_SET_BITRATE. What the encoder produces is measured
    /// rather than assumed, and the two appear next to each other.
    #[arg(long, default_value_t = 128)]
    bitrate_kbps: u32,

    /// Also write the gate envelope here, for `xtask verdict` to decide on.
    ///
    /// Additional rather than instead of: the keyed block on stdout is what a
    /// person reads when a gate fails, and the document is what an evaluator
    /// reads, and neither audience is served by being handed the other's form.
    #[arg(long, value_name = "PATH")]
    envelope: Option<PathBuf>,

    /// Which arm of the gate this run is. Mandatory in the document even when a
    /// gate has one arm, because a gate with a negative control has two and a
    /// result filed under neither is a result nobody can place.
    #[arg(long, default_value = "radio")]
    arm: String,

    /// The commit the envelope records, stated by whoever invoked this rather
    /// than read from git: a wrong provenance is worse than an absent one.
    #[arg(long, value_name = "HASH")]
    commit: Option<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let options = Options {
        send_to: args.send_to,
        bind: args.bind,
        seconds: args.seconds,
        bitrate_kbps: args.bitrate_kbps,
    };
    // The wall clock says when the run began and the monotonic clock inside the
    // run says how long it lasted. Taking the span from two wall-clock readings
    // would fold anything that adjusted the clock into the measurement.
    let started = SystemTime::now();

    match e2e_sender::run(&options) {
        Ok(measurement) => {
            print!("{measurement}");
            if let Some(path) = &args.envelope {
                let document = e2e_envelope::document(
                    &measurement,
                    options,
                    started,
                    &args.arm,
                    args.commit.as_deref(),
                );
                if let Err(error) = fs::write(path, document) {
                    // Louder than the counters above, because a gate whose
                    // evaluator has no document to read cannot decide anything
                    // at all, and a silent absence there reads as a gate that
                    // was never run.
                    eprintln!(
                        "audio-e2e-sender: could not write the envelope to {}: {error}",
                        path.display()
                    );
                    return ExitCode::FAILURE;
                }
            }
            // The exit code carries the two conditions that make every number
            // above meaningless, and nothing else: the criteria live in the
            // document, where an evaluator judges them. A run that captured no
            // packets measured an idle endpoint, and one whose captured and
            // encoded sample counts disagree lost audio before the radio was
            // reached, which is the failure a receiver cannot distinguish from
            // loss on the air.
            if measurement.carried.totals.packets == 0 {
                eprintln!(
                    "audio-e2e-sender: the endpoint delivered no packets, so nothing here \
                     describes the link; loopback delivers nothing at all while the endpoint is \
                     idle, which means no source was playing"
                );
                return ExitCode::FAILURE;
            }
            if measurement.sample_disagreement() != 0 {
                eprintln!(
                    "audio-e2e-sender: {} samples captured against {} encoded, a disagreement of \
                     {}; the split is exact or it is not a split",
                    measurement.samples_captured(),
                    measurement.samples_encoded(),
                    measurement.sample_disagreement(),
                );
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("audio-e2e-sender: {error}");
            ExitCode::FAILURE
        }
    }
}
