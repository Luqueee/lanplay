//! The phase 3A gate, as code.
//!
//! Deliberately not "is the delivery delay under N milliseconds". The whole
//! reason phase 3 exists is that nobody knows what those numbers are yet, and
//! a threshold invented before the measurement would either be met by
//! construction or fail for reasons the measurement is supposed to explain.
//!
//! What is gated is the set of things that are wrong at *any* value: a backlog
//! that grows, a timestamp that goes backwards, resident memory that climbs, a
//! stall the capture never comes back from, a frame that reached the CPU. Each
//! of those ends the same way regardless of how fast the first thousand frames
//! were, and none of them needs a number chosen in advance.
//!
//! Every input is a field of [`RunReport`], so the verdict can be recomputed
//! from the published JSON by anyone who doubts it.

use lanplay_telemetry::P99_SOAK_FRAMES;

use crate::report::{CheckReport, GateReport, RunReport};

/// Share of the source rate the capture must actually deliver. Two percent of
/// slack covers the partial period at each end of the window, not a dropped
/// frame: at 100 Hz over a minute this still fails on the 121st missing frame.
const MIN_CADENCE_FRACTION: f64 = 0.98;
/// Growth in queued source frames that counts as accumulation, per minute.
/// Same figure the phase 2 gate uses for the decoder queue, because it is the
/// same failure: a queue that gains half a frame a minute is a queue that ends
/// the run somewhere it did not start.
const MAX_BACKLOG_GROWTH: f64 = 0.5;
/// Backlog the API may still be holding at the end before it counts as real
/// rather than transient.
const MAX_TRAILING_BACKLOG: f64 = 2.0;
/// Resident memory growth that counts as a leak, in bytes per minute.
const MAX_MEMORY_GROWTH: f64 = 1_048_576.0;
/// How far past its own configured timeout a single acquire may run. An API
/// that blocks for twice what it was asked to wait is misbehaving at any
/// latency; this is a promise the caller made to the API, not a number chosen
/// for this benchmark.
const ACQUIRE_TIMEOUT_SLACK: f64 = 2.0;
/// Growth in pool starvation events per minute that counts as the pool being
/// too small for the rate rather than occasionally unlucky.
const MAX_STARVATION_GROWTH: f64 = 0.5;
/// Share of copies that must come back with a GPU timestamp before the copy
/// numbers describe the run rather than a subset of it. Not a performance
/// threshold: a measurement-validity one.
const MIN_GPU_RESULT_FRACTION: f64 = 0.90;

#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Verdict {
    pub checks: Vec<Check>,
    /// Checks nothing in the run exercised. Their pass is not evidence and is
    /// named here so a reader does not read it as such.
    pub untested: Vec<String>,
    /// True when the run saw enough frames for its tail numbers to be quoted.
    pub soaked: bool,
}

impl Verdict {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }

    pub fn to_report(&self) -> GateReport {
        GateReport {
            passed: self.passed(),
            soaked: self.soaked,
            checks: self
                .checks
                .iter()
                .map(|check| CheckReport {
                    name: check.name.to_owned(),
                    passed: check.passed,
                    detail: check.detail.clone(),
                })
                .collect(),
            untested: self.untested.clone(),
        }
    }
}

pub fn evaluate(report: &RunReport) -> Verdict {
    let mut checks = Vec::new();
    let mut untested = Vec::new();

    let capture = &report.capture;
    let stability = &report.stability;

    // 1. The cadence has to be the source's cadence. Everything else in the
    //    report is conditional on the capture having actually kept up.
    let delivered = if capture.expected_frames > 0.0 {
        capture.frames as f64 / capture.expected_frames
    } else {
        0.0
    };
    checks.push(Check {
        name: "capture cadence",
        passed: delivered >= MIN_CADENCE_FRACTION,
        detail: format!(
            "{} frames in {:.2} s = {:.2}/s against a {:.3} Hz source ({:.1}% of {:.0} expected)",
            capture.frames,
            capture.window_s,
            capture.frames_per_second,
            report.config.source_hz,
            delivered * 100.0,
            capture.expected_frames
        ),
    });

    // 2. A backlog that grows is a failure at any average rate, because it
    //    only ends one way. Both APIs report their own: AccumulatedFrames for
    //    Desktop Duplication, pool depth for WGC.
    let backlog_ok = match stability.backlog_slope_per_min {
        Some(slope) => {
            slope <= MAX_BACKLOG_GROWTH && stability.backlog_trailing <= MAX_TRAILING_BACKLOG
        }
        None => false,
    };
    checks.push(Check {
        name: "no growing backlog",
        passed: backlog_ok,
        detail: match stability.backlog_slope_per_min {
            Some(slope) => format!(
                "peak {:.2}, {:.2} queued at exit, growth {slope:+.3}/min over {} samples",
                stability.backlog_peak, stability.backlog_trailing, stability.backlog_samples
            ),
            None => format!(
                "growth unmeasured: {} samples, which is not enough to fit a line",
                stability.backlog_samples
            ),
        },
    });

    // 3. Nothing may reach system memory. A single Map would make every
    //    latency in this report a measurement of the PCIe bus.
    let no_readback = stability.mapped_bytes == 0 && !stability.pool_cpu_accessible;
    checks.push(Check {
        name: "no CPU readback",
        passed: no_readback,
        detail: format!(
            "{} bytes mapped; pool textures {}",
            stability.mapped_bytes,
            if stability.pool_cpu_accessible {
                "are CPU-accessible, so a readback is possible behind this check"
            } else {
                "are GPU-only (CPUAccessFlags 0, USAGE_DEFAULT)"
            }
        ),
    });

    // 4. Resident memory over the steady-state window. A surface leaked per
    //    frame is invisible in any single sample and fatal over an evening.
    let memory_ok = report
        .system
        .memory_slope_bytes_per_min
        .is_some_and(|slope| slope <= MAX_MEMORY_GROWTH);
    checks.push(Check {
        name: "memory slope flat",
        passed: memory_ok,
        detail: match report.system.memory_slope_bytes_per_min {
            Some(slope) => format!(
                "{:+.0} bytes/min over {} samples, {} resident at exit",
                slope,
                report.system.memory_samples,
                report
                    .system
                    .working_set_bytes
                    .map(|bytes| format!("{:.1} MB", bytes as f64 / 1_048_576.0))
                    .unwrap_or_else(|| "unknown".to_owned())
            ),
            None => format!(
                "unmeasured: {} samples of resident memory",
                report.system.memory_samples
            ),
        },
    });

    // 5. A source mark that goes backwards makes every delivery delay computed
    //    from it meaningless, so the tolerance is zero rather than small.
    let monotonic =
        stability.source_timestamp_regressions == 0 && stability.acquire_timestamp_regressions == 0;
    checks.push(Check {
        name: "timestamps monotonic",
        passed: monotonic,
        detail: format!(
            "{} source-mark regressions (worst {:.3} ms back), {} acquire-clock regressions",
            stability.source_timestamp_regressions,
            stability.source_regression_worst_ms,
            stability.acquire_timestamp_regressions
        ),
    });

    // 6. Loss is expected, not exceptional: a mode change or a fullscreen
    //    transition invalidates the capture. What must not happen is a restart
    //    that fails or a restart after which no frame ever arrives.
    if stability.access_lost == 0 {
        untested.push(
            "recovery from Acquired::Lost: no loss occurred, so restart was never exercised"
                .to_owned(),
        );
    }
    let recovery_ok = stability.restart_failures == 0
        && (stability.access_lost == 0 || stability.frames_after_last_restart > 0);
    checks.push(Check {
        name: "recovers from Lost",
        passed: recovery_ok,
        detail: format!(
            "{} losses, {} restarts, {} failures, {} frames after the last restart",
            stability.access_lost,
            stability.api_resets,
            stability.restart_failures,
            stability.frames_after_last_restart
        ),
    });

    // 7. The API was asked to wait a bounded time. Blocking for twice that is
    //    the API breaking its own contract, whatever the latency is.
    let ceiling = report.config.acquire_timeout_ms as f64 * ACQUIRE_TIMEOUT_SLACK;
    checks.push(Check {
        name: "acquire honours timeout",
        passed: capture.acquire.max_ms <= ceiling,
        detail: format!(
            "worst acquire {:.3} ms against a {} ms timeout (ceiling {ceiling:.0} ms), {} timeouts",
            capture.acquire.max_ms, report.config.acquire_timeout_ms, capture.timeouts
        ),
    });

    // 8. The deliberate stall. Falling behind is allowed; not coming back is
    //    not, and phase 7 needs to know which of the two the API does.
    match &report.injected_stall {
        Some(stall) => checks.push(Check {
            name: "recovers from stall",
            passed: stall.recovered,
            detail: format!(
                "{:.0} ms stall, first acquire after it {:.3} ms, {} accumulated, back on cadence \
                 after {}",
                stall.actual_ms,
                stall.first_acquire_ms,
                stall
                    .first_frame_accumulated
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "n/a".to_owned()),
                stall
                    .frames_to_recover
                    .map(|frames| format!("{frames} frames"))
                    .unwrap_or_else(|| "never".to_owned())
            ),
        }),
        None => untested.push(
            "recovery from a consumer stall: --stall-ms was not set, so it was never provoked"
                .to_owned(),
        ),
    }

    // 9 and 10. Handoff only, and both are about the pool and the measurement
    //    rather than about speed.
    if let Some(handoff) = &report.handoff {
        let starvation_ok = handoff.pool_starvation == 0
            || handoff
                .pool_starvation_slope_per_min
                .is_some_and(|slope| slope <= MAX_STARVATION_GROWTH);
        checks.push(Check {
            name: "pool keeps up",
            passed: starvation_ok,
            detail: format!(
                "{} starvations of a {}-slot pool, growth {}",
                handoff.pool_starvation,
                handoff.pool_size,
                handoff
                    .pool_starvation_slope_per_min
                    .map(|slope| format!("{slope:+.3}/min"))
                    .unwrap_or_else(|| "unmeasured".to_owned())
            ),
        });

        checks.push(Check {
            name: "gpu copy measured",
            passed: handoff.copies == 0 || handoff.gpu_result_fraction >= MIN_GPU_RESULT_FRACTION,
            detail: format!(
                "{} of {} copies returned a GPU timestamp ({:.1}%); {} found no free query, {} \
                 disjoint, {} unresolved at exit",
                handoff.queries_resolved,
                handoff.copies,
                handoff.gpu_result_fraction * 100.0,
                handoff.queries_slot_exhausted,
                handoff.queries_disjoint_discarded,
                handoff.queries_unresolved_at_exit
            ),
        });
    }

    Verdict {
        checks,
        untested,
        soaked: capture.frames >= P99_SOAK_FRAMES,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{HandoffReport, InjectedStallReport};
    use crate::series::Summary;

    /// A run in which nothing went wrong. Each test breaks exactly one thing,
    /// so a check that stops discriminating shows up as a test that stops
    /// failing.
    fn healthy() -> RunReport {
        let mut report = RunReport::new("native", "dda");
        report.config.source_hz = 100.0;
        report.capture.window_s = 60.0;
        report.capture.frames = 6_000;
        report.capture.frames_per_second = 100.0;
        report.capture.expected_frames = 6_000.0;
        report.capture.acquire = Summary {
            count: 6_000,
            max_ms: 11.0,
            ..Summary::default()
        };
        report.config.acquire_timeout_ms = 100;
        report.stability.backlog_slope_per_min = Some(0.0);
        report.stability.backlog_samples = 240;
        report.stability.backlog_trailing = 1.0;
        report.system.memory_slope_bytes_per_min = Some(0.0);
        report.system.memory_samples = 240;
        report
    }

    fn check<'a>(verdict: &'a Verdict, name: &str) -> &'a Check {
        verdict
            .checks
            .iter()
            .find(|check| check.name == name)
            .expect("check present")
    }

    /// Names what broke, so a failing assertion says which check rather
    /// than only that one did.
    fn failures(verdict: &Verdict) -> String {
        verdict
            .checks
            .iter()
            .filter(|check| !check.passed)
            .map(|check| format!("{}: {}", check.name, check.detail))
            .collect::<Vec<_>>()
            .join("; ")
    }

    #[test]
    fn a_healthy_run_passes_every_check() {
        let verdict = evaluate(&healthy());
        assert!(verdict.passed(), "{}", failures(&verdict));
        assert!(verdict.soaked);
    }

    #[test]
    fn every_check_carries_a_detail_even_when_it_passes() {
        // A passing check with no number is not evidence.
        let verdict = evaluate(&healthy());
        assert!(verdict.checks.iter().all(|check| !check.detail.is_empty()));
    }

    #[test]
    fn losing_two_percent_of_the_frames_fails_the_cadence() {
        let mut report = healthy();
        report.capture.frames = 5_800;
        report.capture.frames_per_second = 96.7;
        let verdict = evaluate(&report);
        assert!(!check(&verdict, "capture cadence").passed);
        assert!(!verdict.passed());
    }

    #[test]
    fn the_cadence_is_judged_against_the_measured_rate_not_a_constant() {
        // 100 frames/s is a pass at 100 Hz and a failure at 120. Nothing in
        // the gate may assume either.
        let mut report = healthy();
        report.config.source_hz = 120.0;
        report.capture.expected_frames = 7_200.0;
        assert!(!check(&evaluate(&report), "capture cadence").passed);
    }

    #[test]
    fn a_backlog_that_grows_fails_even_at_the_right_rate() {
        let mut report = healthy();
        report.stability.backlog_slope_per_min = Some(1.0);
        let verdict = evaluate(&report);
        assert!(!check(&verdict, "no growing backlog").passed);
        assert!(
            check(&verdict, "capture cadence").passed,
            "the rate was fine; that is the point"
        );
    }

    #[test]
    fn a_backlog_left_behind_at_exit_fails_even_with_a_flat_slope() {
        let mut report = healthy();
        report.stability.backlog_trailing = 9.0;
        assert!(!check(&evaluate(&report), "no growing backlog").passed);
    }

    #[test]
    fn an_unmeasured_slope_is_not_a_pass() {
        let mut report = healthy();
        report.stability.backlog_slope_per_min = None;
        report.stability.backlog_samples = 1;
        let verdict = evaluate(&report);
        assert!(!check(&verdict, "no growing backlog").passed);
        assert!(
            check(&verdict, "no growing backlog")
                .detail
                .contains("unmeasured")
        );
    }

    #[test]
    fn a_single_mapped_byte_fails_the_readback_check() {
        let mut report = healthy();
        report.stability.mapped_bytes = 1;
        assert!(!check(&evaluate(&report), "no CPU readback").passed);
    }

    #[test]
    fn a_cpu_accessible_pool_fails_even_with_nothing_mapped() {
        let mut report = healthy();
        report.stability.pool_cpu_accessible = true;
        assert!(!check(&evaluate(&report), "no CPU readback").passed);
    }

    #[test]
    fn a_climbing_memory_slope_fails() {
        let mut report = healthy();
        report.system.memory_slope_bytes_per_min = Some(4.0 * 1_048_576.0);
        assert!(!check(&evaluate(&report), "memory slope flat").passed);
    }

    #[test]
    fn memory_returned_to_the_os_is_not_a_leak() {
        let mut report = healthy();
        report.system.memory_slope_bytes_per_min = Some(-8.0 * 1_048_576.0);
        assert!(check(&evaluate(&report), "memory slope flat").passed);
    }

    #[test]
    fn one_backwards_timestamp_fails() {
        let mut report = healthy();
        report.stability.source_timestamp_regressions = 1;
        assert!(!check(&evaluate(&report), "timestamps monotonic").passed);
    }

    #[test]
    fn a_backwards_acquire_clock_fails_too() {
        let mut report = healthy();
        report.stability.acquire_timestamp_regressions = 1;
        assert!(!check(&evaluate(&report), "timestamps monotonic").passed);
    }

    #[test]
    fn a_run_without_loss_reports_recovery_as_untested() {
        let verdict = evaluate(&healthy());
        assert!(check(&verdict, "recovers from Lost").passed);
        assert!(
            verdict
                .untested
                .iter()
                .any(|note| note.contains("Acquired::Lost")),
            "a check nothing exercised must not read as evidence"
        );
    }

    #[test]
    fn loss_followed_by_frames_is_a_recovery() {
        let mut report = healthy();
        report.stability.access_lost = 3;
        report.stability.api_resets = 3;
        report.stability.frames_after_last_restart = 900;
        let verdict = evaluate(&report);
        assert!(check(&verdict, "recovers from Lost").passed);
        assert!(
            verdict.untested.is_empty()
                || !verdict
                    .untested
                    .iter()
                    .any(|note| note.contains("Acquired::Lost"))
        );
    }

    #[test]
    fn loss_with_no_frames_afterwards_is_not_a_recovery() {
        let mut report = healthy();
        report.stability.access_lost = 1;
        report.stability.api_resets = 1;
        report.stability.frames_after_last_restart = 0;
        assert!(!check(&evaluate(&report), "recovers from Lost").passed);
    }

    #[test]
    fn a_restart_that_errored_fails_even_if_frames_resumed() {
        let mut report = healthy();
        report.stability.access_lost = 2;
        report.stability.api_resets = 2;
        report.stability.restart_failures = 1;
        report.stability.frames_after_last_restart = 500;
        assert!(!check(&evaluate(&report), "recovers from Lost").passed);
    }

    #[test]
    fn an_acquire_that_outlasts_twice_its_timeout_fails() {
        let mut report = healthy();
        report.capture.acquire.max_ms = 201.0;
        assert!(!check(&evaluate(&report), "acquire honours timeout").passed);
    }

    #[test]
    fn the_timeout_ceiling_follows_the_configured_timeout() {
        let mut report = healthy();
        report.capture.acquire.max_ms = 201.0;
        report.config.acquire_timeout_ms = 200;
        assert!(check(&evaluate(&report), "acquire honours timeout").passed);
    }

    #[test]
    fn an_uninjected_stall_adds_no_check_and_one_note() {
        let verdict = evaluate(&healthy());
        assert!(
            !verdict
                .checks
                .iter()
                .any(|check| check.name == "recovers from stall")
        );
        assert!(
            verdict
                .untested
                .iter()
                .any(|note| note.contains("--stall-ms"))
        );
    }

    #[test]
    fn a_stall_that_never_recovers_fails() {
        let mut report = healthy();
        report.injected_stall = Some(InjectedStallReport {
            requested_ms: 500,
            actual_ms: 500.4,
            recovered: false,
            frames_to_recover: None,
            ..InjectedStallReport::default()
        });
        let verdict = evaluate(&report);
        assert!(!check(&verdict, "recovers from stall").passed);
        assert!(
            check(&verdict, "recovers from stall")
                .detail
                .contains("never")
        );
    }

    #[test]
    fn a_stall_the_capture_comes_back_from_passes() {
        let mut report = healthy();
        report.injected_stall = Some(InjectedStallReport {
            requested_ms: 500,
            actual_ms: 500.4,
            recovered: true,
            frames_to_recover: Some(2),
            ..InjectedStallReport::default()
        });
        assert!(check(&evaluate(&report), "recovers from stall").passed);
    }

    fn with_handoff(copies: u64, resolved: u64, starvation: u64, slope: Option<f64>) -> RunReport {
        let mut report = healthy();
        report.mode = "handoff".to_owned();
        report.handoff = Some(HandoffReport {
            pool_size: 3,
            copies,
            queries_resolved: resolved,
            pool_starvation: starvation,
            pool_starvation_slope_per_min: slope,
            gpu_result_fraction: if copies == 0 {
                0.0
            } else {
                resolved as f64 / copies as f64
            },
            ..HandoffReport::default()
        });
        report
    }

    #[test]
    fn native_runs_have_no_handoff_checks() {
        let verdict = evaluate(&healthy());
        assert!(
            !verdict
                .checks
                .iter()
                .any(|check| check.name == "pool keeps up")
        );
        assert!(
            !verdict
                .checks
                .iter()
                .any(|check| check.name == "gpu copy measured")
        );
    }

    #[test]
    fn occasional_starvation_that_does_not_grow_passes() {
        let verdict = evaluate(&with_handoff(6_000, 6_000, 4, Some(0.1)));
        assert!(check(&verdict, "pool keeps up").passed);
    }

    #[test]
    fn starvation_that_grows_fails() {
        let verdict = evaluate(&with_handoff(6_000, 6_000, 400, Some(9.0)));
        assert!(!check(&verdict, "pool keeps up").passed);
    }

    #[test]
    fn copy_numbers_from_a_tenth_of_the_copies_are_not_evidence() {
        let verdict = evaluate(&with_handoff(6_000, 600, 0, Some(0.0)));
        assert!(!check(&verdict, "gpu copy measured").passed);
        assert!(!verdict.passed());
    }

    #[test]
    fn a_handful_of_unresolved_queries_is_tolerated() {
        let verdict = evaluate(&with_handoff(6_000, 5_950, 0, Some(0.0)));
        assert!(check(&verdict, "gpu copy measured").passed);
        assert!(verdict.passed(), "{}", failures(&verdict));
    }

    #[test]
    fn a_short_run_is_reported_as_unsoaked() {
        let mut report = healthy();
        report.capture.frames = P99_SOAK_FRAMES - 1;
        report.capture.expected_frames = (P99_SOAK_FRAMES - 1) as f64;
        let verdict = evaluate(&report);
        assert!(!verdict.soaked);
        assert!(verdict.passed(), "an unsoaked run is not a failed one");
    }

    #[test]
    fn the_report_form_of_the_verdict_carries_every_check() {
        let verdict = evaluate(&healthy());
        let report = verdict.to_report();
        assert_eq!(report.checks.len(), verdict.checks.len());
        assert_eq!(report.passed, verdict.passed());
        assert!(report.checks.iter().all(|check| !check.name.is_empty()));
    }
}
