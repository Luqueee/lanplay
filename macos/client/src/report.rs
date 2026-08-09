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
    pub decode: Decode,
    pub display: Display,
    pub environment: Environment,
    pub windows: Vec<Window>,
}

#[derive(Serialize)]
pub struct Run {
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
    pub duplicates: u64,
}

#[derive(Serialize)]
pub struct Network {
    pub arrival_p50_ms: f64,
    pub arrival_p95_ms: f64,
    pub arrival_p99_ms: f64,
    pub arrival_max_ms: f64,
    pub rtp_jitter_us: f64,
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
    pub callbacks: u64,
    pub rendered: u64,
    pub superseded: u64,
    pub empty_refreshes: u64,
    pub callback_interval_p50_ms: f64,
    pub callback_interval_p95_ms: f64,
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
    pub callback_hz: f64,
    pub render_hz: f64,
    pub superseded_pct: f64,
    pub frame_age_p99_ms: f64,
}
