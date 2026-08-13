//! One captured input event, one UDP datagram, and what that costs locally.
//!
//! This is the send end of the input path, deliberately as plain as it can be:
//! there is no pacing, no coalescing and no timer anywhere near the send. The
//! measurement it produces is the baseline that any later batching has to beat,
//! and a batching send loop compared against a paced one would tell nobody
//! anything.
//!
//! What is measured is the interval from reading the clock in the event
//! callback to `send_to` returning, entirely on this machine's monotonic clock.
//! Nothing here subtracts a host timestamp from a local one: the two machines
//! share no epoch, and the difference would be a clock offset dressed up as a
//! latency.
//!
//! The total dx and dy are printed because they are the one figure an operator
//! can compare across machines. Motion is additive, so the sum of what was sent
//! must equal the sum of what was injected, whatever order the datagrams
//! arrived in and however many were lost. A mismatch is loss; a mismatch that
//! grows is a bug. Wheel notches are printed for the same reason and are read
//! the same way, since a notch is additive too and a host that moved by three
//! where one was sent has deduplicated nothing.
//!
//! Keys and buttons are counted differently, because they are not additive and
//! losing one is not a smoothed-over error. The figure that matters for them is
//! whether every press was followed by a release, since an unmatched press is a
//! key or a button held down on the host after the player has let go, and it is
//! printed whether it is good or bad: a run that only reports its faults teaches
//! an operator to read silence as success.
//!
//! There are two ways to produce input here. Capturing real events needs Input
//! Monitoring, and a machine that has not granted it delivers no events at all
//! rather than an error, which is why a run that saw nothing says so instead of
//! reporting a clean zero. The synthetic cycles need no permission and no human,
//! and exist so the wire format and the host's injection can be exercised on
//! their own; they are the only paced thing in this file, because there they
//! stand in for the player's fingers rather than for the capture path.
//!
//! AppKit will not deliver events to a process that is not an application, so an
//! `NSApplication` is created and its run loop is turned by hand. Turning it by
//! hand rather than calling `run` is what lets the probe stop at a deadline
//! without a timer.
//!
//! None of the reliability arithmetic is in this file. The retransmission
//! ladder, the acknowledgement window, the snapshot cadence and the heartbeat
//! live in the library, where the clock is a variable and every deadline can be
//! tested without waiting for it. What this file owns is the loop that drives
//! them, the socket they share with the sends, and the counters an operator
//! reads afterwards. The one that decides whether a run was clean is the last of
//! them: how many reliable events were still unacknowledged at exit.
//!
//! A run also declares what it meant to exercise, with `--expect`, and fails
//! with status 4 if any of it stayed at zero. That exists because a gate arm
//! can pass having exercised nothing at all, and a green run that sent no
//! wheel event is not evidence that the wheel works: not observed must never
//! equal pass.
//!
//! The capture and focus state machine itself is the library's rather than this
//! file's. What lives here is the loop that drives it, the window server calls
//! it asks for, and a `--cycles` mode that walks it through whole captures and
//! every one of its exits with no person, no permission and no display
//! involved. That mode is how the ordering the release barrier depends on is
//! demonstrated rather than asserted: the ids themselves say whether anything
//! was admitted after the release took its own.

use std::cell::RefCell;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use hdrhistogram::Histogram;
use lanplay_input_capture::{
    Capture, ExitCause, FocusWatcher, Heartbeat, INPUT_PORT, Keyboard, Machine, MouseEvent,
    Reliable, ScanCode,
    capture::{
        Action, CAPTURE_BUTTON, LEFT_COMMAND, LEFT_CONTROL, LEFT_OPTION, Outcome, State, TAB,
    },
    heartbeat::HEARTBEAT_INTERVAL,
    reliable::MAX_RETRANSMISSIONS,
};
use lanplay_input_protocol::{
    Button, Datagram, EventId, MAX_DATAGRAM, Message, Sequence, SessionId, decode, encode,
};
use lanplay_telemetry::{Nanos, Timestamp};
use objc2::MainThreadMarker;
use objc2::rc::autoreleasepool;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

/// Widest interval the histogram holds: one second. A send that took longer
/// than that is a stall and not a latency, and is clipped and counted.
const MAX_NANOS: u64 = 1_000_000_000;

/// Three significant figures, because a send on the loopback lands around a
/// microsecond and coarser buckets collapse the whole distribution into a few
/// cells.
const SIGNIFICANT_FIGURES: u8 = 3;

/// How long one turn of the run loop waits for an event. Short, because the
/// retransmission deadlines are only looked at between turns and the first of
/// them falls twenty milliseconds after a send: a turn longer than that would
/// make every repair late. An idle probe therefore wakes a couple of hundred
/// times a second to do nothing, which is cheaper than a lost key release.
const TURN_SECONDS: f64 = 0.005;

/// The macOS virtual key codes for W, A, S and D, in the order the synthetic
/// cycle walks them. Virtual codes rather than scan codes so the cycle goes
/// through the same table a real key would, which is what makes it a test of
/// the whole send path and not just of the socket.
const SYNTHETIC_KEYS: [u16; 4] = [0x0D, 0x00, 0x01, 0x02];

/// The buttons the synthetic cycle walks, all five of them, because the two a
/// hand reaches for are not the two a wire format gets wrong.
const SYNTHETIC_BUTTONS: [Button; 5] = [
    Button::Left,
    Button::Right,
    Button::Middle,
    Button::X1,
    Button::X2,
];

/// What one synthetic wheel event carries. One notch, always the same way, so
/// the total a run reports is a number a host can be subtracted against: three
/// notches on the host where one was sent is a retransmission the host applied
/// three times, and that is the whole reason for sending them in one direction
/// rather than in a tidy back-and-forth that would sum to nothing.
const SYNTHETIC_NOTCH: i16 = 1;

/// The virtual key codes the capture cycles press. Virtual codes rather than
/// scan codes for the same reason the synthetic key cycle uses them: the press
/// then goes through the same table a real key would.
///
/// The keys of the release combination are deliberately absent from this list.
/// Those come from the recognizer's own constants, so a cycle cannot fire a
/// combination the recognizer does not have and a run cannot pass by pressing
/// keys nothing was listening for.
const VK_W: u16 = 0x0D;
const VK_S: u16 = 0x01;
const VK_LEFT_SHIFT: u16 = 0x38;

/// How long a read waits before the loop moves on. See [`Sink::receive`] for
/// why the receive has a timeout rather than being polled or blocking.
const RECV_TIMEOUT: Duration = Duration::from_millis(1);

/// How long to keep pumping after the last send.
///
/// Without this the unacknowledged figure would always count the `ReleaseAll`
/// sent a microsecond before it was read, and the one number a fault-injection
/// run turns on would never be zero. Bounded because a host that has gone away
/// is exactly the case this exists to survive, and long enough for the whole
/// retransmission ladder of the last event to run out, so that whatever is
/// still outstanding at the end really was given up on rather than merely not
/// waited for.
const LINGER: Nanos = Nanos::from_millis(500);

/// What a run declares it meant to exercise.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Subsystem {
    Motion,
    Keys,
    Buttons,
    Wheel,
    Acks,
    Snapshots,
    Heartbeats,
    Captures,
    Suppressed,
}

impl Subsystem {
    const ALL: [Subsystem; 9] = [
        Subsystem::Motion,
        Subsystem::Keys,
        Subsystem::Buttons,
        Subsystem::Wheel,
        Subsystem::Acks,
        Subsystem::Snapshots,
        Subsystem::Heartbeats,
        Subsystem::Captures,
        Subsystem::Suppressed,
    ];

    const fn name(self) -> &'static str {
        match self {
            Subsystem::Motion => "motion",
            Subsystem::Keys => "keys",
            Subsystem::Buttons => "buttons",
            Subsystem::Wheel => "wheel",
            Subsystem::Acks => "acks",
            Subsystem::Snapshots => "snapshots",
            Subsystem::Heartbeats => "heartbeats",
            Subsystem::Captures => "captures",
            Subsystem::Suppressed => "suppressed",
        }
    }

    /// How much of this the run actually did. Zero is the only value that
    /// matters, so these are the counts of events offered rather than of
    /// datagrams the socket accepted: a subsystem that was exercised against a
    /// host that is not there was still exercised.
    fn count(self, sink: &Sink) -> u64 {
        let counts = sink.reliable.counts();
        let machine = sink.machine.counts();
        match self {
            Subsystem::Motion => sink.motion_events,
            Subsystem::Keys => sink.presses + sink.releases,
            Subsystem::Buttons => sink.button_presses + sink.button_releases,
            Subsystem::Wheel => sink.wheel_events,
            Subsystem::Acks => counts.acks,
            Subsystem::Snapshots => counts.snapshots,
            Subsystem::Heartbeats => sink.heartbeat.sent(),
            Subsystem::Captures => machine.captures,
            // Both halves of the suppression claim in one figure: a click that
            // asked for capture instead of firing a weapon, and a combination
            // whose keys never left. A run that captured nothing suppressed
            // nothing either, which is exactly the silence this flag exists to
            // turn into an exit status.
            Subsystem::Suppressed => {
                machine.capture_clicks_suppressed + machine.hotkey_events_suppressed
            }
        }
    }
}

fn parse_subsystem(text: &str) -> Result<Subsystem, String> {
    Subsystem::ALL
        .into_iter()
        .find(|subsystem| subsystem.name() == text)
        .ok_or_else(|| {
            let names: Vec<&str> = Subsystem::ALL.iter().map(|each| each.name()).collect();
            format!(
                "{text} is not a subsystem; expected one of {}",
                names.join(", ")
            )
        })
}

/// Why a `ReleaseAll` was sent.
///
/// Counted apart rather than summed, because the invariant is that every one
/// of these converges the host to nothing held, and a run that never lost focus
/// has not exercised the same path as one that did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReleaseCause {
    /// This process stopped being the one the input is for.
    FocusLost,
    /// The player typed the combination that asks for their own machine back.
    ReleaseHotkey,
    /// The player asked macOS for the application switcher.
    CommandTab,
    /// The session stopped working under a running capture.
    SessionFailure,
    /// The capture was given up deliberately, so the mouse belongs to this
    /// machine again.
    CaptureReleased,
    /// The run ended.
    Exit,
}

impl ReleaseCause {
    const ALL: [ReleaseCause; 6] = [
        ReleaseCause::FocusLost,
        ReleaseCause::ReleaseHotkey,
        ReleaseCause::CommandTab,
        ReleaseCause::SessionFailure,
        ReleaseCause::CaptureReleased,
        ReleaseCause::Exit,
    ];

    const fn name(self) -> &'static str {
        match self {
            ReleaseCause::FocusLost => "focus lost",
            ReleaseCause::ReleaseHotkey => "release hotkey",
            ReleaseCause::CommandTab => "command-tab",
            ReleaseCause::SessionFailure => "session failure",
            ReleaseCause::CaptureReleased => "capture released",
            ReleaseCause::Exit => "exit",
        }
    }

    const fn index(self) -> usize {
        match self {
            ReleaseCause::FocusLost => 0,
            ReleaseCause::ReleaseHotkey => 1,
            ReleaseCause::CommandTab => 2,
            ReleaseCause::SessionFailure => 3,
            ReleaseCause::CaptureReleased => 4,
            ReleaseCause::Exit => 5,
        }
    }
}

/// Every exit the state machine takes owes a release, and the cause survives
/// the crossing so that the machine's count of exits and this file's count of
/// releases can be read against each other rather than believed separately.
impl From<ExitCause> for ReleaseCause {
    fn from(cause: ExitCause) -> ReleaseCause {
        match cause {
            ExitCause::FocusLost => ReleaseCause::FocusLost,
            ExitCause::ReleaseHotkey => ReleaseCause::ReleaseHotkey,
            ExitCause::CommandTab => ReleaseCause::CommandTab,
            ExitCause::SessionFailure => ReleaseCause::SessionFailure,
        }
    }
}

/// A window server call the machine asked for and has not had.
///
/// Deferred for the reason the focus watcher defers a loss: changing the
/// cursor's association inside AppKit's dispatch of the event that asked for it
/// would run a window server call underneath one. Nothing is lost by waiting,
/// because the machine has already closed admission by the time one of these
/// is outstanding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    /// Take the cursor away from the mouse, then say which way it went.
    Detach,
    /// Give the cursor back, the last step of an exit whose release has gone.
    Restore,
}

#[derive(Parser)]
#[command(
    about = "Sends one input-protocol datagram per macOS mouse or key event.",
    long_about = None
)]
struct Args {
    /// Where to send. A bare host or address is given the input port, 5006.
    #[arg(long, value_name = "ADDR[:PORT]")]
    send_to: String,

    /// How long to capture before reporting.
    #[arg(long, default_value_t = 30)]
    seconds: u64,

    /// Must match the host, which drops and counts anything else.
    #[arg(long, default_value_t = 1)]
    session_id: u32,

    /// Also capture the keyboard. Needs Input Monitoring.
    #[arg(long)]
    keys: bool,

    /// Send a deterministic W, A, S, D cycle instead of capturing anything.
    ///
    /// Refuses to run alongside `--keys`, because the host deduplicates on event
    /// ids and two sources minting them from two counters would hand it the same
    /// id for two different keys.
    #[arg(long, conflicts_with = "keys")]
    synthetic_keys: bool,

    /// Send a deterministic cycle of all five mouse buttons instead of
    /// capturing anything. Combines with the other two synthetic cycles, which
    /// then take turns.
    #[arg(long, conflicts_with = "keys")]
    synthetic_buttons: bool,

    /// Send a deterministic run of wheel notches instead of capturing
    /// anything. Every notch goes the same way, so the total is a figure a host
    /// can be compared against.
    #[arg(long, conflicts_with = "keys")]
    synthetic_wheel: bool,

    /// Synthetic events per second, counting each press and each release.
    /// Paces every synthetic cycle, not only the keys it is named for.
    #[arg(long, default_value_t = 20, value_name = "PER_SECOND")]
    key_rate: u32,

    /// What this run was meant to exercise, comma separated. Any of them left
    /// at zero prints a line and exits 4, because a gate arm that observed
    /// nothing must not be able to pass.
    #[arg(
        long,
        value_name = "LIST",
        value_delimiter = ',',
        value_parser = parse_subsystem,
    )]
    expect: Vec<Subsystem>,

    /// Drive the capture and focus state machine through this many synthetic
    /// capture cycles instead of capturing anything, walking every exit the
    /// machine has. Needs no permission, no display and no hand.
    #[arg(
        long,
        default_value_t = 0,
        value_name = "N",
        conflicts_with_all = ["keys", "synthetic_keys", "synthetic_buttons", "synthetic_wheel"],
    )]
    cycles: u64,
}

/// Everything the event callback touches, and everything the report reads.
struct Sink {
    socket: UdpSocket,
    target: SocketAddr,
    session: SessionId,
    next_sequence: u32,
    motion_events: u64,
    datagrams: u64,
    failed: u64,
    clipped: u64,
    total_dx: i64,
    total_dy: i64,
    cost: Histogram<u64>,
    key_datagrams: u64,
    presses: u64,
    releases: u64,
    button_presses: u64,
    button_releases: u64,
    /// Wheel events offered, and the notches they carried on each axis. The
    /// notches are the figure a host is compared against; the event count is
    /// what says the wheel was exercised at all.
    wheel_events: u64,
    total_notches_x: i64,
    total_notches_y: i64,
    /// What is believed held, what has not been acknowledged, and when the next
    /// snapshot is due. The held set lives in there rather than beside it so
    /// that the count reported here and the set a snapshot describes cannot
    /// drift apart.
    reliable: Reliable,
    /// Liveness, on its own timer. Beside the reliability layer rather than
    /// inside it because a heartbeat proves the session exists and a snapshot
    /// proves what it holds, and those are different claims.
    heartbeat: Heartbeat,
    releases_by_cause: [u64; ReleaseCause::ALL.len()],
    /// Keys and buttons held when control was last given up, read before the
    /// `ReleaseAll` that clears them. The worst of those readings rather than
    /// the last, so a focus loss in the middle of a run cannot hide what it was
    /// holding by having cleared it long before the exit.
    held_at_stop: usize,
    buttons_at_stop: u32,
    /// A press for a key already believed held, and a release for one that was
    /// not. Either means the pressed set and the player have diverged, and both
    /// are separate from the held count because a run can end balanced and still
    /// have been wrong in the middle.
    double_presses: u64,
    unmatched_releases: u64,
    /// The machine that decides whether any of the above is allowed to leave.
    /// Here rather than beside the sink because the event path has to consult it
    /// and `--expect` has to be able to count it.
    machine: Machine,
    pending: Option<Pending>,
    cursor_detaches: u64,
    cursor_restores: u64,
    /// The id of the last release, which is the barrier the host measures every
    /// reliable event against, and the id of the last reliable event of any
    /// kind. Two numbers rather than a claim: they are what says a recaptured
    /// session's first event outranks the release that ended the capture before
    /// it, and would say so just as plainly if it did not.
    last_release: Option<EventId>,
    last_reliable: Option<EventId>,
}

impl Sink {
    /// Builds one datagram and sends it. Shared by the event path, the
    /// retransmissions, the snapshots and the heartbeats, because they are the
    /// same socket and the same wire format and only what is measured
    /// afterwards differs.
    ///
    /// Called from the AppKit event callback, so it allocates nothing: the
    /// datagram is built in a stack buffer of the largest size the wire format
    /// can produce.
    fn send_now(&mut self, message: Message, at: Timestamp) -> bool {
        let datagram = Datagram {
            session: self.session,
            sequence: Sequence(self.next_sequence),
            sent_at_ns: at.as_nanos(),
            message,
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);

        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(&datagram, &mut buffer).expect("every message here fits in MAX_DATAGRAM");
        // Counted rather than reported per event, because printing from the
        // event path would itself become the thing being measured.
        let sent = self.socket.send_to(&buffer[..len], self.target).is_ok();
        if !sent {
            self.failed += 1;
        }
        sent
    }

    /// The event path: one send, with the interval from reading the clock to
    /// the syscall returning recorded.
    ///
    /// Retransmissions, snapshots and heartbeats deliberately do not come
    /// through here. They leave on a deadline rather than on an event, so
    /// folding their cost into this histogram would describe neither of the
    /// two.
    fn emit(&mut self, message: Message, at: Timestamp) -> bool {
        let sent = self.send_now(message, at);
        // Measured after the syscall returns whether it succeeded or not: the
        // cost of a refused send is still the cost this path pays.
        let cost = Timestamp::now().saturating_since(at);
        if cost.get() > MAX_NANOS {
            self.clipped += 1;
        }
        self.cost.saturating_record(cost.get());
        sent
    }

    fn send_motion(&mut self, dx: i32, dy: i32, at: Timestamp) {
        self.motion_events += 1;
        if self.emit(Message::Motion { dx, dy }, at) {
            self.datagrams += 1;
            self.total_dx += i64::from(dx);
            self.total_dy += i64::from(dy);
        }
    }

    /// The id comes from the reliability layer, which is the only thing in this
    /// process minting them: the host deduplicates on that id, so two counters
    /// would hand it the same id for two different keys and it would inject one
    /// of them and discard the other as a retransmission.
    fn send_key(&mut self, scan: ScanCode, down: bool, at: Timestamp) {
        // Asked before the layer folds the key in, since afterwards the answer
        // is whatever this event just made it.
        let held = self.reliable.holds_key(scan);
        if down {
            self.presses += 1;
            if held {
                self.double_presses += 1;
            }
        } else {
            self.releases += 1;
            if !held {
                self.unmatched_releases += 1;
            }
        }

        // Bookkeeping before the send, and on offer rather than on success: a
        // key the socket refused is still a key the player pressed, and hiding
        // it would make a failing network look like a balanced run.
        let message = self.reliable.key(scan, down, at);
        self.last_reliable = message.event_id();
        if self.emit(message, at) {
            self.key_datagrams += 1;
        }
    }

    /// A button is state, so it goes through the same layer a key does and
    /// lands in the same held mask a snapshot describes.
    fn send_button(&mut self, button: Button, down: bool, at: Timestamp) {
        if down {
            self.button_presses += 1;
        } else {
            self.button_releases += 1;
        }
        let message = self.reliable.button(button, down, at);
        self.last_reliable = message.event_id();
        self.emit(message, at);
    }

    /// A notch is reliable and stateless, so it is retransmitted until the host
    /// admits it and never appears in a snapshot: there is no such thing as a
    /// held wheel.
    fn send_wheel(&mut self, dx: i16, dy: i16, at: Timestamp) {
        self.wheel_events += 1;
        self.total_notches_x += i64::from(dx);
        self.total_notches_y += i64::from(dy);
        let message = self.reliable.wheel(dx, dy, at);
        self.last_reliable = message.event_id();
        self.emit(message, at);
    }

    /// The gate every captured event passes through.
    ///
    /// The machine decides and this sends, which is what makes the decision
    /// checkable: nothing here can admit an event the machine refused, and
    /// nothing the machine refused ever reaches the reliability layer, so a
    /// refused event cannot be holding an id.
    fn offer_mouse(&mut self, event: MouseEvent, at: Timestamp) {
        match event {
            MouseEvent::Motion { dx, dy } => {
                if self.machine.admit() {
                    self.send_motion(dx, dy, at);
                }
            }
            MouseEvent::Wheel { dx, dy } => {
                if self.machine.admit() {
                    self.send_wheel(dx, dy, at);
                }
            }
            MouseEvent::Button { button, down } => {
                let outcome = self.machine.button(button, down);
                self.send_flushed(&outcome, at);
                if outcome.admitted() {
                    self.send_button(button, down, at);
                }
                self.follow(outcome.action(), at);
            }
        }
    }

    fn offer_key(&mut self, scan: ScanCode, down: bool, at: Timestamp) {
        let outcome = self.machine.key(scan, down);
        self.send_flushed(&outcome, at);
        if outcome.admitted() {
            self.send_key(scan, down, at);
        }
        self.follow(outcome.action(), at);
    }

    /// The presses the recognizer had been holding while it decided, sent ahead
    /// of the event that ended the hold so the host sees them in the order the
    /// player made them.
    fn send_flushed(&mut self, outcome: &Outcome, at: Timestamp) {
        for scan in outcome.flushed() {
            self.send_key(*scan, true, at);
        }
    }

    /// The work the machine handed back.
    ///
    /// The release leaves here and now, on the event path, because the barrier
    /// means everything the capture sent only if its id is taken with admission
    /// already closed and nothing in between. The cursor is the only part that
    /// waits for the loop, and by then nothing more can be admitted anyway.
    fn follow(&mut self, action: Action, at: Timestamp) {
        match action {
            Action::None => {}
            Action::Enter => self.pending = Some(Pending::Detach),
            Action::Exit(cause) => {
                self.release(cause.into(), at);
                self.pending = Some(Pending::Restore);
            }
        }
    }

    /// The reliability half of every loop in this file: acknowledgements in,
    /// then whatever the deadlines say is due out.
    ///
    /// Separate from the event path on purpose. An event still leaves the
    /// moment it arrives, and only the repairs and the proof of life are on a
    /// timer, because a deadline is the whole point of them. The two timers are
    /// asked separately and are two different things: one says the session
    /// exists, the other says what it holds.
    fn pump(&mut self, now: Timestamp) {
        self.receive();
        while let Some(message) = self.reliable.next_due(now) {
            self.send_now(message, Timestamp::now());
        }
        if let Some(snapshot) = self.reliable.snapshot_due(now) {
            self.send_now(snapshot, Timestamp::now());
        }
        if let Some(heartbeat) = self.heartbeat.due(now) {
            self.send_now(heartbeat, Timestamp::now());
        }
    }

    /// Drains whatever the host has sent, on the socket the sends go out on so
    /// that no second port has to be agreed on or opened through a firewall.
    ///
    /// The read timeout is what keeps this off the send path. A blocking read
    /// would hold the sends behind a host with nothing to say, and a
    /// non-blocking one would turn an idle loop into a spin, so a millisecond
    /// is the most a captured event can be delayed by a read and it is far
    /// below the shortest retransmission deadline.
    fn receive(&mut self) {
        let mut buffer = [0u8; MAX_DATAGRAM];
        // Only from the address the sends went to, and only for this session.
        // The session id on its own is a weak filter, since anything that has
        // seen one datagram can copy it, and an acknowledgement retires events:
        // a forged one would silence a retransmission that was needed.
        while let Ok((len, from)) = self.socket.recv_from(&mut buffer) {
            if from != self.target {
                continue;
            }
            let Ok(datagram) = decode(&buffer[..len]) else {
                continue;
            };
            if datagram.session != self.session {
                continue;
            }
            if let Message::Ack { top, missing } = datagram.message {
                self.reliable.ack(top, missing);
            }
        }
    }

    /// Reads what was held, then tells the host to let go of everything.
    ///
    /// The counts are taken before the release rather than after, because they
    /// are the diagnostic and the release is the repair: read afterwards they
    /// would be zero for every run, however badly the run had gone.
    ///
    /// Called for every cause the invariant names, and safe to call twice in a
    /// row: the host ends in the same empty state whether it receives one of
    /// these or ten.
    fn release(&mut self, cause: ReleaseCause, at: Timestamp) {
        self.held_at_stop = self.held_at_stop.max(self.reliable.keys().held().count());
        self.buttons_at_stop = self
            .buttons_at_stop
            .max(self.reliable.buttons().count_ones());
        self.releases_by_cause[cause.index()] += 1;
        let message = self.reliable.release_all(at);
        // Both, because a release is a reliable event like any other and is
        // also the barrier every event below it is measured against.
        self.last_reliable = message.event_id();
        self.last_release = self.last_reliable;
        self.send_now(message, at);
    }

    /// Whether the keys and buttons this run sent add up: nothing left down, no
    /// press on top of a press, no release without one.
    fn input_balanced(&self) -> bool {
        self.held_at_stop == 0
            && self.buttons_at_stop == 0
            && self.double_presses == 0
            && self.unmatched_releases == 0
    }
}

/// Microseconds, because every interval here is far below a millisecond and in
/// milliseconds they all print as zero.
fn micros(value: Nanos) -> f64 {
    value.get() as f64 / 1_000.0
}

/// Accepts a bare host as well as `host:port`, so an operator does not have to
/// remember which of the project's three ports this one is.
fn resolve(spec: &str) -> Result<SocketAddr, String> {
    let with_port = if spec.parse::<SocketAddr>().is_ok() || spec.rfind(':').is_some() {
        spec.to_string()
    } else {
        format!("{spec}:{INPUT_PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|why| format!("{spec} is not an address: {why}"))?
        .next()
        .ok_or_else(|| format!("{spec} resolved to nothing"))
}

fn new_sink(socket: UdpSocket, target: SocketAddr, session_id: u32) -> Sink {
    let now = Timestamp::now();
    Sink {
        socket,
        target,
        session: SessionId(session_id),
        next_sequence: 0,
        motion_events: 0,
        datagrams: 0,
        failed: 0,
        clipped: 0,
        total_dx: 0,
        total_dy: 0,
        cost: Histogram::new_with_bounds(1, MAX_NANOS, SIGNIFICANT_FIGURES)
            .expect("valid histogram bounds"),
        key_datagrams: 0,
        presses: 0,
        releases: 0,
        button_presses: 0,
        button_releases: 0,
        wheel_events: 0,
        total_notches_x: 0,
        total_notches_y: 0,
        reliable: Reliable::new(now),
        heartbeat: Heartbeat::new(now),
        releases_by_cause: [0; ReleaseCause::ALL.len()],
        held_at_stop: 0,
        buttons_at_stop: 0,
        double_presses: 0,
        unmatched_releases: 0,
        machine: Machine::new(),
        pending: None,
        cursor_detaches: 0,
        cursor_restores: 0,
        last_release: None,
        last_reliable: None,
    }
}

/// The key and button half of the report, printed by both modes because the
/// question it answers is the same one either way.
fn report_input(sink: &Sink) {
    println!(
        "key datagrams {}  presses {}  releases {}",
        sink.key_datagrams, sink.presses, sink.releases
    );
    println!(
        "button presses {}  releases {}",
        sink.button_presses, sink.button_releases
    );
    println!(
        "wheel events {}  notches sent dx {}  dy {}",
        sink.wheel_events, sink.total_notches_x, sink.total_notches_y
    );
    // Held when control was given up rather than now: a `ReleaseAll` has been
    // sent since, and reporting after it would be reporting the repair.
    println!(
        "keys still held {}  buttons still held {}  every press matched by a release: {}",
        sink.held_at_stop,
        sink.buttons_at_stop,
        if sink.input_balanced() { "yes" } else { "no" }
    );
    if sink.double_presses > 0 || sink.unmatched_releases > 0 {
        println!(
            "{} presses landed on a key already held and {} releases had no press",
            sink.double_presses, sink.unmatched_releases
        );
    }
}

/// The safety-invariant half of the report: how many times control was lost,
/// and what lost it.
///
/// Every cause is printed even at zero, because which of them a run exercised
/// is the point. A run that reported four releases without saying that none of
/// them came from a focus loss would look like it had tested the path that
/// matters most and had not.
fn report_releases(sink: &Sink) {
    let total: u64 = sink.releases_by_cause.iter().sum();
    let by_cause: Vec<String> = ReleaseCause::ALL
        .iter()
        .map(|cause| format!("{} {}", cause.name(), sink.releases_by_cause[cause.index()]))
        .collect();
    println!("releases sent {total}  by cause: {}", by_cause.join("  "));
    println!(
        "heartbeats sent {} at one every {} ms",
        sink.heartbeat.sent(),
        HEARTBEAT_INTERVAL.get() / 1_000_000
    );
}

/// The capture path's half of the report.
///
/// The suppression figures are the point of it. They are the evidence for two
/// claims that are otherwise only plausible: that the click asking for capture
/// did not also fire a weapon, and that nothing reached the host while the
/// mouse belonged to this machine. Every cause is printed even at zero, because
/// which of the four a run exercised is what a reader needs to know.
fn report_machine(sink: &Sink) {
    let counts = sink.machine.counts();
    println!(
        "capture cycles {}  capture clicks suppressed {}  hotkey events suppressed {}",
        counts.captures, counts.capture_clicks_suppressed, counts.hotkey_events_suppressed
    );
    let by_cause: Vec<String> = ExitCause::ALL
        .iter()
        .map(|cause| format!("{} {}", cause.name(), counts.exits[cause.index()]))
        .collect();
    println!(
        "exits {}  by cause: {}",
        counts.exits.iter().sum::<u64>(),
        by_cause.join("  ")
    );
    println!(
        "releases the machine asked for {}  events refused while uncaptured {}  entries that \
         failed closed {}",
        counts.releases, counts.refused, counts.entries_failed
    );
    println!(
        "cursor taken {} times and given back {}  focus losses the machine saw {}",
        sink.cursor_detaches,
        sink.cursor_restores,
        sink.machine.focus().losses()
    );
    println!("the machine ended {}", sink.machine.state().name());
}

/// The reliability half of the report, printed by both modes.
///
/// Every figure is here even when it is zero, because the interesting runs are
/// the ones where a number that should be zero is not, and a report that only
/// prints its faults teaches an operator to read silence as success.
fn report_reliability(sink: &Sink) {
    let counts = sink.reliable.counts();
    let outstanding = sink.reliable.unacked() as u64;
    println!(
        "reliable events sent {}  acknowledged {}  abandoned {}  still outstanding at exit {}",
        counts.reliable_sent, counts.acknowledged, counts.abandoned, outstanding
    );
    // Printed whether or not it adds up, because this is what says the three
    // figures above are the whole story: applied by the host, given up on, or
    // still in flight are the only places a reliable event can end. A total
    // short of what was sent is this client losing track of an event rather
    // than the link losing one, which is a different fault and a worse one.
    let accounted = counts.acknowledged + counts.abandoned + outstanding;
    println!(
        "events accounted {accounted} of {}{}",
        counts.reliable_sent,
        if accounted == counts.reliable_sent {
            ""
        } else {
            "  which does not add up"
        }
    );
    println!(
        "retransmissions {} over a ladder of {MAX_RETRANSMISSIONS}  acknowledgement datagrams \
         received {}  snapshots sent {}",
        counts.retransmissions, counts.acks, counts.snapshots
    );
    let lost = counts.abandoned + outstanding;
    if lost > 0 {
        println!(
            "{lost} events the host may never have applied, which the snapshots are what repair"
        );
    }
}

/// The declaration a run was made under, checked against what it did.
///
/// Silence is the failure mode this exists for: two gate arms passed having
/// exercised nothing, and neither run said so because neither run had been
/// asked to. So a declared subsystem that stayed at zero is named and the exit
/// status carries it.
fn check_expectations(expected: &[Subsystem], sink: &Sink) -> ExitCode {
    if expected.is_empty() {
        return ExitCode::SUCCESS;
    }
    let mut unexercised = 0;
    for subsystem in expected {
        let count = subsystem.count(sink);
        if count == 0 {
            eprintln!(
                "expected {} and the run exercised none of it",
                subsystem.name()
            );
            unexercised += 1;
        }
    }
    if unexercised > 0 {
        eprintln!(
            "{unexercised} of {} declared subsystems were never exercised",
            expected.len()
        );
        return ExitCode::from(4);
    }
    let names: Vec<&str> = expected.iter().map(|each| each.name()).collect();
    println!("declared and exercised: {}", names.join(", "));
    ExitCode::SUCCESS
}

/// Sends the `ReleaseAll` and then keeps the loop turning long enough for the
/// last acknowledgements to arrive. See [`LINGER`] for why the wait is bounded.
fn finish(sink: &mut Sink) {
    let now = Timestamp::now();
    sink.release(ReleaseCause::Exit, now);
    let deadline = now.add(LINGER);
    while sink.reliable.unacked() > 0 && Timestamp::now() < deadline {
        sink.pump(Timestamp::now());
    }
}

fn report_cost(sink: &Sink, label: &str) {
    if sink.cost.is_empty() {
        println!("nothing was sent on the event path, so there is no cost to report");
        return;
    }
    println!(
        "{label}:  p50 {:.2}µs  p95 {:.2}µs  p99 {:.2}µs  max {:.2}µs",
        micros(Nanos(sink.cost.value_at_quantile(0.50))),
        micros(Nanos(sink.cost.value_at_quantile(0.95))),
        micros(Nanos(sink.cost.value_at_quantile(0.99))),
        micros(Nanos(sink.cost.max())),
    );
    if sink.clipped > 0 {
        println!("{} sends took longer than a second", sink.clipped);
    }
}

/// One of the deterministic cycles a synthetic run walks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Cycle {
    Keys,
    Buttons,
    Wheel,
}

/// Spends the wait until `due` pumping rather than sleeping, so a
/// retransmission, a snapshot or a heartbeat that falls due between two
/// synthetic events still goes out on time; the socket's read timeout is what
/// makes this a wait rather than a spin. At least one turn either way, so that
/// a rate high enough to leave no gap at all still repairs its losses.
fn pump_until(sink: &mut Sink, due: Timestamp) {
    loop {
        sink.pump(Timestamp::now());
        if due <= Timestamp::now() {
            return;
        }
    }
}

/// The synthetic cycles. No capture, no AppKit, no permission and no hand: what
/// is being tested here is the wire and whatever is injecting at the other end.
///
/// The deadline is only checked between whole steps, so a cycle never stops
/// between a press and its release and never leaves a key or a button held on
/// the host. That also makes the balance figure meaningful rather than a coin
/// toss on when the clock ran out.
fn run_synthetic(args: &Args, socket: UdpSocket, target: SocketAddr) -> ExitCode {
    if args.key_rate == 0 {
        eprintln!("--key-rate must be at least one event per second");
        return ExitCode::FAILURE;
    }
    let interval = Nanos(1_000_000_000 / u64::from(args.key_rate));

    let mut cycles = Vec::new();
    if args.synthetic_keys {
        cycles.push(Cycle::Keys);
    }
    if args.synthetic_buttons {
        cycles.push(Cycle::Buttons);
    }
    if args.synthetic_wheel {
        cycles.push(Cycle::Wheel);
    }

    let mut sink = new_sink(socket, target, args.session_id);
    let names: Vec<&str> = cycles
        .iter()
        .map(|cycle| match cycle {
            Cycle::Keys => "a W, A, S, D cycle",
            Cycle::Buttons => "all five buttons",
            Cycle::Wheel => "wheel notches",
        })
        .collect();
    println!(
        "sending {} to {target} as session {} for {} s at {} events/s",
        names.join(" and "),
        args.session_id,
        args.seconds,
        args.key_rate
    );

    let start = Timestamp::now();
    let deadline = start.add(Nanos(args.seconds.saturating_mul(1_000_000_000)));
    // Events emitted so far, which is what the pacing is measured in: paced
    // against the start rather than against the last send, so a slow send does
    // not push the whole run later and the count after n seconds is the one an
    // operator can predict.
    let mut emitted = 0u64;
    let mut turn = 0usize;

    while Timestamp::now() < deadline {
        let cycle = cycles[turn % cycles.len()];
        let step = turn / cycles.len();
        turn += 1;

        match cycle {
            Cycle::Keys => {
                let virtual_key = SYNTHETIC_KEYS[step % SYNTHETIC_KEYS.len()];
                let scan = ScanCode::from_virtual_key(virtual_key)
                    .expect("the synthetic cycle only uses keys the table covers");
                for down in [true, false] {
                    pump_until(&mut sink, start.add(Nanos(emitted * interval.get())));
                    sink.send_key(scan, down, Timestamp::now());
                    emitted += 1;
                }
            }
            Cycle::Buttons => {
                let button = SYNTHETIC_BUTTONS[step % SYNTHETIC_BUTTONS.len()];
                for down in [true, false] {
                    pump_until(&mut sink, start.add(Nanos(emitted * interval.get())));
                    sink.send_button(button, down, Timestamp::now());
                    emitted += 1;
                }
            }
            Cycle::Wheel => {
                pump_until(&mut sink, start.add(Nanos(emitted * interval.get())));
                sink.send_wheel(0, SYNTHETIC_NOTCH, Timestamp::now());
                emitted += 1;
            }
        }
    }

    finish(&mut sink);

    println!("failed sends {}", sink.failed);
    report_input(&sink);
    report_releases(&sink);
    report_reliability(&sink);
    report_cost(&sink, "clock read to send_to returning");
    check_expectations(&args.expect, &sink)
}

/// One of the four sequences a cycles run walks, in the order it walks them so
/// that a run of four or more covers every one.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scenario {
    /// A key and a button held through the release hotkey, and let go of
    /// afterwards, when there is nothing left to let go of them on.
    Held,
    /// Command-tab out and come back, which is not asking to play.
    Switched,
    /// The barrier from both sides: a stale event refused and a fresh one
    /// outranking it.
    Barrier,
    /// A refused cursor and a failed session, both of which must close.
    Failed,
}

impl Scenario {
    const ALL: [Scenario; 4] = [
        Scenario::Held,
        Scenario::Switched,
        Scenario::Barrier,
        Scenario::Failed,
    ];

    const fn label(self) -> &'static str {
        match self {
            Scenario::Held => "held",
            Scenario::Switched => "switched",
            Scenario::Barrier => "barrier",
            Scenario::Failed => "failed",
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Scenario::Held => "a key and a button held through the release hotkey",
            Scenario::Switched => {
                "command-tab, and a return to the application that does not \
                                  recapture"
            }
            Scenario::Barrier => {
                "a stale event refused and a recaptured one outranking the \
                                  barrier"
            }
            Scenario::Failed => "an entry the window server refused and a session that failed",
        }
    }
}

/// What the sequences found, as counts rather than as assertions, because a run
/// that stopped at the first surprise would report nothing about the rest of
/// what it was asked to walk.
#[derive(Default)]
struct Findings {
    /// Events made before a barrier and offered after it. Refused here, so they
    /// never take an id at all and the host is never asked to decide.
    stale_refused: u64,
    /// Recaptured sessions whose first event outranked the release that ended
    /// the capture before it, and any that did not, which the host would refuse
    /// as pre-barrier and the player would experience as a dead key.
    outranking: u64,
    misordered: u64,
    /// Returns to the application that left the machine uncaptured, and any
    /// that recaptured, which would take the cursor from somebody who had come
    /// back to read rather than to play.
    returns_without_capture: u64,
    returns_that_recaptured: u64,
    /// Entries the window server refused.
    entries_refused: u64,
    /// Times the machine was left anywhere but uncaptured where the contract
    /// says it must be uncaptured: after an entry that failed, and after a
    /// release that has been applied. Half-captured is the state that must be
    /// unreachable, and this is the figure that says it was.
    not_uncaptured: u64,
}

/// Paces the synthetic events against the start of the run rather than against
/// the last one, so a slow send does not push the whole run later and the count
/// after n seconds is a figure an operator can predict.
struct Pace {
    start: Timestamp,
    interval: Nanos,
    emitted: u64,
}

impl Pace {
    /// Waits for this event's slot, pumping rather than sleeping so that a
    /// retransmission, a snapshot or a heartbeat falling due in between still
    /// goes out on time, and returns the moment the event happened.
    fn beat(&mut self, sink: &mut Sink) -> Timestamp {
        pump_until(
            sink,
            self.start.add(Nanos(self.emitted * self.interval.get())),
        );
        self.emitted += 1;
        Timestamp::now()
    }
}

/// A synthetic key, taken through the same virtual-key table a real one goes
/// through, so a cycle exercises the whole send path and not only the socket.
fn synthetic_key(virtual_key: u16) -> ScanCode {
    ScanCode::from_virtual_key(virtual_key).expect("the cycles only press keys the table covers")
}

fn press(sink: &mut Sink, pace: &mut Pace, scan: ScanCode, down: bool) {
    let at = pace.beat(sink);
    sink.offer_key(scan, down, at);
}

/// A key offered at the instant it is made rather than in a paced slot, for the
/// keys of a local combination and nothing else.
///
/// None of them produces a datagram, since the recognizer holds them and the
/// exit throws them away, so spending paced slots on them buys nothing and
/// costs something real: it pushes the release several slots past the last
/// event the capture actually sent, and the case a reordering link is here to
/// produce is that event arriving *after* the release. A link can only do that
/// if the two are close enough together for its reordering to span them, which
/// is also how a hand types three keys at once rather than one every fiftieth
/// of a second.
fn press_local(sink: &mut Sink, scan: ScanCode) {
    sink.offer_key(scan, true, Timestamp::now());
}

fn click(sink: &mut Sink, pace: &mut Pace, button: Button, down: bool) {
    let at = pace.beat(sink);
    sink.offer_mouse(MouseEvent::Button { button, down }, at);
}

/// Performs whatever the machine asked for, with a counter where a real run has
/// the window server.
///
/// A counter and not `CGAssociateMouseAndMouseCursorPosition`, for the same
/// reason the synthetic keys are not a hand: this mode exists to be runnable
/// without a display, without a permission and without a person, and on a
/// machine with no window server every entry would otherwise fail closed and
/// the run would report a capture path it never exercised. The real cursor is
/// driven by the capture mode, which is the one a player uses.
fn settle_counted(sink: &mut Sink) {
    match sink.pending.take() {
        None => {}
        Some(Pending::Detach) => {
            sink.cursor_detaches += 1;
            sink.machine.entered();
        }
        Some(Pending::Restore) => {
            sink.cursor_restores += 1;
            sink.machine.released();
        }
    }
}

/// The click that asks for capture and the cursor call it asks for. The up is
/// part of the same click and travels no further than the down did.
fn enter(sink: &mut Sink, pace: &mut Pace) {
    click(sink, pace, CAPTURE_BUTTON, true);
    settle_counted(sink);
    click(sink, pace, CAPTURE_BUTTON, false);
}

/// The window server refusing the cursor, which is what failure closed has to
/// survive.
fn refuse_entry(sink: &mut Sink) {
    sink.pending = None;
    sink.machine.entry_failed();
}

/// The release combination, typed in the order the recognizer requires.
fn type_the_way_out(sink: &mut Sink) {
    for scan in [LEFT_COMMAND, LEFT_CONTROL, LEFT_OPTION] {
        press_local(sink, scan);
    }
}

/// A cause that arrives from outside an event, which is a focus loss or a
/// session that stopped working. Both take the exit an event-borne cause takes,
/// and a cause that arrives while the exit is already under way takes nothing.
fn exit_now(sink: &mut Sink, cause: Option<ExitCause>) {
    if let Some(cause) = cause {
        let now = Timestamp::now();
        sink.follow(Action::Exit(cause), now);
    }
}

/// Capture, hold a key and a button, type the way out, then let go of both.
/// The letting go happens while uncaptured, so the host hears about neither and
/// the release is what let go of them there.
fn scenario_held(sink: &mut Sink, pace: &mut Pace) {
    enter(sink, pace);
    let w = synthetic_key(VK_W);
    press(sink, pace, w, true);
    click(sink, pace, Button::Right, true);

    type_the_way_out(sink);
    settle_counted(sink);

    press(sink, pace, w, false);
    click(sink, pace, Button::Right, false);
}

/// Hold shift, a key and a button; command-tab out; come back. Coming back is
/// not asking to play, so nothing resumes and an explicit click is what has to
/// ask again.
fn scenario_switched(sink: &mut Sink, pace: &mut Pace, findings: &mut Findings) {
    enter(sink, pace);
    press(sink, pace, synthetic_key(VK_LEFT_SHIFT), true);
    press(sink, pace, synthetic_key(VK_W), true);
    click(sink, pace, Button::Left, true);

    press_local(sink, LEFT_COMMAND);
    press_local(sink, TAB);
    // macOS posts the focus loss command-tab causes a moment later, and it finds
    // the exit already under way. One exit and not one per cause is exactly what
    // makes that harmless.
    let cause = sink.machine.focus_lost();
    exit_now(sink, cause);
    settle_counted(sink);
    if sink.machine.state() != State::Uncaptured {
        findings.not_uncaptured += 1;
    }

    sink.machine.focus_regained();
    if sink.machine.state() == State::Uncaptured {
        findings.returns_without_capture += 1;
    } else {
        findings.returns_that_recaptured += 1;
    }
    // Input offered on the way back, which has nothing to travel on.
    press(sink, pace, synthetic_key(VK_W), false);

    // The explicit click that does ask, and a focus loss to end it.
    enter(sink, pace);
    press(sink, pace, synthetic_key(VK_S), true);
    let cause = sink.machine.focus_lost();
    exit_now(sink, cause);
    settle_counted(sink);
}

/// The barrier from both sides at once. An event made before a release and
/// delivered after it is refused here rather than left for the host to refuse,
/// and the first event of the session that recaptures must outrank that release
/// or the host will refuse that too and the key will do nothing.
fn scenario_barrier(sink: &mut Sink, pace: &mut Pace, findings: &mut Findings) {
    enter(sink, pace);
    let w = synthetic_key(VK_W);
    press(sink, pace, w, true);

    type_the_way_out(sink);
    let barrier = sink.last_release;
    settle_counted(sink);

    let refused = sink.machine.counts().refused;
    press(sink, pace, w, false);
    if sink.machine.counts().refused > refused {
        findings.stale_refused += 1;
    }

    enter(sink, pace);
    let s = synthetic_key(VK_S);
    press(sink, pace, s, true);
    match (barrier, sink.last_reliable) {
        (Some(barrier), Some(after)) if barrier < after => findings.outranking += 1,
        _ => findings.misordered += 1,
    }
    press(sink, pace, s, false);

    let cause = sink.machine.session_failed();
    exit_now(sink, cause);
    settle_counted(sink);
    if sink.machine.state() != State::Uncaptured {
        findings.not_uncaptured += 1;
    }
}

/// Failure closed, twice over. The window server refuses the cursor and the
/// machine has to be exactly what the player started with rather than
/// half-captured; then a session fails under a running capture and takes the
/// same exit every other cause takes.
fn scenario_failed(sink: &mut Sink, pace: &mut Pace, findings: &mut Findings) {
    click(sink, pace, CAPTURE_BUTTON, true);
    refuse_entry(sink);
    findings.entries_refused += 1;
    if sink.machine.state() != State::Uncaptured {
        findings.not_uncaptured += 1;
    }
    // Nothing may travel on the strength of a capture that never happened.
    let w = synthetic_key(VK_W);
    press(sink, pace, w, true);
    click(sink, pace, CAPTURE_BUTTON, false);

    enter(sink, pace);
    press(sink, pace, w, true);
    click(sink, pace, Button::Middle, true);

    let cause = sink.machine.session_failed();
    exit_now(sink, cause);
    settle_counted(sink);
    if sink.machine.state() != State::Uncaptured {
        findings.not_uncaptured += 1;
    }
}

fn report_findings(findings: &Findings) {
    println!(
        "stale events refused after a barrier {}  recaptured events outranking their barrier {}  \
         misordered {}",
        findings.stale_refused, findings.outranking, findings.misordered
    );
    println!(
        "returns to the application that did not recapture {}  that did {}  entries the window \
         server refused {}",
        findings.returns_without_capture,
        findings.returns_that_recaptured,
        findings.entries_refused
    );
    println!(
        "times the machine was left anywhere but uncaptured where it must be uncaptured {}",
        findings.not_uncaptured
    );
}

/// The synthetic capture cycles. No AppKit, no permission, no hand and no
/// window server: what is being tested here is the state machine, the ordering
/// its one exit imposes on the event ids, and whatever is injecting at the far
/// end of the socket.
fn run_cycles(args: &Args, socket: UdpSocket, target: SocketAddr) -> ExitCode {
    if args.key_rate == 0 {
        eprintln!("--key-rate must be at least one event per second");
        return ExitCode::FAILURE;
    }

    let mut sink = new_sink(socket, target, args.session_id);
    println!(
        "driving {} capture cycles to {target} as session {} at {} events/s",
        args.cycles, args.session_id, args.key_rate
    );
    for scenario in Scenario::ALL {
        println!("  {}: {}", scenario.label(), scenario.name());
    }

    let mut pace = Pace {
        start: Timestamp::now(),
        interval: Nanos(1_000_000_000 / u64::from(args.key_rate)),
        emitted: 0,
    };
    let mut findings = Findings::default();
    let mut walked = [0u64; Scenario::ALL.len()];

    for cycle in 0..args.cycles {
        let index = (cycle % Scenario::ALL.len() as u64) as usize;
        walked[index] += 1;
        match Scenario::ALL[index] {
            Scenario::Held => scenario_held(&mut sink, &mut pace),
            Scenario::Switched => scenario_switched(&mut sink, &mut pace, &mut findings),
            Scenario::Barrier => scenario_barrier(&mut sink, &mut pace, &mut findings),
            Scenario::Failed => scenario_failed(&mut sink, &mut pace, &mut findings),
        }
    }

    finish(&mut sink);

    let coverage: Vec<String> = Scenario::ALL
        .iter()
        .zip(walked)
        .map(|(scenario, count)| format!("{} {count}", scenario.label()))
        .collect();
    println!("sequences walked: {}", coverage.join("  "));
    // Named rather than left to a reader to subtract, because a run that walked
    // three of the four is not evidence about the fourth.
    for (scenario, count) in Scenario::ALL.iter().zip(walked) {
        if count == 0 {
            println!("sequence {} was never walked by this run", scenario.label());
        }
    }

    println!("failed sends {}", sink.failed);
    report_machine(&sink);
    report_findings(&findings);
    report_input(&sink);
    // Said plainly, because the figures above are the ones a capture run reads
    // as a fault. Here they are the input a release had to sweep: every one of
    // these sequences exits with something still held, which is the case the
    // barrier exists for.
    println!(
        "these sequences exit on purpose with keys and buttons held, so the held figures above \
         are what a release swept rather than input that leaked"
    );
    report_releases(&sink);
    report_reliability(&sink);
    report_cost(&sink, "clock read to send_to returning");
    check_expectations(&args.expect, &sink)
}

/// Performs the window server call the machine asked for, outside AppKit's
/// dispatch of the event that asked for it.
fn settle_cursor(sink: &Rc<RefCell<Sink>>, capture: &mut Capture) {
    let Some(pending) = sink.borrow_mut().pending.take() else {
        return;
    };
    match pending {
        Pending::Detach => match capture.detach_cursor() {
            Ok(()) => {
                let mut sink = sink.borrow_mut();
                sink.cursor_detaches += 1;
                sink.machine.entered();
            }
            Err(why) => {
                // Failure closed. The player keeps the machine they already had,
                // which is the only outcome that cannot be mistaken for a broken
                // computer.
                eprintln!("the cursor could not be taken, so no capture started: {why}");
                sink.borrow_mut().machine.entry_failed();
            }
        },
        Pending::Restore => {
            if let Err(why) = capture.release_cursor() {
                eprintln!("the cursor could not be given back: {why}");
            }
            let mut sink = sink.borrow_mut();
            sink.cursor_restores += 1;
            // Reported whatever the window server said. A cursor left detached
            // is a bad end to a run; a machine stuck in its exit, refusing every
            // event for the rest of the session, is a worse one.
            sink.machine.released();
        }
    }
}

/// The capture mode: real mouse motion, buttons and wheel, real keys when asked
/// for, and the state machine deciding which of them may leave.
///
/// The session starts uncaptured with the monitors already installed, because
/// the click that asks for capture is itself a mouse event and cannot be acted
/// on before it has been seen.
fn run_capture(args: &Args, socket: UdpSocket, target: SocketAddr) -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("the probe must run on the main thread for AppKit to deliver events");
        return ExitCode::FAILURE;
    };
    let app = NSApplication::sharedApplication(mtm);
    // Accessory rather than Regular: the probe has no window and no dock icon
    // belongs to it, and staying out of the foreground means the global monitor
    // sees the events of whatever application the user is actually pointing at.
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();

    let sink = Rc::new(RefCell::new(new_sink(socket, target, args.session_id)));

    let sending = Rc::clone(&sink);
    let mut capture =
        match Capture::start_attached(move |event, at| sending.borrow_mut().offer_mouse(event, at))
        {
            Ok(capture) => capture,
            Err(why) => {
                eprintln!("cannot capture the mouse: {why}");
                return ExitCode::FAILURE;
            }
        };

    let keyboard = if args.keys {
        let sending = Rc::clone(&sink);
        match Keyboard::start(move |key| {
            // The capture's own event id is ignored: the reliability layer mints
            // the one that goes on the wire, so there is exactly one counter per
            // session however many sources of keys there are.
            sending.borrow_mut().offer_key(key.scan, key.down, key.at);
        }) {
            Ok(keyboard) => Some(keyboard),
            Err(why) => {
                eprintln!("cannot capture the keyboard: {why}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };

    // Started before the loop and asked once per turn. The response to a loss
    // is a datagram and a window server call, and neither belongs inside
    // AppKit's notification dispatch.
    let focus = FocusWatcher::start();

    println!(
        "watching {} for {} s, sending to {target} as session {} from the moment a click asks \
         for the capture",
        if args.keys {
            "the mouse and the keyboard"
        } else {
            "the mouse"
        },
        args.seconds,
        args.session_id
    );

    let deadline = Timestamp::now().add(Nanos(args.seconds.saturating_mul(1_000_000_000)));
    while Timestamp::now() < deadline {
        // A pool per turn, because every event AppKit hands back is autoreleased
        // and a thirty second run would otherwise accumulate all of them.
        autoreleasepool(|_| {
            let until = NSDate::dateWithTimeIntervalSinceNow(TURN_SECONDS);
            if let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&until),
                unsafe { NSDefaultRunLoopMode },
                true,
            ) {
                app.sendEvent(&event);
            }
        });

        // Focus is the loss that happens most and the easiest to miss, and it
        // goes through the machine rather than being undone here. That is what
        // makes the four causes one exit instead of four handlers each deciding
        // for themselves what to put back.
        if focus.take_loss() {
            let mut sink = sink.borrow_mut();
            let cause = sink.machine.focus_lost();
            exit_now(&mut sink, cause);
        }
        if focus.take_regain() {
            // Not a recapture. Somebody who switched back to look at the window
            // has not asked to play, and the click is what asks.
            sink.borrow_mut().machine.focus_regained();
        }

        settle_cursor(&sink, &mut capture);
        sink.borrow_mut().pump(Timestamp::now());
    }

    // The release goes out whatever the window server said. A cursor that
    // could not be reattached is a bad end to a run, but a host still holding
    // the player's keys is a worse one, so the failure is carried to the exit
    // status rather than allowed to skip the invariant.
    let cursor = capture.release();
    // Giving up the capture is itself a loss of control, so it owes a release
    // of its own rather than leaning on the one the exit is about to send.
    sink.borrow_mut()
        .release(ReleaseCause::CaptureReleased, Timestamp::now());

    {
        let mut sink = sink.borrow_mut();
        finish(&mut sink);
    }

    let sink = sink.borrow();
    println!(
        "mouse events {}  motion datagrams {}  failed sends {}",
        sink.motion_events, sink.datagrams, sink.failed
    );
    println!("total dx {}  total dy {}", sink.total_dx, sink.total_dy);
    println!("focus lost {} times during the run", focus.losses());

    if let Some(keyboard) = &keyboard {
        println!(
            "keys captured {}  repeats suppressed {}",
            keyboard.captured(),
            keyboard.repeats_suppressed()
        );
        // A refused permission is silence, not an error, so silence has to be
        // named. Reporting a clean run of zero here is how an operator ends up
        // believing a capture works when nothing was ever delivered to it.
        if keyboard.captured() == 0 {
            println!(
                "no key event was ever seen: grant Input Monitoring to this binary in \
                 System Settings > Privacy & Security, or the capture stays silent"
            );
        }
    }

    report_machine(&sink);
    // A run in which nobody clicked captured nothing, and a capture path that
    // was never entered has to say so rather than print a tidy set of zeroes.
    if sink.machine.counts().captures == 0 {
        println!(
            "nothing ever asked for the capture, so nothing was sent: click once with the left \
             button to take the mouse, and press command, control and option together to give \
             it back"
        );
    }
    report_input(&sink);
    report_releases(&sink);
    report_reliability(&sink);
    report_cost(&sink, "callback to send_to returning");
    let expectations = check_expectations(&args.expect, &sink);
    if let Err(why) = cursor {
        eprintln!("the cursor could not be reattached: {why}");
        return ExitCode::FAILURE;
    }
    expectations
}

fn main() -> ExitCode {
    let args = Args::parse();

    let target = match resolve(&args.send_to) {
        Ok(target) => target,
        Err(why) => {
            eprintln!("{why}");
            return ExitCode::FAILURE;
        }
    };

    let bind = if target.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let socket = match UdpSocket::bind(bind) {
        Ok(socket) => socket,
        Err(why) => {
            eprintln!("cannot open a socket: {why}");
            return ExitCode::FAILURE;
        }
    };

    // A timeout rather than a non-blocking socket, because the receive shares
    // its loop with the sends: see `Sink::receive` for why that is the shape.
    if let Err(why) = socket.set_read_timeout(Some(RECV_TIMEOUT)) {
        eprintln!("cannot set a read timeout on the socket: {why}");
        return ExitCode::FAILURE;
    }

    if args.cycles > 0 {
        run_cycles(&args, socket, target)
    } else if args.synthetic_keys || args.synthetic_buttons || args.synthetic_wheel {
        run_synthetic(&args, socket, target)
    } else {
        run_capture(&args, socket, target)
    }
}
