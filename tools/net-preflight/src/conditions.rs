//! What the radio was doing while the probe ran.
//!
//! Recorded because a run whose conditions were not recorded cannot be compared
//! with any other run, which is not a hypothetical: thirteen arms of the A8
//! sweep were measured before the radio was being written down beside them, and
//! what they cost was the ability to say whether the arms differed or the link
//! did.
//!
//! Two reads and no sampler. One association read costs 3.2 ms at p50 and 15.5
//! ms at worst, measured by `tools/radio-sample/examples/read-cost.rs`, which is
//! longer than a 120 Hz frame period - so a 1 Hz sampler on its own thread is
//! what a ten-minute session needs, and a five-second probe needs neither the
//! thread nor the four samples it would collect. Both reads here happen with the
//! receive loop stopped, before it starts and after it ends, so the cost lands
//! nowhere near a measurement.
//!
//! The pair is worth more than either read. A probe whose channel or width moved
//! between its two ends was taken under two conditions and belongs to neither,
//! and the reads are the only thing in the run that can say so.
//!
//! Nothing here decides anything. `NETWORK.md` bars the radio tier from
//! deciding, and the reason is measured: a link at -48 dBm negotiating 1200 Mbps
//! produced concealment ratios from 0.196 to 7.442 per cent across ten arms of
//! the A8 sweep. What these reads decide is whether the run is *comparable*,
//! which is a question about the record and not about the link.

use lanplay_capabilities::wifi::{self, Association};
use lanplay_network_health::RadioHint;

pub struct Conditions {
    /// Taken before the socket starts receiving.
    pub before: Option<Association>,
    /// Taken after the last datagram, so the pair brackets the measurement.
    pub after: Option<Association>,
}

impl Conditions {
    /// Reads that answered. The population under every zero this block states:
    /// a Mac whose driver said nothing has conditions that are absent rather
    /// than conditions that held still.
    pub fn reads(&self) -> u64 {
        u64::from(self.before.is_some()) + u64::from(self.after.is_some())
    }

    /// One when the channel or the width moved under the probe, zero when they
    /// held, and zero when there was nothing to compare - which is why this is
    /// only ever read beside [`Conditions::reads`].
    ///
    /// Signal is deliberately not part of it. Two reads a few seconds apart
    /// differ by a decibel or two on a link nobody touched, and a criterion
    /// that fires on that is a criterion that refuses every run. A channel
    /// change is categorical: the same probe measured two different radios.
    pub fn channel_moves(&self) -> u64 {
        match (&self.before, &self.after) {
            (Some(before), Some(after)) => {
                u64::from(before.channel != after.channel || before.width_mhz != after.width_mhz)
            }
            _ => 0,
        }
    }

    /// How far the signal moved across the probe, for the record rather than
    /// for a criterion.
    pub fn signal_drift_db(&self) -> Option<f64> {
        let before = self.before.as_ref()?;
        let after = self.after.as_ref()?;
        Some((after.rssi_dbm - before.rssi_dbm) as f64)
    }
}

/// A passive read of the current association. Never a scan: `system_profiler
/// SPAirPortDataType` takes the radio off its channel, and one reading of it
/// produced exactly the bunching an experiment had gone looking for.
pub fn read() -> Option<Association> {
    wifi::association()
}

/// The four quantities the classifier's vocabulary names, filled from the read.
///
/// Built through `lanplay_network_health::RadioHint` rather than written out
/// here so that this harness and the monitor cannot drift into two names for the
/// negotiated rate. It is a ceiling on throughput and not throughput, and it is
/// reported and never divided into anything.
pub fn hint(association: &Association) -> RadioHint {
    RadioHint {
        rssi_dbm: association.rssi_dbm,
        noise_dbm: association.noise_dbm,
        tx_rate_mbps: association.tx_rate_mbps,
        channel: association.channel,
        width_mhz: association.width_mhz,
    }
}
