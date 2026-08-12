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

/// How many frames back the arrival marks remember having been emitted.
///
/// Sized against the depacketiser's reorder window: a packet older than the
/// window is discarded rather than reassembled, so a frame that has fallen
/// out of this ring can no longer produce a mark either.
const MARKED_FRAMES: usize = 64;

/// Which frames have already had their arrival marks recorded.
///
/// A reordered packet can arrive after the next frame has started, so "is
/// this frame new?" cannot be answered by remembering only the last id: on a
/// link that reorders, the older frame would be marked a second time and the
/// collector would count a duplicate. A fixed ring costs one linear scan of
/// 64 `u64`s per packet, allocates nothing, and never grows.
struct MarkedFrames {
    /// Ring of ids; `FrameId::NONE` is the empty slot.
    ids: [FrameId; MARKED_FRAMES],
    /// Set for a frame whose marker packet has been seen.
    ended: [bool; MARKED_FRAMES],
    next: usize,
}

impl MarkedFrames {
    fn new() -> Self {
        MarkedFrames {
            ids: [FrameId::NONE; MARKED_FRAMES],
            ended: [false; MARKED_FRAMES],
            next: 0,
        }
    }

    fn slot(&self, frame: FrameId) -> Option<usize> {
        self.ids.iter().position(|id| *id == frame)
    }

    /// Whether this packet is the first sighting of its frame.
    fn arrived(&mut self, frame: FrameId) -> bool {
        if self.slot(frame).is_some() {
            return false;
        }
        self.ids[self.next] = frame;
        self.ended[self.next] = false;
        self.next = (self.next + 1) % MARKED_FRAMES;
        true
    }

    /// Whether this is the first marker packet seen for its frame. A
    /// duplicated marker is the other way the same mark arrives twice.
    fn ended(&mut self, frame: FrameId) -> bool {
        let Some(slot) = self.slot(frame) else {
            // Evicted between its first packet and its marker: too old to
            // mark, and reassembly will have discarded it as well.
            return false;
        };
        if self.ended[slot] {
            return false;
        }
        self.ended[slot] = true;
        true
    }
}

pub struct TransportOutcome {
    /// Gap fill percentiles, kept beside `rx` because `RxStats` is `Copy`
    /// and a tail figure needs a histogram.
    pub reorder_wait: lanplay_transport::ReorderWait,
    pub tx: TxStats,
    pub rx: RxStats,
    pub jitter: Nanos,
    pub verified: u64,
    pub mismatched: u64,
    /// Bytes handed to the socket, against the bytes the access units held.
    pub wire_bytes: u64,
    pub payload_bytes: u64,
    /// The service class the datagrams carried when they got here.
    pub dscp: crate::dscp::Observed,
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
    /// Percentiles of the gap fill interval, which `RxStats` cannot hold:
    /// it is `Copy` and a tail needs a histogram.
    pub reorder_wait: lanplay_transport::ReorderWait,
    pub jitter: Nanos,
    pub verified: u64,
    pub mismatched: u64,
    pub submitted: u64,
    pub max_backlog: usize,
    pub trailing_backlog: usize,
    pub backlog: Trend,
    /// What service class the datagrams actually carried on arrival. The only
    /// evidence that a QoS marking survived the path.
    pub dscp: crate::dscp::Observed,
}

/// Reassembles access units and submits them to the decoder.
#[allow(clippy::too_many_arguments)]
pub fn receive_loop(
    socket: UdpSocket,
    mut decoder: VideoToolboxDecoder,
    recorder: Recorder,
    ledger: Arc<VerifyLedger>,
    sample_interval: Duration,
    // Bumped for every access unit handed to the decoder, so a watchdog can
    // tell a live stream from a finished one.
    progress: Arc<std::sync::atomic::AtomicU64>,
    // Delivery cadence, timestamped where delivery happens rather than
    // inferred from when a frame was eventually shown.
    delivery: Arc<lanplay_link_metrics::Delivery>,
    stop: Arc<AtomicBool>,
) -> Result<(ReceiverOutcome, VideoToolboxDecoder), Box<dyn Error + Send + Sync>> {
    socket.set_read_timeout(Some(RECV_TIMEOUT))?;
    let mut depacketizer = Depacketizer::new(DepacketizerConfig {
        payload_type: H264_PAYLOAD_TYPE,
        reorder_window: 64,
        max_access_unit_bytes: 4 * 1024 * 1024,
    });

    let mut datagram = [0u8; MAX_UDP_PAYLOAD];
    let mut marked = MarkedFrames::new();
    let mut outcome = ReceiverOutcome {
        rx: RxStats::default(),
        reorder_wait: lanplay_transport::ReorderWait::default(),
        jitter: Nanos::ZERO,
        verified: 0,
        mismatched: 0,
        submitted: 0,
        max_backlog: 0,
        trailing_backlog: 0,
        backlog: Trend::new(),
        dscp: crate::dscp::Observed::default(),
    };
    if !crate::dscp::request_tos(&socket) {
        eprintln!("transport: the kernel refused IP_RECVTOS; arriving DSCP is unobservable");
    }
    let mut next_sample = Timestamp::now();

    while !stop.load(Ordering::Acquire) {
        let (received, dscp) = match crate::dscp::recv_with_dscp(&socket, &mut datagram) {
            Ok(result) => result,
            // A timeout is the loop's chance to notice `stop`, not an error.
            Err(_) => continue,
        };
        outcome.dscp.record(dscp);
        let arrival = Timestamp::now();
        let bytes = &datagram[..received];

        // Parsed here as well as inside the depacketiser so the first and last
        // packet of a frame can be marked without the depacketiser knowing
        // anything about telemetry. Parsing is a few loads and no allocation.
        if let Ok(packet) = parse_packet(bytes)
            && let Some(frame) = packet.header.frame_id
        {
            if marked.arrived(frame) {
                recorder.mark(frame, Stage::NetworkReceiveFirst);
                // The other half of the delivery split: when the unit
                // started arriving, as against when it finished. A link that
                // starts every unit on time and finishes some of them late
                // has a different fault from one that starts them late.
                delivery.first_seen(arrival);
            }
            if packet.header.marker && marked.ended(frame) {
                recorder.mark(frame, Stage::NetworkReceiveLast);
            }
        }

        if let Some(unit) = depacketizer.push(bytes, arrival) {
            recorder.mark(unit.id, Stage::FrameReassembled);
            // Before the decoder, before the sink, before anything that
            // could block: this is the instant the network finished with the
            // frame, and nothing downstream may be allowed to move it.
            delivery.completed(arrival);
            match ledger.check(unit.id, &unit.data) {
                Some(true) => outcome.verified += 1,
                Some(false) => outcome.mismatched += 1,
                None => {}
            }
            decoder.submit(&unit)?;
            outcome.submitted += 1;
            progress.fetch_add(1, Ordering::Relaxed);

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
    outcome.reorder_wait = depacketizer.reorder_wait();
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

    /// The failure this exists to stop: on a link that reorders, a late
    /// packet of an earlier frame must not mark that frame's arrival twice.
    #[test]
    fn a_reordered_packet_does_not_mark_its_frame_a_second_time() {
        let mut marked = MarkedFrames::new();
        assert!(marked.arrived(FrameId::new(1)));
        assert!(marked.arrived(FrameId::new(2)));
        // A straggler from frame 1, arriving after frame 2 started.
        assert!(!marked.arrived(FrameId::new(1)));
        assert!(!marked.arrived(FrameId::new(2)));
    }

    #[test]
    fn only_the_first_marker_packet_ends_a_frame() {
        let mut marked = MarkedFrames::new();
        marked.arrived(FrameId::new(9));
        assert!(marked.ended(FrameId::new(9)));
        // A duplicated marker packet is the other way the mark doubles.
        assert!(!marked.ended(FrameId::new(9)));
        // Never seen: too old to be reassembled, so too old to be marked.
        assert!(!marked.ended(FrameId::new(10)));
    }

    #[test]
    fn the_ring_forgets_frames_older_than_the_reorder_window() {
        let mut marked = MarkedFrames::new();
        for index in 1..=(MARKED_FRAMES as u64 + 1) {
            assert!(marked.arrived(FrameId::new(index)));
        }
        // Frame 1 was evicted by frame 65, so it reads as new again. That is
        // the bound: memory is fixed, and a frame that old is unreassemblable.
        assert!(marked.arrived(FrameId::new(1)));
        assert!(!marked.arrived(FrameId::new(MARKED_FRAMES as u64 + 1)));
    }
}
