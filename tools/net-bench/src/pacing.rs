//! When each datagram of an access unit is allowed to hit the wire.
//!
//! Every schedule is derived from an absolute deadline, never from sleeping a
//! relative amount in a loop: a relative sleep accumulates its own overshoot,
//! and over a sixty-second soak that drift is larger than everything this
//! harness is trying to measure.

use clap::ValueEnum;
use lanplay_telemetry::{Nanos, Timestamp};

/// IPv4 plus UDP headers. The rate pacer bills for these because a rate limit
/// applies to the wire, not to the part of the wire we happened to fill.
const IP_UDP_OVERHEAD: u64 = 28;

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum PacerKind {
    /// Hand the whole access unit to the socket as fast as it will take it.
    Burst,
    /// Spread the access unit's packets evenly across a fixed window.
    Micro,
    /// Pace strictly to a bitrate, packet by packet.
    Rate,
}

pub struct Pacer {
    kind: PacerKind,
    /// `micro`: how long one access unit is spread over.
    window: Nanos,
    /// `rate`: nanoseconds of wire time one byte costs.
    nanos_per_byte: f64,
    /// `rate`: when the link next becomes free. Never rewinds, so a burst that
    /// overruns its budget is paid for out of the following frames instead of
    /// being quietly forgiven.
    wire_clock: u64,
    release: Timestamp,
    packets: u32,
    index: u32,
}

impl Pacer {
    pub fn new(kind: PacerKind, window: Nanos, bitrate_mbps: f64) -> Self {
        Pacer {
            kind,
            window,
            nanos_per_byte: if bitrate_mbps > 0.0 {
                8_000.0 / bitrate_mbps
            } else {
                0.0
            },
            wire_clock: 0,
            release: Timestamp::from_nanos(0),
            packets: 1,
            index: 0,
        }
    }

    pub fn kind(&self) -> PacerKind {
        self.kind
    }

    /// When this access unit may begin transmitting.
    ///
    /// Separate from [`Pacer::start_access_unit`] because the answer is known
    /// before the packet count is: only the rate limiter defers an access
    /// unit, and it defers the whole unit, not individual datagrams. Waiting
    /// here rather than in front of the first datagram keeps the pacer's
    /// queueing delay out of the packetisation measurement.
    pub fn admit(&mut self, deadline: Timestamp) -> Timestamp {
        match self.kind {
            PacerKind::Burst | PacerKind::Micro => deadline,
            PacerKind::Rate => Timestamp::from_nanos(self.wire_clock.max(deadline.as_nanos())),
        }
    }

    /// Begins one access unit cleared to start at `release` and made of
    /// `packets` datagrams.
    pub fn start_access_unit(&mut self, release: Timestamp, packets: u32) {
        self.release = release;
        self.packets = packets.max(1);
        self.index = 0;
    }

    /// When the next datagram, `len` bytes of UDP payload, should be sent.
    pub fn packet_deadline(&mut self, len: usize) -> Timestamp {
        let at = match self.kind {
            PacerKind::Burst => self.release,
            PacerKind::Micro => {
                let offset = u64::from(self.index) * self.window.get() / u64::from(self.packets);
                self.release.add(Nanos(offset))
            }
            PacerKind::Rate => {
                // Never before the frame exists, so the link idles rather than
                // borrows from the future.
                let at = self.wire_clock.max(self.release.as_nanos());
                let bytes = len as u64 + IP_UDP_OVERHEAD;
                self.wire_clock = at + (bytes as f64 * self.nanos_per_byte) as u64;
                Timestamp::from_nanos(at)
            }
        };
        self.index += 1;
        at
    }

    /// How far the rate limiter has fallen behind the media clock, or zero for
    /// the pacers that impose no rate.
    pub fn backlog(&self, now: Timestamp) -> Nanos {
        match self.kind {
            PacerKind::Rate => Nanos(self.wire_clock.saturating_sub(now.as_nanos())),
            _ => Nanos::ZERO,
        }
    }
}
