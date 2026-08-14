//! The loopback probe: one capture, one report, one exit code.
//!
//! Nothing else runs alongside it. There is no encoder, no socket and no second
//! thread, so the packet sizes and wakeup intervals it prints are the audio
//! stack's own and not a queue's somewhere downstream of it.
//!
//! The exit code is part of the answer rather than a formality. Zero means
//! frames arrived carrying something; four means no frames arrived at all;
//! five means frames arrived and every one of them was silence. A probe that
//! returned zero for the last two would let a gate pass a machine that captured
//! nothing, which has happened here twice already.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Exit code for a run that could not be set up at all.
#[cfg(windows)]
const BROKEN: u8 = 2;

/// Exit code for a run on a machine with no WASAPI.
#[cfg(not(windows))]
const NOT_WINDOWS: u8 = 3;

/// Exit code for a run that captured no frames.
#[cfg(windows)]
const NOTHING: u8 = 4;

/// Exit code for a run whose every packet was silence.
#[cfg(windows)]
const ONLY_SILENCE: u8 = 5;

#[derive(Parser)]
#[command(
    name = "audio-capture-probe",
    about = "Captures the default render endpoint in WASAPI loopback mode and reports what it \
             actually delivered: the mix format, the packet cadence, every frame accounted for \
             against the device position, and the dominant frequency of each channel.",
    after_help = "Exit codes: 0 captured audio, 2 could not start, 3 not a Windows machine, \
                  4 captured nothing, 5 captured only silence."
)]
struct Cli {
    /// How long to capture for, in seconds.
    #[arg(long, default_value_t = 5.0)]
    seconds: f64,

    /// Poll instead of waiting on the endpoint's event, at half the device
    /// period. Event-driven loopback is supported from Windows 10 1703 and is
    /// the default; this is here so the two can be compared on one host.
    #[arg(long)]
    poll: bool,

    /// Write the captured samples to a wav file, unconverted, for listening to.
    /// A correctness tool and not for timing runs: it copies every packet and
    /// holds the whole capture in memory, so the intervals a run with it
    /// reports are not the intervals of a run without it.
    #[arg(long, value_name = "PATH")]
    wav: Option<PathBuf>,
}

#[cfg(windows)]
pub fn main() -> ExitCode {
    use crate::capture::{Captured, Request};
    use crate::report::Verdict;

    let cli = Cli::parse();
    if !(cli.seconds > 0.0) {
        eprintln!("audio-capture-probe: --seconds must be a positive number of seconds");
        return ExitCode::from(BROKEN);
    }

    let request = Request {
        seconds: cli.seconds,
        force_poll: cli.poll,
        keep_pcm: cli.wav.is_some(),
    };
    let Captured { report, pcm } = match crate::capture::run(&request) {
        Ok(captured) => captured,
        Err(error) => {
            eprintln!("audio-capture-probe: {error}");
            return ExitCode::from(BROKEN);
        }
    };

    print!("{report}");

    // Written after the report, so a run that fills a disk still leaves the
    // measurement it made behind.
    if let Some(path) = &cli.wav {
        match crate::wav::write(path, &report.format, &pcm) {
            Ok(()) => println!("wrote {} bytes of samples to {}", pcm.len(), path.display()),
            Err(error) => {
                eprintln!(
                    "audio-capture-probe: cannot write {}: {error}",
                    path.display()
                );
                return ExitCode::from(BROKEN);
            }
        }
    }

    match report.verdict() {
        Verdict::Captured => ExitCode::SUCCESS,
        Verdict::OnlySilence => ExitCode::from(ONLY_SILENCE),
        Verdict::Nothing => ExitCode::from(NOTHING),
    }
}

/// Off Windows the arguments are still parsed, so that `--help` and a typo in a
/// flag behave the same everywhere, and the run is then refused. Refused rather
/// than faked: a probe that printed an empty report here would be indis-
/// tinguishable from one that ran against a silent endpoint.
#[cfg(not(windows))]
pub fn main() -> ExitCode {
    let _ = Cli::parse();
    eprintln!(
        "audio-capture-probe: WASAPI loopback capture exists only on Windows, and this machine \
         is not one. Run it on the host whose audio is to be measured."
    );
    ExitCode::from(NOT_WINDOWS)
}
