//! Phase 3: which desktop capture API should this streamer use.
//!
//! Two levels, deliberately separate subcommands because they measure
//! different things. `native` asks what the API itself costs — acquire, mark,
//! release, nothing downstream. `handoff` asks what ownership costs on top of
//! that: a GPU copy into a texture we own, the source released as early as the
//! API allows, and the owned texture held for a configurable stand-in for the
//! encoder that does not exist yet.

// Off Windows the binary only refuses to run, but the logic modules stay
// compiled so their tests can run here: the gate, the stall classifier and the
// alternating scheduler are pure arithmetic and are the parts most worth
// testing on the machine doing the development. Their constants are then
// unreferenced in a non-test build on this platform, which is not a defect.
#![cfg_attr(not(windows), allow(dead_code))]
//!
//! `compare` runs the same scenario against both backends in alternating short
//! blocks, because a minute of one followed by a minute of the other measures
//! the APIs plus the difference between the first minute and the second.
//!
//! Neither level reports a verdict on latency. The gate at the end judges only
//! the things that are wrong at any value, because the numbers this phase
//! exists to discover cannot also be its pass criteria.

mod display;
mod gate;
mod gpu;
mod human;
mod output;
mod report;
mod run;
mod schedule;
mod seam;
mod series;
mod stall;
mod stats;

use core::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::output::Output;
use crate::schedule::BackendKind;

/// Owned textures to rotate through when nothing was asked for. Three is one
/// in the encoder, one being copied into and one spare, which is the shape
/// phase 4 will most likely want.
const DEFAULT_POOL: u32 = 3;
/// Stand-in for the encoder's hold, in milliseconds. Roughly one 100 Hz frame
/// period, so the default measures a downstream that keeps up.
const DEFAULT_HOLD_MS: f64 = 4.0;

#[derive(Parser)]
#[command(
    name = "capture-bench",
    about = "Windows desktop capture: WGC versus Desktop Duplication"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 3A: what the capture API itself costs. Acquire, mark, release.
    Native(Box<Scenario>),
    /// 3B: what owning the frame costs on top of that.
    Handoff(Box<Scenario>),
    /// Both backends, alternating blocks, side by side.
    Compare(Box<Comparison>),
}

#[derive(Args, Clone, Debug)]
struct Common {
    /// How long the measured window lasts, in seconds. For `compare` this is
    /// the total across every block of both backends.
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,
    /// Surfaces the API is asked to keep in flight. WGC only; Desktop
    /// Duplication has no such knob.
    #[arg(long, default_value_t = 2)]
    buffers: u32,
    /// Which output to capture.
    #[arg(long, default_value_t = 0)]
    output: u32,
    /// Time discarded before the measured window. Device creation, the first
    /// allocation and the driver's first path through the copy happen here and
    /// are reported separately.
    #[arg(long, default_value_t = 15.0)]
    warmup_seconds: f64,
    /// Write the full result as JSON.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Write the human block here as well as to stdout. Defaults to the report
    /// path with a .txt extension, because a capture run has no readable
    /// stdout when it is launched into the interactive session.
    #[arg(long)]
    log: Option<PathBuf>,
    /// Stop consuming for this long, halfway through the run, then resume.
    /// What each API does when the consumer falls behind.
    #[arg(long, default_value_t = 0)]
    stall_ms: u64,
    /// How long an acquire waits for a new frame.
    #[arg(long, default_value_t = 100)]
    acquire_timeout_ms: u32,
    /// The rate to judge the cadence against. Defaults to the output's
    /// measured mode; set this only when the producer is not running at the
    /// display's rate.
    #[arg(long)]
    source_hz: Option<f64>,
    /// Ask the API to composite the cursor. Off by default: a cursor is
    /// content and the two APIs composite it at different points.
    #[arg(long)]
    cursor: bool,
}

/// Options that only mean anything once the harness owns a texture.
#[derive(Args, Clone, Debug)]
struct Ownership {
    /// Owned textures to rotate through. Handoff only.
    #[arg(long)]
    pool: Option<u32>,
    /// How long to hold an owned texture before returning it, standing in for
    /// the encoder. Handoff only.
    #[arg(long)]
    hold_ms: Option<f64>,
}

#[derive(Args, Clone, Debug)]
struct Scenario {
    #[arg(long, value_enum)]
    backend: BackendKind,
    #[command(flatten)]
    common: Common,
    #[command(flatten)]
    ownership: Ownership,
}

#[derive(Args, Clone, Debug)]
struct Comparison {
    #[command(flatten)]
    common: Common,
    #[command(flatten)]
    ownership: Ownership,
    /// Length of one alternating block. Short enough that thermal and driver
    /// drift lands on both backends equally.
    #[arg(long, default_value_t = 5.0)]
    block_seconds: f64,
    /// Decides which backend goes first. Fixed by default so a suspicious
    /// result can be re-run exactly.
    #[arg(long, default_value_t = 0x00C0_FFEE)]
    seed: u64,
    /// Compare the handoff scenario instead of the native one.
    #[arg(long)]
    handoff: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Native,
    Handoff,
}

/// The command line once it has been checked: everything here means something
/// for the scenario it belongs to, and the defaults have been filled in only
/// where they apply.
///
/// Built before any capture code is reached, so an argument mistake is caught
/// on any platform and reported the same way everywhere.
#[derive(Clone, Debug)]
struct Settings {
    level: Level,
    /// `None` for `compare`, which runs both.
    backend: Option<BackendKind>,
    common: Common,
    pool: u32,
    hold_ms: f64,
    block_seconds: f64,
    seed: u64,
}

fn settings(cli: &Cli) -> Result<Settings, String> {
    let (level, backend, common, ownership, block_seconds, seed) = match &cli.command {
        Command::Native(scenario) => (
            Level::Native,
            Some(scenario.backend),
            &scenario.common,
            &scenario.ownership,
            0.0,
            0,
        ),
        Command::Handoff(scenario) => (
            Level::Handoff,
            Some(scenario.backend),
            &scenario.common,
            &scenario.ownership,
            0.0,
            0,
        ),
        Command::Compare(comparison) => {
            if comparison.common.stall_ms > 0 {
                // A stall lands in one block and leaves the other untouched,
                // which is exactly the asymmetry the alternation exists to
                // remove. Provoke it per backend with `native --stall-ms`.
                return Err(
                    "--stall-ms cannot be used with `compare`: the stall would fall in \
                            one block and make the two columns incomparable"
                        .to_owned(),
                );
            }
            (
                if comparison.handoff {
                    Level::Handoff
                } else {
                    Level::Native
                },
                None,
                &comparison.common,
                &comparison.ownership,
                comparison.block_seconds,
                comparison.seed,
            )
        }
    };

    // An option that is accepted and then ignored is worse than one that is
    // refused: the operator believes they measured something they did not.
    if level == Level::Native && (ownership.pool.is_some() || ownership.hold_ms.is_some()) {
        return Err(
            "--pool and --hold-ms describe the owned-texture handoff; this scenario owns no \
             textures. Use `handoff`, or `compare --handoff`."
                .to_owned(),
        );
    }

    Ok(Settings {
        level,
        backend,
        common: common.clone(),
        pool: ownership.pool.unwrap_or(DEFAULT_POOL),
        hold_ms: ownership.hold_ms.unwrap_or(DEFAULT_HOLD_MS),
        block_seconds,
        seed,
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let common = match &cli.command {
        Command::Native(scenario) | Command::Handoff(scenario) => &scenario.common,
        Command::Compare(comparison) => &comparison.common,
    };
    let log = output::resolve_log_path(common.log.clone(), common.report.as_deref());
    let mut out = Output::new(log);

    let outcome = settings(&cli)
        .map_err(Into::into)
        .and_then(|settings| dispatch(&settings, &mut out));
    let passed = match outcome {
        Ok(passed) => passed,
        Err(error) => {
            // Into the block, not just stderr: a run launched into the
            // interactive session has no readable stderr either.
            let _ = writeln!(out, "\ncapture-bench failed: {error}");
            false
        }
    };

    if let Some(path) = out.path().map(PathBuf::from) {
        let _ = writeln!(out, "\nblock written to {}", path.display());
    }
    if let Err(error) = out.finish() {
        eprintln!("capture-bench: the human block could not be written: {error}");
        return ExitCode::FAILURE;
    }

    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(windows)]
fn dispatch(settings: &Settings, out: &mut Output) -> Result<bool, Box<dyn core::error::Error>> {
    use crate::run::{Mode, Plan};

    let common = &settings.common;
    let plan = Plan {
        mode: match settings.level {
            Level::Native => Mode::Native,
            Level::Handoff => Mode::Handoff,
        },
        seconds: common.seconds,
        warmup_seconds: common.warmup_seconds,
        buffers: common.buffers,
        output: common.output,
        acquire_timeout_ms: common.acquire_timeout_ms,
        cursor: common.cursor,
        source_hz: common.source_hz,
        stall_ms: common.stall_ms,
        pool: settings.pool,
        hold_ms: settings.hold_ms,
        block_seconds: settings.block_seconds,
    };

    match settings.backend {
        Some(backend) => {
            let mut report = run::single(&plan, backend)?;
            report.gate = Some(gate::evaluate(&report).to_report());
            human::run_block(out, &report)?;
            write_json(out, common.report.as_deref(), &report)?;
            Ok(report.gate.as_ref().is_some_and(|gate| gate.passed))
        }
        None => {
            let mut report = run::compare(&plan, settings.seed)?;
            report.wgc.gate = Some(gate::evaluate(&report.wgc).to_report());
            report.dda.gate = Some(gate::evaluate(&report.dda).to_report());
            human::compare_block(out, &report)?;
            write_json(out, common.report.as_deref(), &report)?;
            Ok([&report.wgc, &report.dda]
                .iter()
                .all(|run| run.gate.as_ref().is_some_and(|gate| gate.passed)))
        }
    }
}

/// Off Windows there is no desktop capture API to compare, so the harness says
/// so rather than reporting zeroes.
#[cfg(not(windows))]
fn dispatch(_settings: &Settings, _out: &mut Output) -> Result<bool, Box<dyn core::error::Error>> {
    Err("capture-bench measures the Windows desktop capture APIs; this platform has neither".into())
}

#[cfg(windows)]
fn write_json<T: serde::Serialize>(
    out: &mut Output,
    path: Option<&std::path::Path>,
    report: &T,
) -> Result<(), Box<dyn core::error::Error>> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(report)?)?;
    writeln!(out, "\nreport written to {}", path.display())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Settings, String> {
        settings(&Cli::parse_from(args))
    }

    #[test]
    fn native_carries_its_backend_and_the_defaults_that_apply_to_it() {
        let settings = parse(&["capture-bench", "native", "--backend", "dda"]).expect("valid");
        assert_eq!(settings.level, Level::Native);
        assert_eq!(settings.backend, Some(BackendKind::Dda));
        assert_eq!(settings.common.warmup_seconds, 15.0);
    }

    #[test]
    fn ownership_options_are_refused_where_nothing_is_owned() {
        // Accepting and ignoring them would leave the operator believing they
        // measured a pool size that never existed.
        let error = parse(&["capture-bench", "native", "--backend", "wgc", "--pool", "5"])
            .expect_err("rejected");
        assert!(error.contains("--pool"));
        assert!(error.contains("handoff"));

        assert!(
            parse(&[
                "capture-bench",
                "native",
                "--backend",
                "wgc",
                "--hold-ms",
                "8"
            ])
            .is_err()
        );
    }

    #[test]
    fn handoff_takes_the_ownership_options_it_was_given() {
        let settings = parse(&[
            "capture-bench",
            "handoff",
            "--backend",
            "wgc",
            "--pool",
            "5",
            "--hold-ms",
            "8.5",
        ])
        .expect("valid");
        assert_eq!(settings.level, Level::Handoff);
        assert_eq!(settings.pool, 5);
        assert_eq!(settings.hold_ms, 8.5);
    }

    #[test]
    fn handoff_without_them_gets_the_documented_defaults() {
        let settings = parse(&["capture-bench", "handoff", "--backend", "dda"]).expect("valid");
        assert_eq!(settings.pool, DEFAULT_POOL);
        assert_eq!(settings.hold_ms, DEFAULT_HOLD_MS);
    }

    #[test]
    fn compare_runs_both_backends_so_it_names_neither() {
        let settings = parse(&["capture-bench", "compare", "--seconds", "40"]).expect("valid");
        assert_eq!(settings.backend, None);
        assert_eq!(settings.level, Level::Native);
        assert_eq!(settings.block_seconds, 5.0);
        assert_eq!(settings.common.seconds, 40.0);
    }

    #[test]
    fn compare_handoff_switches_the_scenario_and_accepts_a_pool() {
        let settings =
            parse(&["capture-bench", "compare", "--handoff", "--pool", "2"]).expect("valid");
        assert_eq!(settings.level, Level::Handoff);
        assert_eq!(settings.pool, 2);
    }

    #[test]
    fn compare_refuses_a_stall_because_it_would_land_in_one_column() {
        let error =
            parse(&["capture-bench", "compare", "--stall-ms", "500"]).expect_err("rejected");
        assert!(error.contains("--stall-ms"));
        assert!(error.contains("incomparable"));
    }

    #[test]
    fn a_stall_is_fine_on_a_single_backend() {
        let settings = parse(&[
            "capture-bench",
            "native",
            "--backend",
            "dda",
            "--stall-ms",
            "500",
        ])
        .expect("valid");
        assert_eq!(settings.common.stall_ms, 500);
    }

    #[test]
    fn the_seed_reaches_the_scheduler_unchanged() {
        let settings = parse(&["capture-bench", "compare", "--seed", "7"]).expect("valid");
        assert_eq!(settings.seed, 7);
        assert_eq!(
            schedule::alternating(20.0, settings.block_seconds, settings.seed)[0].backend,
            schedule::first_backend(7)
        );
    }
}
