//! Plays the contract tone out of this machine's default output device and
//! reports what the device did with it.
//!
//! The exit code carries the finding, because a gate reads that before it reads
//! anything else. A run whose callback never fired exits 3 and says so in
//! words: its underrun count is zero, its overrun count is zero and its
//! occupancy distribution is absent, and every one of those is what a perfect
//! run looks like to a parser that is not paying attention. A run that fired
//! and was starved exits 4, because silence sent to a device in place of audio
//! is the failure this whole phase exists to detect. Everything else — a device
//! at the wrong rate, a buffer size the device would not take — is a finding
//! printed in full and an exit of zero, since those are answers and not faults.

use std::process::ExitCode;

use clap::Parser;
use lanplay_audio_render::{Options, Verdict, run};

#[derive(Parser)]
#[command(
    about = "Renders the 997/1997 Hz contract tone through a CoreAudio IOProc and measures the \
             output path"
)]
struct Args {
    /// How long to render for. Five minutes is what the plan asks for: a
    /// shorter run cannot show a drift in the ring's occupancy.
    #[arg(long, default_value_t = 300.0)]
    seconds: f64,

    /// Frames per IO cycle to ask the device for. A request and not a setting;
    /// the report prints what the device actually granted.
    #[arg(long, default_value_t = 256)]
    buffer_frames: u32,

    /// How many IO buffers the ring holds. Four is the default because two is
    /// the theoretical floor — one buffer being drained and one ready — and two
    /// more give the producer a whole missed wake-up of slack without pushing
    /// the ring past a couple of dozen milliseconds of audio.
    #[arg(long, default_value_t = 4)]
    ring_multiple: u32,
}

fn main() -> ExitCode {
    let args = Args::parse();
    if args.seconds <= 0.0 {
        eprintln!("--seconds must be positive; a run of no seconds measures nothing");
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

    let report = match run(Options {
        seconds: args.seconds,
        buffer_frames: args.buffer_frames,
        ring_multiple: args.ring_multiple,
    }) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };

    print!("{report}");
    match report.verdict() {
        Verdict::Silent => ExitCode::from(3),
        Verdict::Underran => ExitCode::from(4),
        Verdict::Rendered => ExitCode::SUCCESS,
    }
}
