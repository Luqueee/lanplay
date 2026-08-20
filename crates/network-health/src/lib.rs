//! What link this system has in front of it, and which thing is failing.
//!
//! `NETWORK.md` fixes the observation contract and this crate is it. Three
//! tiers, and the separation between them is enforced by the type system rather
//! than by a comment, because a comment saying "never decide from RSSI" erodes
//! and a function that cannot see RSSI does not:
//!
//! ```text
//! RadioHint          diagnostic only, never decides
//! StreamBehaviour    decides
//! Experience         describes what the user got, never decides
//! ```
//!
//! [`classify`] takes one argument and it is the middle tier. The other two are
//! not parameters, so no later edit can quietly start deciding from them. That
//! is a claim about this file, so it is checked here rather than asserted. This
//! compiles:
//!
//! ```
//! use lanplay_network_health::{NetworkCondition, NetworkObservation, classify};
//!
//! fn decide(seen: &NetworkObservation) -> NetworkCondition {
//!     classify(&seen.stream)
//! }
//! ```
//!
//! and reaching for the radio does not, because `&Option<RadioHint>` is not
//! `&StreamBehaviour`:
//!
//! ```compile_fail
//! use lanplay_network_health::{NetworkCondition, NetworkObservation, classify};
//!
//! fn decide(seen: &NetworkObservation) -> NetworkCondition {
//!     classify(&seen.radio)
//! }
//! ```
//!
//! nor does reaching for what the user got:
//!
//! ```compile_fail
//! use lanplay_network_health::{NetworkCondition, NetworkObservation, classify};
//!
//! fn decide(seen: &NetworkObservation) -> NetworkCondition {
//!     classify(&seen.experience)
//! }
//! ```
//!
//! The three are stated as a set rather than as two refusals on their own. A
//! `compile_fail` block passes when the code fails to build for any reason at
//! all, including a renamed crate or a typo, so a refusal beside a working call
//! that shares its imports and its shape is the difference between evidence and
//! a block that would pass with the whole contract deleted. `trybuild` would pin
//! the error code as well and was rejected only because it is a dependency this
//! workspace does not already carry, for a guarantee the pairing already gives.
//!
//! The delivery tier is `crates/link-metrics` and is not rebuilt here. Its
//! [`Window`] is composed rather than copied, so there is one definition of
//! every percentile, every counted crossing and every stall gap in this
//! repository.

use core::num::NonZeroU64;

use lanplay_link_metrics::{THRESHOLDS, Window};

pub mod corpus;

/// What the radio was doing, which answers *why* and never *whether*.
///
/// A link at -48 dBm negotiating 1200 Mbps produced concealment ratios from
/// 0.196 to 7.442 per cent across the ten arms of
/// `results/audio/jitter-target-a8`, the steadiest signal this project has
/// measured, which is a spread of a factor of thirty-eight in what the stream
/// received. Meanwhile 3 dB of signal difference between those arms moved the
/// negotiated rate by nothing at all. Signal is a proxy for rate and rate is a
/// proxy for airtime; the stream's own behaviour is the thing itself, and it was
/// measured disagreeing with its proxies in one run.
///
/// Held as its own quantities rather than as
/// `lanplay_capabilities::wifi::Association`, which is what fills this live: the
/// offline harness reads a `.wifi.csv` or `.radio.csv` row and has no BSSID and
/// no country code, and inventing two fields to satisfy a type is worse than a
/// small honest one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RadioHint {
    pub rssi_dbm: i64,
    pub noise_dbm: i64,
    /// The negotiated rate, which is a ceiling on throughput and not throughput.
    pub tx_rate_mbps: f64,
    pub channel: i64,
    pub width_mhz: u32,
}

/// A count over the population it was counted in.
///
/// The population is [`NonZeroU64`] because a zero measured over nothing is an
/// absence and not a result, and a classifier handed `0.0 / 0` would read a run
/// that received no datagrams at all as a run that lost none. Unrepresentable is
/// better than checked: [`classify`] then needs no branch for it and cannot
/// grow one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fraction {
    events: u64,
    population: NonZeroU64,
}

impl Fraction {
    /// `None` when the population is empty, which callers must refuse rather
    /// than round to zero.
    pub fn new(events: u64, population: u64) -> Option<Self> {
        NonZeroU64::new(population).map(|population| Fraction { events, population })
    }

    pub fn events(&self) -> u64 {
        self.events
    }

    pub fn population(&self) -> u64 {
        self.population.get()
    }

    pub fn value(&self) -> f64 {
        self.events as f64 / self.population.get() as f64
    }
}

/// A count of datagram events, with the population it was counted in when the
/// instrument wrote one down.
///
/// Two variants, and the reason was found by reading the writer rather than the
/// reader. The video envelopes state `stream.packet_loss` and `stream.reordered`
/// as datagrams and `stream.expected` as access units, one per frame the host was
/// asked to feed, so dividing the first by the last is datagrams over access
/// units. On the committed arms that quotient is 0/14400 and the zero numerator
/// hid it; the reorder column had no zero numerator and read 30.8 per cent where
/// the datagram figure is nearer 0.69, because a 40 Mbps access unit at 120 fps
/// is some forty-five datagrams and not one.
///
/// `macos/client/src/session.rs` computes the right populations, `rx.lost +
/// rx.received` and `rx.received`, and prints them; it does not write them into
/// the `stream` section, so they are not recoverable from what is committed.
/// Rather than divide by the wrong number, an envelope that states no population
/// gives up [`Incidence::Bare`], and no rule reads a bare count as a rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Incidence {
    /// Events over the datagrams they were counted among.
    Of(Fraction),
    /// Events counted, population never written down.
    Bare(u64),
}

impl Incidence {
    pub fn events(&self) -> u64 {
        match self {
            Incidence::Of(fraction) => fraction.events(),
            Incidence::Bare(events) => *events,
        }
    }

    pub fn population(&self) -> Option<u64> {
        match self {
            Incidence::Of(fraction) => Some(fraction.population()),
            Incidence::Bare(_) => None,
        }
    }

    /// `None` when no population was written down, which is not the same as
    /// zero and must never be printed as one.
    pub fn value(&self) -> Option<f64> {
        match self {
            Incidence::Of(fraction) => Some(fraction.value()),
            Incidence::Bare(_) => None,
        }
    }

    /// Whether anything happened at all.
    ///
    /// The only question about loss this corpus can answer, and it needs no
    /// population: fifty-eight of the fifty-nine committed sessions lost nothing
    /// and the fifty-ninth lost 382 datagrams, so the boundary the corpus
    /// establishes is at zero and a level threshold would need a denominator
    /// every video envelope leaves out.
    pub fn any(&self) -> bool {
        self.events() > 0
    }
}

/// What the stream did, which is the only tier that decides.
///
/// Delivery is `crates/link-metrics`' own [`Window`], composed rather than
/// unpacked, so there is one definition of every percentile, every counted
/// crossing and every stall gap in this repository. Loss and reorder sit beside
/// it because they are counted at the socket by the depacketiser and the delivery
/// window does not carry them.
///
/// There is deliberately no way to express "the cadence was never counted". An
/// earlier draft had one, and it was wrong: a session with no delivery tier came
/// out as [`NetworkCondition::UnknownDegradation`], which turns "this cannot be
/// read" into "something is wrong with the network" and reads a missing
/// instrument as evidence against a link. `results/audio/e2e-clean/clean-600s` is
/// the cleanest arm this project has recorded - 0 lost of 120005, no render
/// underrun in 112493 callbacks - and it was being printed as a degradation. The
/// state of the instrument is not part of the vocabulary that describes the
/// network, so it lives in [`corpus::Unreadable`] instead: a reader that cannot
/// fill this struct refuses the session and names the tier it is missing, which
/// `TASKS.md` section 0.2 keeps distinct from a finding.
#[derive(Clone, Copy, Debug)]
pub struct StreamBehaviour {
    pub delivery: Window,
    /// Datagrams that never arrived, over the datagrams the transport accounted
    /// for. Both sides the same unit, which is the whole requirement and was the
    /// whole defect.
    ///
    /// `None` when no envelope stated an honest population, which is every
    /// session in the committed corpus. The datagram counters there -
    /// `packet_loss`, `reordered`, `duplicates` - have no datagram total anywhere,
    /// and `stream.au_loss` divides by `target_fps` times the nominal run length,
    /// a number nothing produced and which link loss, run truncation and host
    /// under-production all feed. `macos/client/src/report.rs` now carries
    /// `loss_events` beside `loss_population`, so from the passive monitor onward
    /// this is `Some` and [`NetworkCondition::SevereLoss`] becomes confirmable.
    ///
    /// `None` means the loss tier is absent, not that nothing was lost. Every
    /// caller that prints a verdict taken with it absent has to say so.
    pub loss_ratio: Option<Fraction>,
    /// Datagrams that arrived out of sequence. Carried and read by no rule: 4441
    /// of them beside 14400 access units read as 30.8 per cent where the datagram
    /// figure is nearer 0.69, and there is nothing in the committed envelopes to
    /// divide by. It becomes a criterion when an envelope states what it
    /// reordered out of, and not before.
    pub reorder: Incidence,
}

/// What the user got. Feeds the interface, indicts nothing.
///
/// This is barred from deciding for the same reason the radio is, and the reason
/// is less obvious: anything measured through a later stage carries that stage's
/// faults. `crates/link-metrics` exists because a suspended display link made a
/// healthy link read 141 ms at p99 while it was losing nothing, and
/// `results/audio/jitter-target-a8` shows the audio equivalent - the same link
/// produced concealment from 0.196 to 7.442 per cent depending on the jitter
/// target, which is a parameter of the playout stage rather than of the link.
#[derive(Clone, Copy, Debug, Default)]
pub struct Experience {
    /// The fraction of display ticks that presented a frame newer than the one
    /// presented at the tick before. `None` for a run with no display, which is
    /// every audio arm and every link-only video arm.
    pub fresh_tick_ratio: Option<f64>,
    pub frame_age_p99_ms: Option<f64>,
    /// Source audio replaced by the concealer, over the frames that arrived.
    /// Source fidelity, not playout continuity, which is the distinction
    /// `results/audio` had to be re-read to make.
    pub concealed_ratio: Option<f64>,
    /// Cycles the ring could not fill, each one a whole buffer of silence sent
    /// to a device. Zero in all forty committed audio envelopes.
    pub silence_events: Option<u64>,
}

/// One observation of the link, in three tiers.
#[derive(Clone, Copy, Debug)]
pub struct NetworkObservation {
    /// `None` when CoreWLAN did not answer, which must not stop anything: the
    /// classifier never reads this tier at all. One association read costs
    /// 3.2 ms at p50 and 15.5 ms at worst, so the sampler that fills this runs
    /// at 1 Hz on a thread of its own and is allowed to come back empty.
    pub radio: Option<RadioHint>,
    pub stream: StreamBehaviour,
    pub experience: Experience,
}

/// What is wrong, in the vocabulary `NETWORK.md` fixed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkCondition {
    Healthy,
    /// Loss rising with throughput near what the PHY can carry.
    ///
    /// Unreachable from the committed corpus and deliberately not implemented,
    /// which is a finding rather than an omission. Its discriminator as
    /// `NETWORK.md` states it needs PHY capacity, which is a [`RadioHint`] and
    /// therefore invisible here by construction. The stream-side substitute -
    /// loss rising as offered bitrate rises - needs two windows at different
    /// bitrates, which is `tools/bitrate-sweep.sh`, and `NETWORK.md` records
    /// that nothing under `results/` holds its output. Inventing a threshold to
    /// reach this variant would be inventing the evidence for it, so
    /// [`classify`] never returns it and `tools/classify-sessions.sh` says so on
    /// every run.
    CapacityPressure,
    /// Loss at or near zero, with counted crossings and clusters both risen.
    CadenceDegraded,
    SevereLoss,
    /// Disturbances that did not recur, which must not be treated as
    /// degradation: the failure this phase exists to avoid is a controller that
    /// reacts to one 80 ms stall.
    TransientStall,
    /// Something is wrong, or may be, and this tier cannot name it.
    ///
    /// Load-bearing. A classifier with no way to say this will name the wrong
    /// thing instead, and there are two routes to it here - a run whose cadence
    /// nobody counted, and a run whose crossing rate falls in the band the
    /// corpus left empty between its two populations.
    UnknownDegradation,
}

impl NetworkCondition {
    pub fn name(&self) -> &'static str {
        match self {
            NetworkCondition::Healthy => "Healthy",
            NetworkCondition::CapacityPressure => "CapacityPressure",
            NetworkCondition::CadenceDegraded => "CadenceDegraded",
            NetworkCondition::SevereLoss => "SevereLoss",
            NetworkCondition::TransientStall => "TransientStall",
            NetworkCondition::UnknownDegradation => "UnknownDegradation",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "Healthy" => Some(NetworkCondition::Healthy),
            "CapacityPressure" => Some(NetworkCondition::CapacityPressure),
            "CadenceDegraded" => Some(NetworkCondition::CadenceDegraded),
            "SevereLoss" => Some(NetworkCondition::SevereLoss),
            "TransientStall" => Some(NetworkCondition::TransientStall),
            "UnknownDegradation" => Some(NetworkCondition::UnknownDegradation),
            _ => None,
        }
    }
}

/// The crossing every rule below is stated in: access units delivered at or
/// past two source periods.
///
/// Two periods rather than another entry in [`THRESHOLDS`] because it is the
/// multiple every comparison in this corpus is already written in - `>2T/min` is
/// the column in `results/b3-channel/report.txt`, in
/// `crates/capabilities/src/wifi.rs` and in each of the three commits that
/// established the channel result - so a threshold stated in it can be checked
/// against what was recorded rather than recomputed from a different column.
const TWO_PERIODS: usize = 2;
const _: () = assert!(
    THRESHOLDS[TWO_PERIODS] == 2.0,
    "the rules below are stated in crossings of two source periods, and \
     link-metrics has reordered THRESHOLDS under them"
);

/// Below this median gap between stalls, the disturbance is recurring fast
/// enough to be the stream's cadence rather than an event in it.
///
/// This is the axis the corpus separates on, and it is not the one a first cut
/// reached for. Ranking on the crossing rate looked defensible - the runs
/// recorded as carrying the 220 ms mechanism span 58.6 to 364.0 crossings a
/// minute and the runs recorded as mitigated span 0.0 to 19.0 - but it split a
/// population: `results/phase/lottery/2` and `/6` sit at 58.6 a minute with
/// their stalls 229 and 233 ms apart, which is the same mechanism as their four
/// siblings at 61.3 to 71.9, and any rate threshold above 58.6 calls the six of
/// them two different things. `NETWORK.md` says why in advance: a stall rate
/// alone cannot tell a timer from contention, and the gap distribution can.
///
/// The gaps are disjoint by a factor of 4.9 and the split is exact over all
/// twenty-four committed runs that carry the tail counters:
///
/// ```text
/// 101 to 233 ms   every run recorded as carrying a periodic mechanism -
///                 awdl-down-r1 and r2 at 221 and 222, b3-channel's three
///                 ch116-return arms at 222, pcap-parallel's four at 222,
///                 phase's ten at 101 to 233
/// 1140 ms and up  every run recorded as mitigated or clean - ch36-r1 at
///                 1139.80, ch36-return-r1 at 1573.91, ch36-r2 at 1600.07,
///                 soak-1080p120 at 2766.14
/// ```
///
/// `32a826f` decides an arm on exactly this quantity: the 220 ms period does not
/// return, stalls in that run sit about a second apart at the median, which is
/// not a clock. Stated as 500 ms, the geometric midpoint of 233 and 1140 being
/// 515, and geometric rather than arithmetic because the two populations are
/// separated by a ratio and a midpoint should leave the same ratio of margin
/// either side.
///
/// The modal gap was tried first and rejected: `results/b3-channel/report.txt`
/// puts ch36-return-r1's modal gap at 216 ms, indistinguishable from the DFS
/// runs' 220. What separates them is how much of the distribution sits there,
/// 22 per cent of its 36 gaps against 65 to 70 per cent of the DFS runs', a
/// concentration the median carries and the mode does not. The mode is also not
/// in the committed JSON and the median is.
const RECURRING_STALL_GAP_MS: f64 = 500.0;

/// The rate of crossings and clusters below which a recurring disturbance is
/// still what a working link on this hardware looks like.
///
/// The worst run recorded as mitigated is `results/b3-channel/ch36-return-r1`, at
/// 19.00 crossings and 18.50 clusters a minute, which `32a826f` calls the worst
/// of the four non-DFS runs and still nowhere near the DFS population under the
/// verdict `channel 36 fixed  MITIGATION VALIDATED`. That commit's own table
/// transcribes its crossing rate as 18.5, which is its cluster rate; 19.00 is
/// what the JSON and `results/b3-channel/report.txt` both state, and the larger
/// of the two is the one used. Rounded up to 20 because the corpus locates this
/// boundary only to within a factor of three.
///
/// This is a guard on amount rather than the thing that decides. Stalls
/// recurring five times a second in a burst that occupies a twentieth of a run
/// are not that run's cadence, and no committed run has that shape, so refusing
/// to name it is cheaper than naming it wrongly.
const CLEAR_PER_MIN: f64 = 20.0;

/// The longest gap between stalls `crates/link-metrics` can measure.
///
/// Its histogram is bounded at ten seconds, so a `stall_gap_p50_ms` at the
/// ceiling - reported as 10007.6 by the clipping bucket - means the median
/// interval between stalls was longer than the instrument can hold. That is not
/// a cadence; it is disturbances that did not recur inside any window this phase
/// could react in. `results/b3-channel/ch36-r3` is the only committed run there,
/// with four stalls in 119.98 s and a worst interval of 26.07 ms, recorded in
/// `ba79a59` as three stalls in two minutes and no interval anywhere near a
/// period.
///
/// Not a tuned number: it is the instrument's own bound, so it moves only if
/// `crates/link-metrics` widens its histogram.
const ISOLATED_STALL_GAP_MS: f64 = 10_000.0;

/// Stalls needed before a gap between them exists.
///
/// `crates/link-metrics` records a gap only from the second stall onwards, so one
/// stall leaves the gap histogram empty and `value_at_quantile` on an empty
/// histogram answers 0.0. Read without this guard, the archetypal transient - a
/// single disturbance and then normal service - would present the tightest
/// recurrence the instrument can express.
const GAP_NEEDS_STALLS: u64 = 2;

/// Which condition the stream is in.
///
/// Rules, not a score. A score is a single number that cannot be argued with and
/// cannot be traced back to the run that set it; each branch below names the runs
/// its constant came from.
///
/// One argument, and it is the middle tier. See this module's header for the
/// blocks that will not compile if that changes.
pub fn classify(stream: &StreamBehaviour) -> NetworkCondition {
    // Loss decides on its own and first, when there is a loss tier to read. A
    // datagram that never arrived is a statement about the link no cadence figure
    // can soften, and reading the cadence first would name the symptom and miss
    // the cause.
    //
    // Any loss rather than a level, and that is forced rather than chosen. Zero is
    // the established state of this link - 0 of 120005 datagrams over the air in
    // results/audio/e2e-clean/clean-600s, and the decision to leave FEC and NACK
    // out of scope rests on it - and the only positive draw anybody has recorded,
    // results/audio/jitter-target-a8/t20-p3 at 382 of 23997, was refused in
    // fbe503b with the radio named rather than the buffer blamed. A single
    // positive draw cannot locate a threshold below itself, so the boundary the
    // corpus establishes is at zero and no second level is invented. Splitting
    // this into a mild band and a severe one waits on a run that loses a little.
    //
    // `None` is the absence of the tier and never a zero in it. Every committed
    // session is `None`, so this branch is wired and unconfirmed rather than dead:
    // it decides as soon as a session recorded with the monitor's own loss tier is
    // committed.
    if let Some(ratio) = stream.loss_ratio
        && ratio.value() > 0.0
    {
        return NetworkCondition::SevereLoss;
    }

    let window = stream.delivery;

    // The stall count and the crossing count at two periods are the same number:
    // `crates/link-metrics` opens a stall on the same comparison it counts this
    // threshold with. The const assertion above is what keeps that true.
    let stalls = window.tail.over[TWO_PERIODS];
    if stalls == 0 {
        return NetworkCondition::Healthy;
    }

    // A disturbance that did not recur, however many of them there were. This
    // comes before anything about how much of the stream was affected, because
    // the failure this phase exists to avoid is a controller that reacts to one
    // 80 ms stall.
    if stalls < GAP_NEEDS_STALLS || window.tail.stall_gap_p50_ms >= ISOLATED_STALL_GAP_MS {
        return NetworkCondition::TransientStall;
    }

    let crossings = window.tail.per_minute(TWO_PERIODS, window.span_s);
    let clusters = window.tail.clusters_per_minute(window.span_s);

    // Recurring, and no more of the stream affected than on the runs the record
    // calls mitigated or clean. Either quantity above that ceiling is enough to
    // leave here, because falling through on one of them would let a run with a
    // stall every third of a second and nothing catching up afterwards read as
    // well.
    if crossings <= CLEAR_PER_MIN && clusters <= CLEAR_PER_MIN {
        return NetworkCondition::Healthy;
    }

    // Crossings and clusters both, because the taxonomy asks for both and they
    // say different things: a stall followed by units arriving early is the link
    // holding traffic back and releasing it together, while a stall with no
    // catch-up is a plain gap. Every committed run carrying the periodic
    // mechanism has the two within a few per cent of each other, so the
    // conjunction costs nothing here and declines to name a shape the corpus has
    // never produced.
    let both_risen = crossings > CLEAR_PER_MIN && clusters > CLEAR_PER_MIN;
    if both_risen && window.tail.stall_gap_p50_ms < RECURRING_STALL_GAP_MS {
        return NetworkCondition::CadenceDegraded;
    }

    // More of the stream affected than any run the record calls clean, and either
    // recurring seconds apart or recurring fast with nothing catching up. Neither
    // shape is in the corpus, so neither gets named.
    NetworkCondition::UnknownDegradation
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanplay_link_metrics::Tail;

    /// Builds the middle tier the way the harness does, from the figures a
    /// committed session states, so a test reads like the row it came from.
    fn counted(
        span_s: f64,
        crossings: u64,
        clusters: u64,
        stall_gap_p50_ms: f64,
    ) -> StreamBehaviour {
        let mut over = [0u64; THRESHOLDS.len()];
        over[TWO_PERIODS] = crossings;
        StreamBehaviour {
            delivery: Window {
                delivered: 14_400,
                span_s,
                tail: Tail {
                    over,
                    clusters,
                    stall_gap_p50_ms,
                    ..Tail::default()
                },
                ..Window::default()
            },
            // Absent, which is every session in the committed corpus: no envelope
            // there states a datagram population. Reorder is a bare count for the
            // same reason and no rule reads it.
            loss_ratio: None,
            reorder: Incidence::Bare(4_441),
        }
    }

    #[test]
    fn an_empty_population_has_no_fraction() {
        assert_eq!(Fraction::new(0, 0), None);
        assert_eq!(
            Fraction::new(0, 120_005).map(|f| f.value()),
            Some(0.0),
            "a zero over a real population is a result and must survive"
        );
    }

    #[test]
    fn the_dfs_clock_is_cadence_degraded() {
        // results/awdl/awdl-down-r2: 131 stalls and 131 clusters over 119.98 s,
        // sitting 222.17 ms apart at the median. The mildest committed run
        // carrying the 220 ms period on the crossing rate, which is why ranking
        // on that rate looked workable and is not what decides here.
        let stream = counted(119.9816035, 131, 131, 222.167039);
        assert_eq!(classify(&stream), NetworkCondition::CadenceDegraded);
    }

    #[test]
    fn the_same_clock_at_a_lower_rate_is_still_the_clock() {
        // results/phase/lottery/2: 44 stalls and 43 clusters over 45.03 s, so
        // 58.62 and 57.29 a minute - below every run that could have pinned a
        // rate threshold - with its stalls 229.24 ms apart. Its five siblings run
        // 58.57 to 71.91 a minute at 224.92 to 232.91 ms, one mechanism at one
        // period, and a classifier that splits the six of them is ranking
        // sampling noise. This is the case that moved the rule off the rate and
        // onto the gap.
        let stream = counted(45.034703, 44, 43, 229.2449);
        assert_eq!(classify(&stream), NetworkCondition::CadenceDegraded);
    }

    #[test]
    fn the_mitigated_channel_is_healthy() {
        // results/b3-channel/ch36-return-r1: 38 crossings, 37 clusters over
        // 119.98 s, stalls 1573.91 ms apart. The worst run whose record says the
        // mechanism was absent, so it is the one that pins the ceiling.
        let stream = counted(119.982351959, 38, 37, 1573.912575);
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
    }

    #[test]
    fn ten_minutes_of_recurring_stalls_at_a_low_rate_is_healthy() {
        // results/soak-1080p120/soak: 77 crossings and 77 clusters over
        // 599.97 s, stalls 2766.14 ms apart, recorded in 71c1714 as clean.
        let stream = counted(599.970285667, 77, 77, 2766.143487);
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
    }

    #[test]
    fn stalls_that_did_not_recur_are_a_transient() {
        // results/b3-channel/ch36-r3: 4 stalls over 119.98 s with the median gap
        // between them at the histogram's ceiling.
        let stream = counted(119.9808535, 4, 4, 10_007.609343);
        assert_eq!(classify(&stream), NetworkCondition::TransientStall);
    }

    #[test]
    fn one_stall_is_a_transient_and_not_the_tightest_clock_measurable() {
        // One stall leaves link-metrics' gap histogram empty and an empty
        // histogram answers 0.0, which is a shorter median gap than any real
        // clock in the corpus. Without the guard on the stall count this run -
        // the archetypal single disturbance - would present as the most periodic
        // thing ever measured.
        let stream = counted(120.0, 1, 1, 0.0);
        assert_eq!(classify(&stream), NetworkCondition::TransientStall);
    }

    #[test]
    fn a_transient_needs_a_stall_to_have_happened() {
        // The same isolation figure with nothing counted under it. A run with no
        // stall at all is healthy, and a stall gap read off an empty histogram
        // must not manufacture a disturbance.
        let stream = counted(119.9808535, 0, 0, 10_007.609343);
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
    }

    #[test]
    fn recurring_seconds_apart_above_the_clear_ceiling_is_not_called() {
        // Sixty stalls a minute with the median gap at a second: too much of the
        // stream affected for any run the record calls clean, and too slow a
        // recurrence for the mechanism the record calls a clock. No committed run
        // has this shape and the honest answer is that this tier cannot say.
        let stream = counted(120.0, 120, 120, 1_000.0);
        assert_eq!(classify(&stream), NetworkCondition::UnknownDegradation);
    }

    #[test]
    fn fast_recurrence_without_bunching_is_not_called() {
        // A hundred and forty stalls a minute 300 ms apart with nothing catching
        // up afterwards. Fast enough to be a cadence and with no bunching in it,
        // which is a plain gap rather than the link holding traffic back and
        // releasing it together, and a shape the corpus has never produced.
        let stream = counted(120.0, 280, 0, 300.0);
        assert_eq!(classify(&stream), NetworkCondition::UnknownDegradation);
        assert_ne!(
            classify(&stream),
            NetworkCondition::Healthy,
            "a stall every 300 ms must never fall through to well"
        );
    }

    #[test]
    fn loss_decides_before_cadence() {
        // A loss tier that states its own population - datagrams over datagrams -
        // which is what `macos/client` emits from the passive monitor onward.
        // Against a delivery window that would otherwise read healthy, the loss
        // still decides.
        let mut stream = counted(120.0, 0, 0, 0.0);
        stream.loss_ratio = Fraction::new(41, 405_032);
        assert_eq!(classify(&stream), NetworkCondition::SevereLoss);
    }

    #[test]
    fn one_datagram_of_four_hundred_thousand_is_still_loss() {
        // The corpus places the boundary at zero and supports no second level, so
        // this is the stated behaviour rather than an accident of arithmetic.
        // 405032 is the population one committed-adjacent monitor arm reports.
        let mut stream = counted(120.0, 0, 0, 0.0);
        stream.loss_ratio = Fraction::new(1, 405_032);
        assert_eq!(classify(&stream), NetworkCondition::SevereLoss);
    }

    #[test]
    fn a_zero_in_a_real_population_is_a_result_and_an_absent_tier_is_not() {
        // The distinction the whole loss tier turns on. A stated zero over 405032
        // datagrams says the air lost nothing; `None` says nobody counted, and
        // both must leave the cadence to decide without either being mistaken for
        // the other.
        let mut stream = counted(119.982351959, 38, 37, 1573.912575);
        stream.loss_ratio = Fraction::new(0, 405_032);
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
        assert_eq!(
            stream.loss_ratio.map(|ratio| ratio.value()),
            Some(0.0),
            "a zero over a real population has to survive as a measurement"
        );

        stream.loss_ratio = None;
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
        assert_eq!(stream.loss_ratio, None);
    }

    #[test]
    fn reorder_indicts_nothing_and_no_rule_can_read_it() {
        // 4441 datagrams of 14400 access units arrive out of order in every video
        // run in this corpus, with no access unit lost and a reorder wait of
        // 0.12 ms at p99. Dividing those two numbers read 30.8 per cent where the
        // datagram figure is nearer 0.69, which is why it is a bare count with no
        // population and no rule above it.
        let mut stream = counted(119.982351959, 38, 37, 1573.912575);
        stream.reorder = Incidence::Bare(4_441);
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
        assert_eq!(stream.reorder.value(), None);

        // Every datagram reordered, and the verdict does not move.
        stream.reorder = Incidence::Bare(u64::MAX);
        assert_eq!(classify(&stream), NetworkCondition::Healthy);
    }

    #[test]
    fn capacity_pressure_is_unreachable_and_stays_that_way() {
        // A guard on the finding rather than on the code. If a later edit gives
        // `classify` a route to CapacityPressure, it will have done so from a
        // quantity this tier cannot see or from a threshold nothing measured, and
        // this test is where that shows up.
        let mut lossy = counted(120.0, 131, 131, 222.167039);
        lossy.loss_ratio = Fraction::new(2_528, 405_032);
        let shapes = [
            counted(120.0, 0, 0, 0.0),
            counted(120.0, 1, 1, 0.0),
            counted(120.0, 4, 4, 10_007.609343),
            counted(120.0, 131, 131, 222.167039),
            counted(120.0, 120, 120, 1_000.0),
            counted(120.0, 280, 0, 300.0),
            lossy,
        ];
        for shape in shapes {
            assert_ne!(classify(&shape), NetworkCondition::CapacityPressure);
        }
    }
}
