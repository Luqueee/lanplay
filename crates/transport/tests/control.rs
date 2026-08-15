//! Control plane over real loopback TCP.
//!
//! Most of this file checks that the handshake and the framing behave. The end
//! of it checks the property the control plane exists to keep: that it cannot
//! reach the media path even when it is completely wedged. That last claim is
//! split in two, because half of it is logic and half of it is a measurement.
//! The logic - a peer that stops reading turns the server's writes into
//! timeouts - holds on any machine and stays in the suite. The measurement -
//! what that costs a 120 Hz producer - is ignored here and runs in
//! `tools/cadence-isolation-gate.sh`, which can refuse a machine that was in no
//! position to answer. `cargo test` has only pass and fail, and neither is the
//! right answer to a runner with three cores and no core to spare.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use lanplay_telemetry::{Nanos, Timestamp, wait_until};
use lanplay_transport::{
    CONTROL_MAGIC, CONTROL_VERSION, ControlClient, ControlError, ControlFrame, ControlMessage,
    ControlServer, ControlSession, FRAME_HEADER_LEN, PROTOCOL_VERSION, SessionToken, Ssrc,
};

/// Largest single allocation this test binary has ever asked for.
///
/// A `payload_len` of 100 MB must not become a 100 MB buffer. Nothing else in
/// this binary allocates anywhere near that, so a global high-water mark is a
/// direct, non-flaky witness for "the oversized length was rejected before it
/// reached the allocator".
static PEAK_ALLOCATION: AtomicUsize = AtomicUsize::new(0);

struct PeakTracking;

// SAFETY: every method forwards its arguments unchanged to `System`, which
// upholds the `GlobalAlloc` contract; the only added work is a relaxed atomic.
unsafe impl GlobalAlloc for PeakTracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        PEAK_ALLOCATION.fetch_max(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is forwarded unchanged from a valid caller.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        PEAK_ALLOCATION.fetch_max(layout.size(), Ordering::Relaxed);
        // SAFETY: `layout` is forwarded unchanged from a valid caller.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        PEAK_ALLOCATION.fetch_max(new_size, Ordering::Relaxed);
        // SAFETY: `ptr`, `layout` and `new_size` come unchanged from a caller
        // that already satisfies `realloc`'s preconditions.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` and `layout` come unchanged from a caller that already
        // satisfies `dealloc`'s preconditions.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: PeakTracking = PeakTracking;

const SECOND: Nanos = Nanos::from_millis(1_000);
const FRAME_PERIOD: Nanos = Nanos(8_333_333);

fn server(name: &str) -> Arc<ControlServer> {
    Arc::new(ControlServer::bind("127.0.0.1:0", name).expect("bind loopback control server"))
}

fn control_error(error: &io::Error) -> ControlError {
    *error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ControlError>())
        .unwrap_or_else(|| panic!("expected a ControlError, got {error:?}"))
}

fn raw_header(magic: u32, version: u16, message_type: u16, payload_len: u32) -> [u8; 12] {
    let mut header = [0u8; FRAME_HEADER_LEN];
    header[0..4].copy_from_slice(&magic.to_be_bytes());
    header[4..6].copy_from_slice(&version.to_be_bytes());
    header[6..8].copy_from_slice(&message_type.to_be_bytes());
    header[8..12].copy_from_slice(&payload_len.to_be_bytes());
    header
}

/// Sends one malformed header over loopback and reports how the server
/// rejected the connection.
fn reject_raw_header(header: [u8; 12]) -> ControlError {
    let server = server("host");
    let addr = server.local_addr().unwrap();
    let accepting = thread::spawn(move || server.accept_session(SECOND).map(|_| ()));

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.write_all(&header).expect("write header");
    // No payload follows: the server must reject on the header alone.
    let error = accepting
        .join()
        .unwrap()
        .expect_err("a malformed header must not produce a session");
    control_error(&error)
}

#[test]
fn a_handshake_issues_a_token_that_binds_a_udp_peer_exactly_once() {
    let server = server("host");
    let addr = server.local_addr().unwrap();
    let accepting = {
        let server = Arc::clone(&server);
        thread::spawn(move || server.accept_session(SECOND).expect("accept session"))
    };

    let mut client = ControlClient::connect(addr, SECOND).expect("connect");
    let token = client.hello("mac").expect("hello");
    let session = accepting.join().unwrap();

    assert_eq!(session.token(), token);
    assert_eq!(session.client_name(), "mac");
    assert_eq!(client.server_name(), "host");
    assert_eq!(server.session_count(), 1);
    assert!(server.udp_peer(token).is_none());

    let media: SocketAddr = "127.0.0.1:50000".parse().unwrap();
    assert!(server.bind_udp_peer(token, media));
    assert_eq!(server.udp_peer(token), Some(media));

    // A second claim on the same token, from anywhere, must not redirect the
    // stream.
    let impostor: SocketAddr = "127.0.0.1:50001".parse().unwrap();
    assert!(!server.bind_udp_peer(token, impostor));
    assert_eq!(server.udp_peer(token), Some(media));

    drop(session);
    assert_eq!(server.session_count(), 0);
    assert!(server.udp_peer(token).is_none());
}

#[test]
fn an_unknown_token_is_refused() {
    let server = server("host");
    let addr = server.local_addr().unwrap();
    let accepting = {
        let server = Arc::clone(&server);
        thread::spawn(move || server.accept_session(SECOND).expect("accept session"))
    };
    let mut client = ControlClient::connect(addr, SECOND).expect("connect");
    let real = client.hello("mac").expect("hello");
    let _session = accepting.join().unwrap();

    let mut forged = *real.as_bytes();
    forged[0] ^= 0xFF;
    let media: SocketAddr = "127.0.0.1:50002".parse().unwrap();

    assert!(!server.bind_udp_peer(SessionToken::from_bytes(forged), media));
    assert!(!server.bind_udp_peer(SessionToken::generate(), media));
    assert!(server.udp_peer(SessionToken::from_bytes(forged)).is_none());
    // The real token is still usable: a refusal must not poison the session.
    assert!(server.bind_udp_peer(real, media));
}

#[test]
fn a_bad_magic_closes_the_connection() {
    assert_eq!(
        reject_raw_header(raw_header(0x4745_5420, CONTROL_VERSION, 1, 0)),
        ControlError::BadMagic(0x4745_5420)
    );
}

#[test]
fn an_unknown_version_closes_the_connection() {
    assert_eq!(
        reject_raw_header(raw_header(CONTROL_MAGIC, 0xBEEF, 1, 0)),
        ControlError::UnsupportedVersion(0xBEEF)
    );
}

#[test]
fn a_hundred_megabyte_payload_length_is_rejected_without_allocating() {
    const ABSURD: u32 = 100 * 1024 * 1024;

    // Direct, so the assertion is about `ControlFrame::read` and not about
    // whatever else a socket path might allocate. The reader counts how much
    // it was asked for: a rejection that still tried to fill the buffer would
    // consume far more than the header.
    struct Counting {
        header: [u8; FRAME_HEADER_LEN],
        offset: usize,
        requested: usize,
    }
    impl Read for Counting {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.requested += out.len();
            let n = out.len().min(self.header.len() - self.offset);
            out[..n].copy_from_slice(&self.header[self.offset..self.offset + n]);
            self.offset += n;
            Ok(n)
        }
    }

    let mut reader = Counting {
        header: raw_header(CONTROL_MAGIC, CONTROL_VERSION, 1, ABSURD),
        offset: 0,
        requested: 0,
    };
    let error = ControlFrame::read(&mut reader).expect_err("absurd length must be rejected");

    assert_eq!(control_error(&error), ControlError::PayloadTooLarge(ABSURD));
    assert_eq!(
        reader.requested, FRAME_HEADER_LEN,
        "the payload must never be read"
    );
    assert!(
        PEAK_ALLOCATION.load(Ordering::Relaxed) < ABSURD as usize,
        "no allocation may be sized by the declared length"
    );
    // Nothing here times the rejection. The two checks above are what "cheap"
    // means and they hold whatever the machine was doing; a wall-clock ceiling
    // beside them would add no information and would be the only line in this
    // test a descheduled thread could break.

    // And the same over loopback, where it must also close the connection.
    assert_eq!(
        reject_raw_header(raw_header(CONTROL_MAGIC, CONTROL_VERSION, 1, ABSURD)),
        ControlError::PayloadTooLarge(ABSURD)
    );
}

#[test]
fn a_silent_peer_times_out_instead_of_hanging() {
    let server = server("host");
    let addr = server.local_addr().unwrap();
    let accepting = {
        let server = Arc::clone(&server);
        thread::spawn(move || server.accept_session(SECOND).expect("accept session"))
    };
    let mut client = ControlClient::connect(addr, SECOND).expect("connect");
    client.hello("mac").expect("hello");
    let mut session = accepting.join().unwrap();

    // The client is alive and says nothing for the rest of the test.
    let timeout = Nanos::from_millis(150);
    let started = Timestamp::now();
    for _ in 0..3 {
        assert!(
            session
                .next_message(timeout)
                .expect("timeout is not an error")
                .is_none(),
            "a silent peer must yield Ok(None)"
        );
    }
    let elapsed = Timestamp::now().saturating_since(started);
    assert!(
        elapsed >= Nanos::from_millis(400),
        "each poll must actually wait its timeout, waited {elapsed}"
    );
    assert!(
        elapsed < Nanos::from_millis(1_500),
        "polls must not accumulate past their timeouts, waited {elapsed}"
    );

    // Still a working connection afterwards.
    client.stop_stream().expect("send stop");
    assert_eq!(
        session.next_message(SECOND).unwrap(),
        Some(ControlMessage::StopStream)
    );
}

#[test]
fn a_full_exchange_pings_and_starts_a_stream() {
    let server = server("host");
    let addr = server.local_addr().unwrap();
    let media: SocketAddr = "127.0.0.1:50100".parse().unwrap();
    let (tx, rx) = mpsc::channel();

    let responder = {
        let server = Arc::clone(&server);
        thread::spawn(move || {
            let mut session = server.accept_session(SECOND).expect("accept session");
            loop {
                let Some(message) = session.next_message(SECOND).expect("poll") else {
                    continue;
                };
                match message {
                    ControlMessage::UdpBind { session_token } => {
                        assert!(server.bind_udp_peer(session_token, media));
                        session
                            .send(&ControlMessage::UdpBindAck {
                                ssrc: Ssrc(0x1234_5678),
                                payload_type: 96,
                                clock_rate: 90_000,
                            })
                            .expect("ack");
                    }
                    ControlMessage::Ping { nonce } => {
                        session.send(&ControlMessage::Pong { nonce }).expect("pong");
                    }
                    ControlMessage::StartStream { width, height, fps } => {
                        tx.send((width, height, fps)).unwrap();
                    }
                    ControlMessage::StopStream => break,
                    other => panic!("unexpected {other:?}"),
                }
            }
        })
    };

    let mut client = ControlClient::connect(addr, SECOND).expect("connect");
    let token = client.hello("mac").expect("hello");

    let binding = client.bind_udp().expect("bind udp");
    assert_eq!(binding.ssrc, Ssrc(0x1234_5678));
    assert_eq!(binding.payload_type, 96);
    assert_eq!(binding.clock_rate, 90_000);
    assert_eq!(server.udp_peer(token), Some(media));

    let rtt = client.ping().expect("ping");
    assert!(
        rtt > Nanos::ZERO && rtt < Nanos::from_millis(500),
        "rtt {rtt}"
    );

    client.start_stream(1920, 1080, 120).expect("start");
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        (1920, 1080, 120)
    );

    client.stop_stream().expect("stop");
    responder.join().unwrap();
}

/// Stand-in for the media loop: `ticks` items paced from an absolute deadline,
/// returning the inter-item intervals in nanoseconds.
///
/// The deadline advances by a fixed period rather than from "now", so a late
/// wakeup is absorbed by the next interval instead of accumulating.
fn produce(ticks: usize, period: Nanos) -> Vec<u64> {
    produce_with(ticks, period, || {})
}

/// The same, with work done on the producer's own thread after each item is
/// stamped, so that the work delays the item after it rather than the one it
/// belongs to. That is what a media loop doing anything else looks like from
/// the inside.
fn produce_with(ticks: usize, period: Nanos, mut per_tick: impl FnMut()) -> Vec<u64> {
    let mut deadline = Timestamp::now().add(period);
    let mut previous: Option<Timestamp> = None;
    let mut intervals = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        wait_until(deadline);
        let now = Timestamp::now();
        if let Some(previous) = previous {
            intervals.push(now.saturating_since(previous).get());
        }
        previous = Some(now);
        per_tick();
        deadline = deadline.add(period);
    }
    intervals
}

fn percentile(intervals: &[u64], fraction: f64) -> Nanos {
    let mut sorted = intervals.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 * fraction) as usize).min(sorted.len() - 1);
    Nanos(sorted[index])
}

/// How long a sender waits on a write that has nowhere to go.
///
/// Short enough that a sender keeps hammering rather than parking once for the
/// whole stall, which is what makes the wedge a continuous condition instead of
/// a single event.
const WRITE_TIMEOUT: Nanos = Nanos::from_millis(100);

/// A control connection whose peer has stopped reading.
///
/// The client is held rather than used: dropping it closes the socket, the
/// server's writes drain and complete, and the condition being studied stops
/// existing. The listener is held for the same reason.
struct Wedged {
    client: ControlClient,
    server: Arc<ControlServer>,
    session: ControlSession,
}

fn wedged() -> Wedged {
    let server = server("host");
    let addr = server.local_addr().unwrap();
    let accepting = {
        let server = Arc::clone(&server);
        thread::spawn(move || server.accept_session(SECOND).expect("accept session"))
    };

    let mut client = ControlClient::connect(addr, SECOND).expect("connect");
    client.hello("mac").expect("hello");
    // From here the client never reads again. The server's writes have nowhere
    // to go.
    let session = accepting.join().unwrap();
    session
        .set_write_timeout(WRITE_TIMEOUT)
        .expect("write timeout");

    Wedged {
        client,
        server,
        session,
    }
}

/// A near-maximum frame.
///
/// macOS grows a loopback socket buffer while it is being filled, so a stream
/// of thirty-byte pings outruns the buffer for seconds and the connection never
/// actually wedges; a test that only sometimes reproduces the condition it
/// names is worse than no test. At 60 KiB a frame the two 4 MiB buffers are
/// full in well under a hundred writes.
fn filler() -> ControlMessage {
    ControlMessage::ServerHello {
        protocol_version: PROTOCOL_VERSION,
        session_token: SessionToken::from_bytes([0u8; 16]),
        server_name: "x".repeat(60_000),
    }
}

/// Frames that got out before the buffers filled, and writes that timed out
/// afterwards.
///
/// Both are populations rather than rates. A connection that carried nothing
/// was never wedged but broken, and one whose every write succeeded was never
/// wedged at all, so a run reporting a zero in either has proved nothing
/// whatever the cadence figures next to it say.
#[derive(Clone, Copy, Default)]
struct Writes {
    out: u64,
    blocked: u64,
}

impl Writes {
    fn record(&mut self, sent: io::Result<()>) {
        match sent {
            Ok(()) => self.out += 1,
            Err(_) => self.blocked += 1,
        }
    }
}

/// The half of the isolation claim that is logic: a peer that stops reading
/// must turn the server's writes into timeouts rather than into a thread that
/// never comes back.
///
/// Nothing here is a measurement, which is why it stays in the suite. The
/// deadline is a liveness bound two orders of magnitude looser than the time a
/// loopback buffer takes to fill, not a claim about how fast this machine is.
#[test]
fn a_control_peer_that_stops_reading_turns_the_servers_writes_into_timeouts() {
    let mut wedged = wedged();
    let filler = filler();
    let mut writes = Writes::default();

    let deadline = Timestamp::now().add(Nanos::from_millis(30_000));
    while writes.blocked == 0 && Timestamp::now() < deadline {
        writes.record(wedged.session.send(&filler));
    }

    assert!(
        writes.out > 0,
        "the server got no frame out at all, so the connection was broken \
         rather than wedged"
    );
    assert!(
        writes.blocked > 0,
        "no write timed out in thirty seconds, so either the socket never \
         filled or a send is still parked in the kernel"
    );
}

/// How much cadence the isolation claim allows a wedged control connection to
/// cost, and where the number comes from.
///
/// The client aims each frame at least 2.00 ms in front of the refresh it is
/// meant for - `MARGIN_FLOOR` in `macos/client/src/phase.rs`. That cushion is
/// not spare: 1.22 ms of it is the reference run's decode spread between p50
/// and p99, and 0.25 ms is the phase loop's dead zone, which leaves 0.53 ms
/// nothing has claimed. A producer perturbed by more than that eats into a
/// cushion something else is already spending, and the frame it lands on waits
/// a whole period for the next refresh.
///
/// Restated here rather than imported, because this crate must not depend on
/// the client; a number that crosses that seam by hand has to say where it came
/// from.
const CADENCE_TOLERANCE: Nanos = Nanos(530_000);

/// A second of production thrown away before the quiet half is taken.
///
/// The process has just been started by cargo, which is still linking, waiting
/// and reaping around it, and a baseline taken in that second describes cargo
/// rather than the machine: it read 9.05 ms at p99 with a 13.07 ms worst
/// interval on an otherwise idle Mac that then held 8.34 ms for the rest of the
/// run. Since the quiet half is the evidence the gate refuses on, a systematic
/// artefact in it would spend its refusals on the wrong runs.
fn warm_up() {
    produce(120, FRAME_PERIOD);
}

/// The one line the gate reads, and the only place these numbers leave the
/// process.
///
/// The bound is printed with the measurement rather than being known to the
/// script as well: a criterion stated in two places is a criterion that drifts,
/// so the gate applies the one the run was actually judged against. Every value
/// is named, so that a renamed key stops the gate instead of being read as a
/// zero - which is how a sibling harness once read 6001 captured packets as
/// none.
fn report(arm: &str, baseline: &[u64], stalled: &[u64], writes: Writes) -> i64 {
    let baseline_p99 = percentile(baseline, 0.99);
    let stalled_p99 = percentile(stalled, 0.99);
    let perturbation = stalled_p99.get() as i64 - baseline_p99.get() as i64;

    println!(
        "cadence-isolation arm={arm} period_ns={} tolerance_ns={} \
         baseline_intervals={} stalled_intervals={} \
         baseline_p99_ns={} stalled_p99_ns={} \
         baseline_max_ns={} stalled_max_ns={} perturbation_ns={perturbation} \
         frames_written={} blocked_writes={}",
        FRAME_PERIOD.get(),
        CADENCE_TOLERANCE.get(),
        baseline.len(),
        stalled.len(),
        baseline_p99.get(),
        stalled_p99.get(),
        percentile(baseline, 1.0).get(),
        percentile(stalled, 1.0).get(),
        writes.out,
        writes.blocked,
    );
    perturbation
}

/// The isolation measurement, and the reason the control plane is allowed a
/// thread of its own.
///
/// A control peer that stops reading while the server keeps writing is the
/// worst realistic case: the socket buffer fills, the server's writes block and
/// then time out, and the connection stays wedged for seconds. None of that may
/// be visible to a 120 fps producer.
///
/// Both halves are produced by this thread moments apart, so what changes
/// between them is the wedge and not where the producer was placed. Whether the
/// machine was in a position to be asked at all is the gate's question: the
/// quiet half is the evidence, and a machine whose quiet half could not hold
/// the cadence makes the gate refuse rather than fail.
///
/// Ignored because it is a measurement rather than logic. It needs three cores
/// it can have to itself for five seconds - one for the producer, which spins
/// out the last three milliseconds of every period, one for the writer
/// hammering the wedged socket, and one for everything else the machine is
/// doing. `tools/cadence-isolation-gate.sh` is where it runs and what refuses
/// on its behalf when they were not there.
#[test]
#[ignore = "a measurement: tools/cadence-isolation-gate.sh, three free cores"]
fn a_wedged_control_connection_does_not_perturb_a_120hz_producer() {
    const TICKS: usize = 240; // two seconds at 120 Hz

    warm_up();
    // The quiet half, with the control plane entirely absent.
    let baseline = produce(TICKS, FRAME_PERIOD);

    let Wedged {
        client,
        server,
        mut session,
    } = wedged();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let filler = filler();
            let mut writes = Writes::default();
            while !stop.load(Ordering::Relaxed) {
                writes.record(session.send(&filler));
            }
            writes
        })
    };

    let stalled = produce(TICKS, FRAME_PERIOD);
    stop.store(true, Ordering::Relaxed);
    let writes = writer.join().unwrap();
    drop(client);
    drop(server);

    let perturbation = report("wedged", &baseline, &stalled, writes);

    assert!(
        writes.blocked > 0,
        "the control connection never actually wedged, so this proves nothing"
    );
    assert!(
        writes.out > 0,
        "the server got no frame out at all, so the connection was broken \
         rather than wedged"
    );

    // The claim in the name is differential. Comparing each half against an
    // absolute 8.33 ms measures the machine's scheduler instead, and a busy
    // machine then fails a test about isolation while proving nothing either
    // way.
    assert!(
        perturbation < CADENCE_TOLERANCE.get() as i64,
        "the wedged connection cost the producer {:.3} ms at p99, against a \
         bound of {CADENCE_TOLERANCE}",
        perturbation as f64 / 1_000_000.0
    );
}

/// The arm that must fail the criterion above, and the reason passing it means
/// anything.
///
/// One thing changes from the arm above: the same wedge, the same timeout, the
/// same filler, sent from the producer's own thread instead of from a thread of
/// its own. That is the arrangement this design rejected, and a measurement
/// that cannot tell the two apart has nothing to say about the one it likes -
/// which is the shape both false passes in this project had.
///
/// Half as many ticks as the isolated arm, because every tick of its second
/// half costs the write timeout and the point is made a hundred times over by
/// the end of a second.
#[test]
#[ignore = "a measurement: tools/cadence-isolation-gate.sh, three free cores"]
fn a_producer_that_sends_on_the_control_plane_itself_is_wrecked_by_the_same_wedge() {
    const TICKS: usize = 120; // one second at 120 Hz

    warm_up();
    let baseline = produce(TICKS, FRAME_PERIOD);

    let mut wedged = wedged();
    let filler = filler();
    let mut writes = Writes::default();
    let stalled = produce_with(TICKS, FRAME_PERIOD, || {
        writes.record(wedged.session.send(&filler));
    });

    let perturbation = report("contended", &baseline, &stalled, writes);

    assert!(
        writes.blocked > 0,
        "the control connection never actually wedged, so this arm contended \
         with nothing"
    );
    assert!(
        perturbation >= CADENCE_TOLERANCE.get() as i64,
        "a producer making the blocking write itself lost only {:.3} ms at \
         p99, so this measurement cannot see the arrangement it exists to rule \
         out",
        perturbation as f64 / 1_000_000.0
    );
}
