//! N2: the startup probe, video-shaped, and a report that classifies nothing.
//!
//! A session is about to be started and something has to decide what to ask the
//! host for. This measures the link for three to five seconds with the product's
//! own traffic and writes down what it saw, so that the choice is made from a
//! measurement instead of from a default. Both halves of that sentence are load
//! bearing, and the second one is the one that gets forgotten: what it writes
//! down is a description, and the moment a description of five seconds is read
//! as a prediction about ten minutes it becomes the most confident wrong number
//! in the system. The evidence is `results/audio/e2e-clean/radio-trace-full.csv`:
//! 1117 association reads at 1 Hz over one session whose first thirty seconds
//! spread 4 dB and 576 to 648 Mbps, and which then spread 11 dB and 103 to 816
//! Mbps across the rest of itself, with 8 dB of movement inside one six-minute
//! lag. So N2 selects a starting point and the monitor does the watching.
//!
//! # Why it is built on `net-bench`
//!
//! `tools/net-bench send` already paces a real H.264 fixture at a real frame
//! rate through real datagrams, and `--pacer burst` hands a whole access unit to
//! the kernel at once, which is the shape this product presents to the air:
//! about forty datagrams with no gap between them, forty times a second. A probe
//! that generated a smooth stream of the same bitrate would be measuring a link
//! nobody is going to use, and it would come back optimistic - the burst is the
//! part an access point fails at. So this program is the receiving half only,
//! and the generator is the one the rest of the project already ranks channels
//! with.
//!
//! # What it measures, and through what
//!
//! Loss and reordering come from the depacketiser's sequence accounting.
//! Cadence, stall clusters and the gaps between stalls come from
//! `crates/link-metrics` and from nowhere else, marked where
//! `macos/client/src/transport.rs` marks them, so a preflight figure and a
//! mid-session figure are the same quantity. The negotiated rate, the channel,
//! the width and the signal come from one passive CoreWLAN read either side of
//! the probe - never a scan, because `system_profiler SPAirPortDataType` takes
//! the radio off its channel and one use of it contaminated an experiment.
//!
//! Nothing is asked of the socket. The default receive buffer on this Mac is
//! 786896 B (`sysctl net.inet.udp.recvspace`), some 650 datagrams against an
//! access unit's burst of forty, so the probe configures nothing and cannot be
//! accused of having configured its own result.
//!
//! # Exit codes
//!
//! 0  the probe measured the link, and the report says what it found
//! 2  refused: there was nothing to measure, and the report says which absence
//! 1  the probe could not run at all, which is neither of the above

mod conditions;
mod envelope;
mod probe;
mod report;

use std::fs;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, ValueEnum};

use crate::conditions::Conditions;
use crate::envelope::Expect;
use crate::probe::{Outcome, ProbeConfig};
use crate::report::Provenance;

#[derive(Parser)]
#[command(
    name = "net-preflight",
    about = "measure the link for a few seconds with the product's own traffic, and write down \
             what was measured"
)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:5004")]
    bind: SocketAddr,
    /// Provisionally five seconds, and provisional on purpose: three so a
    /// hundred intervals exist to summarise, five so nobody waits. N3 fixes the
    /// window from recorded sessions rather than from this line.
    #[arg(long, default_value_t = 5.0)]
    seconds: f64,
    /// The rate the sender was told to pace at, which is what every threshold in
    /// `crates/link-metrics` is a multiple of.
    #[arg(long, default_value_t = 120.0)]
    fps: f64,
    /// The sender's whole-datagram budget, header included.
    #[arg(long, default_value_t = 1200)]
    mtu: usize,
    /// How long to wait for the first datagram. Generous, because the sender is
    /// started over ssh on another machine and its build may be cold.
    #[arg(long, default_value_t = 20.0)]
    wait_seconds: f64,
    /// Which arm this is, for the record. A result filed under no arm is a
    /// result nobody can place.
    #[arg(long, default_value = "clean")]
    arm: String,
    /// What the arm is claimed to be, which decides which criteria it states. A
    /// receive-only probe cannot tell a fault relay from a bad link, so the
    /// claim is the caller's and is recorded as the caller's.
    #[arg(long, value_enum, default_value_t = ExpectArg::Clean)]
    expect: ExpectArg,
    /// The sender's pacer, for the record.
    #[arg(long, default_value = "burst")]
    pacer: String,
    /// What the fault relay was told to do, in its own words.
    #[arg(long)]
    faults: Option<String>,
    /// The relay's seed. An arm with injected faults and no seed on record is an
    /// arm nobody can re-run.
    #[arg(long)]
    fault_seed: Option<u64>,
    #[arg(long, default_value = "net-preflight")]
    gate: String,
    #[arg(long)]
    commit: Option<String>,
    /// Where the report goes. This is the artefact; the envelope below is the
    /// gate's business.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Where the document `xtask verdict` decides goes.
    #[arg(long)]
    envelope: Option<PathBuf>,
    /// Where the same observations go, stating the *other* arm's criteria.
    ///
    /// This is how a control arm here fails. The fault arm's own criteria are
    /// must-not-be-zeros - the injected loss reached the path, the bunching was
    /// seen - and passing those is not failing anything, so an arm judged only
    /// by them can never be the control `tools/gates.toml` asks every gate for.
    /// Judged by the clean arm's criteria instead, the same numbers must be
    /// refused, and the criteria the gate actually asserts are then shown
    /// capable of disagreeing. `tools/audio-rtp-gate.sh` arrived at the same
    /// arrangement: its control arm is judged against the clean arm's criteria
    /// rather than against a threshold of its own.
    ///
    /// Symmetric on purpose. A clean arm judged by the fault arm's criteria
    /// must fail too, because a link that lost nothing cannot have shown an
    /// injected loss, and a gate that only ever crosses one way is a gate whose
    /// other direction nobody has tried.
    #[arg(long)]
    cross_envelope: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ExpectArg {
    Clean,
    Faults,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let expect = match cli.expect {
        ExpectArg::Clean => Expect::Clean,
        ExpectArg::Faults => Expect::Faults,
    };

    let socket = match UdpSocket::bind(cli.bind) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("net-preflight: {}: {error}", cli.bind);
            return ExitCode::from(1);
        }
    };

    let config = ProbeConfig {
        seconds: cli.seconds,
        fps: cli.fps,
        mtu: cli.mtu,
        wait: Duration::from_secs_f64(cli.wait_seconds.max(0.0)),
    };
    let provenance = Provenance {
        arm: cli.arm.clone(),
        pacer: cli.pacer.clone(),
        faults: cli.faults.clone(),
        commit: cli.commit.clone(),
    };

    // Before the readiness line, and that ordering is the point rather than
    // tidiness. A read costs 3.2 ms at p50 and 15.5 ms at worst, and the socket
    // is already bound, so a sender started on the strength of the line below
    // would have its first two access units sitting in the kernel buffer while
    // the driver is being asked what channel it is on. They would then be read
    // back to back, and the first intervals in the series would be the
    // association read's rather than the link's.
    let before = conditions::read();

    println!("arm       {} ({})", cli.arm, expect.label());
    if let Some(faults) = &cli.faults {
        println!("relay     {faults}");
    }
    println!(
        "listening {} for {:.1} s at {:.0} fps, mtu {}",
        cli.bind, cli.seconds, cli.fps, cli.mtu
    );

    let outcome = match probe::run(&socket, &config) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("net-preflight: receiving on {}: {error}", cli.bind);
            return ExitCode::from(1);
        }
    };
    let conditions = Conditions {
        before,
        after: conditions::read(),
    };

    print_conditions(&conditions);
    print_outcome(&outcome);

    let report = report::build(&outcome, &config, &conditions, &provenance);
    match &cli.report {
        Some(path) => {
            if let Err(error) = write(path, &report) {
                eprintln!("net-preflight: writing {}: {error}", path.display());
                return ExitCode::from(1);
            }
        }
        // Said rather than assumed. A measurement nobody kept is a measurement
        // nobody can compare with the next one, and silence here reads exactly
        // like a report that was written somewhere the caller forgot.
        None => println!("note      no --report was given, so nothing was persisted"),
    }
    for (path, stated, crossed) in [
        (&cli.envelope, expect, false),
        (&cli.cross_envelope, expect.crossed(), true),
    ] {
        let Some(path) = path else {
            continue;
        };
        // The crossed document is renamed, because two files stating different
        // criteria over one measurement under one arm name is how a reader ends
        // up quoting the judgement of the arm nobody ran.
        let stated_by = Provenance {
            arm: if crossed {
                format!("{}-as-{}", cli.arm, stated.label())
            } else {
                cli.arm.clone()
            },
            ..provenance.clone()
        };
        let document = envelope::build(
            &cli.gate,
            &outcome,
            &config,
            &conditions,
            &stated_by,
            stated,
            cli.fault_seed,
        );
        if let Err(error) = write(path, &document) {
            eprintln!("net-preflight: writing {}: {error}", path.display());
            return ExitCode::from(1);
        }
    }

    match &outcome {
        Outcome::Measured(_) => ExitCode::SUCCESS,
        // Two rather than one, and never zero. A probe that measured nothing has
        // not found a clean link, and the exit code says so in the three answers
        // this project's harnesses already exchange.
        Outcome::Nothing { why, .. } => {
            println!();
            println!("REFUSED   {why}");
            ExitCode::from(2)
        }
    }
}

fn write<T: serde::Serialize>(path: &PathBuf, document: &T) -> std::io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(document)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    text.push('\n');
    fs::write(path, text)?;
    println!("wrote     {}", path.display());
    Ok(())
}

fn print_conditions(conditions: &Conditions) {
    for (when, read) in [("before", &conditions.before), ("after", &conditions.after)] {
        match read {
            Some(association) => println!(
                "radio {when:<6} channel {} at {} MHz, {} dBm over {} dBm, {:.0} Mbps negotiated{}",
                association.channel,
                association.width_mhz,
                association.rssi_dbm,
                association.noise_dbm,
                association.tx_rate_mbps,
                if association.uses_radar_band() {
                    ", radar band"
                } else {
                    ""
                },
            ),
            // Said rather than omitted: an absent read is why a run cannot be
            // compared with another, and a missing line reads as a line nobody
            // printed.
            None => println!("radio {when:<6} the driver reported no association"),
        }
    }
    if conditions.channel_moves() > 0 {
        println!("radio     the channel or width MOVED across the probe");
    }
}

fn print_outcome(outcome: &Outcome) {
    let Outcome::Measured(measurement) = outcome else {
        return;
    };
    let window = measurement.window;
    println!(
        "shape     {} datagrams, {:.0} B mean, {:.1} per access unit, {:.1} Mbps",
        measurement.datagrams,
        measurement.mean_datagram_bytes(),
        measurement.datagrams_per_access_unit(),
        measurement.megabits_per_second(),
    );
    match measurement.access_units_expected() {
        Some(expected) => println!(
            "stream    {} of {expected} access units, {} datagrams lost of {} accounted, \
             {} reordered",
            window.delivered,
            measurement.rx.lost,
            measurement.datagrams_accounted(),
            measurement.rx.reordered,
        ),
        None => println!(
            "stream    {} access units, {} datagrams lost of {} accounted, {} reordered \
             (the sender stated no frame id, so it never said how many it sent)",
            window.delivered,
            measurement.rx.lost,
            measurement.datagrams_accounted(),
            measurement.rx.reordered,
        ),
    }
    println!(
        "cadence   p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms over {:.2} s",
        window.p50_ms, window.p99_ms, window.max_ms, window.span_s,
    );
    println!(
        "tail      {} over two periods, {} stall clusters, gaps p50 {:.1} ms p95 {:.1} ms",
        window.tail.over[2],
        window.tail.clusters,
        window.tail.stall_gap_p50_ms,
        window.tail.stall_gap_p95_ms,
    );
    if let Some(error) = &measurement.recv_error {
        println!("error     the receive loop ended on {error}");
    }
}
