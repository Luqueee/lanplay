//! Send timestamps published from the sender thread to the receiver thread.
//!
//! The telemetry `network` segment is defined as
//! `NetworkSendLast -> NetworkReceiveFirst`, which assumes an access unit is
//! fully transmitted before any of it arrives. That holds on a store-and-
//! forward hop and is false on loopback, where the first datagram is delivered
//! while the sender is still issuing syscalls for the rest — the interval goes
//! negative and the segment records nothing at all.
//!
//! So the one number this whole harness exists to produce needs its own
//! channel: a fixed ring of slots, one atomic store per access unit, no
//! allocation and no lock. Loopback only. Two machines have two clocks and
//! subtracting across them is phase 8's problem, not this ring's.

use std::sync::atomic::{AtomicU64, Ordering};

use lanplay_protocol::FrameId;
use lanplay_telemetry::Timestamp;

/// At 120 fps this is 34 seconds of history, and an access unit is read back
/// microseconds after it is written.
pub const SLOTS: usize = 4096;

struct Slot {
    frame: AtomicU64,
    first: AtomicU64,
    last: AtomicU64,
}

pub struct WireTimes {
    slots: Box<[Slot]>,
}

impl WireTimes {
    pub fn new() -> WireTimes {
        WireTimes {
            slots: (0..SLOTS)
                .map(|_| Slot {
                    frame: AtomicU64::new(FrameId::NONE.get()),
                    first: AtomicU64::new(0),
                    last: AtomicU64::new(0),
                })
                .collect(),
        }
    }

    fn slot(&self, frame: FrameId) -> &Slot {
        &self.slots[(frame.get() % SLOTS as u64) as usize]
    }

    /// Publishes the moment this access unit's first datagram left `send_to`.
    pub fn begin(&self, frame: FrameId, at: Timestamp) {
        let slot = self.slot(frame);
        // Invalidate first: a reader must never pair a new frame id with the
        // previous occupant's timestamps.
        slot.frame.store(FrameId::NONE.get(), Ordering::Relaxed);
        slot.first.store(at.as_nanos(), Ordering::Relaxed);
        slot.last.store(at.as_nanos(), Ordering::Relaxed);
        slot.frame.store(frame.get(), Ordering::Release);
    }

    /// Advances the last-datagram timestamp.
    pub fn extend(&self, frame: FrameId, at: Timestamp) {
        self.slot(frame)
            .last
            .store(at.as_nanos(), Ordering::Release);
    }

    /// The first and last send timestamps for `frame`, if the slot still holds
    /// it.
    pub fn get(&self, frame: FrameId) -> Option<(Timestamp, Timestamp)> {
        let slot = self.slot(frame);
        if slot.frame.load(Ordering::Acquire) != frame.get() {
            return None;
        }
        let first = slot.first.load(Ordering::Acquire);
        let last = slot.last.load(Ordering::Acquire);
        if slot.frame.load(Ordering::Acquire) != frame.get() {
            return None;
        }
        Some((Timestamp::from_nanos(first), Timestamp::from_nanos(last)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_frame_reads_back_and_a_recycled_slot_does_not() {
        let wire = WireTimes::new();
        let frame = FrameId::new(7);
        wire.begin(frame, Timestamp::from_nanos(1_000));
        wire.extend(frame, Timestamp::from_nanos(1_500));
        assert_eq!(
            wire.get(frame),
            Some((Timestamp::from_nanos(1_000), Timestamp::from_nanos(1_500)))
        );

        let recycled = FrameId::new(7 + SLOTS as u64);
        wire.begin(recycled, Timestamp::from_nanos(9_000));
        assert_eq!(wire.get(frame), None, "the old frame must not read stale");
        assert_eq!(
            wire.get(recycled),
            Some((Timestamp::from_nanos(9_000), Timestamp::from_nanos(9_000)))
        );
    }

    #[test]
    fn an_unpublished_frame_reads_as_absent() {
        let wire = WireTimes::new();
        assert_eq!(wire.get(FrameId::new(3)), None);
    }
}
