//! The phase 1A path: the same fixture, but routed through RTP over UDP
//! loopback before it reaches the decoder.
//!
//! Three threads, matching the shape the real system will have. The feed
//! thread owns the fixture and the packetiser and does nothing but send; the
//! receive thread owns the socket, the depacketiser and the decoder; the main
//! thread renders. Nothing is shared between them except the frame slot and
//! the telemetry recorder, both of which are lock-free or nearly so.
//!
//! Everything runs on one clock, so the `network` segment is real here in a
//! way it will not be across a wire until phase 8 estimates the offset.

use std::collections::VecDeque;
use std::error::Error;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lanplay_decoder_videotoolbox::VideoToolboxDecoder;
use lanplay_protocol::FrameId;
use lanplay_telemetry::{Nanos, Recorder, Stage, Timestamp, Trend, wait_until};
use lanplay_transport::{
    Depacketizer, DepacketizerConfig, H264_CLOCK_RATE, H264_PAYLOAD_TYPE, MAX_UDP_PAYLOAD,
    Packetizer, RtpClock, RxStats, Ssrc, TxStats, parse_packet, random_u32,
};
use lanplay_video_core::{AccessUnitSource, FixtureSource, VideoDecoder};
use sha2::{Digest, Sha256};

/// Digests kept while their access unit is in flight. Loopback delivers within
/// microseconds, so this only has to outlive a handful of frames; a bound is
/// what stops a misbehaving receiver from turning verification into a leak.
const VERIFY_WINDOW: usize = 1024;

/// How long the receive loop blocks before checking whether it should stop.
const RECV_TIMEOUT: Duration = Duration::from_millis(100);

pub struct TransportOutcome {
    pub tx: TxStats,
    pub rx: RxStats,
    pub jitter: Nanos,
    pub verified: u64,
    pub mismatched: u64,
    /// Bytes handed to the socket, against the bytes the access units held.
    pub wire_bytes: u64,
    pub payload_bytes: u64,
}

impl TransportOutcome {
    pub fn overhead_ratio(&self) -> f64 {
        if self.payload_bytes == 0 {
            return 0.0;
        }
        self.wire_bytes as f64 / self.payload_bytes as f64
    }
}

/// Shared between the two transport threads: the sender records what it sent,
/// the receiver checks it.
pub struct VerifyLedger {
    entries: parking_lot::Mutex<VecDeque<(FrameId, [u8; 32])>>,
    enabled: bool,
}

impl VerifyLedger {
    pub fn new(enabled: bool) -> Arc<Self> {
        Arc::new(VerifyLedger {
            entries: parking_lot::Mutex::new(VecDeque::with_capacity(VERIFY_WINDOW)),
            enabled,
        })
    }

    fn record(&self, frame: FrameId, bytes: &[u8]) {
        if !self.enabled {
            return;
        }
        let mut entries = self.entries.lock();
        if entries.len() == VERIFY_WINDOW {
            entries.pop_front();
        }
        entries.push_back((frame, digest(bytes)));
    }

    /// Returns whether the reconstructed access unit matched, or `None` when
    /// verification is off or the original has already aged out.
    fn check(&self, frame: FrameId, bytes: &[u8]) -> Option<bool> {
        if !self.enabled {
            return None;
        }
        let mut entries = self.entries.lock();
        let position = entries.iter().position(|(id, _)| *id == frame)?;
        // Everything before the match is older and will never be claimed.
        entries.drain(..position);
        let (_, expected) = entries.pop_front()?;
        Some(digest(bytes) == expected)
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

pub struct SenderConfig {
    pub target: SocketAddr,
    pub fps: f64,
    pub frames: u64,
    pub mtu: usize,
}

/// Feeds the fixture into the socket, paced from absolute deadlines.
pub fn send_loop(
    socket: UdpSocket,
    mut source: FixtureSource,
    recorder: Recorder,
    ledger: Arc<VerifyLedger>,
    config: SenderConfig,
    stop: Arc<AtomicBool>,
) -> Result<(TxStats, u64, u64), Box<dyn Error + Send + Sync>> {
    let clock = RtpClock::new(H264_CLOCK_RATE, random_u32());
    let mut packetizer = Packetizer::new(
        Ssrc(random_u32()),
        clock,
        H264_PAYLOAD_TYPE,
        config.mtu.min(MAX_UDP_PAYLOAD),
    );
    let mut stats = TxStats::default();
    let mut payload_bytes = 0u64;
    let mut wire_bytes = 0u64;

    let period = Nanos((1_000_000_000.0 / config.fps) as u64);
    let start = Timestamp::now();

    for index in 0..config.frames {
        if stop.load(Ordering::Acquire) {
            break;
        }
        wait_until(start.add(Nanos(period.get() * index)));

        let Some(unit) = source.next_access_unit() else {
            break;
        };
        recorder.mark(unit.id, Stage::FrameCreated);
        ledger.record(unit.id, &unit.data);
        payload_bytes += unit.data.len() as u64;

        recorder.mark(unit.id, Stage::PacketizationStart);
        let mut first = true;
        let mut sent = 0u64;
        let mut errors = 0u64;
        let packetized = packetizer.packetize(&unit, |datagram| {
            if first {
                // Marked before the syscall: the send itself belongs to the
                // send segment, not to packetisation.
                recorder.mark(unit.id, Stage::NetworkSendFirst);
                first = false;
            }
            match socket.send_to(datagram, config.target) {
                Ok(written) => sent += written as u64,
                Err(_) => errors += 1,
            }
        })?;
        recorder.mark(unit.id, Stage::NetworkSendLast);

        wire_bytes += sent;
        stats.access_units += 1;
        stats.packets += u64::from(packetized.packets);
        stats.bytes += packetized.bytes;
        stats.single_nal += u64::from(packetized.single_nal);
        stats.fu_a += u64::from(packetized.fu_a);
        stats.send_errors += errors;
    }

    Ok((stats, wire_bytes, payload_bytes))
}

pub struct ReceiverOutcome {
    pub rx: RxStats,
    pub jitter: Nanos,
    pub verified: u64,
    pub mismatched: u64,
    pub submitted: u64,
    pub max_backlog: usize,
    pub trailing_backlog: usize,
    pub backlog: Trend,
}

/// Reassembles access units and submits them to the decoder.
pub fn receive_loop(
    socket: UdpSocket,
    mut decoder: VideoToolboxDecoder,
    recorder: Recorder,
    ledger: Arc<VerifyLedger>,
    sample_interval: Duration,
    stop: Arc<AtomicBool>,
) -> Result<(ReceiverOutcome, VideoToolboxDecoder), Box<dyn Error + Send + Sync>> {
    socket.set_read_timeout(Some(RECV_TIMEOUT))?;
    let mut depacketizer = Depacketizer::new(DepacketizerConfig {
        payload_type: H264_PAYLOAD_TYPE,
        reorder_window: 64,
        max_access_unit_bytes: 4 * 1024 * 1024,
    });

    let mut datagram = [0u8; MAX_UDP_PAYLOAD];
    let mut in_flight: Option<FrameId> = None;
    let mut outcome = ReceiverOutcome {
        rx: RxStats::default(),
        jitter: Nanos::ZERO,
        verified: 0,
        mismatched: 0,
        submitted: 0,
        max_backlog: 0,
        trailing_backlog: 0,
        backlog: Trend::new(),
    };
    let mut next_sample = Timestamp::now();

    while !stop.load(Ordering::Acquire) {
        let received = match socket.recv(&mut datagram) {
            Ok(len) => len,
            // A timeout is the loop's chance to notice `stop`, not an error.
            Err(_) => continue,
        };
        let arrival = Timestamp::now();
        let bytes = &datagram[..received];

        // Parsed here as well as inside the depacketiser so the first and last
        // packet of a frame can be marked without the depacketiser knowing
        // anything about telemetry. Parsing is a few loads and no allocation.
        if let Ok(packet) = parse_packet(bytes)
            && let Some(frame) = packet.header.frame_id
        {
            if in_flight != Some(frame) {
                recorder.mark(frame, Stage::NetworkReceiveFirst);
                in_flight = Some(frame);
            }
            if packet.header.marker {
                recorder.mark(frame, Stage::NetworkReceiveLast);
            }
        }

        if let Some(unit) = depacketizer.push(bytes, arrival) {
            recorder.mark(unit.id, Stage::FrameReassembled);
            match ledger.check(unit.id, &unit.data) {
                Some(true) => outcome.verified += 1,
                Some(false) => outcome.mismatched += 1,
                None => {}
            }
            decoder.submit(&unit)?;
            outcome.submitted += 1;

            let backlog = decoder.in_flight();
            outcome.max_backlog = outcome.max_backlog.max(backlog);
            if arrival >= next_sample {
                outcome.backlog.record_at(arrival, backlog as f64);
                next_sample = arrival.add(Nanos(sample_interval.as_nanos() as u64));
            }
        }
    }

    decoder.flush()?;
    outcome.rx = *depacketizer.stats();
    outcome.jitter = depacketizer.jitter();
    outcome.trailing_backlog = decoder.in_flight();
    Ok((outcome, decoder))
}

/// Binds a sender and a receiver socket on the loopback interface.
///
/// The receive buffer is raised because a burst pacer hands a whole access
/// unit to the kernel at once: on a 50 Mbps 1080p120 stream that is up to 70
/// datagrams arriving with no gap between them, and the default buffer drops
/// some of them on the floor. That loss would be an artefact of the harness,
/// not of the transport.
pub fn loopback_sockets() -> std::io::Result<(UdpSocket, UdpSocket, SocketAddr)> {
    let receiver = UdpSocket::bind("127.0.0.1:0")?;
    let target = receiver.local_addr()?;
    let sender = UdpSocket::bind("127.0.0.1:0")?;
    Ok((sender, receiver, target))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ledger_matches_a_frame_and_forgets_the_ones_before_it() {
        let ledger = VerifyLedger::new(true);
        ledger.record(FrameId::new(1), b"one");
        ledger.record(FrameId::new(2), b"two");
        ledger.record(FrameId::new(3), b"three");

        // Frame 2 arrives; frame 1 is never coming and must not accumulate.
        assert_eq!(ledger.check(FrameId::new(2), b"two"), Some(true));
        assert_eq!(ledger.entries.lock().len(), 1);
        assert_eq!(ledger.check(FrameId::new(3), b"three"), Some(true));
        assert_eq!(ledger.check(FrameId::new(3), b"three"), None);
    }

    #[test]
    fn a_corrupted_access_unit_is_reported_as_a_mismatch() {
        let ledger = VerifyLedger::new(true);
        ledger.record(FrameId::new(7), b"original bytes");
        assert_eq!(
            ledger.check(FrameId::new(7), b"different bytes"),
            Some(false)
        );
    }

    #[test]
    fn the_ledger_is_bounded_however_much_is_pushed_through_it() {
        let ledger = VerifyLedger::new(true);
        for index in 1..=(VERIFY_WINDOW as u64 * 4) {
            ledger.record(FrameId::new(index), b"payload");
        }
        assert_eq!(ledger.entries.lock().len(), VERIFY_WINDOW);
    }

    #[test]
    fn verification_off_costs_nothing_and_claims_nothing() {
        let ledger = VerifyLedger::new(false);
        ledger.record(FrameId::new(1), b"one");
        assert!(ledger.entries.lock().is_empty());
        assert_eq!(ledger.check(FrameId::new(1), b"one"), None);
    }
}
