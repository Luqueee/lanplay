use std::process::ExitCode;

use clap::Parser;

/// A WASAPI render source for the loopback measurement.
///
/// Plays the batch's contract tone — 48000 Hz stereo, 997 Hz left, 1997 Hz
/// right, at -20 dBFS — into the default render endpoint in shared mode, so a
/// loopback capture has real audio to find instead of silence. The tone itself
/// takes no arguments: the capture side asserts those numbers, and a flag that
/// changed them would turn a disagreement between the two halves into a pass.
#[derive(Parser, Debug)]
#[command(name = "tone-source", version, about, long_about = None)]
struct Args {
    /// How long to play. 0 runs until the console interrupts it.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64).range(0..=86_400))]
    seconds: u64,
}

#[cfg(windows)]
fn main() -> ExitCode {
    use lanplay_tone_source::render::{self, Options};

    let args = Args::parse();

    match render::run(Options {
        seconds: args.seconds,
    }) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tone-source: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The source is a WASAPI render client; there is no endpoint off Windows for it
/// to play into. The binary still builds so the workspace does, and the tone
/// generator it would have played is testable anywhere.
#[cfg(not(windows))]
fn main() -> ExitCode {
    let _ = Args::parse();
    eprintln!("tone-source: needs Windows; there is no WASAPI endpoint to render into here.");
    ExitCode::FAILURE
}
