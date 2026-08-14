//! The machine-readable result of a run.
//!
//! `xtask` reads this rather than parsing the human report, so the two cannot
//! drift. Every number here is measured on the client's own clock: the
//! sender's timestamps are not comparable until clock offset estimation
//! lands, and pretending otherwise would turn clock skew into latency.

use serde::Serialize;

#[derive(Serialize)]
pub struct Report {
    pub run: Run,
    pub stream: Stream,
    pub network: Network,
    /// The link's own cadence, independent of everything after it.
    pub delivery: Delivery,
    pub decode: Decode,
    pub display: Display,
    /// What the phase estimator did, which is not derivable from the wait it
    /// was trying to shrink.
    pub phase: Phase,
    pub environment: Environment,
    pub windows: Vec<Window>,
}

#[derive(Serialize)]
pub struct Run {
    /// What the callback counters mean. Under `immediate` they count draw
    /// attempts rather than refreshes, so no rate derived from them is a
    /// display rate.
    pub drive_mode: &'static str,
    pub seconds: f64,
    pub target_fps: f64,
    /// True when something changed underneath the run that makes the
    /// presentation numbers untrustworthy, even if it recovered afterwards.
    pub invalidated: bool,
    pub invalidating_events: Vec<String>,
}

#[derive(Serialize)]
pub struct Stream {
    pub expected: u64,
    pub reconstructed: u64,
    pub packet_loss: u64,
    pub au_loss: u64,
    pub corruption: u64,
    pub reordered: u64,
    /// Furthest ahead of the missing packet that arrivals kept coming.
    pub max_reorder_depth: u32,
    /// How long a visible gap took to fill itself when nothing was lost.
    /// A NACK delay shorter than this asks for packets already in flight,
    /// and the mean hides exactly the tail that decides whether one is
    /// worth building.
    pub reorder_wait_mean_ms: f64,
    pub reorder_wait_p50_ms: f64,
    pub reorder_wait_p99_ms: f64,
    pub reorder_wait_max_ms: f64,
    pub reorder_gaps: u64,
    pub duplicates: u64,
}

#[derive(Serialize)]
pub struct Network {
    pub arrival_p50_ms: f64,
    pub arrival_p95_ms: f64,
    pub arrival_p99_ms: f64,
    pub arrival_max_ms: f64,
    pub rtp_jitter_us: f64,
    /// The DSCP most arriving datagrams carried, and its share. `None` when
    /// the kernel would not report the TOS byte.
    pub observed_dscp: Option<u8>,
    pub observed_dscp_share: f64,
}

/// What the network delivered, measured where delivery happens.
///
/// Kept apart from [`Display`] on purpose. Delivery cadence used to be read
/// off the presentation clock, and a suspended display link then made a
/// healthy link look like one stalling for 141 ms at p99. A stage is
/// measured at that stage or not at all.
#[derive(Serialize)]
pub struct Delivery {
    /// Complete access units the depacketiser handed over.
    pub delivered: u64,
    /// Interval between consecutive complete access units, on the receiver's
    /// own clock. The series a link experiment is ranked by.
    pub au_interval_p50_ms: f64,
    pub au_interval_p95_ms: f64,
    pub au_interval_p99_ms: f64,
    pub au_interval_max_ms: f64,
    /// Interval between the *first* datagram of consecutive access units.
    ///
    /// Compared against the complete-interval above, this says whether late
    /// units started late or merely finished late, which are faults of
    /// different parts of the link.
    pub first_interval_p50_ms: f64,
    pub first_interval_p95_ms: f64,
    pub first_interval_p99_ms: f64,
    pub first_interval_max_ms: f64,
    /// Wall time the delivery series covered, which turns counts into rates.
    pub span_s: f64,
    /// Access units per minute arriving at or beyond each multiple of the
    /// source period. Counted, never inferred from a percentile: a p99 of
    /// 15.92 ms against a 16.67 ms threshold says nothing about how many
    /// units crossed it.
    pub over_1_25t_per_min: f64,
    pub over_1_5t_per_min: f64,
    pub over_2t_per_min: f64,
    pub over_3t_per_min: f64,
    pub over_4t_per_min: f64,
    pub over_6t_per_min: f64,
    /// Stalls beyond two periods that were followed by units arriving inside
    /// one. This is bunching itself, rather than a percentile that bunching
    /// happens to move.
    pub stall_clusters_per_min: f64,
    pub mean_catch_up_units: f64,
    pub max_catch_up_units: u64,
    /// Interval between the starts of consecutive stalls. A tight
    /// distribution indicts a timer; a broad one indicts contention.
    pub stall_gap_p50_ms: f64,
    pub stall_gap_p95_ms: f64,
}

#[derive(Serialize)]
pub struct Decode {
    pub decoded: u64,
    pub errors: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub backlog_slope_per_min: f64,
}

#[derive(Serialize)]
pub struct Display {
    /// What the display is capable of, from the link's own reckoning. Not a
    /// measurement of this run.
    pub nominal_hz: f64,
    /// Callbacks divided by the span they were counted over. The span ends
    /// with the stream rather than with the clock, so dividing `callbacks`
    /// by the run's nominal seconds gives a different and wrong answer.
    pub observed_hz: f64,
    /// Whether the display link ran at a rate that makes anything below it a
    /// measurement.
    ///
    /// `false` means the instrument was not observing: the window was behind
    /// something, the screen slept, the Space changed. Not a failure of the
    /// pipeline - an absence of evidence about it, and the two must never be
    /// reported as the same thing.
    pub cadence_valid: bool,
    pub invalid_reason: Option<String>,
    pub callbacks: u64,
    pub rendered: u64,
    pub superseded: u64,
    pub empty_refreshes: u64,
    pub callback_interval_p50_ms: f64,
    pub callback_interval_p95_ms: f64,
    /// Share of display ticks that had a frame newer than the one shown at
    /// the previous tick.
    ///
    /// The experience metric. Rendered frames per second says how many
    /// pictures were drawn; this says how many of the viewer's refresh
    /// opportunities carried something new, which is what bunching actually
    /// costs: three ticks with nothing followed by one tick where three
    /// frames arrive and two are discarded.
    pub fresh_tick_ratio: f64,
    pub callback_interval_p99_ms: f64,
    pub callback_interval_max_ms: f64,
    /// The client's `local_age`: first local mark to present. Not the sender's
    /// frame age, which needs a synchronised clock.
    pub frame_age_p50_ms: f64,
    pub frame_age_p95_ms: f64,
    pub frame_age_p99_ms: f64,
}

/// The phase estimator's own account of the run.
///
/// A run that sent no shift and a run whose estimator never existed produce the
/// same presentation wait and mean completely different things, so the state is
/// recorded rather than inferred from the counts being zero.
#[derive(Serialize)]
pub struct Phase {
    /// `on`, `observe` or `off`. Three states rather than a flag, because a run
    /// that measured the phase and deliberately sent nothing is neither of the
    /// other two and is the control the comparison needs.
    pub mode: &'static str,
    /// False only for `off`. An observing run has the loop enabled: what it does
    /// not have is a wire.
    pub enabled: bool,
    /// True only when the loop actually observed this run.
    pub ran: bool,
    /// Why it did not, when it was asked for and could not run.
    pub unavailable_reason: Option<String>,
    /// How far in front of the display link's deadline frames ended up being
    /// aimed. Chosen from the jitter the run measured, so a run can only be read
    /// against this rather than against the constant it started from.
    pub margin_ms: f64,
    /// The least it would have aimed for whatever it measured.
    pub margin_floor_ms: f64,
    /// Scatter of the phases the margin was chosen from: the jitter the margin
    /// exists to absorb. Zero when no batch was ever believable, which
    /// `decisions` tells apart from a genuinely steady run.
    pub spread_ms: f64,
    /// Share of the period the measured phase visited, in sixteenths.
    ///
    /// The number that says whether a phase was held or left alone: two
    /// unsynchronised 120 Hz clocks beat through the whole period about every
    /// 33 s, so an untouched run of a few minutes approaches one while a held
    /// phase stays near a sixteenth.
    pub phase_coverage: f64,
    pub samples: u64,
    pub decisions: u64,
    /// Shifts that reached the wire. Zero while observing.
    pub shifts: u64,
    /// Shifts a decision asked for that were deliberately not sent. Non-zero
    /// only while observing, where it is the count of what an acting run of the
    /// same shape would have done.
    pub shifts_withheld: u64,
    /// Decisions that found the phase already where it was aimed.
    pub holds: u64,
    /// Decisions refused for want of evidence.
    pub declined: u64,
    /// The phase of the first believable batch: where the two clocks sat before
    /// anything was asked of them.
    pub first_phase_ms: Option<f64>,
    pub last_phase_ms: Option<f64>,
    pub last_delay_ms: Option<f64>,
    pub last_refusal: Option<String>,
    pub send_errors: u64,
    /// Decisions the series holds, reported separately so a series that stopped
    /// growing cannot be read as a phase that stopped moving.
    pub trace_entries: usize,
    /// Decisions past the series' capacity, and therefore missing from it.
    pub trace_dropped: u64,
    /// What the monotonic `at_ns` below was against the wall clock, sampled once
    /// when this report was written.
    ///
    /// An offset between two bases rather than one clock: the monotonic side
    /// counts through sleep, which is what makes a single pairing valid for the
    /// whole run. It is here so a decision can be lined up with an event in
    /// somebody else's log, which is the only reason to cross at all.
    pub clock_epoch_at_ns: u64,
    pub clock_epoch_unix_ms: f64,
    pub trace: Vec<PhaseSample>,
}

/// One decision from the phase loop.
///
/// The series exists because endpoints cannot settle anything on a link that
/// drifts: two clocks a quarter of a millisecond a second apart sweep two whole
/// periods across seventy seconds, which buries any single step applied in the
/// middle. Reading the phase either side of an event, close enough that the
/// drift between the readings is small against the step, needs every decision
/// rather than the first and the last.
#[derive(Serialize)]
pub struct PhaseSample {
    /// The newest frame the decision was computed from, on the same monotonic
    /// clock as every stage mark in this pipeline.
    pub at_ns: u64,
    /// Seconds from the first traced decision, which is what a reader wants when
    /// lining the series up against its own log.
    pub at_s: f64,
    pub phase_ms: f64,
    /// What the phase was being aimed at when this decision was taken.
    pub margin_ms: f64,
    /// The delay asked for, absent on a decision that asked for nothing.
    pub delay_ms: Option<f64>,
    /// Whether that delay reached the wire. Always false in `observe`.
    pub sent: bool,
}

#[derive(Serialize)]
pub struct Environment {
    pub occlusion_changes: u64,
    pub space_changes: u64,
    pub miniaturise_events: u64,
    pub display_changes: u64,
    pub link_pauses: u64,
    pub app_nap_protection: bool,
}

#[derive(Serialize)]
pub struct Window {
    pub from_s: f64,
    pub to_s: f64,
    /// Callbacks per second. A refresh rate only under `display-link`: in
    /// `immediate` mode a callback is a draw attempt from a loop that spins as
    /// fast as the slot can be read, and the figure runs to thousands. See
    /// `run.drive_mode` before reading this as a display rate.
    pub callback_hz: f64,
    /// Access units reassembled per second: what the link delivered.
    pub source_hz: f64,
    pub decode_hz: f64,
    pub render_hz: f64,
    pub superseded_pct: f64,
    /// Ticks that had something new to show, as a share of the window's
    /// ticks. The per-window form of `fresh_tick_ratio`, and the column to
    /// rank a link configuration by.
    pub fresh_pct: f64,
    /// Interval between complete access units inside the window, measured at
    /// the depacketiser. A cumulative percentile cannot be differenced, so
    /// this is the only place a link stall is visible - and it is taken from
    /// the delivery clock, not the presentation one.
    pub au_interval_p50_ms: f64,
    pub au_interval_p99_ms: f64,
    /// Counted crossings of two source periods inside this window. A run's
    /// median cannot show a bad twenty seconds; this can.
    pub over_2t_per_min: f64,
    /// Access units the host produced in this window that never arrived.
    pub au_loss: u64,
    /// The client's `local_age`, not the sender's frame age.
    pub frame_age_p99_ms: f64,
}
