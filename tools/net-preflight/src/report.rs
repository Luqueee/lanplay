//! The document N2 leaves behind.
//!
//! # It classifies nothing
//!
//! `NetworkPreflightReport` records what was measured and the conditions it was
//! measured under, and it states no condition, no grade and no verdict about the
//! link. That is not modesty about the arithmetic, it is what a probe of this
//! length is worth. `results/audio/e2e-clean/radio-trace-full.csv` holds 1117
//! association reads at 1 Hz across one 1116 s session: its first thirty seconds
//! spread 4 dB, -62 to -58 dBm, with the negotiated rate between 576 and 648
//! Mbps - as steady as any window in this repository. Over the whole session the
//! same association spread 11 dB, -67 to -56, and its negotiated rate ran 103 to
//! 816 Mbps, a factor of eight; one six-minute lag inside it moved 8 dB on its
//! own, -67 dBm at t=93 s against -59 dBm at t=454 s.
//!
//! So this report selects a starting point and expires. Anything that reads it
//! as a statement about the next ten minutes is reading a thirty-second window
//! of that trace and calling it the session. The monitor is what watches; this
//! is what the monitor starts from, and the two are not interchangeable in
//! either direction.
//!
//! # The sections say what the client's sections say
//!
//! `delivery` and `stream` carry the field names `macos/client/src/report.rs`
//! already uses, value for value, so a preflight figure and a mid-session figure
//! are the same quantity rather than two dialects. What is deliberately *not*
//! here is the rest of a session envelope: no `display`, no `decode` and no
//! `environment`, because nothing was presented to anybody and there is no
//! experience tier to fill. That absence is also what keeps
//! `lanplay_network_health::corpus` reading this as not-a-session rather than as
//! a session whose diagnosis nobody wrote down, and `kind` is stated so a reader
//! can refuse it by name instead of by the shape of what it lacks.
//!
//! # A refusal leaves no numbers behind
//!
//! When there was nothing to measure the measured sections are absent, not
//! zeroed, and the probe decides that before this file is reached. Zero
//! datagrams lost out of zero sent is the most common way an instrument in this
//! project has lied, and a number that is never written down cannot be quoted
//! back.

use std::time::{SystemTime, UNIX_EPOCH};

use lanplay_capabilities::wifi::Association;
use lanplay_network_health::{Fraction, Incidence, StreamBehaviour};
use serde::Serialize;

use crate::conditions::{self, Conditions};
use crate::probe::{Measurement, Outcome, ProbeConfig};

#[derive(Serialize)]
pub struct NetworkPreflightReport {
    /// Named rather than inferred, so a reader that must not treat this as a
    /// session can say so about a field instead of about an absence.
    pub kind: &'static str,
    pub run: Run,
    pub conditions: ConditionsSection,
    /// The three measured sections stand or fall together: they come from one
    /// outcome, and a run that produced none of them is a refusal with a reason
    /// rather than three empty tables.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<Stream>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<Refusal>,
}

#[derive(Serialize)]
pub struct Run {
    pub started_unix_ms: u64,
    pub arm: String,
    /// What the probe was asked for, beside what it observed. A sender on
    /// another machine starts when it starts, and the difference between the two
    /// is the only thing that says whether the probe saw the traffic it asked
    /// for.
    pub seconds_requested: f64,
    pub span_s: f64,
    /// The rate the sender was told to pace at. Every threshold in
    /// `crates/link-metrics` is a multiple of its reciprocal and nothing in a
    /// received stream states it, so it is recorded as the argument it is.
    pub fps: f64,
    pub mtu: usize,
    /// The sender's pacer, as declared by whoever started it. `burst` is the
    /// product's shape: a whole access unit handed to the kernel at once.
    pub pacer: String,
    /// The fault relay's settings when the traffic came through one, exactly as
    /// it was told to be unreliable. An arm with injected faults and no seed on
    /// record is an arm nobody can re-run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faults: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

/// The radio either side of the probe.
///
/// Two reads and a statement about whether they agree, rather than one read
/// presented as "the conditions". A probe whose channel moved between its ends
/// measured two links.
#[derive(Serialize)]
pub struct ConditionsSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<Radio>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<Radio>,
    /// How many of the two reads answered. The population under `channel_moved`:
    /// a Mac that reported no association has conditions that are absent, and an
    /// absence must not read as a channel that held still.
    pub reads: u64,
    pub channel_moved: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal_drift_db: Option<f64>,
}

/// One association read, in the column names `results/*/*.wifi.csv` already
/// uses.
#[derive(Serialize)]
pub struct Radio {
    pub rssi_dbm: i64,
    pub noise_dbm: i64,
    /// The negotiated rate: a ceiling on throughput, and not throughput. It is
    /// reported and nothing is divided by it.
    pub tx_rate_mbps: f64,
    pub channel: i64,
    pub width_mhz: u32,
    /// Whether the occupied span falls where EN 301 893 requires radar
    /// detection. Moving this access point from channel 116 to channel 36 took
    /// access units arriving more than two periods late from 69 a minute to 5.5,
    /// so this is the first thing the reader of a bad preflight should see.
    pub radar_band: bool,
    pub country: String,
}

/// Whether the traffic that was measured looked like the product's traffic.
#[derive(Serialize)]
pub struct Shape {
    pub datagrams: u64,
    pub datagram_bytes: u64,
    /// Measured off the wire, so it carries whatever the link did to the
    /// sender's intent. Not compared against a target: the fixture's content
    /// varies frame to frame and an arm relayed through a fault injector is
    /// short by design, so a tolerance wide enough for both could only fail on
    /// an arm that received nothing - which is already refused for that reason.
    pub mbps: f64,
    pub mean_datagram_bytes: f64,
    pub datagrams_per_access_unit: f64,
    /// Access units that would have fitted in one datagram. The shape criterion
    /// is that this is zero over a non-zero population of access units: this
    /// product's video arrives as a burst of some tens of datagrams, and a
    /// stream whose units arrive whole is a stream this link was never asked to
    /// carry.
    pub under_one_datagram: u64,
}

/// The transport counters, in the client's names.
#[derive(Serialize)]
pub struct Stream {
    /// Access units the sender produced while the probe was listening, from the
    /// first and last frame id it stated. Absent from a sender that emits no
    /// frame id extension, because `fps` multiplied by the span is a rounding
    /// error that reports one unit lost on a run that lost nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<u64>,
    pub reconstructed: u64,
    /// Datagrams the sequence machine never saw. Datagrams, while `expected`
    /// counts access units - held as two named quantities because
    /// `macos/client/src/report.rs` holds the same pair and the ratio of the two
    /// is not a loss ratio. `datagrams_accounted` is the population that is.
    pub packet_loss: u64,
    pub datagrams_accounted: u64,
    pub loss_ratio: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub au_loss: Option<u64>,
    pub reordered: u64,
    pub reorder_ratio: f64,
    pub duplicates: u64,
    pub rtp_jitter_us: f64,
}

/// `crates/link-metrics`' own window, written out field for field.
///
/// The names are the client's so the two are one dialect. Nothing is recomputed
/// on the way out: the rates come from `Tail::per_minute` and
/// `Tail::clusters_per_minute`, which is the only division of these counts in
/// the project.
#[derive(Serialize)]
pub struct Delivery {
    pub delivered: u64,
    pub au_interval_p50_ms: f64,
    pub au_interval_p95_ms: f64,
    pub au_interval_p99_ms: f64,
    pub au_interval_max_ms: f64,
    pub first_interval_p50_ms: f64,
    pub first_interval_p95_ms: f64,
    pub first_interval_p99_ms: f64,
    pub first_interval_max_ms: f64,
    pub span_s: f64,
    pub over_1_25t_per_min: f64,
    pub over_1_5t_per_min: f64,
    pub over_2t_per_min: f64,
    pub over_3t_per_min: f64,
    pub over_4t_per_min: f64,
    pub over_6t_per_min: f64,
    pub stall_clusters_per_min: f64,
    /// Counted as well as rated, which the client's section does not do. Over
    /// five seconds a rate per minute is a count multiplied by twelve, and the
    /// committed clean arms on this channel sit at 2.0 to 18.5 clusters a
    /// minute, which is 0.17 to 1.5 clusters inside a five-second window. A
    /// reader handed only the rate would be comparing twelve times a coin flip
    /// against a hundred and twenty seconds of evidence.
    pub stall_clusters: u64,
    pub over_2t: u64,
    pub mean_catch_up_units: f64,
    pub max_catch_up_units: u64,
    pub stall_gap_p50_ms: f64,
    pub stall_gap_p95_ms: f64,
}

#[derive(Serialize)]
pub struct Refusal {
    pub why: String,
    /// Kept because "nothing arrived" and "something arrived and none of it
    /// reassembled" are different faults, on different machines, and a refusal
    /// that does not separate them sends its reader to the wrong one.
    pub datagrams: u64,
}

/// Everything about the run that the probe cannot observe for itself.
#[derive(Clone)]
pub struct Provenance {
    pub arm: String,
    pub pacer: String,
    pub faults: Option<String>,
    pub commit: Option<String>,
}

pub fn build(
    outcome: &Outcome,
    config: &ProbeConfig,
    conditions: &Conditions,
    provenance: &Provenance,
) -> NetworkPreflightReport {
    let measured = match outcome {
        Outcome::Measured(measurement) => Some((measurement, behaviour(measurement))),
        Outcome::Nothing { .. } => None,
    };

    NetworkPreflightReport {
        kind: "network-preflight",
        run: Run {
            started_unix_ms: unix_ms(),
            arm: provenance.arm.clone(),
            seconds_requested: config.seconds,
            span_s: measured
                .as_ref()
                .map(|(measurement, _)| measurement.elapsed.as_secs_f64())
                .unwrap_or_default(),
            fps: config.fps,
            mtu: config.mtu,
            pacer: provenance.pacer.clone(),
            faults: provenance.faults.clone(),
            commit: provenance.commit.clone(),
        },
        conditions: ConditionsSection {
            before: conditions.before.as_ref().map(radio),
            after: conditions.after.as_ref().map(radio),
            reads: conditions.reads(),
            channel_moved: conditions.channel_moves() > 0,
            signal_drift_db: conditions.signal_drift_db(),
        },
        shape: measured.as_ref().map(|(measurement, _)| shape(measurement)),
        stream: measured
            .as_ref()
            .map(|(measurement, behaviour)| stream(measurement, behaviour)),
        delivery: measured
            .as_ref()
            .map(|(_, behaviour)| delivery(behaviour.delivery)),
        refusal: match outcome {
            Outcome::Nothing { why, datagrams } => Some(Refusal {
                why: why.clone(),
                datagrams: *datagrams,
            }),
            Outcome::Measured(_) => None,
        },
    }
}

/// The middle tier, built through the vocabulary the classifier owns.
///
/// Routed through `StreamBehaviour` rather than serialised straight out of the
/// probe so that this harness and the monitor cannot drift into two names for
/// the same quantity. The loss and reorder counts arrive as `Incidence::Of`
/// rather than `Incidence::Bare` because this probe does write its population
/// down: `Bare` is what the committed video envelopes are stuck with, and a
/// bare count can never be ranked on a level.
pub fn behaviour(measurement: &Measurement) -> StreamBehaviour {
    let population = measurement.datagrams_accounted();
    let incidence = |events| {
        Incidence::Of(
            Fraction::new(events, population)
                .expect("a measured run accounted for at least one datagram"),
        )
    };
    StreamBehaviour {
        delivery: measurement.window,
        loss: incidence(measurement.rx.lost),
        reorder: incidence(measurement.rx.reordered),
    }
}

fn shape(measurement: &Measurement) -> Shape {
    Shape {
        datagrams: measurement.datagrams,
        datagram_bytes: measurement.datagram_bytes,
        mbps: measurement.megabits_per_second(),
        mean_datagram_bytes: measurement.mean_datagram_bytes(),
        datagrams_per_access_unit: measurement.datagrams_per_access_unit(),
        under_one_datagram: measurement.under_one_datagram,
    }
}

fn stream(measurement: &Measurement, behaviour: &StreamBehaviour) -> Stream {
    Stream {
        expected: measurement.access_units_expected(),
        reconstructed: measurement.window.delivered,
        packet_loss: behaviour.loss.events(),
        datagrams_accounted: measurement.datagrams_accounted(),
        // Both unwrapped against a population the probe has already refused a
        // run without, which is why neither of these is an `Option` in the
        // document: an `Incidence` that could not state its population would
        // mean the measured sections should not exist at all.
        loss_ratio: behaviour
            .loss
            .value()
            .expect("a measured run states its datagram population"),
        au_loss: measurement.access_units_lost(),
        reordered: behaviour.reorder.events(),
        reorder_ratio: behaviour
            .reorder
            .value()
            .expect("the same population the loss ratio was taken over"),
        duplicates: measurement.rx.duplicates,
        rtp_jitter_us: measurement.jitter.get() as f64 / 1000.0,
    }
}

fn delivery(window: lanplay_link_metrics::Window) -> Delivery {
    let tail = window.tail;
    let span = window.span_s;
    Delivery {
        delivered: window.delivered,
        au_interval_p50_ms: window.p50_ms,
        au_interval_p95_ms: window.p95_ms,
        au_interval_p99_ms: window.p99_ms,
        au_interval_max_ms: window.max_ms,
        first_interval_p50_ms: window.first_p50_ms,
        first_interval_p95_ms: window.first_p95_ms,
        first_interval_p99_ms: window.first_p99_ms,
        first_interval_max_ms: window.first_max_ms,
        span_s: span,
        over_1_25t_per_min: tail.per_minute(0, span),
        over_1_5t_per_min: tail.per_minute(1, span),
        over_2t_per_min: tail.per_minute(2, span),
        over_3t_per_min: tail.per_minute(3, span),
        over_4t_per_min: tail.per_minute(4, span),
        over_6t_per_min: tail.per_minute(5, span),
        stall_clusters_per_min: tail.clusters_per_minute(span),
        stall_clusters: tail.clusters,
        over_2t: tail.over[2],
        mean_catch_up_units: tail.mean_catch_up(),
        max_catch_up_units: tail.catch_up_max,
        stall_gap_p50_ms: tail.stall_gap_p50_ms,
        stall_gap_p95_ms: tail.stall_gap_p95_ms,
    }
}

fn radio(association: &Association) -> Radio {
    let hint = conditions::hint(association);
    Radio {
        rssi_dbm: hint.rssi_dbm,
        noise_dbm: hint.noise_dbm,
        tx_rate_mbps: hint.tx_rate_mbps,
        channel: hint.channel,
        width_mhz: hint.width_mhz,
        radar_band: association.uses_radar_band(),
        country: association.country.clone(),
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanplay_link_metrics::THRESHOLDS;

    /// The six rates this file writes out by hand, against the multiples
    /// `crates/link-metrics` actually counts. A rate called `over_2t_per_min`
    /// filled from a different index is a mislabelled number nobody can see is
    /// wrong.
    #[test]
    fn the_delivery_names_match_the_multiples_that_are_counted() {
        assert_eq!(THRESHOLDS, [1.25, 1.5, 2.0, 3.0, 4.0, 6.0]);
        let mut window = lanplay_link_metrics::Window {
            span_s: 60.0,
            ..Default::default()
        };
        // One crossing at each threshold, so a rate that read the wrong index
        // would come back with the wrong count rather than with the same one.
        window.tail.over = [1, 2, 3, 4, 5, 6];
        let stated = delivery(window);
        assert_eq!(stated.over_1_25t_per_min, 1.0);
        assert_eq!(stated.over_1_5t_per_min, 2.0);
        assert_eq!(stated.over_2t_per_min, 3.0);
        assert_eq!(stated.over_3t_per_min, 4.0);
        assert_eq!(stated.over_4t_per_min, 5.0);
        assert_eq!(stated.over_6t_per_min, 6.0);
        assert_eq!(stated.over_2t, 3);
    }
}
