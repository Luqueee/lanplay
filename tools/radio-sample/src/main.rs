//! What the Wi-Fi radio is doing, sampled without disturbing it.
//!
//! `system_profiler SPAirPortDataType` was the obvious instrument and it is
//! the wrong one: its report includes "Other Local Wi-Fi Networks", which it
//! can only fill by scanning. A scan takes the radio off channel. Sampling
//! once a second with it turned a link whose access units arrive every
//! 8.09 ms at p50 and 11.35 ms at p99 into one reading 2.04 ms at p50 and
//! 133 ms at p99 - the instrument produced exactly the bunching the
//! experiment was looking for.
//!
//! The reading itself lives in `lanplay-capabilities`, because the client's
//! preflight needs the same facts and two copies of an OS probe is two
//! chances to disagree about what the radio was doing.
//!
//! usage:
//!   radio-sample [SECONDS] [INTERVAL_MS]
//!   radio-sample --seconds SECONDS --interval-ms INTERVAL_MS
//!
//! Two spellings for the same two numbers, because both are already in use.
//! Every gate script passes them positionally and those runs must keep
//! working; the long forms exist because `--seconds 1100` was passed to a
//! binary that read its arguments by position, took `--seconds` as an
//! unparseable window, silently fell back to its 120-second default, and
//! returned a two-minute trace that was then filed as covering a
//! seventeen-minute run. An instrument that is the record of the conditions
//! every audio measurement is judged under cannot answer a question nobody
//! asked it, so a window it will not honour is now a refusal and not a
//! default.

use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::Parser;

/// The columns, and their order, as committed in
/// `results/audio/e2e-corrected/radio-trace-first-120s.csv`. Traces from
/// before this argument parser existed sit in `results/` as evidence and are
/// compared against traces taken after it, so a reader must never have to ask
/// which version of this binary wrote the file in front of them.
const HEADER: &str = "t_s,unix_s,rssi_dbm,noise_dbm,tx_rate_mbps,channel,width_mhz,radar_band";

/// Two minutes: long enough for the tail of a delivery-interval distribution
/// to stop being thin, short enough that the link has not had time to become a
/// different link. `tools/jitter-target-sweep.sh` documents its arm length by
/// pointing at this number, so it is part of the interface and not an
/// implementation detail.
const DEFAULT_SECONDS: u64 = 120;

/// One second, which is the cadence every committed trace was taken at.
const DEFAULT_INTERVAL_MS: u64 = 1000;

/// The floor the previous version applied in silence, kept because
/// `examples/read-cost.rs` says it is the right order of magnitude and not
/// because it looked round. Two runs of 2000 reads on this machine put one
/// CoreWLAN association read at 3.19 and 3.16 ms at p50, 4.59 and 5.40 ms at
/// p99, and 7.55 and 15.50 ms at worst. A 50 ms cadence therefore spends about
/// six per cent of its time inside the probe and clears the worst read seen
/// with three times its cost to spare, while a cadence under about 16 ms
/// cannot be relied on to hold its grid: the sampler would spend a run
/// measuring its own cost. A caller asking for less is refused rather than
/// clamped, because a trace whose rows are 50 ms apart when 10 ms was asked
/// for is a trace nobody can read.
const MIN_INTERVAL_MS: u64 = 50;

/// `clap`'s ranged parser refuses out-of-range values by printing the range,
/// and `50..18446744073709551615` reads as a bug in the tool rather than as an
/// answer to what was asked, so both bounds explain themselves instead.
fn window_seconds(raw: &str) -> Result<u64, String> {
    match raw.parse::<u64>() {
        Ok(0) => Err("a zero-second window writes a header and no rows".to_string()),
        Ok(seconds) => Ok(seconds),
        Err(error) => Err(format!("{raw} is not a number of seconds: {error}")),
    }
}

fn interval_milliseconds(raw: &str) -> Result<u64, String> {
    match raw.parse::<u64>() {
        Ok(ms) if ms < MIN_INTERVAL_MS => Err(format!(
            "{ms} ms is below the {MIN_INTERVAL_MS} ms floor, and one association \
             read costs 3.2 ms at p50 and up to 15.5 ms at worst"
        )),
        Ok(ms) => Ok(ms),
        Err(error) => Err(format!("{raw} is not a number of milliseconds: {error}")),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "radio-sample",
    about = "Sample the Wi-Fi association once per interval, as CSV on stdout",
    long_about = "Sample the Wi-Fi association through CoreWLAN, which reads the \
                  current link without scanning, and write one CSV row per tick to \
                  stdout. The window and cadence may be given positionally, as every \
                  gate script does, or by name; giving the same number both ways is \
                  an error rather than a guess about which one was meant."
)]
struct Args {
    /// Seconds to sample for [default: 120]
    #[arg(value_name = "SECONDS", value_parser = window_seconds)]
    seconds: Option<u64>,

    /// Milliseconds between samples [default: 1000]
    #[arg(value_name = "INTERVAL_MS", value_parser = interval_milliseconds)]
    interval_ms: Option<u64>,

    /// Seconds to sample for, by name [default: 120]
    #[arg(long = "seconds", value_name = "SECONDS", conflicts_with = "seconds", value_parser = window_seconds)]
    seconds_flag: Option<u64>,

    /// Milliseconds between samples, by name [default: 1000]
    #[arg(long = "interval-ms", value_name = "MS", conflicts_with = "interval_ms", value_parser = interval_milliseconds)]
    interval_flag: Option<u64>,
}

impl Args {
    fn window(&self) -> Duration {
        Duration::from_secs(
            self.seconds
                .or(self.seconds_flag)
                .unwrap_or(DEFAULT_SECONDS),
        )
    }

    fn interval(&self) -> Duration {
        Duration::from_millis(
            self.interval_ms
                .or(self.interval_flag)
                .unwrap_or(DEFAULT_INTERVAL_MS),
        )
    }
}

fn main() {
    // Before the radio is touched, so `--help` answers on a machine with no
    // association and a mistyped flag costs nothing.
    let args = Args::parse();
    let deadline = args.window();
    let interval = args.interval();

    if lanplay_capabilities::wifi::association().is_none() {
        eprintln!("no Wi-Fi association");
        std::process::exit(1);
    }

    println!("{HEADER}");
    let start = Instant::now();
    while start.elapsed() < deadline {
        let at = start.elapsed();
        let Some(link) = lanplay_capabilities::wifi::association() else {
            // A momentary loss is a fact about the run, not a reason to stop
            // sampling: the gap in the series is the observation.
            continue;
        };
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        println!(
            "{:.3},{unix:.3},{},{},{:.0},{},{},{}",
            at.as_secs_f64(),
            link.rssi_dbm,
            link.noise_dbm,
            link.tx_rate_mbps,
            link.channel,
            link.width_mhz,
            u8::from(link.uses_radar_band())
        );
        // Flushed every row: a sampler killed at the end of a run must not
        // lose the tail of what it saw.
        let _ = std::io::stdout().flush();
        let elapsed = start.elapsed();
        let next = interval * ((elapsed.as_nanos() / interval.as_nanos()) as u32 + 1);
        if next > elapsed {
            std::thread::sleep(next - elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("radio-sample").chain(args.iter().copied()))
            .expect("these arguments are accepted")
    }

    fn refusal(args: &[&str]) -> clap::Error {
        Args::try_parse_from(std::iter::once("radio-sample").chain(args.iter().copied()))
            .expect_err("these arguments are refused")
    }

    /// The spelling in `tools/audio-e2e-gate.sh`, `tools/link-arm.sh`,
    /// `tools/mtu-sweep.sh` and `tools/jitter-target-sweep.sh`. A gate that
    /// stops recording its conditions is a gate that cannot be interpreted.
    #[test]
    fn positional_arguments_are_the_committed_spelling() {
        let args = parse(&["1", "1000"]);
        assert_eq!(args.window(), Duration::from_secs(1));
        assert_eq!(args.interval(), Duration::from_millis(1000));

        let args = parse(&["1", "100"]);
        assert_eq!(args.window(), Duration::from_secs(1));
        assert_eq!(args.interval(), Duration::from_millis(100));

        let args = parse(&["150"]);
        assert_eq!(args.window(), Duration::from_secs(150));
        assert_eq!(args.interval(), Duration::from_millis(DEFAULT_INTERVAL_MS));
    }

    /// The failure this parser exists for: a seventeen-minute run whose
    /// conditions were recorded for its first two minutes.
    #[test]
    fn a_named_window_is_honoured_rather_than_defaulted() {
        assert_eq!(
            parse(&["--seconds", "1100"]).window(),
            Duration::from_secs(1100)
        );
        assert_eq!(
            parse(&["--seconds", "1100"]).interval(),
            Duration::from_millis(DEFAULT_INTERVAL_MS)
        );
        assert_eq!(
            parse(&["--interval-ms", "250"]).interval(),
            Duration::from_millis(250)
        );
        assert_eq!(
            parse(&["--interval-ms", "250"]).window(),
            Duration::from_secs(DEFAULT_SECONDS)
        );
    }

    #[test]
    fn no_arguments_is_two_minutes_at_one_second() {
        let args = parse(&[]);
        assert_eq!(args.window(), Duration::from_secs(120));
        assert_eq!(args.interval(), Duration::from_millis(1000));
    }

    /// Exit 2 and the offending token in the message, because a gate script
    /// reads the exit code and a person reads the line.
    #[test]
    fn an_unknown_argument_is_refused_by_name() {
        let error = refusal(&["--minutes", "5"]);
        assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        assert_eq!(error.exit_code(), 2);
        assert!(error.to_string().contains("--minutes"), "{error}");
    }

    /// Silently honouring one of two contradictory windows is how a trace ends
    /// up describing a run it did not cover.
    #[test]
    fn a_window_given_twice_is_refused() {
        let error = refusal(&["60", "--seconds", "600"]);
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        assert_eq!(error.exit_code(), 2);

        let error = refusal(&["60", "1000", "--interval-ms", "100"]);
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
        assert_eq!(error.exit_code(), 2);
    }

    /// Both bounds used to be applied in silence: a zero window produced a
    /// header and no rows, and a 10 ms cadence produced 50 ms.
    #[test]
    fn a_window_or_cadence_that_cannot_be_honoured_is_refused() {
        for args in [
            vec!["0"],
            vec!["--seconds", "0"],
            vec!["60", "10"],
            vec!["--interval-ms", "10"],
            vec!["not-a-number"],
        ] {
            let error = refusal(&args);
            assert_eq!(error.exit_code(), 2, "{args:?} gave {error}");
        }
    }

    #[test]
    fn help_is_an_answer_and_not_a_two_minute_sample() {
        let error = refusal(&["--help"]);
        assert_eq!(error.kind(), ErrorKind::DisplayHelp);
        assert_eq!(error.exit_code(), 0);
        assert!(!error.use_stderr(), "help belongs on stdout");
        let rendered = error.to_string();
        for expected in ["--seconds", "--interval-ms", "SECONDS", "INTERVAL_MS"] {
            assert!(
                rendered.contains(expected),
                "{expected} missing from {rendered}"
            );
        }
    }

    /// The columns are evidence. Traces taken before this parser existed are
    /// committed under `results/` and are read beside traces taken after it, so
    /// the header is checked against the file itself rather than against a copy
    /// of it made here.
    #[test]
    fn the_header_matches_the_committed_trace() {
        let trace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../results/audio/e2e-corrected/radio-trace-first-120s.csv");
        let committed = std::fs::read_to_string(&trace)
            .unwrap_or_else(|e| panic!("{} is committed evidence: {e}", trace.display()));
        assert_eq!(
            committed.lines().next().expect("the trace has a header"),
            HEADER
        );
    }
}
