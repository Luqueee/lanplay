//! The evidence, as plain data.
//!
//! Everything downstream of the capture loop reads this and nothing else: the
//! human block, the JSON file and the gate all take a `RunReport`. That is
//! deliberate. A gate that consulted state the report does not contain could
//! pass a run whose published numbers say it failed, and a reader could not
//! check the verdict from the file. It also makes the gate a pure function of
//! plain numbers, so it is testable without a GPU.
//!
//! No windows types appear here, which is why the module compiles everywhere.

use serde::Serialize;

use crate::series::{Distribution, Summary};

pub const SCHEMA: &str = "lanplay.capture-bench/1";

/// Which output produced these numbers, and at what mode.
///
/// A capture benchmark that does not say which GPU and driver produced it is a
/// number without a subject, and one that does not say the refresh rate cannot
/// be checked against its own cadence claim.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DeviceReport {
    pub adapter: String,
    pub luid: i64,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_vram_mb: u64,
    pub feature_level: String,
    pub output: String,
    pub output_width: u32,
    pub output_height: u32,
    /// Current mode's refresh rate, as the exact rational the display reports.
    pub refresh_numerator: u32,
    pub refresh_denominator: u32,
    pub refresh_hz: f64,
    /// How the rate above was obtained: which of the fallbacks answered.
    pub refresh_source: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ConfigReport {
    pub seconds: f64,
    pub warmup_seconds: f64,
    pub buffers: u32,
    pub output: u32,
    pub acquire_timeout_ms: u32,
    pub cursor: bool,
    /// The rate the cadence check and the stall classifier judge against.
    pub source_hz: f64,
    /// True when `--source-hz` overrode the detected display rate.
    pub source_hz_overridden: bool,
    pub stall_ms: u64,
    pub pool: Option<u32>,
    pub hold_ms: Option<f64>,
    pub seed: Option<u64>,
    pub block_seconds: Option<f64>,
}

/// What the run cost before it was allowed to count.
///
/// Reported rather than discarded: device creation and the first allocation
/// are real costs the product will pay once, and a phase that threw them away
/// entirely could not say how long a capture takes to come up.
#[derive(Clone, Debug, Default, Serialize)]
pub struct StartupReport {
    pub device_open_ms: f64,
    pub backend_start_ms: f64,
    pub pool_create_ms: Option<f64>,
    /// Backend start to the first frame in hand.
    pub first_frame_ms: Option<f64>,
    pub warmup_frames: u64,
    pub warmup_seconds: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CaptureReport {
    pub window_s: f64,
    pub frames: u64,
    pub frames_per_second: f64,
    /// What the source rate implies for this window.
    pub expected_frames: f64,
    pub timeouts: u64,
    pub duplicates: u64,
    pub superseded: u64,
    /// WGC only: frames the pool discarded because we were not draining fast
    /// enough.
    pub drained: u64,
    /// Which event the source clock marked, in the API's own terms.
    pub source_mark: String,
    /// Source mark to acquire return, per frame.
    pub delivery_delay: Summary,
    /// Frames whose source mark could not be used: Desktop Duplication reports
    /// a zero `LastPresentTime` for a cursor-only update.
    pub delivery_delay_unusable: u64,
    /// Time inside the backend's `acquire`, for acquires that returned a frame.
    pub acquire: Summary,
    /// Between consecutive acquires that returned a frame.
    pub interval: Summary,
    /// Desktop Duplication's `AccumulatedFrames`.
    pub accumulated_frames: Distribution,
    /// WGC frame pool depth at the moment of the acquire.
    pub pending_frames: Distribution,
}

/// What ownership cost.
///
/// Three copy numbers with three different names, because they are three
/// different things and conflating them is the specific error this benchmark
/// exists to avoid: `CopyResource` returning on the CPU says nothing about the
/// GPU having done the work.
#[derive(Clone, Debug, Default, Serialize)]
pub struct HandoffReport {
    pub pool_size: u32,
    pub hold_ms: f64,
    pub copies: u64,
    /// CPU time inside `CopyResource`. Submission cost, not copy cost.
    pub copy_submit_cpu: Summary,
    /// GPU execution time of the copy, from a `D3D11_QUERY_TIMESTAMP` pair
    /// bracketing it inside a `D3D11_QUERY_TIMESTAMP_DISJOINT`. The real
    /// number.
    pub copy_gpu: Summary,
    /// Wall time from submitting the copy to the first poll that found the
    /// query ready. An upper bound: the loop polls once per iteration, so this
    /// is quantised by the capture period and is not a GPU measurement.
    pub copy_completion_observed: Summary,
    /// Acquire return to the release of the source, which happens at the head
    /// of the next acquire.
    pub source_hold: Summary,
    pub pool_starvation: u64,
    pub pool_starvation_slope_per_min: Option<f64>,
    /// Times the owned pool had to be rebuilt because the output changed
    /// size under it. Non-zero means part of the run measured a different
    /// resolution from the rest.
    pub owned_pool_rebuilds: u64,
    pub queries_resolved: u64,
    /// Copies made without a query because every query set was still in
    /// flight. Counted, never waited for.
    pub queries_slot_exhausted: u64,
    /// Results thrown away because the GPU reported the timestamps unreliable
    /// across the interval.
    pub queries_disjoint_discarded: u64,
    /// Queries still not ready when the run ended. Never flushed to force
    /// them: a `Flush` here would measure a pipeline the product must not have.
    pub queries_unresolved_at_exit: u64,
    /// `queries_resolved / copies`. Below one, the copy GPU numbers describe a
    /// sample of the copies rather than all of them, and the gate says so.
    pub gpu_result_fraction: f64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SystemReport {
    pub process_cpu_percent: Option<f64>,
    pub working_set_bytes: Option<u64>,
    pub working_set_start_bytes: Option<u64>,
    pub memory_slope_bytes_per_min: Option<f64>,
    pub memory_samples: usize,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StabilityReport {
    /// `Acquired::Lost` answers, each of which cost a `restart`.
    pub access_lost: u64,
    pub api_resets: u64,
    pub restart_failures: u64,
    pub frames_after_last_restart: u64,
    pub intervals_over_1x: u64,
    pub intervals_over_2x: u64,
    pub max_interval_ms: f64,
    pub period_ms: f64,
    pub pool_recreations: u64,
    /// WGC only: false when the OS forced the capture border into the frames,
    /// which is content and would pollute a pixel comparison.
    pub border_suppressed: Option<bool>,
    pub source_timestamp_regressions: u64,
    pub source_regression_worst_ms: f64,
    pub acquire_timestamp_regressions: u64,
    pub backlog_slope_per_min: Option<f64>,
    pub backlog_samples: usize,
    pub backlog_peak: f64,
    pub backlog_trailing: f64,
    /// True if any pool texture was created CPU-accessible, which would make a
    /// readback possible behind the harness's back.
    pub pool_cpu_accessible: bool,
    /// Bytes the loop moved to system memory. Structurally zero: nothing here
    /// maps a resource or copies to a staging texture.
    pub mapped_bytes: u64,
}

/// What the API did when the consumer deliberately stopped consuming.
///
/// Phase 7 has to decide what the streamer does when the encoder falls behind,
/// and the two APIs behave differently: one accumulates and tells you, the
/// other drops and recycles. Measuring it now is cheaper than guessing later.
#[derive(Clone, Debug, Default, Serialize)]
pub struct InjectedStallReport {
    pub requested_ms: u64,
    pub actual_ms: f64,
    /// How long the first acquire after the stall took.
    pub first_acquire_ms: f64,
    /// How stale the first frame after the stall was.
    pub first_frame_delivery_delay_ms: Option<f64>,
    /// Desktop Duplication's count of what piled up while we were away.
    pub first_frame_accumulated: Option<u32>,
    /// Frames until the interval came back inside one source period.
    pub frames_to_recover: Option<u64>,
    pub recovered: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CheckReport {
    pub name: String,
    pub passed: bool,
    /// What was measured and what it came to. Present whether the check passed
    /// or not: a passing check with no number is not evidence.
    pub detail: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct GateReport {
    pub passed: bool,
    /// True when the run saw enough frames for its tail numbers to be quoted.
    pub soaked: bool,
    pub checks: Vec<CheckReport>,
    /// Checks that could not be exercised, named so a reader does not read
    /// their pass as evidence.
    pub untested: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RunReport {
    pub schema: String,
    /// `native`, `handoff`, or the same for a compare block.
    pub mode: String,
    pub backend: String,
    /// The backend's own name for itself, which is not the CLI's short name.
    pub backend_api: String,
    pub device: DeviceReport,
    pub config: ConfigReport,
    pub startup: StartupReport,
    pub capture: CaptureReport,
    pub handoff: Option<HandoffReport>,
    pub system: SystemReport,
    pub stability: StabilityReport,
    pub injected_stall: Option<InjectedStallReport>,
    pub gate: Option<GateReport>,
}

impl RunReport {
    pub fn new(mode: &str, backend: &str) -> Self {
        RunReport {
            schema: SCHEMA.to_owned(),
            mode: mode.to_owned(),
            backend: backend.to_owned(),
            ..RunReport::default()
        }
    }
}

/// One backend's slice of one alternating block.
#[derive(Clone, Debug, Default, Serialize)]
pub struct BlockReport {
    pub index: usize,
    pub backend: String,
    pub seconds: f64,
    pub frames: u64,
    pub frames_per_second: f64,
    pub delivery_delay: Summary,
    pub acquire: Summary,
    pub interval: Summary,
    pub intervals_over_2x: u64,
    pub access_lost: u64,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct CompareReport {
    pub schema: String,
    pub mode: String,
    pub device: DeviceReport,
    pub config: ConfigReport,
    /// The order the blocks actually ran in, starting backend included.
    pub blocks: Vec<BlockReport>,
    pub wgc: RunReport,
    pub dda: RunReport,
}

impl CompareReport {
    pub fn new() -> Self {
        CompareReport {
            schema: SCHEMA.to_owned(),
            mode: "compare".to_owned(),
            ..CompareReport::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON is the artefact the phase 3 decision gets argued from, and the
    /// gate is a pure function of it. If a section stops being emitted, the
    /// verdict stops being checkable from the file.
    #[test]
    fn a_run_report_serialises_every_section_the_gate_reads() {
        let value: serde_json::Value =
            serde_json::to_value(RunReport::new("native", "wgc")).expect("serialises");
        let object = value.as_object().expect("an object");

        for key in [
            "schema",
            "mode",
            "backend",
            "backend_api",
            "device",
            "config",
            "startup",
            "capture",
            "handoff",
            "system",
            "stability",
            "injected_stall",
            "gate",
        ] {
            assert!(object.contains_key(key), "{key} missing from the report");
        }
        assert_eq!(object["schema"], SCHEMA);
        // Absent rather than zeroed: a native run owns no textures and a run
        // with no injected stall provoked nothing.
        assert!(object["handoff"].is_null());
        assert!(object["injected_stall"].is_null());
    }

    #[test]
    fn the_period_the_cadence_was_judged_against_is_in_the_file() {
        // A cadence number with no denominator cannot be re-checked, so both
        // the detected mode and any override have to survive into the JSON.
        let value = serde_json::to_value(RunReport::new("native", "dda")).expect("serialises");
        assert!(value["config"].get("source_hz").is_some());
        assert!(value["config"].get("source_hz_overridden").is_some());
        assert!(value["device"].get("refresh_numerator").is_some());
        assert!(value["device"].get("refresh_denominator").is_some());
        assert!(value["device"].get("refresh_source").is_some());
        assert!(value["stability"].get("period_ms").is_some());
    }

    #[test]
    fn the_three_copy_numbers_are_three_separate_keys() {
        let mut report = RunReport::new("handoff", "wgc");
        report.handoff = Some(HandoffReport::default());
        let value = serde_json::to_value(report).expect("serialises");
        let handoff = &value["handoff"];
        assert!(handoff.get("copy_submit_cpu").is_some());
        assert!(handoff.get("copy_gpu").is_some());
        assert!(handoff.get("copy_completion_observed").is_some());
    }

    #[test]
    fn a_compare_report_carries_both_runs_and_the_order_they_ran_in() {
        let value = serde_json::to_value(CompareReport::new()).expect("serialises");
        assert_eq!(value["mode"], "compare");
        for key in ["schema", "device", "config", "blocks", "wgc", "dda"] {
            assert!(value.get(key).is_some(), "{key} missing");
        }
    }
}
