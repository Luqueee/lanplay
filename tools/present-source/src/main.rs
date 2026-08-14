use std::process::ExitCode;

use clap::Parser;

/// A synthetic full-screen producer for the capture benchmark.
///
/// Presents a picture that is a function of the frame index, at a rate held in
/// software, so a capture backend can be measured against continuous,
/// reproducible motion instead of an idle desktop.
#[derive(Parser, Debug)]
#[command(name = "present-source", version, about, long_about = None)]
struct Args {
    #[arg(long, default_value_t = 1920, value_parser = clap::value_parser!(u32).range(16..=16384))]
    width: u32,

    #[arg(long, default_value_t = 1080, value_parser = clap::value_parser!(u32).range(16..=16384))]
    height: u32,

    /// Presents per second. Not clamped to the refresh rate: outrunning the
    /// panel is a case the capture backends have to be measured against.
    #[arg(long, default_value_t = 120, value_parser = clap::value_parser!(u32).range(1..=1000))]
    fps: u32,

    /// How long to run. 0 runs until the window is closed.
    #[arg(long, default_value_t = 0)]
    seconds: u64,

    /// Cover the whole monitor with a borderless window.
    #[arg(long)]
    fullscreen: bool,

    /// Which monitor to present on, as a DXGI output index.
    #[arg(long, default_value_t = 0)]
    monitor: u32,
}

#[cfg(windows)]
fn main() -> ExitCode {
    use lanplay_present_source::present::{self, Options};

    let args = Args::parse();
    let options = Options {
        width: args.width,
        height: args.height,
        fps: args.fps,
        seconds: args.seconds,
        fullscreen: args.fullscreen,
        monitor: args.monitor,
    };

    match present::run(options) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("present-source: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The producer is a D3D11 swap chain driving a Win32 window; there is nothing
/// off Windows for it to present into. The binary still builds so the
/// workspace does, and its pacing arithmetic is testable anywhere.
#[cfg(not(windows))]
fn main() -> ExitCode {
    let _ = Args::parse();
    eprintln!("present-source: needs Windows; there is no D3D11 swap chain to present into here.");
    ExitCode::FAILURE
}
