//! Counters for one direction of one session.
//!
//! Plain `Copy` structs rather than atomics: each is owned by the single
//! thread that drives its side of the transport, and a snapshot is taken by
//! copying the whole struct. Nothing here is on a shared cache line, so
//! counting costs an increment that the compiler keeps in a register.
//!
//! Every counter answers a question that comes up when a stream misbehaves.
//! `lost` and `duplicates` separate a lossy link from a duplicating one,
//! `reordered` says whether the reorder window is earning its memory, and
//! `access_units_dropped` next to `access_units_completed` is the only honest
//! measure of what the decoder actually got.

use core::fmt;

use crate::h264::PacketizedAu;

#[derive(Default, Clone, Copy, Debug)]
pub struct TxStats {
    pub access_units: u64,
    pub packets: u64,
    pub bytes: u64,
    pub single_nal: u64,
    pub fu_a: u64,
    /// Datagrams the socket refused. The packetiser never produces these; the
    /// sender that owns the socket does.
    pub send_errors: u64,
}

impl TxStats {
    /// Folds one access unit's packetisation report into the running totals.
    pub fn record(&mut self, packetized: &PacketizedAu) {
        self.access_units += 1;
        self.packets += u64::from(packetized.packets);
        self.bytes += packetized.bytes;
        self.single_nal += u64::from(packetized.single_nal);
        self.fu_a += u64::from(packetized.fu_a);
    }

    /// Mean packets per access unit, or zero before the first one.
    pub fn packets_per_access_unit(&self) -> f64 {
        if self.access_units == 0 {
            return 0.0;
        }
        self.packets as f64 / self.access_units as f64
    }
}

impl fmt::Display for TxStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "tx  {} au, {} pkt ({:.1}/au), {:.1} MB",
            self.access_units,
            self.packets,
            self.packets_per_access_unit(),
            self.bytes as f64 / 1e6,
        )?;
        write!(
            f,
            "    single-nal {}, fu-a {}, send errors {}",
            self.single_nal, self.fu_a, self.send_errors,
        )
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct RxStats {
    /// Datagrams accepted into the sequence machine.
    pub packets: u64,
    pub bytes: u64,
    /// Datagrams that are not parseable RTP, or larger than we ever send.
    pub malformed: u64,
    pub unknown_ssrc: u64,
    pub unknown_payload_type: u64,
    /// Sequence numbers at or behind the in-order cursor: a retransmission,
    /// a duplicated datagram, or an arrival too late to use.
    pub duplicates: u64,
    /// Packets that arrived early, waited in the reorder window and were
    /// recovered in sequence order.
    pub reordered: u64,
    /// Furthest ahead of the cursor a packet has ever arrived, in sequence
    /// numbers. What the reorder window has to be big enough for.
    pub max_reorder_depth: u32,
    /// How long the missing packet took to arrive once a gap became visible,
    /// summarised rather than kept: this struct is `Copy` and lives on the
    /// receive thread's stack.
    ///
    /// This is the number a NACK delay has to be built from. Sending a NACK
    /// sooner than legitimate reordering resolves itself asks the sender to
    /// retransmit a packet already in flight; sending it later wastes the
    /// deadline the retransmission has to meet. Neither can be guessed.
    pub reorder_wait_max_ns: u64,
    pub reorder_wait_sum_ns: u64,
    pub reorder_waits: u64,
    pub lost: u64,
    pub access_units_started: u64,
    pub access_units_completed: u64,
    pub access_units_dropped: u64,
    /// NAL units abandoned because a fragment went missing.
    pub missing_fragments: u64,
    /// Access units that hit the size ceiling, almost always a sender that
    /// never set a marker bit.
    pub oversized_access_units: u64,
}

impl RxStats {
    /// Fraction of started access units the decoder never saw.
    pub fn drop_ratio(&self) -> f64 {
        if self.access_units_started == 0 {
            return 0.0;
        }
        self.access_units_dropped as f64 / self.access_units_started as f64
    }

    /// Mean time a visible gap took to fill itself, or zero if none did.
    pub fn mean_reorder_wait_ns(&self) -> u64 {
        if self.reorder_waits == 0 {
            return 0;
        }
        self.reorder_wait_sum_ns / self.reorder_waits
    }
}

impl fmt::Display for RxStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "rx  {} pkt, {:.1} MB, {} au started ({} completed, {} dropped, {:.3}% loss)",
            self.packets,
            self.bytes as f64 / 1e6,
            self.access_units_started,
            self.access_units_completed,
            self.access_units_dropped,
            self.drop_ratio() * 100.0,
        )?;
        writeln!(
            f,
            "    lost {}, duplicate {}, reordered {}, missing fragments {}",
            self.lost, self.duplicates, self.reordered, self.missing_fragments,
        )?;
        writeln!(
            f,
            "    reorder depth max {}, gap filled in {:.3} ms mean / {:.3} ms max over {} gaps",
            self.max_reorder_depth,
            self.mean_reorder_wait_ns() as f64 / 1e6,
            self.reorder_wait_max_ns as f64 / 1e6,
            self.reorder_waits,
        )?;
        write!(
            f,
            "    malformed {}, wrong ssrc {}, wrong payload type {}, oversized {}",
            self.malformed,
            self.unknown_ssrc,
            self.unknown_payload_type,
            self.oversized_access_units,
        )
    }
}
