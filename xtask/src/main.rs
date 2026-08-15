//! Repository automation that is too long-winded to keep in a shell script.
//!
//! Four things live here. `gate-1c` is the clean display baseline: it drives a
//! Windows sender over SSH into the macOS client on this machine, and its
//! whole reason to exist is to refuse to report a number it cannot trust.
//! `gates` answers the question that costs the most time when working
//! unattended - which harnesses can run right now - out of `tools/gates.toml`
//! and out of what the machine can be seen to have, rather than out of
//! somebody rereading eighteen shell scripts. `verdict` reads the envelope a
//! probe emitted and decides, so that no harness parses another program's prose
//! ever again, and answers in the three the harnesses here already exchange:
//! held, did not hold, and could not be read, the last of which is a refusal
//! and not a pass. `platforms` checks the Windows job's exclude list against
//! what each crate declares it supports, which is what the workflow's comment
//! about failing loudly claimed and, until a macOS-only crate arrived there as
//! an error about a missing module, did not have.

mod envelope;
mod environment;
mod gates;
mod platforms;
mod preflight;
mod report;
mod run;
mod verdict;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::verdict::Verdict;

/// Something went wrong before, or instead of, a measurement. Distinct from a
/// gate failure: a gate failure is a result, this is the absence of one.
///
/// `Debug` so that a test unwrapping one shows what it said rather than the
/// name of the type.
#[derive(Debug)]
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
    /// What every harness proves, what it needs, and what can run right now.
    Gates(GatesArgs),
    /// The windows job's exclude list against what each crate says it supports.
    Platforms,
    /// Decide one gate run from the envelope its probe emitted.
    Verdict(VerdictArgs),
}

#[derive(Args)]
pub struct GatesArgs {
    /// Detect what this machine and the host can satisfy, and separate the
    /// gates that can run now from the ones that cannot, with the requirement
    /// that excluded each.
    #[arg(long)]
    pub runnable: bool,
    /// Only the gates whose negative control has never been observed.
    #[arg(long)]
    pub debt: bool,
    /// One JSON document, for a reader that is not a person.
    #[arg(long)]
    pub json: bool,
    /// SSH destination of the lab host.
    #[arg(long, default_value = "windows")]
    pub host: String,
}

#[derive(Args)]
pub struct VerdictArgs {
    /// The JSON document one arm of one gate produced.
    pub envelope: PathBuf,
    /// State one observation and decide nothing. A harness that has to compute
    /// a figure across two arms needs the numbers out of both, and every read
    /// of an envelope goes through the one parser that refuses a name it cannot
    /// find rather than through a shell that would print an empty string.
    #[arg(long, value_name = "NAME")]
    pub observation: Option<String>,
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
        Command::Gates(args) => match list_gates(&args) {
            Ok(listing) => {
                print!("{listing}");
                ExitCode::SUCCESS
            }
            Err(Abort(why)) => {
                eprintln!("gates: {why}");
                ExitCode::from(2)
            }
        },
        Command::Platforms => match platforms::audit(&platforms::default_workflow()) {
            Ok((report, agreed)) => {
                print!("{report}");
                if agreed {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                }
            }
            Err(Abort(why)) => {
                eprintln!("platforms: {why}");
                ExitCode::from(2)
            }
        },
        Command::Verdict(args) => match decide(&args) {
            Ok((text, answer)) => {
                print!("{text}");
                match answer {
                    Verdict::Passed => ExitCode::SUCCESS,
                    Verdict::Failed => ExitCode::from(1),
                    // The same two as an unreadable document below, and
                    // deliberately the same: a criterion nobody could evaluate
                    // and a document nobody could parse are one absence of a
                    // result, and a caller has nothing to gain from telling
                    // them apart.
                    Verdict::Refused => ExitCode::from(2),
                }
            }
            Err(Abort(why)) => {
                eprintln!("verdict: {why}");
                ExitCode::from(2)
            }
        },
    }
}

/// Reading the index and detecting the environment are two separate steps on
/// purpose, and the second one only happens when something asked for it: a
/// plain listing must not pay five seconds for a host nobody enquired about.
fn list_gates(args: &GatesArgs) -> Result<String, Abort> {
    let index = gates::Index::load(&gates::Index::default_path())?;
    let detected = args
        .runnable
        .then(|| environment::detect(&args.host, &index.requirements()));
    let selection = gates::Selection { debt: args.debt };
    Ok(if args.json {
        let mut document = gates::json(&index, &selection, detected.as_ref());
        document.push('\n');
        document
    } else {
        gates::human(&index, &selection, detected.as_ref())
    })
}

/// Reading one envelope, and either deciding it or stating one of its numbers.
/// Stating a number is not a verdict, so it prints the number alone and says
/// nothing about the run.
fn decide(args: &VerdictArgs) -> Result<(String, Verdict), Abort> {
    let envelope = envelope::Envelope::load(&args.envelope)?;
    if let Some(name) = &args.observation {
        let value = envelope.observation(name).ok_or_else(|| {
            Abort::new(format!(
                "{} reports no observation named {name}, and a harness computing a figure from a \
                 name that is not there is the defect this envelope exists to retire",
                args.envelope.display()
            ))
        })?;
        return Ok((format!("{value}\n"), Verdict::Passed));
    }
    Ok(verdict::report(&envelope))
}
