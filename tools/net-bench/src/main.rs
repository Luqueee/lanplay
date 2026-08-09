//! Phase 1 transport harness.
//!
//! Two questions, and nothing else: what does RTP over UDP cost between an
//! encoder's output and a decoder's input, and does it degrade honestly when
//! the network misbehaves? No VideoToolbox, no Metal, no encoder, no capture —
//! phase 2 already measured those in isolation, and mixing them back in here
//! would make the delta unattributable.
//!
//! `loopback` is the mode the phase gate uses: sender and receiver on separate
//! threads with separate sockets over `127.0.0.1`, sharing one clock, so every
//! telemetry segment between `packetization` and `reassembly` is a real
//! measurement rather than an estimate.

mod digest;
mod faults;
mod pacing;
mod receiver;
mod report;
mod sender;
mod series;
mod socket;
mod wire;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use lanplay_telemetry::{Nanos, Telemetry, TelemetryConfig, Timestamp, Trend, resident_bytes};
use lanplay_transport::MAX_UDP_PAYLOAD;

use crate::digest::Digests;
use crate::faults::FaultConfig;
use crate::pacing::PacerKind;
use crate::receiver::{ReceiveConfig, ReceiveReport};
use crate::sender::{SendConfig, SendReport};
use crate::wire::WireTimes;

/// Marks in flight before the recorder starts dropping. Seven per access unit
/// at 120 fps is under a thousand a second, but a stalled collector during a
/// `--stall-ms` experiment must not lose any.
const TELEMETRY_QUEUE: usize = 1 << 16;

/// Frames the collector assembles concurrently. Sender and receiver mark the
/// same frame id, so this only has to cover how far apart they can drift.
const TELEMETRY_RING: usize = 2048;

/// How often resident memory is sampled.
const MEMORY_INTERVAL: Duration = Duration::from_millis(100);

/// Time the receiver keeps draining after the sender stops. Long enough for
/// the last access unit's fragments and for anything the reorder ring is
/// holding.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

#[derive(Parser)]
#[command(name = "net-bench", about = "lanplay RTP/UDP transport harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Packetise a fixture and send it to a remote receiver.
    Send(Box<SendCommand>),
    /// Receive, depacketise and account for an RTP stream.
    Receive(Box<ReceiveCommand>),
    /// Both halves in one process over 127.0.0.1, with one combined report.
    Loopback(Box<LoopbackCommand>),
}

#[derive(Args, Clone)]
struct StreamArgs {
    #[arg(long)]
    fixture: PathBuf,
    #[arg(long, default_value_t = 120.0)]
    fps: f64,
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,
    #[arg(long, value_enum, default_value_t = PacerKind::Burst)]
    pacer: PacerKind,
    /// `micro` only: how long one access unit is spread over.
    #[arg(long, default_value_t = 0.5)]
    micro_window_ms: f64,
    /// `rate` only: the link budget to pace to.
    #[arg(long, default_value_t = 60.0)]
    bitrate_mbps: f64,
    /// Whole RTP datagram, header and extension included.
    #[arg(long, default_value_t = MAX_UDP_PAYLOAD)]
    mtu: usize,
    #[arg(long = "drop", default_value_t = 0, value_name = "PPM")]
    drop_ppm: u32,
    #[arg(long = "duplicate", default_value_t = 0, value_name = "PPM")]
    duplicate_ppm: u32,
    #[arg(long = "reorder", default_value_t = 0, value_name = "PPM")]
    reorder_ppm: u32,
    #[arg(long = "corrupt", default_value_t = 0, value_name = "PPM")]
    corrupt_ppm: u32,
    /// Fault injection is deterministic; this picks which run you get.
    #[arg(long, default_value_t = 0x5EED_1234_5678_9ABC)]
    seed: u64,
    #[arg(long, value_name = "BYTES")]
    socket_send_buffer: Option<usize>,
}

impl StreamArgs {
    fn config(&self) -> SendConfig {
        SendConfig {
            fixture: self.fixture.clone(),
            fps: self.fps,
            seconds: self.seconds,
            mtu: self.mtu,
            pacer: self.pacer,
            micro_window: Nanos::from_millis_f64(self.micro_window_ms),
            bitrate_mbps: self.bitrate_mbps,
            faults: FaultConfig {
                drop_ppm: self.drop_ppm,
                duplicate_ppm: self.duplicate_ppm,
                reorder_ppm: self.reorder_ppm,
                corrupt_ppm: self.corrupt_ppm,
                seed: self.seed,
            },
            socket_send_buffer: self.socket_send_buffer,
        }
    }
}

#[derive(Args, Clone)]
struct SinkArgs {
    #[arg(long, value_name = "BYTES")]
    socket_recv_buffer: Option<usize>,
    /// Freeze the receive loop for this long, to prove the sender cannot make
    /// it grow memory.
    #[arg(long, default_value_t = 0)]
    stall_ms: u64,
    /// Stall once every this many datagrams. Zero disables stalling.
    #[arg(long, default_value_t = 0)]
    stall_every: u64,
    /// Compare a SHA-256 of every reconstructed access unit against the
    /// sender's sidecar.
    #[arg(long)]
    verify: bool,
}

impl SinkArgs {
    fn config(&self, seconds: f64) -> ReceiveConfig {
        ReceiveConfig {
            seconds,
            socket_recv_buffer: self.socket_recv_buffer,
            stall: Nanos::from_millis(self.stall_ms),
            stall_every: self.stall_every,
            verify: self.verify,
        }
    }
}

#[derive(Args)]
struct SendCommand {
    #[arg(long)]
    to: SocketAddr,
    #[command(flatten)]
    stream: StreamArgs,
}

#[derive(Args)]
struct ReceiveCommand {
    #[arg(long, default_value = "0.0.0.0:5004")]
    bind: SocketAddr,
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,
    /// Required by `--verify`: the sidecar of digests lives next to it.
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// Frame rate the sender used, which fixes how the fixture splits.
    #[arg(long, default_value_t = 120.0)]
    fps: f64,
    #[command(flatten)]
    sink: SinkArgs,
}

#[derive(Args)]
struct LoopbackCommand {
    #[command(flatten)]
    stream: StreamArgs,
    #[command(flatten)]
    sink: SinkArgs,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Send(command) => run_send(&command),
        Command::Receive(command) => run_receive(&command),
        Command::Loopback(command) => run_loopback(&command),
    }
}

fn run_send(command: &SendCommand) -> ExitCode {
    let stream = &command.stream;
    // The sidecar is the sender's contract with any receiver that verifies, so
    // it is produced whether or not this process is the one checking it.
    let digests = match Digests::ensure(&stream.fixture, stream.fps.round().max(1.0) as u32) {
        Ok(digests) => digests,
        Err(err) => return fail(&format!("digests: {err}")),
    };
    print_header("send", stream);
    print_digests(&digests);

    let socket = match socket::bind("0.0.0.0:0".parse().expect("valid address")) {
        Ok(socket) => socket,
        Err(err) => return fail(&format!("bind: {err}")),
    };

    let telemetry = start_telemetry();
    let recorder = telemetry.recorder();
    let (memory, stop) = start_memory_sampler();

    let report = sender::run(&socket, command.to, &stream.config(), &recorder, None);
    stop.store(true, Ordering::Relaxed);
    let memory = memory.join().unwrap_or_default();

    let report = match report {
        Ok(report) => report,
        Err(err) => return fail(&format!("send: {err}")),
    };
    let snapshot = telemetry.shutdown();

    report::print_send(&report);
    report::print_series(Some(&report), None);
    report::print_cost(&report);
    report::print_snapshot(&snapshot);
    report::print_memory(&memory);
    ExitCode::SUCCESS
}

fn run_receive(command: &ReceiveCommand) -> ExitCode {
    let digests = match (&command.fixture, command.sink.verify) {
        (Some(fixture), _) => match Digests::ensure(fixture, command.fps.round().max(1.0) as u32) {
            Ok(digests) => {
                print_digests(&digests);
                Some(digests)
            }
            Err(err) => return fail(&format!("digests: {err}")),
        },
        (None, true) => {
            return fail("--verify needs --fixture: the digest sidecar lives next to it");
        }
        (None, false) => None,
    };

    println!("== configuration ==");
    println!("mode      receive");
    println!("bind      {}", command.bind);
    println!("seconds   {}", command.seconds);
    print_sink(&command.sink);

    let socket = match socket::bind(command.bind) {
        Ok(socket) => socket,
        Err(err) => return fail(&format!("bind: {err}")),
    };

    let telemetry = start_telemetry();
    let recorder = telemetry.recorder();
    let (memory, stop) = start_memory_sampler();

    let report = receiver::run(
        &socket,
        &command.sink.config(command.seconds),
        &recorder,
        digests.as_ref(),
        None,
        &stop,
    );
    stop.store(true, Ordering::Relaxed);
    let memory = memory.join().unwrap_or_default();

    let report = match report {
        Ok(report) => report,
        Err(err) => return fail(&format!("receive: {err}")),
    };
    let snapshot = telemetry.shutdown();

    report::print_receive(&report);
    report::print_series(None, Some(&report));
    report::print_snapshot(&snapshot);
    report::print_memory(&memory);

    if report.verify_failures > 0 {
        println!("== gate ==");
        println!(
            "FAIL: {} access units did not match their SHA-256",
            report.verify_failures
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run_loopback(command: &LoopbackCommand) -> ExitCode {
    let stream = &command.stream;
    let digests = match Digests::ensure(&stream.fixture, stream.fps.round().max(1.0) as u32) {
        Ok(digests) => digests,
        Err(err) => return fail(&format!("digests: {err}")),
    };
    print_header("loopback", stream);
    print_sink(&command.sink);
    print_digests(&digests);

    let loopback: SocketAddr = "127.0.0.1:0".parse().expect("valid address");
    let (rx_socket, tx_socket) = match (socket::bind(loopback), socket::bind(loopback)) {
        (Ok(rx), Ok(tx)) => (rx, tx),
        (Err(err), _) | (_, Err(err)) => return fail(&format!("bind: {err}")),
    };
    let target = match rx_socket.local_addr() {
        Ok(addr) => addr,
        Err(err) => return fail(&format!("local_addr: {err}")),
    };
    println!("target    {target}");
    println!();

    let telemetry = start_telemetry();
    let wire = Arc::new(WireTimes::new());
    let stop = Arc::new(AtomicBool::new(false));
    let (memory, memory_stop) = start_memory_sampler();

    let receive_config = command
        .sink
        .config(stream.seconds + DRAIN_GRACE.as_secs_f64() * 2.0);
    let send_config = stream.config();

    let outcome = thread::scope(|scope| {
        let receive = {
            let recorder = telemetry.recorder();
            let wire = Arc::clone(&wire);
            let stop = Arc::clone(&stop);
            let digests = &digests;
            let socket = &rx_socket;
            let config = &receive_config;
            thread::Builder::new()
                .name("net-bench-rx".into())
                .spawn_scoped(scope, move || {
                    receiver::run(socket, config, &recorder, Some(digests), Some(&wire), &stop)
                })
        };
        let receive = match receive {
            Ok(handle) => handle,
            Err(err) => return Err(format!("spawn receiver: {err}")),
        };

        let send = {
            let recorder = telemetry.recorder();
            let wire = Arc::clone(&wire);
            let socket = &tx_socket;
            let config = &send_config;
            thread::Builder::new()
                .name("net-bench-tx".into())
                .spawn_scoped(scope, move || {
                    sender::run(socket, target, config, &recorder, Some(&wire))
                })
        };
        let send = match send {
            Ok(handle) => handle,
            Err(err) => {
                stop.store(true, Ordering::Relaxed);
                let _ = receive.join();
                return Err(format!("spawn sender: {err}"));
            }
        };

        let send = send
            .join()
            .map_err(|_| "sender thread panicked".to_string());
        // The receiver keeps draining for a moment: the last access unit's
        // fragments are still in the kernel when the sender's loop ends.
        thread::sleep(DRAIN_GRACE);
        stop.store(true, Ordering::Relaxed);
        let receive = receive
            .join()
            .map_err(|_| "receiver thread panicked".to_string());
        Ok((send, receive))
    });

    memory_stop.store(true, Ordering::Relaxed);
    let memory = memory.join().unwrap_or_default();

    let (send, receive) = match outcome {
        Ok(pair) => pair,
        Err(err) => return fail(&err),
    };
    let send = match send {
        Ok(Ok(report)) => report,
        Ok(Err(err)) => return fail(&format!("send: {err}")),
        Err(err) => return fail(&err),
    };
    let receive = match receive {
        Ok(Ok(report)) => report,
        Ok(Err(err)) => return fail(&format!("receive: {err}")),
        Err(err) => return fail(&err),
    };
    let snapshot = telemetry.shutdown();

    report::print_send(&send);
    report::print_receive(&receive);
    report::print_series(Some(&send), Some(&receive));
    report::print_cost(&send);
    print_delivery(&send, &receive);
    report::print_snapshot(&snapshot);
    report::print_memory(&memory);

    let gate = report::gate(&send, &receive, command.sink.verify);
    report::print_gate(&gate);
    if gate.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Sent against delivered, in the units the gate cares about.
fn print_delivery(send: &SendReport, receive: &ReceiveReport) {
    println!("== delivery ==");
    let accounted = receive.rx.access_units_completed + receive.rx.access_units_dropped;
    println!(
        "access units: {} sent, {} completed, {} dropped, {} unaccounted",
        send.tx.access_units,
        receive.rx.access_units_completed,
        receive.rx.access_units_dropped,
        send.tx.access_units.saturating_sub(accounted),
    );
    println!(
        "datagrams: {} sent, {} received, {} lost, {} duplicate, {} reordered, {} malformed",
        send.datagrams,
        receive.datagrams,
        receive.rx.lost,
        receive.rx.duplicates,
        receive.rx.reordered,
        receive.rx.malformed,
    );
    let ceiling = report::depacketizer_ceiling(receive, send.largest_access_unit);
    println!(
        "depacketiser bound: peaked at {} B of a {} B ceiling ({:.0}% used)",
        receive.depacketizer_peak_bytes,
        ceiling,
        receive.depacketizer_peak_bytes as f64 / ceiling as f64 * 100.0,
    );
}

fn print_header(mode: &str, stream: &StreamArgs) {
    println!("== configuration ==");
    println!("mode      {mode}");
    println!("fixture   {}", stream.fixture.display());
    println!(
        "stream    {} fps for {} s, mtu {} B",
        stream.fps, stream.seconds, stream.mtu
    );
    match stream.pacer {
        PacerKind::Burst => println!("pacer     burst"),
        PacerKind::Micro => println!("pacer     micro, window {} ms", stream.micro_window_ms),
        PacerKind::Rate => println!("pacer     rate, {} Mbps", stream.bitrate_mbps),
    }
    let faults = FaultConfig {
        drop_ppm: stream.drop_ppm,
        duplicate_ppm: stream.duplicate_ppm,
        reorder_ppm: stream.reorder_ppm,
        corrupt_ppm: stream.corrupt_ppm,
        seed: stream.seed,
    };
    println!(
        "faults    {}",
        if faults.is_enabled() {
            faults.to_string()
        } else {
            "none".to_string()
        }
    );
}

fn print_sink(sink: &SinkArgs) {
    println!(
        "sink      verify {}, stall {} ms every {} datagrams",
        sink.verify, sink.stall_ms, sink.stall_every
    );
}

fn print_digests(digests: &Digests) {
    println!(
        "digests   {} entries in {} ({})",
        digests.len(),
        digests.path().display(),
        if digests.generated() {
            "generated"
        } else {
            "reused"
        },
    );
}

fn start_telemetry() -> Telemetry {
    Telemetry::start(TelemetryConfig {
        queue_capacity: TELEMETRY_QUEUE,
        ring_slots: TELEMETRY_RING,
        recent_frames: 64,
        ..TelemetryConfig::default()
    })
}

/// Samples resident memory on its own thread, so a stalled receive loop shows
/// up in the series instead of pausing it.
fn start_memory_sampler() -> (thread::JoinHandle<Trend>, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::Builder::new()
        .name("net-bench-memory".into())
        .spawn(move || {
            let mut trend = Trend::new();
            while !thread_stop.load(Ordering::Relaxed) {
                if let Some(bytes) = resident_bytes() {
                    trend.record_at(Timestamp::now(), bytes as f64);
                }
                thread::sleep(MEMORY_INTERVAL);
            }
            trend
        })
        .expect("spawn memory sampler");
    (handle, stop)
}

fn fail(message: &str) -> ExitCode {
    eprintln!("net-bench: {message}");
    ExitCode::FAILURE
}
