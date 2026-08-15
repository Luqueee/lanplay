//! The receive half: a dedicated thread in a blocking `recv_from`, feeding the
//! depacketiser out of one preallocated buffer.
//!
//! The buffer is allocated once and never grows. That is the property the
//! `--stall-*` knobs exist to prove: freeze this loop and the sender keeps
//! sending, but the backlog accumulates in the kernel's `SO_RCVBUF` — which is
//! a fixed size and drops when full — not in this process.

use core::fmt;
use std::io;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{Nanos, Recorder, Stage, Timestamp, Trend};
use lanplay_transport::{
    Depacketizer, DepacketizerConfig, H264_PAYLOAD_TYPE, RxStats, parse_packet,
};

use crate::digest::Digests;
use crate::series::Series;
use crate::socket::{self, SocketBuffer};
use crate::wire::WireTimes;

/// Big enough for any datagram a conforming sender can emit, and for the
/// jumbo-frame MTUs `--mtu` will accept. One allocation for the whole run.
const RECV_BUFFER: usize = 65_536;

/// Access units the depacketiser may hold at once, in bytes. The ceiling
/// exists so a sender that claims a 500 MB frame gets its fragments dropped
/// instead of being handed the receiver's address space.
const MAX_ACCESS_UNIT_BYTES: usize = 4 * 1024 * 1024;

/// Packets the reorder ring holds while waiting for an earlier sequence
/// number. Sized for a whole 1080p access unit's worth of fragments so a
/// single reordered packet cannot orphan the frame around it.
const REORDER_WINDOW: usize = 256;

/// How long a blocked `recv_from` waits before the loop rechecks its stop
/// flag. Purely a shutdown affordance: it adds no latency to a packet that
/// has arrived.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// Idle time after which a standalone receiver decides the sender is gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);

/// Datagrams between depacketiser footprint samples. Every packet would be a
/// sample per 20 µs, which fits no slope worth reading; the peak is tracked
/// separately and misses nothing.
const MEMORY_SAMPLE_EVERY: u64 = 256;

pub struct ReceiveConfig {
    pub seconds: f64,
    pub socket_recv_buffer: Option<usize>,
    pub stall: Nanos,
    pub stall_every: u64,
    pub verify: bool,
}

pub struct ReceiveReport {
    pub rx: RxStats,
    /// RFC 3550 interarrival jitter, as the depacketiser estimates it.
    pub jitter: Nanos,
    pub inter_arrival: Series,
    /// First datagram out of `recv_from` against the same datagram leaving
    /// `send_to`: the floor of what this transport costs. Loopback only.
    pub wire_first_packet: Series,
    /// Reconstructed access unit in hand against its last datagram leaving
    /// `send_to`. Loopback only.
    pub wire_access_unit: Series,
    pub socket_buffer: SocketBuffer,
    pub datagrams: u64,
    pub datagram_bytes: u64,
    pub verified: u64,
    pub verify_failures: u64,
    /// Access units whose frame id had no digest to compare against.
    pub unverifiable: u64,
    /// Access units the depacketiser produced without a usable frame id.
    pub anonymous: u64,
    pub stalls: u64,
    pub effective_reorder_window: usize,
    /// The depacketiser's own footprint over the run. Process RSS is a blunt
    /// instrument next to this: it moves for reasons that have nothing to do
    /// with the transport, and it would hide a reorder ring that grew by a
    /// few kilobytes under a hostile sender.
    pub depacketizer_memory: Trend,
    pub depacketizer_peak_bytes: usize,
    pub depacketizer_peak_packets: usize,
    pub elapsed: Nanos,
    pub recv_error: Option<String>,
}

impl ReceiveReport {
    pub fn packets_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        self.datagrams as f64 / seconds
    }
}

impl fmt::Debug for ReceiveReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceiveReport")
            .field("datagrams", &self.datagrams)
            .field("verify_failures", &self.verify_failures)
            .finish_non_exhaustive()
    }
}

pub fn run(
    socket: &UdpSocket,
    config: &ReceiveConfig,
    recorder: &Recorder,
    digests: Option<&Digests>,
    wire: Option<&WireTimes>,
    stop: &Arc<AtomicBool>,
) -> io::Result<ReceiveReport> {
    let socket_buffer = socket::recv_buffer(socket, config.socket_recv_buffer)?;
    socket.set_read_timeout(Some(POLL_TIMEOUT))?;

    let mut depacketizer = Depacketizer::new(DepacketizerConfig {
        payload_type: H264_PAYLOAD_TYPE,
        reorder_window: REORDER_WINDOW,
        max_access_unit_bytes: MAX_ACCESS_UNIT_BYTES,
    });

    let mut buffer = vec![0u8; RECV_BUFFER];
    let mut report = ReceiveReport {
        rx: RxStats::default(),
        jitter: Nanos::ZERO,
        inter_arrival: Series::new("inter-arrival"),
        wire_first_packet: Series::new("wire first packet"),
        wire_access_unit: Series::new("wire access unit"),
        socket_buffer,
        datagrams: 0,
        datagram_bytes: 0,
        verified: 0,
        verify_failures: 0,
        unverifiable: 0,
        anonymous: 0,
        stalls: 0,
        effective_reorder_window: depacketizer.reorder_window(),
        depacketizer_memory: Trend::new(),
        depacketizer_peak_bytes: depacketizer.memory_bytes(),
        depacketizer_peak_packets: 0,
        elapsed: Nanos::ZERO,
        recv_error: None,
    };

    let mut previous_arrival: Option<Timestamp> = None;
    let mut first_arrival: Option<Timestamp> = None;
    let mut last_arrival = Timestamp::now();
    // Marking receive-first only when the frame id advances keeps a straggler
    // from an older access unit from re-opening a frame the collector has
    // already moved past, which would show up as a duplicate mark.
    let mut newest = FrameId::NONE;
    let mut newest_last_at = Timestamp::now();

    let deadline = Duration::from_secs_f64(config.seconds.max(0.0));

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match socket.recv_from(&mut buffer) {
            Ok((len, _)) => {
                let at = Timestamp::now();
                report.datagrams += 1;
                report.datagram_bytes += len as u64;
                if let Some(previous) = previous_arrival {
                    report.inter_arrival.record(at.saturating_since(previous));
                }
                previous_arrival = Some(at);
                last_arrival = at;
                let started = *first_arrival.get_or_insert(at);

                let datagram = &buffer[..len];
                // The depacketiser does not expose per-packet frame ids, and
                // the receive-side marks need them before reassembly
                // completes. Re-reading the header is a handful of loads.
                let frame = parse_packet(datagram)
                    .ok()
                    .and_then(|packet| packet.header.frame_id);
                if let Some(frame) = frame {
                    if frame.get() > newest.get() {
                        if !newest.is_none() {
                            recorder.mark_at(newest, Stage::NetworkReceiveLast, newest_last_at);
                        }
                        newest = frame;
                        recorder.mark_at(frame, Stage::NetworkReceiveFirst, at);
                        if let Some(wire) = wire
                            && let Some((send_first, _)) = wire.get(frame)
                            && let Some(delta) = at.since(send_first)
                        {
                            report.wire_first_packet.record(delta);
                        }
                    }
                    if frame == newest {
                        newest_last_at = at;
                    }
                }

                if let Some(unit) = depacketizer.push(datagram, at) {
                    let reassembled = Timestamp::now();
                    if unit.id.is_none() {
                        report.anonymous += 1;
                    } else {
                        recorder.mark_at(unit.id, Stage::FrameReassembled, reassembled);
                        if let Some(wire) = wire
                            && let Some((_, send_last)) = wire.get(unit.id)
                            && let Some(delta) = reassembled.since(send_last)
                        {
                            report.wire_access_unit.record(delta);
                        }
                    }
                    if config.verify
                        && let Some(digests) = digests
                    {
                        match digests.matches(unit.id, &unit.data) {
                            Some(true) => report.verified += 1,
                            Some(false) => report.verify_failures += 1,
                            None => report.unverifiable += 1,
                        }
                    }
                }

                // Sampled after every push, because the moment worth catching
                // is mid-reassembly: a ring that grows only while fragments
                // are outstanding is invisible to a between-frames sample.
                let footprint = depacketizer.memory_bytes();
                report.depacketizer_peak_bytes = report.depacketizer_peak_bytes.max(footprint);
                report.depacketizer_peak_packets = report
                    .depacketizer_peak_packets
                    .max(depacketizer.buffered_packets());
                if report.datagrams.is_multiple_of(MEMORY_SAMPLE_EVERY) {
                    report.depacketizer_memory.record_at(at, footprint as f64);
                }

                if config.stall_every > 0
                    && config.stall.get() > 0
                    && report.datagrams.is_multiple_of(config.stall_every)
                {
                    report.stalls += 1;
                    thread::sleep(config.stall.as_duration());
                }

                if !deadline.is_zero() && at.saturating_since(started).as_duration() >= deadline {
                    break;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                // A standalone receiver has no other way to learn the sender
                // finished; in loopback the driver sets `stop` instead.
                if first_arrival.is_some()
                    && Timestamp::now()
                        .saturating_since(last_arrival)
                        .as_duration()
                        >= IDLE_TIMEOUT
                {
                    break;
                }
            }
            Err(err) => {
                report.recv_error = Some(err.to_string());
                break;
            }
        }
    }

    if !newest.is_none() {
        recorder.mark_at(newest, Stage::NetworkReceiveLast, newest_last_at);
    }
    report.rx = *depacketizer.stats();
    report.jitter = depacketizer.jitter();
    report.elapsed = match first_arrival {
        Some(first) => last_arrival.saturating_since(first),
        None => Nanos::ZERO,
    };
    Ok(report)
}
