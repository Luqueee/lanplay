//! Repository automation that is too long-winded to keep in a shell script.
//!
//! One subcommand today: `gate-1c`, the clean display baseline. It drives a
//! Windows sender over SSH into the macOS client on this machine, and its
//! whole reason to exist is to refuse to report a number it cannot trust.

mod preflight;
mod report;
mod run;

use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

/// Something went wrong before, or instead of, a measurement. Distinct from a
/// gate failure: a gate failure is a result, this is the absence of one.
pub struct Abort(pub String);

impl Abort {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[derive(Parser)]
#[command(name = "xtask", about = "lanplay repository automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Clean display baseline: Windows -> Wi-Fi -> macOS, measured end to end.
    #[command(name = "gate-1c")]
    Gate1c(Box<Gate1c>),
}

#[derive(Args)]
pub struct Gate1c {
    /// How long the sender runs. Ten minutes is the baseline; shorter runs
    /// prove the plumbing but not the drift.
    #[arg(long, default_value_t = 600.0)]
    pub seconds: f64,
    /// Fixture rate, and the rate the sender paces at.
    #[arg(long, default_value_t = 120)]
    pub fps: u32,
    /// SSH destination of the sending machine.
    #[arg(long, default_value = "windows")]
    pub host: String,
    /// Address the sender aims at, i.e. this machine on the Wi-Fi.
    #[arg(long, default_value = "192.168.1.108")]
    pub client_addr: String,
    #[arg(long, default_value_t = 5004)]
    pub port: u16,
    /// Carry on after a failed preflight item. The numbers from a run whose
    /// environment was already wrong are not comparable with a clean one, so
    /// this is for debugging the harness, never for producing a baseline.
    #[arg(long)]
    pub keep_going: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Gate1c(args) => match run::gate_1c(&args) {
            Ok(true) => ExitCode::SUCCESS,
            Ok(false) => ExitCode::from(1),
            Err(Abort(why)) => {
                eprintln!("gate-1c: {why}");
                ExitCode::from(2)
            }
        },
    }
}
