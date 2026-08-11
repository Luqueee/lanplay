//! Turning a stream of datagrams back into access units, without ever waiting
//! and without ever growing.
//!
//! Three ideas carry the whole file.
//!
//! *Nothing waits indefinitely.* A packet that arrives early sits in a ring of
//! fixed-size buffers until the gap in front of it closes or the ring is
//! outrun; the moment a sequence number arrives more than a window ahead, the
//! missing ones are declared lost and the pipeline moves on. Holding a frame
//! for a straggler costs every frame behind it.
//!
//! *Memory is a constant.* The ring is allocated once, the assembly buffer is
//! reused, and every path that could grow one of them is a counter instead. A
//! sender that never sets a marker bit is a bug in the sender; it must not
//! become an allocation in the receiver.
//!
//! *A damaged access unit is dropped, never handed on.* A decoder fed a frame
//! with a hole in it produces artefacts that outlive the frame, and finding
//! them later is far more expensive than the frame was worth.

use core::fmt;
use std::collections::VecDeque;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{Nanos, Timestamp};
use lanplay_video_core::{EncodedAccessUnit, VideoTimestamp};

use crate::h264::NAL_LENGTH_SIZE;
use crate::rtp::{
    H264_CLOCK_RATE, H264_PAYLOAD_TYPE, MAX_UDP_PAYLOAD, RtpHeader, RtpTimestamp, SequenceNumber,
    Ssrc, parse_packet,
};
use crate::stats::RxStats;

const FU_A_TYPE: u8 = 28;
const FU_HEADER_LEN: usize = 2;
const FU_START: u8 = 0x80;
const FU_END: u8 = 0x40;
const NAL_TYPE_MASK: u8 = 0x1F;
const NAL_REF_MASK: u8 = 0xE0;
const IDR_SLICE_TYPE: u8 = 5;

/// Largest reorder window we will allocate: 1024 datagrams, about 1.2 MB.
///
/// A LAN that reorders by more than a thousand packets is not a LAN this
/// project can hide the latency of anyway.
pub const MAX_REORDER_WINDOW: usize = 1024;

const DEFAULT_REORDER_WINDOW: usize = 32;
const DEFAULT_MAX_ACCESS_UNIT_BYTES: usize = 4 << 20;

/// Completed access units held for the caller, who takes one per `push`.
///
/// One datagram can complete more than one access unit when it fills a hole
/// that several frames were queued behind. Small and fixed: the point is that
/// nothing completed is ever thrown away, not that the receiver becomes a
/// queue.
const READY_CAPACITY: usize = 8;

/// Initial guess at an access unit's size, refined from what actually arrives.
const INITIAL_CAPACITY_HINT: usize = 64 << 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DepacketizerConfig {
    pub payload_type: u8,
    /// Packets held while waiting for an earlier sequence number.
    ///
    /// Rounded **up** to a power of two, because the ring is indexed by
    /// sequence number: any other size would alias two live sequence numbers
    /// onto one slot as the 16-bit counter wraps. See
    /// [`Depacketizer::reorder_window`] for the value actually in use.
    pub reorder_window: usize,
    pub max_access_unit_bytes: usize,
}

impl Default for DepacketizerConfig {
    fn default() -> Self {
        DepacketizerConfig {
            payload_type: H264_PAYLOAD_TYPE,
            reorder_window: DEFAULT_REORDER_WINDOW,
            max_access_unit_bytes: DEFAULT_MAX_ACCESS_UNIT_BYTES,
        }
    }
}

pub struct Depacketizer {
    payload_type: u8,
    ssrc: Option<Ssrc>,
    /// Next sequence number to be processed in order.
    cursor: SequenceNumber,
    ring: Ring,
    assembler: Assembler,
    ready: VecDeque<EncodedAccessUnit>,
    stats: RxStats,
    jitter: Jitter,
    /// When the cursor's own packet was first noticed to be missing.
    ///
    /// Set when something ahead of the cursor arrives while the cursor slot
    /// is empty, cleared when the cursor moves. The interval between the two
    /// is what a NACK would have to beat to be worth sending.
    gap_since: Option<Timestamp>,
}

impl Depacketizer {
    pub fn new(config: DepacketizerConfig) -> Self {
        Depacketizer {
            payload_type: config.payload_type,
            ssrc: None,
            cursor: SequenceNumber(0),
            ring: Ring::new(config.reorder_window),
            assembler: Assembler::new(config.max_access_unit_bytes),
            ready: VecDeque::with_capacity(READY_CAPACITY),
            stats: RxStats::default(),
            jitter: Jitter::default(),
            gap_since: None,
        }
    }

    /// Feeds one datagram. Returns an access unit when this packet completed
    /// one.
    pub fn push(&mut self, datagram: &[u8], arrival: Timestamp) -> Option<EncodedAccessUnit> {
        self.accept(datagram, arrival);
        self.ready.pop_front()
    }

    pub fn stats(&self) -> &RxStats {
        &self.stats
    }

    /// RFC 3550 interarrival jitter estimate.
    pub fn jitter(&self) -> Nanos {
        self.jitter.nanos()
    }

    /// The window in use, after rounding the configured value up to a power of
    /// two and clamping it to [`MAX_REORDER_WINDOW`].
    pub fn reorder_window(&self) -> usize {
        self.ring.window
    }

    /// Datagrams currently held waiting for an earlier sequence number. Never
    /// exceeds [`Depacketizer::reorder_window`].
    pub fn buffered_packets(&self) -> usize {
        self.ring.count
    }

    /// Every byte this depacketiser is holding. The number a soak watches:
    /// it must be flat no matter what the sender does.
    pub fn memory_bytes(&self) -> usize {
        self.ring.storage.len()
            + self.ring.slots.len() * size_of::<Slot>()
            + self.assembler.buffer.capacity()
            + self
                .ready
                .iter()
                .map(|unit| unit.data.capacity())
                .sum::<usize>()
    }

    fn accept(&mut self, datagram: &[u8], arrival: Timestamp) {
        // Longer than anything we ever send, so it cannot be one of ours and
        // cannot be buffered without truncating it into a lie.
        if datagram.len() > MAX_UDP_PAYLOAD {
            self.stats.malformed += 1;
            return;
        }
        let Ok(packet) = parse_packet(datagram) else {
            self.stats.malformed += 1;
            return;
        };
        if packet.header.payload_type != self.payload_type {
            self.stats.unknown_payload_type += 1;
            return;
        }
        match self.ssrc {
            None => {
                self.ssrc = Some(packet.header.ssrc);
                self.cursor = packet.header.sequence;
            }
            Some(known) if known == packet.header.ssrc => {}
            Some(_) => {
                self.stats.unknown_ssrc += 1;
                return;
            }
        }

        self.stats.packets += 1;
        self.stats.bytes += datagram.len() as u64;
        // Jitter is a property of arrivals, so it is measured here, in arrival
        // order, before the reorder machinery puts things back in sequence.
        self.jitter.update(packet.header.timestamp, arrival);

        let delta = packet.header.sequence.distance_from(self.cursor);
        if delta == 0 {
            if let Some(since) = self.gap_since.take() {
                let waited = arrival.saturating_since(since).get();
                self.stats.reorder_waits += 1;
                self.stats.reorder_wait_sum_ns += waited;
                self.stats.reorder_wait_max_ns = self.stats.reorder_wait_max_ns.max(waited);
            }
            self.assembler.accept(
                &packet.header,
                packet.payload,
                &mut self.stats,
                &mut self.ready,
            );
            self.cursor = self.cursor.next();
            self.drain();
        } else if delta < 0 {
            // Behind the cursor: already delivered, or already written off.
            self.stats.duplicates += 1;
        } else if (delta as usize) <= self.ring.window {
            self.stats.max_reorder_depth = self.stats.max_reorder_depth.max(delta as u32);
            if !self.ring.store(packet.header.sequence, datagram) {
                self.stats.duplicates += 1;
            }
        } else {
            self.force_forward(packet.header.sequence);
            self.assembler.accept(
                &packet.header,
                packet.payload,
                &mut self.stats,
                &mut self.ready,
            );
            self.cursor = packet.header.sequence.next();
            self.drain();
        }
        // A hole exists for exactly as long as the ring holds anything: those
        // packets are all ahead of the cursor, so something before them is
        // missing. Arming here rather than only on the storing path keeps the
        // clock running across a cursor advance that left the hole in place.
        if self.ring.count > 0 {
            self.gap_since.get_or_insert(arrival);
        } else {
            self.gap_since = None;
        }
    }

    /// Hands over buffered packets that have become contiguous with the
    /// cursor.
    fn drain(&mut self) {
        let Depacketizer {
            cursor,
            ring,
            assembler,
            ready,
            stats,
            ..
        } = self;
        while ready.len() < READY_CAPACITY {
            let Some((offset, len)) = ring.remove(*cursor) else {
                break;
            };
            let datagram = &ring.storage[offset..offset + len];
            // It parsed once on the way in; the ring stores bytes verbatim.
            if let Ok(packet) = parse_packet(datagram) {
                stats.reordered += 1;
                assembler.accept(&packet.header, packet.payload, stats, ready);
            }
            *cursor = cursor.next();
        }
    }

    /// A packet arrived beyond the window, so the window moves to it.
    ///
    /// Everything still buffered lies in `cursor + 1 ..= cursor + window` and
    /// therefore in front of `target`: it goes out in sequence order, and every
    /// sequence number with nothing behind it is written off as lost. That is
    /// `window + 1` positions to sweep, and the sweep also guarantees the ring
    /// is left empty. Bounded by the window, never by the size of the jump.
    fn force_forward(&mut self, target: SequenceNumber) {
        let Depacketizer {
            cursor,
            ring,
            assembler,
            ready,
            stats,
            ..
        } = self;
        let mut gap = false;
        for _ in 0..=ring.window {
            if *cursor == target {
                break;
            }
            if let Some((offset, len)) = ring.remove(*cursor) {
                if gap {
                    assembler.mark_gap(stats);
                    gap = false;
                }
                let datagram = &ring.storage[offset..offset + len];
                if let Ok(packet) = parse_packet(datagram) {
                    stats.reordered += 1;
                    assembler.accept(&packet.header, packet.payload, stats, ready);
                }
            } else {
                stats.lost += 1;
                gap = true;
            }
            *cursor = cursor.next();
        }
        // Past the window nothing can have been buffered, so the rest of the
        // jump is loss by definition.
        let remaining = u64::from(target.0.wrapping_sub(cursor.0));
        stats.lost += remaining;
        if gap || remaining > 0 {
            assembler.mark_gap(stats);
        }
        *cursor = target;
    }
}

/// Fixed ring of datagram buffers, indexed by sequence number.
struct Ring {
    /// Always a power of two, or zero when reordering is switched off.
    window: usize,
    mask: usize,
    storage: Vec<u8>,
    slots: Vec<Slot>,
    count: usize,
}

#[derive(Clone, Copy)]
struct Slot {
    sequence: SequenceNumber,
    len: u32,
    filled: bool,
}

impl Ring {
    fn new(window: usize) -> Self {
        let window = if window == 0 {
            0
        } else {
            window.next_power_of_two().min(MAX_REORDER_WINDOW)
        };
        Ring {
            window,
            mask: window.saturating_sub(1),
            storage: vec![0; window * MAX_UDP_PAYLOAD],
            slots: vec![
                Slot {
                    sequence: SequenceNumber(0),
                    len: 0,
                    filled: false,
                };
                window
            ],
            count: 0,
        }
    }

    /// Returns false when the slot is already taken, which over a window this
    /// size can only mean the same sequence number arriving twice.
    fn store(&mut self, sequence: SequenceNumber, datagram: &[u8]) -> bool {
        if self.window == 0 {
            return false;
        }
        let index = usize::from(sequence.0) & self.mask;
        if self.slots[index].filled {
            return false;
        }
        let offset = index * MAX_UDP_PAYLOAD;
        self.storage[offset..offset + datagram.len()].copy_from_slice(datagram);
        self.slots[index] = Slot {
            sequence,
            len: datagram.len() as u32,
            filled: true,
        };
        self.count += 1;
        true
    }

    fn remove(&mut self, sequence: SequenceNumber) -> Option<(usize, usize)> {
        if self.count == 0 {
            return None;
        }
        let index = usize::from(sequence.0) & self.mask;
        let slot = self.slots[index];
        if !slot.filled || slot.sequence != sequence {
            return None;
        }
        self.slots[index].filled = false;
        self.count -= 1;
        Some((index * MAX_UDP_PAYLOAD, slot.len as usize))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AuState {
    /// Between access units.
    Idle,
    /// Collecting NAL units.
    Active,
    /// This access unit is already written off; skip to the next boundary
    /// without storing anything.
    Discarding,
}

/// Rebuilds AVCC access units from RTP payloads.
struct Assembler {
    max_bytes: usize,
    buffer: Vec<u8>,
    capacity_hint: usize,
    state: AuState,
    timestamp: RtpTimestamp,
    frame_id: FrameId,
    is_idr: bool,
    /// Something was lost or malformed inside this access unit.
    damaged: bool,
    /// A gap seen between access units belongs to the next one: the previous
    /// one had already been completed by its marker.
    pending_damage: bool,
    /// Offset of the in-progress NAL's length prefix, while an FU-A is open.
    nal_start: Option<usize>,
    /// Discard the rest of the current NAL's fragments, per RFC 6184.
    skipping_nal: bool,
    first_timestamp: Option<RtpTimestamp>,
    last_timestamp: RtpTimestamp,
    /// Ticks since the first access unit, accumulated so the 32-bit RTP
    /// timestamp wrapping does not throw the media timeline backwards.
    elapsed_ticks: i64,
}

impl Assembler {
    fn new(max_bytes: usize) -> Self {
        let capacity_hint = INITIAL_CAPACITY_HINT.min(max_bytes);
        Assembler {
            max_bytes,
            buffer: Vec::with_capacity(capacity_hint),
            capacity_hint,
            state: AuState::Idle,
            timestamp: RtpTimestamp(0),
            frame_id: FrameId::NONE,
            is_idr: false,
            damaged: false,
            pending_damage: false,
            nal_start: None,
            skipping_nal: false,
            first_timestamp: None,
            last_timestamp: RtpTimestamp(0),
            elapsed_ticks: 0,
        }
    }

    fn accept(
        &mut self,
        header: &RtpHeader,
        payload: &[u8],
        stats: &mut RxStats,
        ready: &mut VecDeque<EncodedAccessUnit>,
    ) {
        match self.state {
            AuState::Idle => self.start(header, stats),
            _ if header.timestamp != self.timestamp => {
                // A new timestamp without a marker on the old one means the
                // previous access unit lost its last packet.
                if self.state == AuState::Active {
                    stats.access_units_dropped += 1;
                }
                self.reset();
                self.start(header, stats);
            }
            _ => {}
        }

        if self.state == AuState::Discarding {
            if header.marker {
                self.reset();
            }
            return;
        }

        if payload.is_empty() {
            stats.malformed += 1;
            self.damaged = true;
        } else if payload[0] & NAL_TYPE_MASK == FU_A_TYPE {
            self.fragment(payload, stats);
        } else {
            self.whole_nal(payload, stats);
        }

        if header.marker && self.state != AuState::Discarding {
            self.complete(stats, ready);
        }
    }

    fn start(&mut self, header: &RtpHeader, stats: &mut RxStats) {
        stats.access_units_started += 1;
        self.state = AuState::Active;
        self.timestamp = header.timestamp;
        self.frame_id = header.frame_id.unwrap_or(FrameId::NONE);
        self.is_idr = false;
        self.damaged = core::mem::take(&mut self.pending_damage);
        self.nal_start = None;
        self.skipping_nal = false;
        self.buffer.clear();

        match self.first_timestamp {
            None => {
                self.first_timestamp = Some(header.timestamp);
                self.elapsed_ticks = 0;
            }
            Some(_) => {
                self.elapsed_ticks += header.timestamp.distance_from(self.last_timestamp);
            }
        }
        self.last_timestamp = header.timestamp;
    }

    fn whole_nal(&mut self, payload: &[u8], stats: &mut RxStats) {
        if self.nal_start.is_some() {
            // A new NAL began while a fragmented one was still open.
            self.abandon_nal(stats);
        }
        self.skipping_nal = false;
        if !self.reserve(usize::from(NAL_LENGTH_SIZE) + payload.len(), stats) {
            return;
        }
        self.buffer
            .extend_from_slice(&(payload.len() as u32).to_be_bytes());
        self.buffer.extend_from_slice(payload);
        self.is_idr |= payload[0] & NAL_TYPE_MASK == IDR_SLICE_TYPE;
    }

    fn fragment(&mut self, payload: &[u8], stats: &mut RxStats) {
        if payload.len() < FU_HEADER_LEN {
            stats.malformed += 1;
            self.damaged = true;
            return;
        }
        let fu_header = payload[1];
        let body = &payload[FU_HEADER_LEN..];
        let last = fu_header & FU_END != 0;

        if fu_header & FU_START != 0 {
            if self.nal_start.is_some() {
                self.abandon_nal(stats);
            }
            self.skipping_nal = false;
            // The original header byte was never sent: F and NRI rode on the
            // indicator, the type on the FU header.
            let reconstructed = (payload[0] & NAL_REF_MASK) | (fu_header & NAL_TYPE_MASK);
            if !self.reserve(usize::from(NAL_LENGTH_SIZE) + 1 + body.len(), stats) {
                return;
            }
            self.nal_start = Some(self.buffer.len());
            self.buffer
                .extend_from_slice(&[0; NAL_LENGTH_SIZE as usize]);
            self.buffer.push(reconstructed);
            self.buffer.extend_from_slice(body);
            self.is_idr |= fu_header & NAL_TYPE_MASK == IDR_SLICE_TYPE;
        } else if self.skipping_nal {
            // Already written this NAL off; its remaining fragments are noise.
            self.skipping_nal = !last;
        } else {
            let Some(start) = self.nal_start else {
                // A continuation with no start: the start packet was lost, or
                // this is the tail of a NAL we abandoned before it opened.
                stats.missing_fragments += 1;
                self.damaged = true;
                self.skipping_nal = !last;
                return;
            };
            if !self.reserve(body.len(), stats) {
                return;
            }
            self.buffer.extend_from_slice(body);
            if last {
                let length = (self.buffer.len() - start - usize::from(NAL_LENGTH_SIZE)) as u32;
                self.buffer[start..start + usize::from(NAL_LENGTH_SIZE)]
                    .copy_from_slice(&length.to_be_bytes());
                self.nal_start = None;
            }
        }
    }

    /// Throws away the partially reassembled NAL and poisons the access unit.
    fn abandon_nal(&mut self, stats: &mut RxStats) {
        if let Some(start) = self.nal_start.take() {
            self.buffer.truncate(start);
        }
        stats.missing_fragments += 1;
        self.damaged = true;
        self.skipping_nal = true;
    }

    /// True when `extra` bytes may be appended. A false is the size ceiling,
    /// and it takes the access unit with it.
    fn reserve(&mut self, extra: usize, stats: &mut RxStats) -> bool {
        if self.buffer.len() + extra <= self.max_bytes {
            return true;
        }
        stats.oversized_access_units += 1;
        stats.access_units_dropped += 1;
        // Hand the oversized allocation back rather than keeping it as
        // headroom for the next misbehaving sender.
        self.buffer = Vec::with_capacity(self.capacity_hint);
        self.nal_start = None;
        self.skipping_nal = false;
        self.state = AuState::Discarding;
        false
    }

    fn complete(&mut self, stats: &mut RxStats, ready: &mut VecDeque<EncodedAccessUnit>) {
        if self.nal_start.is_some() {
            // The marker arrived while a fragmented NAL was still open.
            self.abandon_nal(stats);
        }
        if self.damaged || self.buffer.is_empty() || ready.len() == READY_CAPACITY {
            stats.access_units_dropped += 1;
            self.reset();
            return;
        }

        // Track the recent high-water mark so each access unit is one exact
        // allocation instead of a run of doubling reallocations, and let it
        // decay so one huge IDR does not size every frame after it.
        self.capacity_hint = self
            .buffer
            .len()
            .max(self.capacity_hint - self.capacity_hint / 8)
            .min(self.max_bytes);
        let data = core::mem::replace(&mut self.buffer, Vec::with_capacity(self.capacity_hint));

        ready.push_back(EncodedAccessUnit {
            id: self.frame_id,
            pts: VideoTimestamp::new(self.elapsed_ticks, H264_CLOCK_RATE),
            is_idr: self.is_idr,
            data,
        });
        stats.access_units_completed += 1;
        self.reset();
    }

    /// A sequence number was written off. Whichever access unit is open owns
    /// the damage; between access units, the next one does.
    fn mark_gap(&mut self, stats: &mut RxStats) {
        match self.state {
            AuState::Active => {
                self.damaged = true;
                if self.nal_start.is_some() {
                    self.abandon_nal(stats);
                }
            }
            AuState::Idle => self.pending_damage = true,
            AuState::Discarding => {}
        }
    }

    fn reset(&mut self) {
        self.state = AuState::Idle;
        self.buffer.clear();
        self.nal_start = None;
        self.skipping_nal = false;
        self.damaged = false;
    }
}

/// RFC 3550 interarrival jitter: `J += (|D(i-1,i)| - J) / 16`.
#[derive(Default)]
struct Jitter {
    /// In RTP ticks. Fractional by construction, and the RFC's own reference
    /// implementation keeps it that way.
    estimate: f64,
    previous: Option<(RtpTimestamp, u64)>,
}

impl Jitter {
    fn update(&mut self, timestamp: RtpTimestamp, arrival: Timestamp) {
        let ticks =
            (u128::from(arrival.as_nanos()) * u128::from(H264_CLOCK_RATE) / 1_000_000_000) as u64;
        if let Some((previous_timestamp, previous_ticks)) = self.previous {
            let transit = (ticks as i64).wrapping_sub(previous_ticks as i64)
                - timestamp.distance_from(previous_timestamp);
            self.estimate += (transit.unsigned_abs() as f64 - self.estimate) / 16.0;
        }
        self.previous = Some((timestamp, ticks));
    }

    fn nanos(&self) -> Nanos {
        Nanos((self.estimate * 1_000_000_000.0 / f64::from(H264_CLOCK_RATE)) as u64)
    }
}

impl fmt::Debug for Depacketizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Depacketizer")
            .field("ssrc", &self.ssrc)
            .field("cursor", &self.cursor)
            .field("buffered", &self.ring.count)
            .field("memory_bytes", &self.memory_bytes())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use lanplay_video_core::to_avcc;

    use super::*;
    use crate::h264::Packetizer;
    use crate::rtp::{RtpClock, random_u32};

    fn round_trip(nals: &[Vec<u8>], window: usize) -> (Option<EncodedAccessUnit>, RxStats) {
        let unit = EncodedAccessUnit {
            id: FrameId::new(42),
            pts: VideoTimestamp::new(1, 120),
            is_idr: true,
            data: to_avcc(nals.iter().map(|n| n.as_slice()), NAL_LENGTH_SIZE),
        };
        let mut packetizer = Packetizer::with_sequence(
            Ssrc(random_u32()),
            RtpClock::new(H264_CLOCK_RATE, 0),
            H264_PAYLOAD_TYPE,
            MAX_UDP_PAYLOAD,
            SequenceNumber(0),
        );
        let mut datagrams = Vec::new();
        packetizer
            .packetize(&unit, |d| datagrams.push(d.to_vec()))
            .expect("packetises");

        let mut depacketizer = Depacketizer::new(DepacketizerConfig {
            reorder_window: window,
            ..DepacketizerConfig::default()
        });
        let mut out = None;
        for datagram in &datagrams {
            if let Some(unit) = depacketizer.push(datagram, Timestamp::now()) {
                out = Some(unit);
            }
        }
        (out, *depacketizer.stats())
    }

    #[test]
    fn a_mixed_access_unit_returns_exactly_its_input_bytes() {
        let nals = vec![vec![0x65; 40], vec![0x41; 30_000], vec![0x06; 12]];
        let expected = to_avcc(nals.iter().map(|n| n.as_slice()), NAL_LENGTH_SIZE);
        let (unit, stats) = round_trip(&nals, 32);

        let unit = unit.expect("access unit completes");
        assert_eq!(unit.data, expected);
        assert_eq!(unit.id, FrameId::new(42));
        assert!(unit.is_idr);
        assert_eq!(stats.access_units_completed, 1);
        assert_eq!(stats.access_units_dropped, 0);
        assert_eq!(stats.lost, 0);
    }

    /// The measurement a NACK delay has to be built from: how far ahead a
    /// packet arrived, and how long the gap it left took to fill.
    #[test]
    fn a_reordered_packet_reports_its_depth_and_how_long_the_gap_stood_open() {
        let nals = [vec![0x65; 40], vec![0x41; 30_000]];
        let unit = EncodedAccessUnit {
            id: FrameId::new(7),
            pts: VideoTimestamp::new(1, 120),
            is_idr: true,
            data: to_avcc(nals.iter().map(|n| n.as_slice()), NAL_LENGTH_SIZE),
        };
        let mut packetizer = Packetizer::with_sequence(
            Ssrc(random_u32()),
            RtpClock::new(H264_CLOCK_RATE, 0),
            H264_PAYLOAD_TYPE,
            MAX_UDP_PAYLOAD,
            SequenceNumber(0),
        );
        let mut datagrams = Vec::new();
        packetizer
            .packetize(&unit, |d| datagrams.push(d.to_vec()))
            .expect("packetises");
        assert!(datagrams.len() > 4, "need enough packets to reorder");

        let mut depacketizer = Depacketizer::new(DepacketizerConfig {
            reorder_window: 32,
            ..DepacketizerConfig::default()
        });
        // The first packet a depacketiser ever sees defines the cursor: it
        // cannot know what came before it, so the stream has to be running
        // before anything counts as out of order.
        let opened = Timestamp::now();
        depacketizer.push(&datagrams[0], opened);

        // Packet 1 is held back a millisecond while three later ones arrive:
        // depth three, and a gap that stood open for that millisecond.
        for (offset, datagram) in datagrams.iter().enumerate().skip(2).take(3) {
            depacketizer.push(datagram, opened.add(Nanos(offset as u64)));
        }
        assert_eq!(depacketizer.stats().max_reorder_depth, 3);
        assert_eq!(depacketizer.stats().reorder_waits, 0, "still missing");

        depacketizer.push(&datagrams[1], opened.add(Nanos(1_000_000)));
        for datagram in datagrams.iter().skip(5) {
            depacketizer.push(datagram, Timestamp::now());
        }
        let stats = *depacketizer.stats();
        assert_eq!(stats.reorder_waits, 1);
        assert!(
            (900_000..1_100_000).contains(&stats.reorder_wait_max_ns),
            "gap stood open for {} ns",
            stats.reorder_wait_max_ns
        );
        assert_eq!(stats.lost, 0);
    }

    #[test]
    fn packets_in_order_never_open_a_gap() {
        let nals = vec![vec![0x65; 40], vec![0x41; 30_000]];
        let (_, stats) = round_trip(&nals, 32);
        assert_eq!(stats.max_reorder_depth, 0);
        assert_eq!(stats.reorder_waits, 0);
        assert_eq!(stats.reorder_wait_max_ns, 0);
    }

    #[test]
    fn reorder_window_is_rounded_to_a_power_of_two() {
        for (configured, effective) in
            [(0, 0), (1, 1), (3, 4), (32, 32), (100, 128), (99_999, 1024)]
        {
            let depacketizer = Depacketizer::new(DepacketizerConfig {
                reorder_window: configured,
                ..DepacketizerConfig::default()
            });
            assert_eq!(depacketizer.reorder_window(), effective, "{configured}");
        }
    }

    #[test]
    fn a_zero_window_still_delivers_an_in_order_stream() {
        let nals = vec![vec![0x65; 5000]];
        let (unit, stats) = round_trip(&nals, 0);
        assert!(unit.is_some());
        assert_eq!(stats.access_units_completed, 1);
        assert_eq!(stats.lost, 0);
    }
}
