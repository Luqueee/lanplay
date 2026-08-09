//! Control plane over real loopback TCP.
//!
//! The last test in this file is the one that matters. Everything else checks
//! that the handshake and the framing behave; that one checks that the control
//! plane cannot reach the media path even when it is completely wedged.

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
    ControlServer, FRAME_HEADER_LEN, PROTOCOL_VERSION, SessionToken, Ssrc,
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
    let started = Timestamp::now();
    let error = ControlFrame::read(&mut reader).expect_err("absurd length must be rejected");
    let elapsed = Timestamp::now().saturating_since(started);

    assert_eq!(control_error(&error), ControlError::PayloadTooLarge(ABSURD));
    assert_eq!(
        reader.requested, FRAME_HEADER_LEN,
        "the payload must never be read"
    );
    assert!(
        PEAK_ALLOCATION.load(Ordering::Relaxed) < ABSURD as usize,
        "no allocation may be sized by the declared length"
    );
    assert!(elapsed < Nanos::from_millis(50), "rejection must be cheap");

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

/// The isolation test.
///
/// A control peer that stops reading while the server keeps writing is the
/// worst realistic case: the socket buffer fills, the server's writes block
/// and then time out, and the connection stays wedged for seconds. None of
/// that may be visible to a 120 fps producer.
#[test]
fn a_wedged_control_connection_does_not_perturb_a_120hz_producer() {
    const TICKS: usize = 240; // two seconds at 120 Hz

    let (phase_tx, phase_rx) = mpsc::channel();
    let (baseline_tx, baseline_rx) = mpsc::channel();
    let producer = thread::spawn(move || {
        let baseline = produce(TICKS, FRAME_PERIOD);
        baseline_tx.send(()).unwrap();
        phase_rx.recv().unwrap();
        (baseline, produce(TICKS, FRAME_PERIOD))
    });

    // Baseline runs with the control plane entirely absent.
    baseline_rx.recv().unwrap();

    let server = server("host");
    let addr = server.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writes_blocked = Arc::new(AtomicUsize::new(0));

    let writer = {
        let server = Arc::clone(&server);
        let stop = Arc::clone(&stop);
        let writes_blocked = Arc::clone(&writes_blocked);
        thread::spawn(move || {
            let mut session = server.accept_session(SECOND).expect("accept session");
            // Short enough that the writer keeps hammering rather than parking
            // once for the whole stall.
            session
                .set_write_timeout(Nanos::from_millis(100))
                .expect("write timeout");
            // Near-maximum frames. macOS grows a loopback socket buffer while
            // it is being filled, so a stream of thirty-byte pings outruns the
            // buffer for seconds and the connection never actually wedges; a
            // test that only sometimes reproduces the condition it names is
            // worse than no test. At 60 KiB a frame the two 4 MiB buffers are
            // full in well under a hundred writes.
            let filler = ControlMessage::ServerHello {
                protocol_version: PROTOCOL_VERSION,
                session_token: SessionToken::from_bytes([0u8; 16]),
                server_name: "x".repeat(60_000),
            };
            let mut sent = 0u64;
            while !stop.load(Ordering::Relaxed) {
                match session.send(&filler) {
                    Ok(()) => sent += 1,
                    Err(_) => {
                        writes_blocked.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            sent
        })
    };

    let mut client = ControlClient::connect(addr, SECOND).expect("connect");
    client.hello("mac").expect("hello");
    // From here the client never reads again. The server's writes have nowhere
    // to go.
    phase_tx.send(()).unwrap();

    let (baseline, stalled) = producer.join().unwrap();
    stop.store(true, Ordering::Relaxed);
    let sent = writer.join().unwrap();
    drop(client);

    let baseline_p99 = percentile(&baseline, 0.99);
    let stalled_p99 = percentile(&stalled, 0.99);
    let baseline_max = percentile(&baseline, 1.0);
    let stalled_max = percentile(&stalled, 1.0);
    println!(
        "cadence p99: baseline {baseline_p99}, stalled {stalled_p99} \
         (max {baseline_max} / {stalled_max}); \
         control frames written {sent}, blocked writes {}",
        writes_blocked.load(Ordering::Relaxed)
    );

    assert!(
        writes_blocked.load(Ordering::Relaxed) > 0,
        "the control connection never actually wedged, so this proves nothing"
    );

    // The claim in the name is differential: the wedged connection must not
    // perturb the producer. Comparing each half against an absolute 8.33 ms
    // measures the machine's scheduler instead, and a busy machine then fails
    // a test about isolation while proving nothing either way. Both halves run
    // on the same machine moments apart, so scheduling noise lands on both and
    // cancels in the difference.
    let perturbation = stalled_p99.get() as f64 - baseline_p99.get() as f64;
    let tolerance = Nanos::from_millis(1).get() as f64;
    assert!(
        perturbation < tolerance,
        "the wedged connection cost the producer {:.3} ms at p99 \
         (baseline {baseline_p99}, stalled {stalled_p99})",
        perturbation / 1_000_000.0
    );

    // A loose floor under the whole measurement: if the baseline itself is
    // nowhere near the target the machine was not producing at 120 Hz at all,
    // and a small difference between two useless numbers proves nothing.
    let target = FRAME_PERIOD.get() as f64;
    let sanity = Nanos::from_millis(4).get() as f64;
    assert!(
        (baseline_p99.get() as f64 - target).abs() < sanity,
        "baseline p99 {baseline_p99} is too far from 8.33 ms for this run to \
         say anything about isolation"
    );
}
