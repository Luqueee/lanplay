//! Phase 0 harness: proves the clock, the telemetry path and the capability
//! probes work before a single real frame exists.

mod synthetic;

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lanplay_protocol::{ClientCapabilities, HostCapabilities};
use lanplay_telemetry::Snapshot;

use crate::synthetic::{Run, SyntheticArgs};

#[derive(Parser)]
#[command(name = "lanplay-bench", about = "lanplay instrumentation harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report what this machine can do.
    Caps,
    /// Run the synthetic pipeline and print per-frame and aggregate timings.
    Synthetic(Box<SyntheticArgs>),
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Caps => {
            print_capabilities();
            ExitCode::SUCCESS
        }
        Command::Synthetic(args) => {
            let run = synthetic::run(&args);
            print_run(&args, &run);
            if gate(&args, &run) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn print_capabilities() {
    let host = lanplay_capabilities::host();
    let client = lanplay_capabilities::client();

    println!("host role");
    if lanplay_capabilities::host_probes_supported() {
        print_host(&host);
    } else {
        println!("  not probed: host discovery is implemented for Windows");
    }

    println!();
    println!("client role");
    if lanplay_capabilities::client_probes_supported() {
        print_client(&client);
    } else {
        println!("  not probed: decoder discovery is implemented for macOS");
        print_displays(&client.displays);
    }
}

fn print_host(host: &HostCapabilities) {
    if host.gpus.is_empty() {
        println!("  gpus: none reported");
    }
    for gpu in &host.gpus {
        println!("  gpu: {} ({:?})", gpu.name, gpu.vendor);
    }
    match host.nvenc {
        Some(nvenc) => println!("  nvenc: api {}.{}", nvenc.api_major, nvenc.api_minor),
        None => println!("  nvenc: unavailable"),
    }
    print_displays(&host.displays);
}

fn print_client(client: &ClientCapabilities) {
    print_displays(&client.displays);
    if client.hardware_decode.is_empty() {
        println!("  hardware decode: none");
    } else {
        let codecs: Vec<String> = client
            .hardware_decode
            .iter()
            .map(ToString::to_string)
            .collect();
        println!("  hardware decode: {}", codecs.join(", "));
    }
}

fn print_displays(displays: &[lanplay_protocol::DisplayInfo]) {
    for display in displays {
        let rates: Vec<String> = display
            .available_refresh_mhz
            .iter()
            .map(|mhz| (f64::from(*mhz) / 1000.0).to_string())
            .collect();
        println!(
            "  display {}{}: {} [{}]{}",
            display.name,
            if display.primary { " (primary)" } else { "" },
            display.current,
            rates.join(" "),
            match display.scale_factor {
                Some(scale) => format!(" scale {scale:.2}x"),
                None => String::new(),
            },
        );
    }
}

fn print_run(args: &SyntheticArgs, run: &Run) {
    println!(
        "synthetic {} ({:.0} Mpx/s), {} frames over {:.1} s",
        run.mode,
        run.mode.pixel_rate() as f64 / 1e6,
        run.expected_frames,
        args.seconds,
    );
    println!();
    match &run.sample {
        Some(timeline) => print!("{timeline}"),
        None => println!("no frame timeline captured"),
    }
    println!();
    println!("{}", run.snapshot);
}

/// Phase 0 asks one question: can every frame be measured, end to end, without
/// the measurement itself losing anything? The numbers are only trustworthy
/// once this passes.
fn gate(args: &SyntheticArgs, run: &Run) -> bool {
    let snapshot: &Snapshot = &run.snapshot;
    let mut failures: Vec<String> = Vec::new();

    if !snapshot.is_lossless() {
        failures.push(format!(
            "instrumentation lost data: {} dropped, {} incomplete, {} duplicate, {} late",
            snapshot.counters.events_dropped,
            snapshot.counters.frames_incomplete,
            snapshot.counters.duplicate_marks,
            snapshot.counters.late_events,
        ));
    }

    if snapshot.counters.frames_presented != run.expected_frames {
        failures.push(format!(
            "presented {} of {} frames",
            snapshot.counters.frames_presented, run.expected_frames,
        ));
    }

    for span in &snapshot.spans {
        let expected = if span.name == "gpu preprocess" && args.preprocess_ms <= 0.0 {
            0
        } else {
            snapshot.counters.frames_presented
        };
        if span.count != expected {
            failures.push(format!(
                "span '{}' measured {} times, expected {expected}",
                span.name, span.count,
            ));
        }
    }

    let measured_fps = snapshot.presented_per_second();
    let drift = (measured_fps - args.fps).abs() / args.fps;
    if drift > 0.02 {
        failures.push(format!(
            "cadence drifted {:.1}%: {measured_fps:.1}/s against {:.1}/s",
            drift * 100.0,
            args.fps,
        ));
    }

    println!();
    if failures.is_empty() {
        println!("gate: PASS ({measured_fps:.1} presented/s, every span measured)");
        return true;
    }
    for failure in &failures {
        println!("gate: FAIL {failure}");
    }
    false
}
