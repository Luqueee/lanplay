//! The receiving end of the input path, exercised on its own.
//!
//! One socket, one thread, no video and no control plane: a datagram arrives,
//! it is decoded, and it is injected before the next `recv_from`. There is no
//! queue between those steps and no timer anywhere, so the histogram this
//! prints is the real cost of the receive-and-inject path rather than the cost
//! of a scheduler.
//!
//! The measured interval starts when `recv_from` returns and ends when
//! injection completes, both read from this machine's clock. Nothing here
//! subtracts the client's `sent_at_ns` from anything: the two machines share
//! no epoch, and the difference would be a clock offset dressed up as a
//! latency.
//!
//! `--dry-run` decodes and counts without injecting, which is what makes the
//! protocol path runnable on a machine whose pointer nobody wants moving --
//! including, off Windows, a machine that has no `SendInput` at all. A run
//! without it on such a machine is refused rather than quietly counted,
//! because a report full of applied events that reached no input system is
//! worse than no report.

use std::net::{SocketAddr, UdpSocket};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use hdrhistogram::Histogram;
use lanplay_input_protocol::{DecodeError, MAX_DATAGRAM, SessionId, decode};
use lanplay_telemetry::{Nanos, Timestamp};

use crate::state::{Action, HostState, Outcome};

/// How long a blocked `recv_from` waits before the loop looks at the clock.
///
/// Not pacing: it is how an idle run reaches its deadline. Injection never
/// waits on it, because a datagram that has arrived wakes the call
/// immediately.
const POLL: Duration = Duration::from_millis(100);

/// Widest interval the histogram holds, matching the rest of the project. A
/// receive-to-injected interval longer than this is a stall, not a latency.
const MAX_NANOS: u64 = 10_000_000_000;

#[derive(Parser)]
#[command(
    name = "input-inject-probe",
    about = "input UDP 5006 -> SendInput, with no pacing in between"
)]
struct Cli {
    #[arg(long, default_value = "0.0.0.0:5006")]
    bind: SocketAddr,
    /// How long to run.
    #[arg(long, default_value_t = 60.0)]
    seconds: f64,
    /// Datagrams carrying any other session id are dropped and counted. Not
    /// negotiated yet, so both ends default to the same number.
    #[arg(long, default_value_t = 1)]
    session_id: u32,
    /// Decode and count, inject nothing.
    #[arg(long)]
    dry_run: bool,
}

/// Where an action goes.
enum Backend {
    Counting,
    #[cfg(windows)]
    Injecting(crate::send::Injector),
}

impl Backend {
    fn deliver(&mut self, action: Action) {
        match self {
            // The action was already counted where it was produced; reaching
            // here and stopping is the whole of a dry run.
            Backend::Counting => _ = action,
            #[cfg(windows)]
            Backend::Injecting(injector) => injector.deliver(action),
        }
    }

    /// Events handed to the OS, which is the denominator the refused count
    /// needs: one refusal in ten is a permissions problem and one in ten
    /// thousand is a window changing under the pointer.
    fn calls(&self) -> u64 {
        match self {
            Backend::Counting => 0,
            #[cfg(windows)]
            Backend::Injecting(injector) => injector.calls(),
        }
    }

    fn refused(&self) -> u64 {
        match self {
            Backend::Counting => 0,
            #[cfg(windows)]
            Backend::Injecting(injector) => injector.refused(),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Backend::Counting => "dry run, nothing is injected",
            #[cfg(windows)]
            Backend::Injecting(_) => "injecting",
        }
    }
}

#[derive(Default)]
struct Counts {
    datagrams: u64,
    applied: u64,
    duplicates: u64,
    wrong_session: u64,
    decode_errors: u64,
    /// Host-to-client messages that arrived at the host.
    ignored: u64,
    /// Summed from the actions, so the operator has one number to hold against
    /// the total the client says it sent. Counted in a dry run as well: it
    /// describes what the client asked for.
    dx: i64,
    dy: i64,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();

    let mut backend = if cli.dry_run {
        Backend::Counting
    } else {
        #[cfg(windows)]
        {
            Backend::Injecting(crate::send::Injector::new())
        }
        #[cfg(not(windows))]
        {
            eprintln!(
                "input-inject-probe: there is no Windows input system here, so nothing can be \
                 injected. Rerun with --dry-run to exercise the protocol path."
            );
            return ExitCode::from(3);
        }
    };

    let socket = match UdpSocket::bind(cli.bind) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("input-inject-probe: cannot bind {}: {error}", cli.bind);
            return ExitCode::from(2);
        }
    };
    if let Err(error) = socket.set_read_timeout(Some(POLL)) {
        eprintln!("input-inject-probe: cannot set a read timeout: {error}");
        return ExitCode::from(2);
    }

    let session = SessionId(cli.session_id);
    println!(
        "input-inject-probe: bound {}, session {}, {}",
        cli.bind,
        cli.session_id,
        backend.label()
    );

    let mut state = HostState::new();
    let mut counts = Counts::default();
    let mut histogram =
        Histogram::<u64>::new_with_bounds(1, MAX_NANOS, 3).expect("valid histogram bounds");
    let mut first_error: Option<DecodeError> = None;
    let mut peer: Option<SocketAddr> = None;

    let mut buffer = [0u8; MAX_DATAGRAM];
    let deadline = Timestamp::now().add(Nanos::from_millis_f64(cli.seconds * 1_000.0));

    while Timestamp::now() < deadline {
        let (len, from) = match socket.recv_from(&mut buffer) {
            Ok(received) => received,
            // A timeout is the idle case, not a failure; anything else is
            // reported and ends the run rather than being counted as input.
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(error) => {
                eprintln!("input-inject-probe: recv_from failed: {error}");
                break;
            }
        };
        let received = Timestamp::now();
        counts.datagrams += 1;
        if peer.is_none() {
            peer = Some(from);
        }

        let datagram = match decode(&buffer[..len]) {
            Ok(datagram) => datagram,
            Err(error) => {
                counts.decode_errors += 1;
                first_error = first_error.or(Some(error));
                continue;
            }
        };
        if datagram.session != session {
            counts.wrong_session += 1;
            continue;
        }

        let outcome = state.apply(&datagram.message, |action| {
            if let Action::Motion { dx, dy } = action {
                counts.dx += dx as i64;
                counts.dy += dy as i64;
            }
            backend.deliver(action);
        });
        match outcome {
            Outcome::Applied => counts.applied += 1,
            Outcome::Duplicate => counts.duplicates += 1,
            Outcome::Ignored => counts.ignored += 1,
            Outcome::Stale => {}
        }

        // Only datagrams that reached the injector are timed. A rejected
        // session id or a malformed datagram costs a decode and nothing else,
        // and mixing that in would flatter the distribution.
        histogram.saturating_record(Timestamp::now().saturating_since(received).get());
    }

    report(&counts, &state, &backend, &histogram, peer, first_error);
    ExitCode::SUCCESS
}

fn report(
    counts: &Counts,
    state: &HostState,
    backend: &Backend,
    histogram: &Histogram<u64>,
    peer: Option<SocketAddr>,
    first_error: Option<DecodeError>,
) {
    match peer {
        Some(peer) => println!("first datagram from {peer}"),
        None => println!("no datagrams arrived"),
    }
    println!();
    println!("datagrams        {:>10}", counts.datagrams);
    println!("applied          {:>10}", counts.applied);
    println!("duplicate        {:>10}", counts.duplicates);
    println!("stale snapshot   {:>10}", state.stale_snapshots());
    println!("wrong session    {:>10}", counts.wrong_session);
    println!("decode errors    {:>10}", counts.decode_errors);
    println!("host to client   {:>10}", counts.ignored);
    println!("sendinput calls  {:>10}", backend.calls());
    println!("refused          {:>10}", backend.refused());
    println!("injected dx      {:>10}", counts.dx);
    println!("injected dy      {:>10}", counts.dy);
    if let Some(error) = first_error {
        println!("first decode error: {error}");
    }
    // Held keys should be empty at the end of a run that ended cleanly, and
    // saying so is cheaper than asking an operator to check the keyboard.
    if !state.nothing_held() {
        println!(
            "still held: keys {:?}, buttons {:#07b}",
            state.held_keys(),
            state.held_buttons()
        );
    }

    println!();
    if histogram.is_empty() {
        println!("recv to injected: nothing to summarise");
        return;
    }
    // Microseconds: injection is a syscall, and in milliseconds every column
    // reads 0.00.
    println!(
        "recv to injected   count {:>8}   p50 {:>8.2}µ   p95 {:>8.2}µ   p99 {:>8.2}µ   max {:>8.2}µ",
        histogram.len(),
        micros(histogram.value_at_quantile(0.50)),
        micros(histogram.value_at_quantile(0.95)),
        micros(histogram.value_at_quantile(0.99)),
        micros(histogram.max()),
    );
}

fn micros(nanos: u64) -> f64 {
    nanos as f64 / 1_000.0
}
