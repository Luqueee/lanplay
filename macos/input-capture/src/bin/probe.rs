//! One captured mouse event, one UDP datagram, and what that costs locally.
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
//! AppKit will not deliver mouse events to a process that is not an
//! application, so an `NSApplication` is created and its run loop is turned by
//! hand. Turning it by hand rather than calling `run` is what lets the probe
//! stop at a deadline without a timer.

use std::cell::RefCell;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::process::ExitCode;
use std::rc::Rc;

use clap::Parser;
use hdrhistogram::Histogram;
use lanplay_input_capture::{Capture, INPUT_PORT};
use lanplay_input_protocol::{Datagram, MAX_DATAGRAM, Message, Sequence, SessionId, encode};
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

/// How long one turn of the run loop waits for an event. Long enough that an
/// idle probe does not spin, short enough that the deadline is honoured
/// promptly.
const TURN_SECONDS: f64 = 0.05;

#[derive(Parser)]
#[command(
    about = "Sends one input-protocol Motion datagram per macOS mouse event.",
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
}

impl Sink {
    /// The send path. Called from the AppKit event callback, so it allocates
    /// nothing: the datagram is built in a stack buffer of the largest size the
    /// wire format can produce.
    fn send(&mut self, dx: i32, dy: i32, at: Timestamp) {
        self.events += 1;

        let datagram = Datagram {
            session: self.session,
            sequence: Sequence(self.next_sequence),
            sent_at_ns: at.as_nanos(),
            message: Message::Motion { dx, dy },
        };
        self.next_sequence = self.next_sequence.wrapping_add(1);

        let mut buffer = [0u8; MAX_DATAGRAM];
        let len = encode(&datagram, &mut buffer).expect("motion fits in MAX_DATAGRAM");
        match self.socket.send_to(&buffer[..len], self.target) {
            Ok(_) => {
                self.datagrams += 1;
                self.total_dx += i64::from(dx);
                self.total_dy += i64::from(dy);
            }
            // Counted rather than reported per event, because printing from the
            // event path would itself become the thing being measured.
            Err(_) => self.failed += 1,
        }

        // Measured after the syscall returns whether it succeeded or not: the
        // cost of a refused send is still the cost this path pays.
        let cost = Timestamp::now().saturating_since(at);
        if cost.get() > MAX_NANOS {
            self.clipped += 1;
        }
        self.cost.saturating_record(cost.get());
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

    let sink = Rc::new(RefCell::new(Sink {
        socket,
        target,
        session: SessionId(args.session_id),
        next_sequence: 0,
        events: 0,
        datagrams: 0,
        failed: 0,
        clipped: 0,
        total_dx: 0,
        total_dy: 0,
        cost: Histogram::new_with_bounds(1, MAX_NANOS, SIGNIFICANT_FIGURES)
            .expect("valid histogram bounds"),
    }));

    let sending = Rc::clone(&sink);
    let mut capture = match Capture::start(move |dx, dy, at| sending.borrow_mut().send(dx, dy, at))
    {
        Ok(capture) => capture,
        Err(why) => {
            eprintln!("cannot capture the mouse: {why}");
            return ExitCode::FAILURE;
        }
    };

    println!(
        "capturing for {} s, sending Motion to {target} as session {}",
        args.seconds, args.session_id
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
    }

    if let Err(why) = capture.release() {
        eprintln!("the cursor could not be reattached: {why}");
        return ExitCode::FAILURE;
    }

    let sink = sink.borrow();
    println!(
        "events {}  datagrams {}  failed sends {}",
        sink.events, sink.datagrams, sink.failed
    );
    println!("total dx {}  total dy {}", sink.total_dx, sink.total_dy);
    if sink.cost.is_empty() {
        println!("no mouse motion was seen, so there is nothing to report");
        return ExitCode::SUCCESS;
    }
    println!(
        "callback to send_to returning:  p50 {:.2}µs  p95 {:.2}µs  p99 {:.2}µs  max {:.2}µs",
        micros(Nanos(sink.cost.value_at_quantile(0.50))),
        micros(Nanos(sink.cost.value_at_quantile(0.95))),
        micros(Nanos(sink.cost.value_at_quantile(0.99))),
        micros(Nanos(sink.cost.max())),
    );
    if sink.clipped > 0 {
        println!("{} sends took longer than a second", sink.clipped);
    }
    ExitCode::SUCCESS
}
