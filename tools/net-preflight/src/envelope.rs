//! The gate document, which is a different document from the report.
//!
//! The report is the product's artefact: it describes the link and decides
//! nothing. This is the arm's own account of itself for `xtask verdict`, which
//! is the only place in this repository where a verdict is decided. Keeping them
//! apart is what lets the report classify nothing while the gate still has
//! criteria that can fail.
//!
//! Every absence here is an absent observation rather than a zero, because
//! `xtask verdict` refuses a criterion whose number is missing and would decide
//! one whose number is a fabricated zero. So a probe that measured nothing
//! states its conditions, states how many datagrams arrived, and states nothing
//! else - and the arm comes back REFUSED with the criterion named, instead of
//! passing on a clean-looking loss of zero.
//!
//! Which criteria an arm states follows from the arm it is told it is, which is
//! the arrangement `tools/audio-rtp-gate.sh` arrived at for the same reason: a
//! receive-only probe cannot tell a fault relay on the loopback interface from a
//! peer across the air, so the claim that faults were injected is the caller's
//! and is recorded as the caller's.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::conditions::Conditions;
use crate::probe::{Measurement, Outcome, ProbeConfig};
use crate::report::Provenance;

/// The worst gap between consecutive access units a clean arm may show.
///
/// Derived from the four 120 s arms committed on this channel -
/// `results/b3-channel/ch36-r1`, `-r2`, `-r3` and `ch36-return-r1` - whose
/// `delivery.au_interval_max_ms` are 26.07, 50.59, 68.62 and 98.57 ms. The bound
/// sits above the worst of them, and a probe of five seconds has a twenty-fourth
/// of their opportunities to reach it. Fixed here rather than passed in, because
/// a bound chosen after the arm ran is a bound fitted to the answer.
const CLEAN_MAX_INTERVAL_MS: f64 = 120.0;

/// What the arm is claimed to be, which decides which criteria it states.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// The link as it is. Loss must be zero over a real population and the worst
    /// gap must stay under the bound above.
    Clean,
    /// The link through `lanplay-udp-fault`. The same two quantities must come
    /// out the other way: a control arm that shows no loss and no bunching is a
    /// fault that never reached the path, and a criterion nobody has seen
    /// disagree is a criterion nobody has grounds to trust.
    Faults,
}

impl Expect {
    pub fn label(self) -> &'static str {
        match self {
            Expect::Clean => "clean",
            Expect::Faults => "faults",
        }
    }

    /// The criteria the other arm states, for judging one arm by the other's.
    pub fn crossed(self) -> Expect {
        match self {
            Expect::Clean => Expect::Faults,
            Expect::Faults => Expect::Clean,
        }
    }
}

pub fn build(
    gate: &str,
    outcome: &Outcome,
    config: &ProbeConfig,
    conditions: &Conditions,
    provenance: &Provenance,
    expect: Expect,
    seed: Option<u64>,
) -> Value {
    let mut observations: BTreeMap<&'static str, f64> = BTreeMap::new();
    // Stated whatever happened: they are the population under the two
    // conditions criteria, and a run with no association read refuses on them
    // rather than being quietly compared with a run taken on another channel.
    observations.insert("conditions_reads", conditions.reads() as f64);
    observations.insert(
        "conditions_channel_moves",
        conditions.channel_moves() as f64,
    );

    match outcome {
        Outcome::Measured(measurement) => measured_observations(&mut observations, measurement),
        Outcome::Nothing { datagrams, .. } => {
            // The one number a run that measured nothing can honestly state.
            // Everything derived from it is left out so that every criterion
            // reading one comes back unavailable.
            observations.insert("datagrams", *datagrams as f64);
        }
    }

    let (declared, exercised) = coverage(outcome, expect);

    json!({
        "gate": gate,
        "run": {
            "started_unix_ms": unix_ms(),
            "span_s": match outcome {
                Outcome::Measured(measurement) => measurement.elapsed.as_secs_f64(),
                Outcome::Nothing { .. } => 0.0,
            },
            "seed": seed,
            "args": args(config, provenance, expect),
            "commit": provenance.commit,
            "arm": provenance.arm,
        },
        "environment": environment(conditions),
        "declared": declared,
        "exercised": exercised,
        "observations": observations,
        "checks": checks(expect),
        "findings": findings(outcome),
    })
}

fn measured_observations(into: &mut BTreeMap<&'static str, f64>, measurement: &Measurement) {
    let window = measurement.window;
    into.insert("datagrams", measurement.datagrams as f64);
    into.insert(
        "datagrams_accounted",
        measurement.datagrams_accounted() as f64,
    );
    into.insert("datagrams_lost", measurement.rx.lost as f64);
    into.insert("datagrams_reordered", measurement.rx.reordered as f64);
    into.insert("access_units_delivered", window.delivered as f64);
    into.insert(
        "access_units_under_one_datagram",
        measurement.under_one_datagram as f64,
    );
    into.insert(
        "datagrams_per_access_unit",
        measurement.datagrams_per_access_unit(),
    );
    into.insert("mbps", measurement.megabits_per_second());
    into.insert("mean_datagram_bytes", measurement.mean_datagram_bytes());
    into.insert("au_interval_p50_ms", window.p50_ms);
    into.insert("au_interval_p99_ms", window.p99_ms);
    into.insert("au_interval_max_ms", window.max_ms);
    into.insert("first_interval_p99_ms", window.first_p99_ms);
    into.insert("delivery_stall_clusters", window.tail.clusters as f64);
    into.insert("delivery_over_2t", window.tail.over[2] as f64);
    // The counts above are what a five-second window supports; these two are
    // what it can be compared against a ninety-second one on, and the gate's
    // separation figure needs the rate because the variance it has to beat was
    // measured as a rate.
    into.insert(
        "delivery_over_2t_per_min",
        window.tail.per_minute(2, window.span_s),
    );
    into.insert(
        "delivery_stall_clusters_per_min",
        window.tail.clusters_per_minute(window.span_s),
    );
    // Absent rather than zero from a sender that states no frame id: the count
    // of access units it produced is then not something this probe read.
    if let Some(expected) = measurement.access_units_expected() {
        into.insert("access_units_expected", expected as f64);
    }
    if let Some(lost) = measurement.access_units_lost() {
        into.insert("access_units_lost", lost as f64);
    }
}

/// What the arm claims to cover and what it reached.
///
/// The delivery entry is derived from the outcome rather than promised in
/// advance, because a run that received nothing is already refused by its
/// criteria and declaring coverage it could not reach would turn that refusal
/// into a failure - and a criterion that was read and disagreed is a stronger
/// statement than one nobody could read.
///
/// The fault entry is the opposite, and it is the one claim a receive-only probe
/// cannot check for itself: the caller says a relay was told to break the
/// traffic, and the run exercises that claim only if loss or bunching actually
/// showed up. `tools/audio-rtp-gate.sh` records what the absence of this costs -
/// its control arm once destroyed 2000 datagrams of 2000 and passed on the
/// harness being broken rather than on any criterion firing.
fn coverage(outcome: &Outcome, expect: Expect) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut declared = Vec::new();
    let mut exercised = Vec::new();
    if let Outcome::Measured(measurement) = outcome {
        declared.push("link-delivery");
        exercised.push("link-delivery");
        if expect == Expect::Faults
            && (measurement.rx.lost > 0 || measurement.window.tail.clusters > 0)
        {
            exercised.push("fault-injection");
        }
    }
    if expect == Expect::Faults {
        declared.push("fault-injection");
    }
    (declared, exercised)
}

fn checks(expect: Expect) -> Vec<Value> {
    let mut checks = vec![
        json!({
            "name": "access units completed",
            "kind": "must_not_be_zero",
            "reads": "access_units_delivered",
            "why": "the population under every zero in this arm. A probe that completed no \
                    access unit measured nothing, and zero datagrams lost out of zero sent is \
                    the most common way an instrument in this project has lied",
        }),
        json!({
            "name": "datagrams accounted for",
            "kind": "must_not_be_zero",
            "reads": "datagrams_accounted",
            "why": "the population under the loss figure: datagrams the sequence machine \
                    accepted plus the gaps it saw. A loss ratio over nothing is not a small \
                    loss",
        }),
        json!({
            "name": "the radio said what it was",
            "kind": "must_not_be_zero",
            "reads": "conditions_reads",
            "why": "a run whose conditions were not recorded cannot be compared with any other \
                    run, which is what happened to thirteen arms of the A8 sweep. This decides \
                    whether the run is comparable and never whether the link is good - the \
                    radio tier is barred from that, because -48 dBm at 1200 Mbps produced \
                    concealment from 0.196 to 7.442 per cent across ten arms",
        }),
        json!({
            "name": "the channel held still",
            "kind": "must_be_zero",
            "reads": "conditions_channel_moves",
            "population": "conditions_reads",
            "why": "the association is read before the probe and after it, and a channel or \
                    width that moved between the two means one report describes two links. \
                    Signal is deliberately not part of it: two reads seconds apart differ by a \
                    decibel or two on a link nobody touched, and a criterion that fires on that \
                    refuses every run",
        }),
        json!({
            "name": "the traffic was video shaped",
            "kind": "must_be_zero",
            "reads": "access_units_under_one_datagram",
            "population": "access_units_delivered",
            "why": "this product hands a whole access unit to the kernel at once, some tens of \
                    datagrams with no gap between them, and the burst is the part a link fails \
                    at. An access unit that would have fitted in one datagram is a probe \
                    pointed at traffic this link is never asked to carry, and every number \
                    beside it would then be about the wrong stream while looking reasonable",
        }),
    ];

    match expect {
        Expect::Clean => {
            checks.push(json!({
                "name": "nothing was lost",
                "kind": "must_be_zero",
                "reads": "datagrams_lost",
                "population": "datagrams_accounted",
                "why": "the four 120 s arms committed on this channel - results/b3-channel \
                        ch36-r1, r2, r3 and ch36-return-r1 - each report stream.packet_loss 0, \
                        so on this link a lost datagram in five seconds is news. The companion \
                        that keeps this from being unfalsifiable is the fault arm, where the \
                        same observation must not be zero",
            }));
            checks.push(json!({
                "name": "the worst gap between access units",
                "kind": "must_be_below",
                "reads": "au_interval_max_ms",
                "bound": CLEAN_MAX_INTERVAL_MS,
                "why": "the same four arms report worst complete-intervals of 26.07, 50.59, \
                        68.62 and 98.57 ms over 120 s each, so the bound sits above every clean \
                        arm this repository has recorded here and a five-second probe has a \
                        twenty-fourth of their opportunities to reach it. Crossing counts are \
                        deliberately not a criterion at this length: those arms cluster at 2.0 \
                        to 18.5 a minute, which is 0.17 to 1.5 clusters in five seconds, and a \
                        count expected to be small but non-zero separates nothing",
            }));
        }
        Expect::Faults => {
            checks.push(json!({
                "name": "the injected loss reached the path",
                "kind": "must_not_be_zero",
                "reads": "datagrams_lost",
                "why": "the relay was told to drop datagrams, and an arm that shows none of it \
                        is a fault that never reached the traffic rather than a link that \
                        survived one. This is the companion to the clean arm's zero: without an \
                        arm where it comes out the other way, that zero is unfalsifiable",
            }));
            checks.push(json!({
                "name": "the probe saw the bunching",
                "kind": "must_not_be_zero",
                "reads": "delivery_stall_clusters",
                "why": "the relay holds every datagram for the length of a stall and then \
                        releases them together, which is bunching and not loss, and \
                        crates/link-metrics counts it as a stall followed by units arriving \
                        early. A control arm whose held datagrams produced no cluster would \
                        mean the instrument cannot see what was done to it, which is the one \
                        thing a clean arm's silence cannot establish",
            }));
        }
    }

    checks
}

/// What the run established that no criterion votes on.
fn findings(outcome: &Outcome) -> Vec<String> {
    let Outcome::Measured(measurement) = outcome else {
        return Vec::new();
    };
    let window = measurement.window;
    let mut findings = vec![
        format!(
            "{} access units over {:.2} s at {:.1} Mbps, {:.1} datagrams per unit of {:.0} bytes \
             mean",
            window.delivered,
            measurement.elapsed.as_secs_f64(),
            measurement.megabits_per_second(),
            measurement.datagrams_per_access_unit(),
            measurement.mean_datagram_bytes(),
        ),
        format!(
            "complete-interval p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms; first-datagram interval \
             p99 {:.3} ms, which separates a unit that starts late from one that finishes badly",
            window.p50_ms, window.p99_ms, window.max_ms, window.first_p99_ms,
        ),
        format!(
            "{} crossings of two source periods and {} stall clusters counted, which over {:.2} s \
             is {:.1} and {:.1} a minute - the counts are what this window supports and the rates \
             are what it can be compared on",
            window.tail.over[2],
            window.tail.clusters,
            window.span_s,
            window.tail.per_minute(2, window.span_s),
            window.tail.clusters_per_minute(window.span_s),
        ),
    ];
    if window.tail.clusters > 0 {
        findings.push(format!(
            "stall gaps p50 {:.1} ms and p95 {:.1} ms: a tight distribution indicts a timer - a \
             scan, a beacon, a power-save cycle - and a broad one indicts contention, and the two \
             need different actions",
            window.tail.stall_gap_p50_ms, window.tail.stall_gap_p95_ms,
        ));
    }
    if let Some(error) = &measurement.recv_error {
        findings.push(format!("the receive loop ended on an error: {error}"));
    }
    findings
}

fn args(config: &ProbeConfig, provenance: &Provenance, expect: Expect) -> Value {
    json!({
        "seconds": config.seconds,
        "fps": config.fps,
        "mtu": config.mtu,
        "pacer": provenance.pacer,
        "expect": expect.label(),
        "faults": provenance.faults,
    })
}

/// The conditions, as what the run depended on and could read.
///
/// In the environment table rather than among the observations because they are
/// not what any criterion here is about: the two that are - how many reads
/// answered, and whether the channel moved - are counts, and these are the
/// values a person reads to find out why an arm looked the way it did.
fn environment(conditions: &Conditions) -> Value {
    let mut table = json!({});
    let entries = table.as_object_mut().expect("an object was just built");
    for (when, read) in [("before", &conditions.before), ("after", &conditions.after)] {
        let Some(association) = read else {
            continue;
        };
        entries.insert(
            format!("radio_{when}"),
            json!(format!(
                "channel {} at {} MHz, {} dBm over {} dBm noise, {:.0} Mbps negotiated{}",
                association.channel,
                association.width_mhz,
                association.rssi_dbm,
                association.noise_dbm,
                association.tx_rate_mbps,
                if association.uses_radar_band() {
                    ", in a radar band"
                } else {
                    ""
                },
            )),
        );
    }
    if let Some(drift) = conditions.signal_drift_db() {
        entries.insert("radio_signal_drift_db".to_string(), json!(drift));
    }
    table
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or_default()
}
