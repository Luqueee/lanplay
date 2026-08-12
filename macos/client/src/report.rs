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
    pub nominal_hz: f64,
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
    /// The client's `local_age`, not the sender's frame age.
    pub frame_age_p99_ms: f64,
}
