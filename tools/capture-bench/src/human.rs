//! The block a person reads.
//!
//! Rendered from [`RunReport`] and nothing else, so the printed block and the
//! JSON can never disagree about what the run did.

use core::fmt::{self, Write};

use crate::report::{BlockReport, CompareReport, RunReport};
use crate::series::{Distribution, Summary};

const RULE: &str = "--------------------------------------------------------------------------";

pub fn run_block(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    header(out, report)?;
    startup(out, report)?;
    capture(out, report)?;
    handoff(out, report)?;
    stall(out, report)?;
    system(out, report)?;
    stability(out, report)?;
    gate(out, report)
}

fn header(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let device = &report.device;
    writeln!(out, "{RULE}")?;
    writeln!(
        out,
        "capture-bench {} :: {} ({})",
        report.mode, report.backend, report.backend_api
    )?;
    writeln!(out, "{}", device.description)?;
    writeln!(
        out,
        "mode        {}x{} @ {:.3} Hz ({}/{} from {})",
        device.output_width,
        device.output_height,
        device.refresh_hz,
        device.refresh_numerator,
        device.refresh_denominator,
        device.refresh_source
    )?;
    let config = &report.config;
    writeln!(
        out,
        "judged at   {:.3} Hz{}  |  {:.1} s measured after {:.1} s warm-up  |  {} buffers, {} ms \
         acquire timeout",
        config.source_hz,
        if config.source_hz_overridden {
            " (--source-hz override)"
        } else {
            " (detected)"
        },
        config.seconds,
        config.warmup_seconds,
        config.buffers,
        config.acquire_timeout_ms
    )?;
    writeln!(out, "{RULE}")
}

fn startup(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let startup = &report.startup;
    writeln!(out, "STARTUP  (excluded from every number below)")?;
    writeln!(
        out,
        "  device open           {:>9.3} ms",
        startup.device_open_ms
    )?;
    writeln!(
        out,
        "  backend start         {:>9.3} ms",
        startup.backend_start_ms
    )?;
    if let Some(pool) = startup.pool_create_ms {
        writeln!(out, "  pool creation         {pool:>9.3} ms")?;
    }
    match startup.first_frame_ms {
        Some(first) => writeln!(out, "  start to first frame  {first:>9.3} ms")?,
        None => writeln!(out, "  start to first frame        never")?,
    }
    writeln!(
        out,
        "  warm-up               {:>9} frames over {:.1} s",
        startup.warmup_frames, startup.warmup_seconds
    )
}

fn capture(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let capture = &report.capture;
    writeln!(out)?;
    writeln!(out, "CAPTURE")?;
    writeln!(
        out,
        "  successful acquires   {:>9}  in {:.2} s = {:.2}/s",
        capture.acquires, capture.window_s, capture.acquires_per_second
    )?;
    writeln!(
        out,
        "  desktop updates       {:>9}  in {:.2} s = {:.2}/s ({:.1}% of {:.0} expected)",
        capture.frames,
        capture.window_s,
        capture.frames_per_second,
        if capture.expected_frames > 0.0 {
            capture.frames as f64 / capture.expected_frames * 100.0
        } else {
            0.0
        },
        capture.expected_frames
    )?;
    writeln!(
        out,
        "  pointer-only updates  {:>9}  = {:.2}/s   anomalous {} = {:.2}/s",
        capture.pointer_only_updates,
        capture.pointer_only_updates_per_second,
        capture.anomalous_updates,
        capture.anomalous_updates_per_second
    )?;
    writeln!(
        out,
        "  timeouts              {:>9}   superseded {}   drained {}",
        capture.timeouts, capture.superseded, capture.drained
    )?;
    if capture.signals > 0 {
        // The pool's own notification rate. If this matches the source and
        // `frames` does not, the consumer lost them; if this matches `frames`
        // and both fall short, the pool never offered them.
        writeln!(
            out,
            "  frame-arrived events  {:>9}   = {:.2}/s",
            capture.signals,
            capture.signals as f64 / capture.window_s.max(f64::EPSILON)
        )?;
    }
    distribution(out, "accumulated frames", &capture.accumulated_frames)?;
    distribution(out, "pool pressure", &capture.pending_frames)?;
    writeln!(out)?;
    writeln!(
        out,
        "  native delivery delay ({} -> acquire return)",
        capture.source_mark
    )?;
    percentiles(out, &capture.delivery_delay)?;
    if capture.delivery_delay_unusable > 0 {
        writeln!(
            out,
            "      {} frames excluded: the source mark was zero and a clock that was never set \
             is not a delay",
            capture.delivery_delay_unusable
        )?;
    }
    writeln!(out, "  acquire duration")?;
    percentiles(out, &capture.acquire)?;
    writeln!(out, "  capture interval")?;
    percentiles(out, &capture.interval)
}

fn handoff(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let Some(handoff) = &report.handoff else {
        return Ok(());
    };
    writeln!(out)?;
    writeln!(
        out,
        "HANDOFF  ({} owned textures, {:.1} ms simulated downstream hold)",
        handoff.pool_size, handoff.hold_ms
    )?;
    writeln!(out, "  GPU copies            {:>9}", handoff.copies)?;
    writeln!(
        out,
        "  copy submit (CPU)     -- time inside CopyResource; the GPU has done nothing yet"
    )?;
    percentiles(out, &handoff.copy_submit_cpu)?;
    writeln!(
        out,
        "  copy GPU time         -- D3D11 timestamp pair around the copy; the real cost"
    )?;
    percentiles(out, &handoff.copy_gpu)?;
    writeln!(
        out,
        "  copy completion seen  -- submit to the first poll that found it done; polled once per \
         loop, so this is an upper bound quantised by the capture period"
    )?;
    percentiles(out, &handoff.copy_completion_observed)?;
    writeln!(out, "  source hold")?;
    percentiles(out, &handoff.source_hold)?;
    writeln!(
        out,
        "  pool starvation       {:>9}   growth {}",
        handoff.pool_starvation,
        handoff
            .pool_starvation_slope_per_min
            .map(|slope| format!("{slope:+.3}/min"))
            .unwrap_or_else(|| "unmeasured".to_owned())
    )?;
    writeln!(
        out,
        "  queries               {} resolved ({:.1}%), {} found no free slot, {} disjoint, {} \
         never ready",
        handoff.queries_resolved,
        handoff.gpu_result_fraction * 100.0,
        handoff.queries_slot_exhausted,
        handoff.queries_disjoint_discarded,
        handoff.queries_unresolved_at_exit
    )?;
    if handoff.gpu_result_fraction < 1.0 {
        writeln!(
            out,
            "      results are polled with D3D11_ASYNC_GETDATA_DONOTFLUSH: an unresolved query \
             means the command buffer had not been submitted, not that the copy was slow. No \
             Flush is issued, because the product must not have one here."
        )?;
    }
    Ok(())
}

fn stall(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let Some(stall) = &report.injected_stall else {
        return Ok(());
    };
    writeln!(out)?;
    writeln!(
        out,
        "INJECTED STALL  ({} ms requested, {:.1} ms actual)",
        stall.requested_ms, stall.actual_ms
    )?;
    writeln!(
        out,
        "  first acquire after   {:>9.3} ms",
        stall.first_acquire_ms
    )?;
    writeln!(
        out,
        "  first frame staleness {}",
        stall
            .first_frame_delivery_delay_ms
            .map(|ms| format!("{ms:>9.3} ms"))
            .unwrap_or_else(|| "  unusable source mark".to_owned())
    )?;
    writeln!(
        out,
        "  accumulated by the API{}",
        stall
            .first_frame_accumulated
            .map(|count| format!("{count:>9}  frames"))
            .unwrap_or_else(|| "        n/a  (this API does not report it)".to_owned())
    )?;
    writeln!(
        out,
        "  back on cadence after {}",
        stall
            .frames_to_recover
            .map(|frames| format!("{frames:>9}  frames"))
            .unwrap_or_else(|| "     never".to_owned())
    )
}

fn system(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let system = &report.system;
    writeln!(out)?;
    writeln!(out, "SYSTEM")?;
    writeln!(
        out,
        "  process CPU           {}",
        system
            .process_cpu_percent
            .map(|percent| format!("{percent:>9.2} % of one core"))
            .unwrap_or_else(|| "unobtainable on this platform".to_owned())
    )?;
    writeln!(
        out,
        "  working set           {}",
        system
            .working_set_bytes
            .map(|bytes| format!("{:>9.1} MB", bytes as f64 / 1_048_576.0))
            .unwrap_or_else(|| "unobtainable on this platform".to_owned())
    )?;
    writeln!(
        out,
        "  memory slope          {}   over {} samples",
        system
            .memory_slope_bytes_per_min
            .map(|slope| format!("{:>+9.1} KB/min", slope / 1024.0))
            .unwrap_or_else(|| "unmeasured".to_owned()),
        system.memory_samples
    )
}

fn stability(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let stability = &report.stability;
    writeln!(out)?;
    writeln!(out, "STABILITY")?;
    writeln!(
        out,
        "  API resets            {:>9}   access lost {}   restart failures {}",
        stability.api_resets, stability.access_lost, stability.restart_failures
    )?;
    // Two different things used to share the word "stall". At a steady rate
    // roughly half of all intervals land just above the period, so calling
    // those stalls reports a thousand of them in a run that missed nothing.
    // Only the second number means a source frame went by unacquired.
    writeln!(
        out,
        "  intervals late        {:>9} of {} (period {:.3} ms)",
        stability.intervals_over_1x, stability.intervals_measured, stability.period_ms
    )?;
    writeln!(
        out,
        "  frames missed         {:>9} intervals ran over two periods",
        stability.intervals_over_2x
    )?;
    writeln!(
        out,
        "  worst interval        {:>9.3} ms",
        stability.max_interval_ms
    )?;
    writeln!(
        out,
        "  pool recreations      {:>9}{}",
        stability.pool_recreations,
        match stability.border_suppressed {
            Some(true) => "   capture border suppressed",
            Some(false) => "   CAPTURE BORDER PRESENT: it is content and pollutes the frames",
            None => "",
        }
    )?;
    writeln!(
        out,
        "  timestamp regressions {:>9} source (worst {:.3} ms back), {} acquire clock",
        stability.source_timestamp_regressions,
        stability.source_regression_worst_ms,
        stability.acquire_timestamp_regressions
    )?;
    writeln!(
        out,
        "  backlog               peak {:.2}, {:.2} at exit, growth {}",
        stability.backlog_peak,
        stability.backlog_trailing,
        stability
            .backlog_slope_per_min
            .map(|slope| format!("{slope:+.3}/min"))
            .unwrap_or_else(|| "unmeasured".to_owned())
    )
}

fn gate(out: &mut impl Write, report: &RunReport) -> fmt::Result {
    let Some(gate) = &report.gate else {
        return Ok(());
    };
    writeln!(out)?;
    writeln!(out, "GATE 3A")?;
    for check in &gate.checks {
        writeln!(
            out,
            "  [{}] {:<26} {}",
            if check.passed { "pass" } else { "FAIL" },
            check.name,
            check.detail
        )?;
    }
    for note in &gate.untested {
        writeln!(out, "  [none] {:<26} {}", "untested", note)?;
    }
    if !gate.soaked {
        writeln!(
            out,
            "  [note] {:<26} too few frames for the tail numbers to be quoted as evidence",
            "soak"
        )?;
    }
    writeln!(
        out,
        "gate 3A: {}",
        if gate.passed { "PASS" } else { "FAIL" }
    )
}

fn percentiles(out: &mut impl Write, summary: &Summary) -> fmt::Result {
    writeln!(
        out,
        "      n={:<7} p50 {:>8.3}   p95 {:>8.3}   p99 {:>8.3}   max {:>8.3}   mean {:>8.3}  ms",
        summary.count,
        summary.p50_ms,
        summary.p95_ms,
        summary.p99_ms,
        summary.max_ms,
        summary.mean_ms
    )
}

fn distribution(out: &mut impl Write, label: &str, distribution: &Distribution) -> fmt::Result {
    if distribution.samples == 0 {
        return Ok(());
    }
    writeln!(
        out,
        "  {label:<21} {:>9} above one (p50 {} p95 {} p99 {} max {}, mean {:.3}) of {} reported",
        distribution.over_one,
        distribution.p50,
        distribution.p95,
        distribution.p99,
        distribution.max,
        distribution.mean().unwrap_or(0.0),
        distribution.samples
    )
}

/// The two backends beside each other, plus the block order that produced
/// them.
pub fn compare_block(out: &mut impl Write, report: &CompareReport) -> fmt::Result {
    let device = &report.device;
    writeln!(out, "{RULE}")?;
    writeln!(
        out,
        "capture-bench compare :: wgc vs dda, alternating blocks"
    )?;
    writeln!(out, "{}", device.description)?;
    writeln!(
        out,
        "mode        {}x{} @ {:.3} Hz ({}/{} from {})",
        device.output_width,
        device.output_height,
        device.refresh_hz,
        device.refresh_numerator,
        device.refresh_denominator,
        device.refresh_source
    )?;
    writeln!(
        out,
        "schedule    {} blocks of {:.2} s, seed {}, {} first",
        report.blocks.len(),
        report.config.block_seconds.unwrap_or(0.0),
        report.config.seed.unwrap_or(0),
        report
            .blocks
            .first()
            .map(|block| block.backend.as_str())
            .unwrap_or("none")
    )?;
    writeln!(out, "{RULE}")?;

    writeln!(out)?;
    writeln!(out, "{:<30}{:>18}{:>18}", "", "wgc", "dda")?;
    let wgc = &report.wgc;
    let dda = &report.dda;
    row(
        out,
        "content frames",
        wgc.capture.frames as f64,
        dda.capture.frames as f64,
        0,
    )?;
    row(
        out,
        "frames/s",
        wgc.capture.frames_per_second,
        dda.capture.frames_per_second,
        2,
    )?;
    row(
        out,
        "delivery p50 (ms)",
        wgc.capture.delivery_delay.p50_ms,
        dda.capture.delivery_delay.p50_ms,
        3,
    )?;
    row(
        out,
        "delivery p95 (ms)",
        wgc.capture.delivery_delay.p95_ms,
        dda.capture.delivery_delay.p95_ms,
        3,
    )?;
    row(
        out,
        "delivery p99 (ms)",
        wgc.capture.delivery_delay.p99_ms,
        dda.capture.delivery_delay.p99_ms,
        3,
    )?;
    row(
        out,
        "delivery max (ms)",
        wgc.capture.delivery_delay.max_ms,
        dda.capture.delivery_delay.max_ms,
        3,
    )?;
    row(
        out,
        "acquire p50 (ms)",
        wgc.capture.acquire.p50_ms,
        dda.capture.acquire.p50_ms,
        3,
    )?;
    row(
        out,
        "acquire p99 (ms)",
        wgc.capture.acquire.p99_ms,
        dda.capture.acquire.p99_ms,
        3,
    )?;
    row(
        out,
        "interval p99 (ms)",
        wgc.capture.interval.p99_ms,
        dda.capture.interval.p99_ms,
        3,
    )?;
    row(
        out,
        "duplicates",
        wgc.capture.duplicates as f64,
        dda.capture.duplicates as f64,
        0,
    )?;
    row(
        out,
        "superseded",
        wgc.capture.superseded as f64,
        dda.capture.superseded as f64,
        0,
    )?;
    row(
        out,
        "backlog above one",
        wgc.capture.pending_frames.over_one as f64,
        dda.capture.accumulated_frames.over_one as f64,
        0,
    )?;
    row(
        out,
        "stalls over two periods",
        wgc.stability.intervals_over_2x as f64,
        dda.stability.intervals_over_2x as f64,
        0,
    )?;
    row(
        out,
        "access lost",
        wgc.stability.access_lost as f64,
        dda.stability.access_lost as f64,
        0,
    )?;
    if let (Some(left), Some(right)) = (&wgc.handoff, &dda.handoff) {
        row(
            out,
            "copy GPU p50 (ms)",
            left.copy_gpu.p50_ms,
            right.copy_gpu.p50_ms,
            3,
        )?;
        row(
            out,
            "copy GPU p99 (ms)",
            left.copy_gpu.p99_ms,
            right.copy_gpu.p99_ms,
            3,
        )?;
        row(
            out,
            "source hold p99 (ms)",
            left.source_hold.p99_ms,
            right.source_hold.p99_ms,
            3,
        )?;
        row(
            out,
            "pool starvation",
            left.pool_starvation as f64,
            right.pool_starvation as f64,
            0,
        )?;
    }

    writeln!(out)?;
    writeln!(out, "BLOCKS  (in the order they ran)")?;
    for block in &report.blocks {
        block_line(out, block)?;
    }

    writeln!(out)?;
    writeln!(out, "=== wgc ===")?;
    run_block(out, &report.wgc)?;
    writeln!(out)?;
    writeln!(out, "=== dda ===")?;
    run_block(out, &report.dda)
}

fn row(out: &mut impl Write, label: &str, left: f64, right: f64, decimals: usize) -> fmt::Result {
    writeln!(
        out,
        "{label:<30}{:>18.*}{:>18.*}",
        decimals, left, decimals, right
    )
}

fn block_line(out: &mut impl Write, block: &BlockReport) -> fmt::Result {
    writeln!(
        out,
        "  {:>2} {:<4} {:>6.2} s  {:>6} frames  {:>7.2}/s  delivery p50 {:>8.3}  p99 {:>8.3}  \
         acquire p99 {:>8.3}  stalls {:>4}  lost {}",
        block.index,
        block.backend,
        block.seconds,
        block.frames,
        block.frames_per_second,
        block.delivery_delay.p50_ms,
        block.delivery_delay.p99_ms,
        block.acquire.p99_ms,
        block.intervals_over_2x,
        block.access_lost
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{DeviceReport, GateReport, HandoffReport, InjectedStallReport, RunReport};

    fn rendered(report: &RunReport) -> String {
        let mut block = String::new();
        run_block(&mut block, report).expect("rendering into a String cannot fail");
        block
    }

    fn populated(mode: &str) -> RunReport {
        let mut report = RunReport::new(mode, "dda");
        report.backend_api = "DuplicateOutput1".to_owned();
        report.device = DeviceReport {
            adapter: "NVIDIA GeForce RTX 4060 Ti".to_owned(),
            output: r"\\.\DISPLAY1".to_owned(),
            output_width: 1920,
            output_height: 1080,
            refresh_numerator: 100,
            refresh_denominator: 1,
            refresh_hz: 100.0,
            refresh_source: "dxgi mode list".to_owned(),
            description: "NVIDIA GeForce RTX 4060 Ti driving \\\\.\\DISPLAY1 at 1920x1080"
                .to_owned(),
            ..DeviceReport::default()
        };
        report.config.source_hz = 100.0;
        report.capture.frames = 6_000;
        report.capture.window_s = 60.0;
        report.capture.frames_per_second = 100.0;
        report.capture.expected_frames = 6_000.0;
        report.capture.source_mark = "desktop presented".to_owned();
        report.stability.period_ms = 10.0;
        report
    }

    #[test]
    fn every_report_names_the_gpu_and_the_output_before_any_number() {
        // A capture benchmark that does not say which GPU and driver produced
        // it is a number without a subject.
        let block = rendered(&populated("native"));
        let identity = block.find("RTX 4060 Ti").expect("the adapter is named");
        assert!(
            identity < block.find("CAPTURE").expect("CAPTURE section"),
            "the subject has to come before the numbers"
        );
        assert!(block.contains(r"\\.\DISPLAY1"));
    }

    #[test]
    fn the_measured_mode_is_printed_with_its_rational_and_its_source() {
        let block = rendered(&populated("native"));
        assert!(block.contains("1920x1080 @ 100.000 Hz (100/1 from dxgi mode list)"));
    }

    #[test]
    fn a_native_run_has_the_four_required_sections_and_no_handoff() {
        let block = rendered(&populated("native"));
        for section in ["STARTUP", "CAPTURE", "SYSTEM", "STABILITY"] {
            assert!(block.contains(section), "{section} missing from the block");
        }
        assert!(!block.contains("HANDOFF"));
        assert!(!block.contains("INJECTED STALL"));
    }

    #[test]
    fn dda_audit_separates_acquires_from_desktop_and_pointer_updates() {
        let mut report = populated("native");
        report.capture.acquires = 9_500;
        report.capture.acquires_per_second = 158.3;
        report.capture.pointer_only_updates = 3_500;
        report.capture.pointer_only_updates_per_second = 58.3;
        report.capture.accumulated_frames.samples = 6_000;
        report.capture.accumulated_frames.p50 = 1;
        report.capture.accumulated_frames.p95 = 1;
        report.capture.accumulated_frames.p99 = 2;
        let block = rendered(&report);
        assert!(block.contains("successful acquires"));
        assert!(block.contains("desktop updates"));
        assert!(block.contains("pointer-only updates"));
        assert!(block.contains("p50 1 p95 1 p99 2"));
    }

    #[test]
    fn a_handoff_run_reports_the_three_copy_numbers_under_three_names() {
        // Conflating any two of these is the specific error the phase exists
        // to avoid, so the block must never show fewer than three.
        let mut report = populated("handoff");
        report.handoff = Some(HandoffReport {
            pool_size: 3,
            copies: 6_000,
            queries_resolved: 6_000,
            gpu_result_fraction: 1.0,
            ..HandoffReport::default()
        });
        let block = rendered(&report);
        assert!(block.contains("copy submit (CPU)"));
        assert!(block.contains("copy GPU time"));
        assert!(block.contains("copy completion seen"));
        assert!(block.contains("pool starvation"));
    }

    #[test]
    fn an_incomplete_gpu_sample_says_why_rather_than_leaving_it_unexplained() {
        let mut report = populated("handoff");
        report.handoff = Some(HandoffReport {
            copies: 6_000,
            queries_resolved: 5_000,
            gpu_result_fraction: 5_000.0 / 6_000.0,
            ..HandoffReport::default()
        });
        let block = rendered(&report);
        assert!(block.contains("DONOTFLUSH"));
        assert!(block.contains("must not have one here"));
    }

    #[test]
    fn an_injected_stall_that_never_recovered_says_never() {
        let mut report = populated("native");
        report.injected_stall = Some(InjectedStallReport {
            requested_ms: 500,
            actual_ms: 500.7,
            frames_to_recover: None,
            recovered: false,
            ..InjectedStallReport::default()
        });
        let block = rendered(&report);
        assert!(block.contains("INJECTED STALL"));
        assert!(block.contains("never"));
    }

    #[test]
    fn the_gate_verdict_closes_the_block() {
        let mut report = populated("native");
        report.gate = Some(GateReport {
            passed: false,
            soaked: true,
            checks: vec![crate::report::CheckReport {
                name: "no CPU readback".to_owned(),
                passed: false,
                detail: "4096 bytes mapped".to_owned(),
            }],
            untested: Vec::new(),
        });
        let block = rendered(&report);
        assert!(block.trim_end().ends_with("gate 3A: FAIL"));
        assert!(block.contains("[FAIL] no CPU readback"));
    }

    #[test]
    fn a_run_too_short_to_quote_says_so_next_to_its_verdict() {
        let mut report = populated("native");
        report.gate = Some(GateReport {
            passed: true,
            soaked: false,
            checks: Vec::new(),
            untested: vec!["recovery from Acquired::Lost: no loss occurred".to_owned()],
        });
        let block = rendered(&report);
        assert!(block.contains("too few frames"));
        assert!(block.contains("no loss occurred"));
    }

    #[test]
    fn unobtainable_system_numbers_say_so_instead_of_reading_as_zero() {
        let block = rendered(&populated("native"));
        assert!(block.contains("process CPU           unobtainable on this platform"));
        assert!(block.contains("memory slope          unmeasured"));
    }
}
