//! The one hop a phase request makes on the host, from the process that hears
//! it to the process that can act on it.
//!
//! The viewer negotiates a decoder with the encoder host, so its request
//! arrives there, but the phase worth moving is the source's draw and the
//! source is a different process on the same machine. See [`crate::pace`] for
//! why the capture side cannot serve the request itself.
//!
//! Loopback UDP, unreliably, deliberately. A request is a correction computed
//! from a batch of recent frames, and a lost one is simply a correction that
//! did not happen: the next batch measures the same error and asks again.
//! Retransmitting would deliver an estimate that has since been superseded,
//! which is worse than delivering nothing, and a stream socket would make the
//! producer's startup depend on the encoder's.
//!
//! Eight bytes, magic first. The producer binds a fixed port on the loopback
//! interface, and anything else that lands there - a stray probe, an old build,
//! a scanner - must be discarded rather than read as a delay, because a
//! misparsed datagram would move the schedule by an arbitrary amount.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use core::fmt;

use lanplay_telemetry::Nanos;

use crate::pace::PhaseInbox;

/// Port the producer listens on. Fixed rather than negotiated: the two
/// processes are started independently, by different scripts, minutes apart.
pub const DEFAULT_PORT: u16 = 5010;

/// First four bytes of every request.
const MAGIC: [u8; 4] = *b"LPPH";

/// A request on the wire: magic, the delay in nanoseconds, then the rate the
/// asker believes this producer is running at, all little endian.
///
/// The rate is carried because a delay alone cannot be checked. It is only
/// ever a fraction of a period, so a request computed against the wrong period
/// is still a plausible number, and the two host processes are started
/// independently: a producer left running at 144 from an earlier experiment
/// will happily serve a run configured for 120. Folding keeps that safe but
/// only witnesses it when a delay happens to reach a whole period, which for a
/// converged loop correcting forwards it never does. One extra field turns a
/// statistical hint into a statement.
pub const DATAGRAM_LEN: usize = 12;

/// Where a request goes by default: this machine, nowhere else. A phase
/// request is meaningless to any host but the one drawing the frames.
pub const fn default_target() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT)
}

/// One request, as it travels between the two processes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    /// How much longer to hold the next frame back.
    pub delay_nanos: u32,
    /// The rate the asker computed that delay against.
    pub fps: u32,
}

pub fn encode(request: Request) -> [u8; DATAGRAM_LEN] {
    let mut datagram = [0u8; DATAGRAM_LEN];
    datagram[..4].copy_from_slice(&MAGIC);
    datagram[4..8].copy_from_slice(&request.delay_nanos.to_le_bytes());
    datagram[8..].copy_from_slice(&request.fps.to_le_bytes());
    datagram
}

/// The request a datagram carries, or `None` if it is not one of ours.
pub fn decode(datagram: &[u8]) -> Option<Request> {
    if datagram.len() != DATAGRAM_LEN || datagram[..4] != MAGIC {
        return None;
    }
    let word = |at: usize| {
        u32::from_le_bytes([
            datagram[at],
            datagram[at + 1],
            datagram[at + 2],
            datagram[at + 3],
        ])
    };
    Some(Request {
        delay_nanos: word(4),
        fps: word(8),
    })
}

/// The sending half: one socket, one target, and a record of what went down it.
///
/// Shared by reference so the thread reading the control connection and the
/// thread writing the report see the same counters.
#[derive(Debug)]
pub struct Relay {
    socket: UdpSocket,
    target: SocketAddr,
    /// The rate this run was configured for, sent with every request so the
    /// producer can say whether it is the rate it is actually pacing.
    fps: u32,
    sent: AtomicU64,
    nanos: AtomicU64,
    errors: AtomicU64,
}

impl Relay {
    /// Binds an ephemeral local port. Nothing is sent until a request arrives.
    pub fn open(target: SocketAddr, fps: u32) -> io::Result<Relay> {
        let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))?;
        Ok(Relay {
            socket,
            target,
            fps,
            sent: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        })
    }

    /// Forwards one request, counting whatever happened.
    ///
    /// A failure is recorded and swallowed. Nothing downstream of a phase
    /// correction depends on it, and a producer that is not running - or not
    /// listening, on an older build - must not be able to end a stream.
    pub fn send(&self, delay_nanos: u32) {
        self.sent.fetch_add(1, Ordering::Relaxed);
        self.nanos
            .fetch_add(u64::from(delay_nanos), Ordering::Relaxed);
        let request = Request {
            delay_nanos,
            fps: self.fps,
        };
        if self.socket.send_to(&encode(request), self.target).is_err() {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn target(&self) -> SocketAddr {
        self.target
    }

    pub fn counts(&self) -> RelayCounts {
        RelayCounts {
            target: self.target,
            sent: self.sent.load(Ordering::Relaxed),
            requested: Nanos(self.nanos.load(Ordering::Relaxed)),
            errors: self.errors.load(Ordering::Relaxed),
        }
    }
}

/// What one run relayed, for the encoder host's report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelayCounts {
    pub target: SocketAddr,
    pub sent: u64,
    /// Everything asked for, before the producer folds any of it into a period.
    pub requested: Nanos,
    pub errors: u64,
}

impl fmt::Display for RelayCounts {
    /// Every run, zeros included. Printed only when something happened, the
    /// line's absence would not distinguish a viewer that never asked from a
    /// host that dropped the request.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "phase requests: {} relayed to {}, {} asked for, {} not sent",
            self.sent, self.target, self.requested, self.errors
        )
    }
}

/// Receives requests for the rest of the run, on its own thread.
///
/// The socket blocks here so the present loop never has to look at it. The
/// thread ends when the socket does, which is when the process does: a producer
/// runs until its window closes and there is nothing to shut down in between.
///
/// `fps` is what this producer is pacing at, and it is checked against what
/// the asker thinks it is pacing at. A disagreement is said once and then the
/// request is obeyed anyway: the delay is still folded into a real period, so
/// nothing breaks, but every correction computed against the wrong period aims
/// somewhere other than where it says. Announcing it is the difference between
/// a run that misbehaves and a run that explains itself.
pub fn listen(port: u16, fps: u32, inbox: Arc<PhaseInbox>) -> io::Result<()> {
    let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))?;
    std::thread::Builder::new()
        .name("phase".into())
        .spawn(move || {
            let mut datagram = [0u8; 64];
            let mut consecutive_errors = 0u32;
            let mut disagreement_said = false;
            loop {
                match socket.recv_from(&mut datagram) {
                    Ok((len, _)) => {
                        consecutive_errors = 0;
                        if let Some(request) = decode(&datagram[..len]) {
                            if request.fps != fps && !disagreement_said {
                                disagreement_said = true;
                                eprintln!(
                                    "present-source: phase requests are computed for {} fps \
                                     and this producer is pacing {fps}; every correction will \
                                     aim at the wrong instant until one of the two is changed",
                                    request.fps
                                );
                            }
                            inbox.post(request.delay_nanos);
                        }
                    }
                    // A datagram too large for the buffer, or a loopback peer
                    // that vanished between its send and this receive, which
                    // Windows reports here rather than there. The next request
                    // is a batch away and recovers from both. A socket that has
                    // actually died, though, fails instantly and forever, so a
                    // run of failures with no receive in between ends the
                    // thread instead of spinning a core on the machine the
                    // whole pipeline is being measured on.
                    Err(_) => {
                        consecutive_errors += 1;
                        if consecutive_errors >= 64 {
                            break;
                        }
                    }
                }
            }
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(delay_nanos: u32, fps: u32) -> Request {
        Request { delay_nanos, fps }
    }

    #[test]
    fn a_datagram_says_exactly_two_things() {
        for asked in [0, 4_166_666, u32::MAX] {
            let sent = request(asked, 120);
            assert_eq!(decode(&encode(sent)), Some(sent));
        }
        // The rate travels intact alongside the delay, because a delay alone
        // cannot be told apart from one computed for a different producer.
        assert_eq!(decode(&encode(request(1, 144))).unwrap().fps, 144);
    }

    #[test]
    fn anything_that_is_not_a_request_is_discarded() {
        // A schedule moved by a misread datagram is worse than one never
        // moved, so the magic and the length are both checked.
        assert_eq!(decode(b""), None);
        assert_eq!(decode(b"LPPH"), None);
        // The length of the format this replaced: a peer that never learnt the
        // rate field is not a peer whose delays can be trusted.
        assert_eq!(decode(b"LPPH\x01\x02\x03\x04"), None);
        assert_eq!(decode(b"LPPH\x01\x02\x03\x04\x05\x06\x07\x08\x09"), None);
        assert_eq!(decode(b"HTTP\x01\x02\x03\x04\x05\x06\x07\x08"), None);
    }

    /// The hop itself, over a real loopback socket: what the encoder host
    /// sends is what the producer's pacer is asked for.
    #[test]
    fn a_relayed_request_reaches_the_inbox() {
        let inbox = Arc::new(PhaseInbox::new());
        // Port 0 then read it back: a fixed port would make this test fail
        // whenever a producer is running on the same machine.
        let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        listen(port, 120, Arc::clone(&inbox)).unwrap();

        let relay =
            Relay::open(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), 120).unwrap();
        relay.send(3_141_592);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let delivered = loop {
            if let Some(delay) = inbox.take() {
                break Some(delay);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(delivered, Some(3_141_592));

        let counts = relay.counts();
        assert_eq!(counts.sent, 1);
        assert_eq!(counts.requested, Nanos(3_141_592));
        assert_eq!(counts.errors, 0);
    }

    #[test]
    fn a_producer_that_is_not_there_does_not_end_the_run() {
        // Nothing is bound on the target port. The send either succeeds into
        // the void or fails; either way the caller carries on and the report
        // says what happened.
        let relay = Relay::open(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1), 120).unwrap();
        relay.send(1_000);
        let counts = relay.counts();
        assert_eq!(counts.sent, 1);
        assert_eq!(counts.requested, Nanos(1_000));
    }

    /// A request computed for another rate is still obeyed. It is folded into
    /// a real period by the pacer and the producer says so on its way past;
    /// refusing it would be a new way for the two processes to disagree
    /// silently.
    #[test]
    fn a_request_from_a_producer_of_another_rate_is_still_delivered() {
        let inbox = Arc::new(PhaseInbox::new());
        let socket = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        listen(port, 144, Arc::clone(&inbox)).unwrap();

        let relay =
            Relay::open(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port), 120).unwrap();
        relay.send(8_208_333);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let delivered = loop {
            if let Some(delay) = inbox.take() {
                break Some(delay);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        assert_eq!(delivered, Some(8_208_333));
    }
}
