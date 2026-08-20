//! Takes the host's Opus stream off the wire and plays it out of this Mac.
//!
//! The exit code carries the finding, because a gate reads that before it reads
//! anything else. A run that received nothing exits 3: its underrun count is
//! zero, its loss count is zero and its occupancy distribution is absent, and
//! every one of those is what a perfect run looks like to a reader that is not
//! paying attention. A run that received packets and decoded none of them exits
//! 4, because a concealer keeping a device fed from an empty buffer is the one
//! failure that looks like success in every other counter. Loss, lateness,
//! concealment and underruns are none of them reasons to exit non-zero: they
//! are the measurements this phase exists to take, and a probe that refused to
//! report its own result would be refusing to do its job. Whether the run
//! passed is `xtask verdict`'s decision, from the envelope.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use clap::Parser;
use lanplay_audio_codec::{CodecConfig, FrameDuration};
use lanplay_audio_render::receive::{ReceiveOptions, receive};
use lanplay_audio_render::receive_envelope;
use lanplay_telemetry::Nanos;

#[derive(Parser)]
#[command(
    name = "audio-e2e-receiver",
    version,
    about = "Receives the host's Opus stream over RTP and renders it through CoreAudio, \
             accounting for source concealment and playout continuity end to end"
)]
struct Args {
    /// Where to listen. The host sends here.
    #[arg(long, default_value = "0.0.0.0:5012")]
    bind: SocketAddr,

    /// Which output device to render through, by the name CoreAudio gives it.
    /// Absent means whatever the system default is when the run starts, which
    /// is how the A5 probe and anyone running this by hand work.
    ///
    /// A gate names one. The default is a system-wide setting that a pair of
    /// headphones reconnecting changes without anybody touching it, and a run
    /// that inherits it finds out that the endpoint mixes at 44100 Hz half a
    /// minute after the measurement started rather than before it.
    #[arg(long, value_name = "NAME")]
    device: Option<String>,

    /// Seconds of audio to account for, counted from the first datagram rather
    /// than from process start. The plan asks for 60 first and then 600.
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,

    /// Frame duration in milliseconds. Opus permits 5, 10, 20, 40 and 60, and
    /// the wire contract is 5.
    #[arg(long, default_value_t = 5)]
    frame_ms: u32,

    /// Audio the jitter buffer aims to hold. Ten milliseconds is what the
    /// earlier phase settled on; A8 is where it gets chosen properly.
    #[arg(long, default_value_t = 10)]
    target_ms: u64,

    /// Frames per IO cycle to ask the device for. A request and not a setting;
    /// the report prints what the device granted.
    #[arg(long, default_value_t = 256)]
    buffer_frames: u32,

    /// How many IO buffers the ring holds. Sixteen rather than the tone
    /// probe's four, and the extra twelve are headroom rather than latency.
    /// This ring rests at the prime plus however long `AudioDeviceStart` took,
    /// which measured between 10.7 and 35.1 ms across runs on one device and
    /// is not something the receiver gets to choose; above that it has to
    /// hold the twenty parts per million between the two audio clocks, which
    /// is 576 frames over the ten minutes the plan asks for. A ring with less
    /// than both starts refusing audio late in a long run for reasons that
    /// have nothing to do with the link.
    #[arg(long, default_value_t = 16)]
    ring_multiple: u32,

    /// How long to wait for a first datagram. Generous by default, because the
    /// host end is started through a scheduled task and takes several seconds
    /// to appear.
    #[arg(long, default_value_t = 30.0)]
    first_packet_wait: f64,

    /// How often to close a counter window.
    #[arg(long, default_value_t = 10.0)]
    window_s: f64,

    /// Also write the gate envelope here, for `xtask verdict` to decide on.
    #[arg(long, value_name = "PATH")]
    envelope: Option<PathBuf>,

    /// Which arm this run is. Mandatory in the document even when a gate has
    /// one arm, because a gate with a negative control has two and a result
    /// filed under neither is a result nobody can place.
    #[arg(long, default_value = "windows to mac")]
    arm: String,

    /// The commit the envelope records, stated by whoever invoked this.
    #[arg(long, value_name = "HASH")]
    commit: Option<String>,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let Some(frame) = FrameDuration::from_millis(args.frame_ms) else {
        eprintln!(
            "--frame-ms must be one of 5, 10, 20, 40 or 60; Opus codes no other duration and a \
             receiver told to expect one would conceal every frame of the run"
        );
        return ExitCode::from(2);
    };
    if args.seconds <= 0.0 {
        eprintln!("--seconds must be positive; a run of no seconds measures nothing");
        return ExitCode::from(2);
    }
    if args.window_s <= 0.0 {
        eprintln!("--window-s must be positive");
        return ExitCode::from(2);
    }
    if args.buffer_frames == 0 {
        eprintln!("--buffer-frames must be positive");
        return ExitCode::from(2);
    }
    if args.ring_multiple < 2 {
        eprintln!(
            "--ring-multiple must be at least 2: one buffer is being drained by the callback \
             while the next is being filled, and a ring smaller than that underruns by \
             construction"
        );
        return ExitCode::from(2);
    }

    let started = SystemTime::now();
    let receipt = match receive(ReceiveOptions {
        bind: args.bind,
        device: args.device.clone(),
        seconds: args.seconds,
        config: CodecConfig::contract(frame, CodecConfig::DEFAULT_BITRATE_BPS),
        target: Nanos::from_millis(args.target_ms),
        buffer_frames: args.buffer_frames,
        ring_multiple: args.ring_multiple,
        first_packet_wait: Duration::from_secs_f64(args.first_packet_wait),
        window: Duration::from_secs_f64(args.window_s),
    }) {
        Ok(receipt) => receipt,
        Err(error) => {
            eprintln!("audio-e2e-receiver: {error}");
            // A run that never heard the stream is told apart from a machine
            // that could not serve it at all, because the two send their reader
            // to opposite ends of the lab.
            let nothing_arrived = error.to_string().starts_with("nothing arrived");
            // Said only where the reader has a lever, which is a refusal about
            // the endpoint rather than about the stream. Seventeen minutes of a
            // measurement went into a message that named the device and not the
            // fact that nothing had chosen it.
            if !nothing_arrived && args.device.is_none() {
                eprintln!(
                    "audio-e2e-receiver: nothing named a device, so this run took the system \
                     default; --device names one and refuses before a run rather than during it"
                );
            }
            return ExitCode::from(if nothing_arrived { 3 } else { 2 });
        }
    };

    print!("{receipt}");

    if let Some(path) = &args.envelope {
        let document =
            receive_envelope::document(&receipt, started, &args.arm, args.commit.as_deref());
        if let Err(error) = std::fs::write(path, document) {
            eprintln!(
                "audio-e2e-receiver: the run finished and its envelope could not be written to \
                 {}: {error}",
                path.display()
            );
            return ExitCode::from(2);
        }
    }

    if receipt.counts.received == 0 {
        eprintln!(
            "audio-e2e-receiver: no packets arrived, so every figure above describes an \
             experiment that did not happen"
        );
        return ExitCode::from(3);
    }
    if receipt.counts.played == 0 {
        eprintln!(
            "audio-e2e-receiver: every frame the device was handed came from the concealer, so \
             the counters describe a buffer keeping a device fed rather than a path carrying \
             audio"
        );
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}
