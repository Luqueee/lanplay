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
//! worse than no report. Acknowledgements are still sent in a dry run: not
//! injecting is not the same as not answering, and a client left unanswered
//! would retransmit everything five times before giving up.
//!
//! Every message the protocol calls reliable is acknowledged, whether it was
//! newly applied or recognised as a retransmission. A client that hears
//! nothing about a duplicate sends another one, so silence about a duplicate
//! guarantees the next copy. Nothing else is acknowledged, and which side of
//! that line a message falls on is asked of
//! [`Message::reliability`](lanplay_input_protocol::Message::reliability)
//! rather than decided here, so a message kind added to the protocol later
//! cannot quietly land on the wrong side.
//!
//! The `sent_at_ns` on an outgoing acknowledgement is this machine's own
//! monotonic clock, and it is written for the same reason every other
//! datagram carries one: so the sender can measure its own intervals. The
//! client must not subtract it from anything of its own. The two machines
//! share no epoch, and the difference would be a clock offset wearing a
//! latency's clothes.

use std::net::{SocketAddr, UdpSocket};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use hdrhistogram::Histogram;
use lanplay_input_protocol::{
    Datagram, DecodeError, MAX_DATAGRAM, Message, Reliability, Sequence, SessionId, decode, encode,
};
use lanplay_telemetry::{Nanos, Timestamp};

use crate::state::{Action, HostState, Outcome};

/// How long a blocked `recv_from` waits before the loop looks at the clock.
///
/// Not pacing: it is how an idle run reaches its deadline. Injection never
/// waits on it, because a datagram that has arrived wakes the call
/// immediately.
const POLL: Duration = Duration::from_millis(100);

/// How long the host goes without hearing from the client before it decides
/// the client is gone and releases everything.
///
/// A starting figure, not a production one: it has never been tuned against a
/// congested link. Two seconds because the stalls already measured on this
/// Wi-Fi reach fifty milliseconds, and a timeout close to that would read an
/// ordinary stall as a departure and rip the keys out from under a player who
/// is still holding them. Two seconds is forty times the worst stall seen,
/// which is the wrong end to be wrong on: a stuck key after a real
/// disconnection lasts at most this long, while a false expiry during a stall
/// happens while the user is playing.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Subsystems this run was meant to exercise. Any of them still at zero
    /// when the run ends is named and the process exits 4.
    ///
    /// Not observed must never equal pass: two gate arms have already gone
    /// green having exercised nothing at all, because a probe that receives no
    /// input still prints a clean report.
    #[arg(long, value_delimiter = ',', value_enum)]
    expect: Vec<Subsystem>,
}

/// A part of the input path a run can be asked to prove it exercised.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
enum Subsystem {
    Motion,
    Keys,
    Buttons,
    Wheel,
    Acks,
    Snapshots,
    Heartbeats,
}

impl Subsystem {
    fn label(self) -> &'static str {
        match self {
            Subsystem::Motion => "motion",
            Subsystem::Keys => "keys",
            Subsystem::Buttons => "buttons",
            Subsystem::Wheel => "wheel",
            Subsystem::Acks => "acks",
            Subsystem::Snapshots => "snapshots",
            Subsystem::Heartbeats => "heartbeats",
        }
    }
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

    /// Motion, key, button and wheel calls. Attributed because a total cannot
    /// tell an innocent excess from a guilty one: one `ReleaseAll` emits an
    /// action per held thing, while a duplicate wheel emitting a second notch
    /// is the failure the design exists to prevent.
    fn calls_by_kind(&self) -> [u64; 4] {
        match self {
            Backend::Counting => [0; 4],
            #[cfg(windows)]
            Backend::Injecting(injector) => injector.calls_by_kind(),
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
    superseded: u64,
    wrong_session: u64,
    decode_errors: u64,
    /// Host-to-client messages that arrived at the host.
    ignored: u64,
    /// Acknowledgements the host put on the wire. The client's unacked count
    /// is the other half of this figure, and a gap between them is loss on
    /// the return path rather than on the way in.
    acks: u64,
    /// Acknowledgements the socket refused. Counted rather than swallowed:
    /// a return path that never works looks exactly like a client that never
    /// sends, unless somebody counts.
    ack_failures: u64,
    /// Summed from the actions, so the operator has one number to hold against
    /// the total the client says it sent. Counted in a dry run as well: it
    /// describes what the client asked for.
    dx: i64,
    dy: i64,
    /// Datagrams accepted for this session, by what they carried. Counted per
    /// datagram rather than per action, and duplicates included, because these
    /// answer whether the subsystem was exercised on the wire at all rather
    /// than how far the pointer moved.
    motion: u64,
    keys: u64,
    buttons: u64,
    wheel: u64,
    snapshots: u64,
    heartbeats: u64,
}

impl Counts {
    fn observed(&self, subsystem: Subsystem) -> u64 {
        match subsystem {
            Subsystem::Motion => self.motion,
            Subsystem::Keys => self.keys,
            Subsystem::Buttons => self.buttons,
            Subsystem::Wheel => self.wheel,
            Subsystem::Acks => self.acks,
            Subsystem::Snapshots => self.snapshots,
            Subsystem::Heartbeats => self.heartbeats,
        }
    }

    fn record(&mut self, message: &Message) {
        match message {
            Message::Motion { .. } => self.motion += 1,
            Message::Key { .. } => self.keys += 1,
            Message::Button { .. } => self.buttons += 1,
            Message::Wheel { .. } => self.wheel += 1,
            Message::Snapshot { .. } => self.snapshots += 1,
            Message::Heartbeat => self.heartbeats += 1,
            // Neither of these is a subsystem here. An acknowledgement is
            // host-to-client traffic, counted as such where it is decided, and
            // a release is counted by the state machine, which is the only
            // place that knows what caused it.
            Message::Ack { .. }
            | Message::ReleaseAll { .. }
            | Message::GamepadAttach { .. }
            | Message::GamepadDetach { .. }
            | Message::GamepadState(_)
            | Message::GamepadFeedback(_) => {}
        }
    }
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

    let mut state = HostState::new(session);
    let mut counts = Counts::default();
    let mut histogram =
        Histogram::<u64>::new_with_bounds(1, MAX_NANOS, 3).expect("valid histogram bounds");
    let mut first_error: Option<DecodeError> = None;
    let mut peer: Option<SocketAddr> = None;

    let mut buffer = [0u8; MAX_DATAGRAM];
    let deadline = Timestamp::now().add(Nanos::from_millis_f64(cli.seconds * 1_000.0));
    // The host's own datagram counter for the return path, unrelated to the
    // client's: it says which acknowledgement this is, so the client can see
    // reorder and loss on the way back.
    let mut ack_sequence: u32 = 0;
    let mut ack_buffer = [0u8; MAX_DATAGRAM];
    // When the last datagram this session arrived, or `None` while the client
    // is not considered live: before the first one, and after an expiry has
    // already swept. Disarming it that way is what keeps a silent run from
    // sweeping once per poll for the rest of its length.
    let mut last_datagram: Option<Timestamp> = None;

    while Timestamp::now() < deadline {
        // Liveness first, because a run that goes quiet reaches this point
        // through the read timeout and never through a datagram.
        if let Some(last) = last_datagram
            && Timestamp::now().saturating_since(last).get() >= CLIENT_TIMEOUT.as_nanos() as u64
        {
            state.expire(|action| backend.deliver(action));
            last_datagram = None;
        }

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
        let outcome = state.apply_datagram(&datagram, |action| {
            if let Action::Motion { dx, dy } = action {
                counts.dx += dx as i64;
                counts.dy += dy as i64;
            }
            backend.deliver(action);
        });
        if outcome == Outcome::WrongSession {
            counts.wrong_session += 1;
            // Deliberately not evidence of liveness. A datagram the host
            // cannot attribute to this session says nothing about whether the
            // client this session belongs to is still there, and letting one
            // refresh the clock would let a departed peer's retransmissions
            // hold the release off indefinitely.
            continue;
        }
        last_datagram = Some(received);
        counts.record(&datagram.message);
        match outcome {
            Outcome::Applied => counts.applied += 1,
            Outcome::Duplicate => counts.duplicates += 1,
            // Counted apart from duplicates. A duplicate is a retransmission
            // of something already applied; a superseded event is one that was
            // never applied and never will be, because a release has since
            // declared its world gone. Pooling them would hide a client still
            // sending presses after it said it had let go.
            Outcome::Superseded => counts.superseded += 1,
            Outcome::Ignored => counts.ignored += 1,
            Outcome::Stale | Outcome::WrongSession => {}
        }

        // Only datagrams that reached the injector are timed. A rejected
        // session id or a malformed datagram costs a decode and nothing else,
        // and mixing that in would flatter the distribution.
        histogram.saturating_record(Timestamp::now().saturating_since(received).get());

        // Sent after the histogram is recorded, so the cost of the reply is
        // not folded into the receive-and-inject figure the run exists to
        // measure.
        if datagram.message.reliability() == Reliability::Reliable
            && outcome.owes_ack()
            && let Some(ack) = state.acknowledgement()
        {
            let reply = Datagram {
                // The client's session, not a number of the host's choosing:
                // an acknowledgement carrying anything else would be dropped
                // by the peer it is meant for.
                session: datagram.session,
                sequence: Sequence(ack_sequence),
                sent_at_ns: Timestamp::now().as_nanos(),
                message: Message::Ack {
                    top: ack.top,
                    missing: ack.missing,
                },
            };
            ack_sequence = ack_sequence.wrapping_add(1);
            let len = encode(&reply, &mut ack_buffer).expect("an ack fits in MAX_DATAGRAM");
            // Back to the address this datagram came from, on the socket it
            // arrived on, because that pair is the only return path the
            // client is listening on.
            match socket.send_to(&ack_buffer[..len], from) {
                Ok(_) => counts.acks += 1,
                Err(_) => counts.ack_failures += 1,
            }
        }
    }

    report(&counts, &state, &backend, &histogram, peer, first_error);
    if unmet(&cli.expect, &counts) {
        return ExitCode::from(4);
    }
    ExitCode::SUCCESS
}

/// Names every declared subsystem that stayed at zero, reporting whether any
/// did.
///
/// Checked against the counters rather than taken on trust, because a run that
/// received nothing still prints a clean report and two gate arms have already
/// passed that way. Not observed must never equal pass.
fn unmet(expected: &[Subsystem], counts: &Counts) -> bool {
    let mut missing = false;
    for subsystem in expected {
        if counts.observed(*subsystem) == 0 {
            eprintln!(
                "input-inject-probe: --expect {} was declared and nothing was observed",
                subsystem.label()
            );
            missing = true;
        }
    }
    missing
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
    println!("superseded       {:>10}", counts.superseded);
    println!("stale snapshot   {:>10}", state.stale_snapshots());
    println!("wrong session    {:>10}", counts.wrong_session);
    println!("decode errors    {:>10}", counts.decode_errors);
    println!("host to client   {:>10}", counts.ignored);
    println!("acks sent        {:>10}", counts.acks);
    println!("ack failures     {:>10}", counts.ack_failures);
    let by_kind = backend.calls_by_kind();
    println!("sendinput calls  {:>10}", backend.calls());
    // Attributed, because an excess of calls over applied messages is only
    // innocent when it can be accounted for. One ReleaseAll emits an action
    // per held thing; a duplicate wheel emitting a second notch would be the
    // failure the whole design exists to prevent, and the totals alone cannot
    // tell those apart.
    println!(
        "  by kind        motion {}, key {}, button {}, wheel {}",
        by_kind[0], by_kind[1], by_kind[2], by_kind[3]
    );
    println!("refused          {:>10}", backend.refused());
    println!("injected dx      {:>10}", counts.dx);
    println!("injected dy      {:>10}", counts.dy);
    println!("motion           {:>10}", counts.motion);
    println!("keys             {:>10}", counts.keys);
    println!("buttons          {:>10}", counts.buttons);
    println!("wheel            {:>10}", counts.wheel);
    println!("snapshots        {:>10}", counts.snapshots);
    println!("heartbeats       {:>10}", counts.heartbeats);
    // Both causes, always, and never summed. They end in the same empty state
    // and mean opposite things about the client: one is a client that said
    // goodbye, the other one that stopped answering, and an operator reading
    // a single release count cannot tell which happened.
    let releases = state.releases();
    println!("released, asked  {:>10}", releases.requested);
    println!("released, expired{:>10}", releases.expired);
    if let Some(error) = first_error {
        println!("first decode error: {error}");
    }
    // Always stated, never only on failure. A gate that greps for a line
    // printed just when something went wrong cannot tell a clean run from a
    // probe that died before reaching this point, and a stuck key is the one
    // failure this whole design exists to prevent.
    println!(
        "still held: keys {:?}, buttons {:#07b}",
        state.held_keys(),
        state.held_buttons()
    );
    // The pair an operator holds against the client's unacked count. They
    // describe the same events from opposite ends, so a client still waiting
    // on an id at or below `top` whose bit reads applied means the
    // acknowledgement was lost on the way back rather than the event on the
    // way in.
    match state.acknowledgement() {
        Some(ack) => println!(
            "acknowledged: top {}, missing {:#034b}",
            ack.top.0, ack.missing
        ),
        None => println!("acknowledged: no reliable event arrived"),
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

    // A maximum is one event and says nothing about how often it happens, which
    // is the whole question when the number is twelve times the p99. Counting
    // the crossings turns an anecdote into a rate, and a rate is what decides
    // whether the outlier is worth building a virtual HID device to avoid.
    for threshold in [500_000u64, 1_000_000, 2_000_000, 5_000_000] {
        let over = histogram.len() - histogram.count_between(0, threshold - 1);
        println!(
            "  over {:>4.1} ms {:>8}   {:>6.3} % of {}",
            threshold as f64 / 1_000_000.0,
            over,
            100.0 * over as f64 / histogram.len() as f64,
            histogram.len(),
        );
    }
}

fn micros(nanos: u64) -> f64 {
    nanos as f64 / 1_000.0
}
