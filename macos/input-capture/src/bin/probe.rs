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
//! grows is a bug.
//!
//! Keys are counted differently, because they are not additive and losing one
//! is not a smoothed-over error. The figure that matters for them is whether
//! every press was followed by a release, since an unmatched press is a key held
//! down on the host after the player has let go, and it is printed whether it is
//! good or bad: a run that only reports its faults teaches an operator to read
//! silence as success.
//!
//! There are two ways to produce keys here. Capturing real ones needs Input
//! Monitoring, and a machine that has not granted it delivers no events at all
//! rather than an error, which is why a run that saw nothing says so instead of
//! reporting a clean zero. The synthetic cycle needs no permission and no human,
//! and exists so the wire format and the host's injection can be exercised on
//! their own; it is the only paced thing in this file, because there it stands in
//! for the player's fingers rather than for the capture path.
//!
//! AppKit will not deliver events to a process that is not an application, so an
//! `NSApplication` is created and its run loop is turned by hand. Turning it by
//! hand rather than calling `run` is what lets the probe stop at a deadline
//! without a timer.
//!
//! None of the reliability arithmetic is in this file. The retransmission
//! ladder, the acknowledgement window and the snapshot cadence live in the
//! library, where the clock is a variable and every deadline can be tested
//! without waiting for it. What this file owns is the loop that drives them,
//! the socket they share with the sends, and the counters an operator reads
//! afterwards. The one that decides whether a run was clean is the last of
//! them: how many reliable events were still unacknowledged at exit.

use std::cell::RefCell;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use clap::Parser;
use hdrhistogram::Histogram;
use lanplay_input_capture::{
    Capture, INPUT_PORT, Keyboard, Reliable, ScanCode, reliable::MAX_RETRANSMISSIONS,
};
use lanplay_input_protocol::{
    Datagram, MAX_DATAGRAM, Message, Sequence, SessionId, decode, encode,
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

    /// Synthetic key events per second, counting each press and each release.
    #[arg(long, default_value_t = 20, value_name = "PER_SECOND")]
    key_rate: u32,
}

/// Everything the event callback touches, and everything the report reads.
struct Sink {
    socket: UdpSocket,
    target: SocketAddr,
    session: SessionId,
    next_sequence: u32,
    events: u64,
    datagrams: u64,
    failed: u64,
    clipped: u64,
    total_dx: i64,
    total_dy: i64,
    cost: Histogram<u64>,
    key_datagrams: u64,
    presses: u64,
    releases: u64,
    /// What is believed held, what has not been acknowledged, and when the next
    /// snapshot is due. The held set lives in there rather than beside it so
    /// that the count reported here and the set a snapshot describes cannot
    /// drift apart.
    reliable: Reliable,
    /// Keys held when capture stopped, read before the `ReleaseAll` that
    /// clears them.
    held_at_stop: usize,
    /// A press for a key already believed held, and a release for one that was
    /// not. Either means the pressed set and the player have diverged, and both
    /// are separate from the held count because a run can end balanced and still
    /// have been wrong in the middle.
    double_presses: u64,
    unmatched_releases: u64,
}

impl Sink {
    /// Builds one datagram and sends it. Shared by the event path, the
    /// retransmissions and the snapshots, because they are the same socket and
    /// the same wire format and only what is measured afterwards differs.
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
    /// Retransmissions and snapshots deliberately do not come through here.
    /// They leave on a deadline rather than on an event, so folding their cost
    /// into this histogram would describe neither of the two.
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
        self.events += 1;
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
        if self.emit(message, at) {
            self.key_datagrams += 1;
        }
    }

    /// The reliability half of every loop in this file: acknowledgements in,
    /// then whatever the deadlines say is due out.
    ///
    /// Separate from the event path on purpose. An event still leaves the
    /// moment it arrives, and only the repairs are on a timer, because a
    /// deadline is the whole point of them.
    fn pump(&mut self, now: Timestamp) {
        self.receive();
        while let Some(message) = self.reliable.next_due(now) {
            self.send_now(message, Timestamp::now());
        }
        if let Some(snapshot) = self.reliable.snapshot_due(now) {
            self.send_now(snapshot, Timestamp::now());
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

    /// Reads the held count, then tells the host to let go of everything.
    ///
    /// The count is taken before the release rather than after, because it is
    /// the diagnostic and the release is the repair: read afterwards it would
    /// be zero for every run, however badly the run had gone.
    fn stop(&mut self, at: Timestamp) {
        self.held_at_stop = self.reliable.keys().held().count();
        let message = self.reliable.release_all(at);
        self.send_now(message, at);
    }

    /// Whether the keys this run sent add up: nothing left down, no press on top
    /// of a press, no release without one.
    fn keys_balanced(&self) -> bool {
        self.held_at_stop == 0 && self.double_presses == 0 && self.unmatched_releases == 0
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
    Sink {
        socket,
        target,
        session: SessionId(session_id),
        next_sequence: 0,
        events: 0,
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
        reliable: Reliable::new(Timestamp::now()),
        held_at_stop: 0,
        double_presses: 0,
        unmatched_releases: 0,
    }
}

/// The key half of the report, printed by both modes because the question it
/// answers is the same one either way.
fn report_keys(sink: &Sink) {
    println!(
        "key datagrams {}  presses {}  releases {}",
        sink.key_datagrams, sink.presses, sink.releases
    );
    // Held when capture stopped rather than now: a `ReleaseAll` has been sent
    // since, and reporting after it would be reporting the repair.
    println!(
        "keys still held at the end {}  every press matched by a release: {}",
        sink.held_at_stop,
        if sink.keys_balanced() { "yes" } else { "no" }
    );
    if sink.double_presses > 0 || sink.unmatched_releases > 0 {
        println!(
            "{} presses landed on a key already held and {} releases had no press",
            sink.double_presses, sink.unmatched_releases
        );
    }
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

/// Sends the `ReleaseAll` and then keeps the loop turning long enough for the
/// last acknowledgements to arrive. See [`LINGER`] for why the wait is bounded.
fn finish(sink: &mut Sink) {
    let now = Timestamp::now();
    sink.stop(now);
    let deadline = now.add(LINGER);
    while sink.reliable.unacked() > 0 && Timestamp::now() < deadline {
        sink.pump(Timestamp::now());
    }
}

fn report_cost(sink: &Sink, label: &str) {
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

/// The synthetic cycle. No capture, no AppKit, no permission, and no keyboard:
/// what is being tested here is the wire and whatever is injecting at the other
/// end.
///
/// The deadline is only checked between pairs, so the cycle never stops between
/// a press and its release and never leaves a key held on the host. That also
/// makes the balance figure meaningful rather than a coin toss on when the clock
/// ran out.
fn run_synthetic(args: &Args, socket: UdpSocket, target: SocketAddr) -> ExitCode {
    if args.key_rate == 0 {
        eprintln!("--key-rate must be at least one event per second");
        return ExitCode::FAILURE;
    }
    let interval = Nanos(1_000_000_000 / u64::from(args.key_rate));

    let mut sink = new_sink(socket, target, args.session_id);
    println!(
        "sending a synthetic W, A, S, D cycle to {target} as session {} for {} s at {} events/s",
        args.session_id, args.seconds, args.key_rate
    );

    let start = Timestamp::now();
    let deadline = start.add(Nanos(args.seconds.saturating_mul(1_000_000_000)));
    let mut step = 0u64;

    while Timestamp::now() < deadline {
        let virtual_key = SYNTHETIC_KEYS[(step as usize / 2) % SYNTHETIC_KEYS.len()];
        let scan = ScanCode::from_virtual_key(virtual_key)
            .expect("the synthetic cycle only uses keys the table covers");

        for down in [true, false] {
            // Paced against the start rather than against the last send, so a
            // slow send does not push the whole cycle later and the count after
            // n seconds is the one an operator can predict.
            let due = start.add(Nanos(step.saturating_mul(interval.get())));
            // The wait is spent pumping rather than sleeping, so a
            // retransmission that falls due between two synthetic keys still
            // goes out on time; the socket's read timeout is what makes this a
            // wait rather than a spin. At least one turn per key, so that a
            // rate high enough to leave no gap at all still repairs its losses.
            loop {
                sink.pump(Timestamp::now());
                if due <= Timestamp::now() {
                    break;
                }
            }

            let at = Timestamp::now();
            sink.send_key(scan, down, at);
            step += 1;
        }
    }

    finish(&mut sink);

    println!("failed sends {}", sink.failed);
    report_keys(&sink);
    report_reliability(&sink);
    if sink.cost.is_empty() {
        println!("nothing was sent, so there is no cost to report");
        return ExitCode::SUCCESS;
    }
    report_cost(&sink, "clock read to send_to returning");
    ExitCode::SUCCESS
}

/// The capture mode: real mouse motion, and real keys when asked for.
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
        match Capture::start(move |dx, dy, at| sending.borrow_mut().send_motion(dx, dy, at)) {
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
            sending.borrow_mut().send_key(key.scan, key.down, key.at);
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

    println!(
        "capturing {} for {} s, sending to {target} as session {}",
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
        sink.borrow_mut().pump(Timestamp::now());
    }

    if let Err(why) = capture.release() {
        eprintln!("the cursor could not be reattached: {why}");
        return ExitCode::FAILURE;
    }

    {
        let mut sink = sink.borrow_mut();
        finish(&mut sink);
    }

    let sink = sink.borrow();
    println!(
        "mouse events {}  motion datagrams {}  failed sends {}",
        sink.events, sink.datagrams, sink.failed
    );
    println!("total dx {}  total dy {}", sink.total_dx, sink.total_dy);

    if let Some(keyboard) = &keyboard {
        println!(
            "keys captured {}  repeats suppressed {}",
            keyboard.captured(),
            keyboard.repeats_suppressed()
        );
        report_keys(&sink);
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

    report_reliability(&sink);

    if sink.cost.is_empty() {
        println!("no input was seen, so there is nothing to report");
        return ExitCode::SUCCESS;
    }
    report_cost(&sink, "callback to send_to returning");
    ExitCode::SUCCESS
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

    if args.synthetic_keys {
        run_synthetic(&args, socket, target)
    } else {
        run_capture(&args, socket, target)
    }
}
