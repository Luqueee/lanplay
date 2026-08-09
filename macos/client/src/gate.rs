//! The phase 2 gate, as code.
//!
//! Deliberately not "is the latency under N milliseconds". At this stage the
//! question is whether the decode-and-present path can hold a rate
//! indefinitely without accumulating anything: a run that averages 120 fps
//! while its decoder queue gains a frame a minute is a failure, and a p99
//! quoted off a five-second run is not evidence.

use core::fmt;

use lanplay_telemetry::{Nanos, P99_SOAK_FRAMES, Segment, Snapshot, Trend};

/// Frames the decoder may still hold at the end of a run before the backlog
/// counts as real rather than transient.
const MAX_TRAILING_BACKLOG: usize = 2;
/// Backlog growth that counts as accumulation, in frames per minute.
const MAX_BACKLOG_GROWTH: f64 = 0.5;
/// Resident memory growth that counts as a leak, in bytes per minute.
const MAX_MEMORY_GROWTH: f64 = 1_048_576.0;
/// Start-up allocations excluded from the leak fit.
const MEMORY_WARMUP: Nanos = Nanos::from_millis(10_000);
/// How far a single present interval may exceed the display period before it
/// is a stall rather than jitter.
const STALL_MULTIPLE: f64 = 4.0;
/// Share of display ticks that may find the slot empty before phase noise
/// stops being a credible explanation.
const MAX_EMPTY_TICK_FRACTION: f64 = 0.05;

/// Everything the gate judges. Every field is measured during the run except
/// the two marked structural, which record how the code is built.
pub struct GateInputs {
    pub target_fps: f64,
    pub expected_frames: u64,
    /// Refresh rate of the display the window was actually on.
    pub display_hz: f64,

    pub hardware_decoder: bool,
    pub submitted: u64,
    pub decoded: u64,
    /// Wall time the feed ran for, used to turn counts into rates.
    pub run_seconds: f64,
    pub decoder_errors: u64,
    pub decoder_dropped: u64,
    pub backlog: Trend,
    pub max_backlog: usize,
    pub trailing_backlog: usize,

    pub rendered: u64,
    pub superseded: u64,
    pub empty_ticks: u64,
    pub still_in_slot: u64,

    pub memory: Trend,
    /// True when the renderer is driven by the display link, so `empty_ticks`
    /// counts refreshes rather than idle polls.
    pub display_driven: bool,
    pub snapshot: Snapshot,

    /// Structural: the render path creates Metal textures straight from the
    /// decoder's pixel buffers, so no plane is ever copied by the CPU.
    pub zero_copy_render_path: bool,
    /// Structural: textures come from a `CVMetalTextureCache` fed with
    /// VideoToolbox output.
    pub metal_texture_cache: bool,
}

pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

pub struct Verdict {
    pub checks: Vec<Check>,
    /// True when the run was long enough for its tail numbers to be quoted.
    pub soaked: bool,
}

impl Verdict {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|check| check.passed)
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for check in &self.checks {
            writeln!(
                f,
                "  [{}] {:<24} {}",
                if check.passed { "pass" } else { "FAIL" },
                check.name,
                check.detail
            )?;
        }
        if !self.soaked {
            writeln!(
                f,
                "  [note] {:<24} run is shorter than the {P99_SOAK_FRAMES}-frame soak; \
                 tail numbers are indicative only",
                "soak"
            )?;
        }
        write!(f, "gate: {}", if self.passed() { "PASS" } else { "FAIL" })
    }
}

pub fn evaluate(inputs: &GateInputs) -> Verdict {
    let mut checks = Vec::new();
    let snapshot = &inputs.snapshot;

    checks.push(Check {
        name: "hardware decoder",
        passed: inputs.hardware_decoder,
        detail: if inputs.hardware_decoder {
            "VTDecompressionSession reports hardware acceleration".to_owned()
        } else {
            "session is not hardware accelerated".to_owned()
        },
    });

    checks.push(Check {
        name: "input sustained",
        passed: inputs.submitted >= inputs.expected_frames,
        detail: format!(
            "{} access units submitted, {} expected at {:.0} fps",
            inputs.submitted, inputs.expected_frames, inputs.target_fps
        ),
    });

    let lossless_decode = inputs.decoded == inputs.submitted
        && inputs.decoder_errors == 0
        && inputs.decoder_dropped == 0;
    checks.push(Check {
        name: "decode throughput",
        passed: lossless_decode,
        detail: format!(
            "{} decoded of {} submitted, {} errors, {} dropped",
            inputs.decoded, inputs.submitted, inputs.decoder_errors, inputs.decoder_dropped
        ),
    });

    // The heart of the gate: a queue that grows is a failure even at the right
    // average rate, because it only ends one way.
    let growth = inputs.backlog.slope_per_minute();
    let backlog_ok = inputs.trailing_backlog <= MAX_TRAILING_BACKLOG
        && growth.is_some_and(|slope| slope <= MAX_BACKLOG_GROWTH);
    checks.push(Check {
        name: "decoder backlog",
        passed: backlog_ok,
        detail: match growth {
            Some(slope) => format!(
                "peak {} frames, {} left at exit, growth {slope:+.2} frames/min",
                inputs.max_backlog, inputs.trailing_backlog
            ),
            None => format!(
                "peak {} frames, {} left at exit, growth unmeasured ({} samples)",
                inputs.max_backlog,
                inputs.trailing_backlog,
                inputs.backlog.count()
            ),
        },
    });

    // Every decoded frame must be accounted for: shown, deliberately skipped,
    // or still held. A frame that is none of those has leaked.
    let accounted = inputs.rendered + inputs.superseded + inputs.still_in_slot;
    checks.push(Check {
        name: "frames accounted",
        passed: accounted == inputs.decoded,
        detail: format!(
            "{} decoded = {} rendered + {} superseded + {} held",
            inputs.decoded, inputs.rendered, inputs.superseded, inputs.still_in_slot
        ),
    });

    // Starvation is a statement about the decoder, and the bar it must clear
    // is the rate that can actually reach the screen: the lower of the source
    // rate and the refresh rate. A 60 fps source on a 120 Hz panel leaves half
    // the refreshes with nothing new by arithmetic, and calling that
    // starvation would condemn a perfectly healthy pipeline.
    //
    // The empty-refresh share is therefore judged against what the rate
    // difference already predicts, and only when a tick is a refresh at all:
    // a decoder-driven renderer polls, so its empty ticks count spin
    // iterations and say nothing about anything.
    let decoded_per_second = inputs.decoded as f64 / inputs.run_seconds.max(f64::EPSILON);
    let deliverable = inputs.target_fps.min(inputs.display_hz);
    let keeps_up = decoded_per_second >= deliverable * 0.99;
    let ticks = inputs.rendered + inputs.empty_ticks;
    let empty_fraction = if ticks == 0 {
        1.0
    } else {
        inputs.empty_ticks as f64 / ticks as f64
    };
    let predicted_empty =
        (1.0 - (inputs.target_fps / inputs.display_hz.max(1.0)).min(1.0)).max(0.0);
    let unexplained_empty = empty_fraction - predicted_empty;
    checks.push(Check {
        name: "no decoder starvation",
        passed: keeps_up
            && (!inputs.display_driven || unexplained_empty <= MAX_EMPTY_TICK_FRACTION),
        detail: if inputs.display_driven {
            format!(
                "decoder produced {decoded_per_second:.1}/s against {deliverable:.1}/s \
                 deliverable on a {:.1} Hz display; {:.2}% of {ticks} refreshes empty, \
                 {:.2}% predicted by the rate difference, {} superseded",
                inputs.display_hz,
                empty_fraction * 100.0,
                predicted_empty * 100.0,
                inputs.superseded,
            )
        } else {
            format!(
                "decoder produced {decoded_per_second:.1}/s against {deliverable:.1}/s \
                 deliverable on a {:.1} Hz display; {} superseded \
                 ({} idle polls, not display refreshes)",
                inputs.display_hz, inputs.superseded, inputs.empty_ticks,
            )
        },
    });

    // Judged against the period presents actually have, which is set by the
    // slower of source and display: a 60 fps source on a 120 Hz panel presents
    // every 16.7 ms by design, and measuring its gaps against the 8.3 ms
    // refresh would call every normal interval a stall.
    //
    // A gap is also only the presenter's fault if the source did not gap
    // first. When the feed thread loses the CPU for four periods the pipeline
    // faithfully reproduces that hole, and blaming Metal for it would send the
    // next investigation to the wrong component. The source gap is reported
    // either way: in later phases it is the network, and then it stops being
    // someone else's problem.
    let present_period = Nanos::from_millis_f64(1000.0 / deliverable.max(1.0));
    let stall_limit = Nanos((present_period.get() as f64 * STALL_MULTIPLE) as u64);
    let worst_interval = snapshot.present_interval.max;
    let worst_source_gap = snapshot.capture_interval.max;
    let inherited = worst_interval <= worst_source_gap;
    checks.push(Check {
        name: "no present stalls",
        passed: worst_interval <= stall_limit || inherited,
        detail: format!(
            "worst present interval {worst_interval} against a {stall_limit} limit \
             ({STALL_MULTIPLE:.0}x the {present_period} present period); \
             worst source gap {worst_source_gap}{}",
            if worst_interval > stall_limit && inherited {
                " - the stall came in with the source, not from the presenter"
            } else {
                ""
            }
        ),
    });

    // Judged after warm-up: filling a decoder pool, compiling a shader and
    // reading a fixture all cost memory once, and a line fitted through them
    // reads as a leak on any short run.
    let steady_memory = inputs.memory.after_warmup(MEMORY_WARMUP);
    checks.push(Check {
        name: "memory stable",
        passed: steady_memory.is_stable(MAX_MEMORY_GROWTH),
        detail: match (
            steady_memory.slope_per_minute(),
            inputs.memory.slope_per_minute(),
        ) {
            (Some(steady), Some(whole)) => format!(
                "{:+.2} MB/min in steady state ({} samples after {MEMORY_WARMUP}), \
                 {:+.2} MB/min including warm-up, peak {:.1} MB",
                steady / 1_048_576.0,
                steady_memory.count(),
                whole / 1_048_576.0,
                inputs.memory.max().unwrap_or(0.0) / 1_048_576.0
            ),
            _ => format!(
                "unmeasured: {} samples, {} after warm-up",
                inputs.memory.count(),
                steady_memory.count()
            ),
        },
    });

    checks.push(Check {
        name: "instrumentation intact",
        passed: snapshot.marks_intact(),
        detail: format!(
            "{} dropped marks, {} duplicate, {} late",
            snapshot.counters.events_dropped,
            snapshot.counters.duplicate_marks,
            snapshot.counters.late_events
        ),
    });

    // A frame that never presents is expected here: that is what superseding
    // means. What must not happen is a frame going missing for any other
    // reason, so every incomplete timeline has to be explained by one.
    let unexplained = snapshot
        .counters
        .frames_incomplete
        .saturating_sub(inputs.superseded + inputs.still_in_slot);
    checks.push(Check {
        name: "drops explained",
        passed: unexplained == 0,
        detail: format!(
            "{} frames never presented, {} superseded + {} held explain them ({unexplained} unexplained)",
            snapshot.counters.frames_incomplete, inputs.superseded, inputs.still_in_slot
        ),
    });

    // The gap is allowed to be non-zero here: this pipeline has no capture or
    // network stages to mark. It is not allowed to be unmeasured.
    checks.push(Check {
        name: "gap instrumented",
        passed: snapshot.unattributed_gap.count == snapshot.counters.frames_presented
            && snapshot.counters.frames_presented > 0,
        detail: format!(
            "{} of {} presented frames have a measured gap, p99 {}",
            snapshot.unattributed_gap.count,
            snapshot.counters.frames_presented,
            snapshot.unattributed_gap.p99
        ),
    });

    checks.push(Check {
        name: "zero copy path",
        passed: inputs.zero_copy_render_path && inputs.metal_texture_cache,
        detail: "structural: CVMetalTextureCache textures over VideoToolbox pixel buffers, \
                 no CPU plane access on the render path"
            .to_owned(),
    });

    let decode = snapshot.segment(Segment::Decode);
    checks.push(Check {
        name: "decode measured",
        passed: decode.count > 0,
        detail: format!(
            "p50 {} p95 {} p99 {} max {} over {} frames",
            decode.p50, decode.p95, decode.p99, decode.max, decode.count
        ),
    });

    Verdict {
        checks,
        soaked: snapshot.p99_is_soaked(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanplay_protocol::FrameId;
    use lanplay_telemetry::{Stage, Telemetry, TelemetryConfig, Timestamp};
    use std::time::Duration;

    /// A synthetic run, so the gate is tested against a real `Snapshot`
    /// rather than a hand-filled struct.
    #[derive(Clone, Copy)]
    struct Run {
        frames: u64,
        period_ms: f64,
        /// The renderer holds this frame back: presentation gaps, the source
        /// does not.
        present_stall_at: Option<u64>,
        /// The source itself gaps here, and presentation inherits the hole.
        source_stall_at: Option<u64>,
        /// Frames that get decode marks but never present, as a superseded
        /// frame does in the real client.
        supersede_every: Option<u64>,
    }

    impl Run {
        fn of(frames: u64) -> Self {
            Run {
                frames,
                period_ms: 8.333,
                present_stall_at: None,
                source_stall_at: None,
                supersede_every: None,
            }
        }
    }

    fn snapshot_of(run: Run) -> Snapshot {
        // Marks are pushed as fast as the CPU allows rather than at 120 fps,
        // so the queue has to hold the whole run: a drop here would show up
        // as an instrumentation failure that the gate is right to report but
        // that says nothing about the gate itself.
        let telemetry = Telemetry::start(TelemetryConfig {
            queue_capacity: 1 << 18,
            ..TelemetryConfig::default()
        });
        let recorder = telemetry.recorder();
        let base = |ms: f64| Timestamp::from_nanos((ms * 1_000_000.0) as u64);
        let mut now = 0.0f64;
        for index in 1..=run.frames {
            let frame = FrameId::new(index);
            if run.source_stall_at == Some(index) {
                now += run.period_ms * 10.0;
            }
            recorder.mark_at(frame, Stage::FrameCreated, base(now));
            recorder.mark_at(frame, Stage::DecodeSubmit, base(now + 1.0));
            recorder.mark_at(frame, Stage::DecodeComplete, base(now + 2.2));
            let held = if run.present_stall_at == Some(index) {
                run.period_ms * 10.0
            } else {
                0.0
            };
            if run.supersede_every.is_none_or(|every| index % every != 0) {
                recorder.mark_at(frame, Stage::RenderSubmit, base(now + 2.4 + held));
                recorder.mark_at(frame, Stage::PresentSubmit, base(now + 2.7 + held));
            }
            now += run.period_ms;
        }
        assert!(telemetry.flush(Duration::from_secs(5)));
        telemetry.shutdown()
    }

    /// Samples spaced like the client's real sampler, so a fixture long
    /// enough to survive the warm-up window looks like a real run.
    fn flat_trend(value: f64, seconds: u64) -> Trend {
        let mut trend = Trend::new();
        for index in 0..seconds * 4 {
            trend.record_at(Timestamp::from_nanos(index * 250_000_000), value);
        }
        trend
    }

    fn healthy(frames: u64) -> GateInputs {
        GateInputs {
            target_fps: 120.0,
            expected_frames: frames,
            display_hz: 120.0,
            hardware_decoder: true,
            submitted: frames,
            decoded: frames,
            run_seconds: frames as f64 / 120.0,
            decoder_errors: 0,
            decoder_dropped: 0,
            backlog: flat_trend(1.0, 60),
            max_backlog: 2,
            trailing_backlog: 0,
            rendered: frames,
            superseded: 0,
            empty_ticks: 0,
            still_in_slot: 0,
            display_driven: true,
            memory: flat_trend(200e6, 60),
            snapshot: snapshot_of(Run::of(frames)),
            zero_copy_render_path: true,
            metal_texture_cache: true,
        }
    }

    #[test]
    fn a_healthy_run_passes() {
        let verdict = evaluate(&healthy(4_000));
        assert!(verdict.passed(), "{verdict}");
        assert!(verdict.soaked);
    }

    #[test]
    fn a_growing_backlog_fails_even_at_the_right_rate() {
        let mut inputs = healthy(4_000);
        let mut backlog = Trend::new();
        for minute in 0..10 {
            backlog.record_at(
                Timestamp::from_nanos(minute * 60_000_000_000),
                minute as f64 * 3.0,
            );
        }
        inputs.backlog = backlog;
        inputs.max_backlog = 30;
        inputs.trailing_backlog = 27;

        let verdict = evaluate(&inputs);
        assert!(!verdict.passed());
        let check = verdict
            .checks
            .iter()
            .find(|check| check.name == "decoder backlog")
            .unwrap();
        assert!(!check.passed, "{}", check.detail);
    }

    #[test]
    fn a_software_decoder_fails_immediately() {
        let mut inputs = healthy(4_000);
        inputs.hardware_decoder = false;
        assert!(!evaluate(&inputs).passed());
    }

    #[test]
    fn a_leak_fails_the_run() {
        let mut inputs = healthy(4_000);
        let mut memory = Trend::new();
        for minute in 0..10 {
            memory.record_at(
                Timestamp::from_nanos(minute * 60_000_000_000),
                200e6 + minute as f64 * 20e6,
            );
        }
        inputs.memory = memory;
        assert!(!evaluate(&inputs).passed());
    }

    #[test]
    fn a_lost_frame_fails_the_accounting() {
        let mut inputs = healthy(4_000);
        inputs.rendered = 3_990;
        assert!(!evaluate(&inputs).passed());
    }

    #[test]
    fn one_long_stall_fails_even_with_a_good_average() {
        let inputs = GateInputs {
            snapshot: snapshot_of(Run {
                present_stall_at: Some(2_000),
                ..Run::of(4_000)
            }),
            ..healthy(4_000)
        };
        let verdict = evaluate(&inputs);
        let check = verdict
            .checks
            .iter()
            .find(|check| check.name == "no present stalls")
            .unwrap();
        assert!(!check.passed, "{}", check.detail);
    }

    #[test]
    fn a_stall_the_source_caused_is_not_blamed_on_the_presenter() {
        // The feed thread loses the CPU for ten refreshes; the pipeline
        // faithfully reproduces the hole. Failing the presenter for that would
        // send the next investigation to the wrong component.
        let inputs = GateInputs {
            snapshot: snapshot_of(Run {
                source_stall_at: Some(2_000),
                ..Run::of(4_000)
            }),
            ..healthy(4_000)
        };
        let verdict = evaluate(&inputs);
        let check = verdict
            .checks
            .iter()
            .find(|check| check.name == "no present stalls")
            .unwrap();
        assert!(check.passed, "{}", check.detail);
        assert!(
            check.detail.contains("came in with the source"),
            "the report must say where the stall came from: {}",
            check.detail
        );
    }

    #[test]
    fn superseded_frames_are_fine_when_the_source_outruns_the_panel() {
        let mut inputs = healthy(4_000);
        inputs.display_hz = 60.0;
        inputs.rendered = 2_000;
        inputs.superseded = 2_000;
        // Half the frames never present, and the cadence follows the 60 Hz
        // panel rather than the 120 fps source.
        inputs.snapshot = snapshot_of(Run {
            period_ms: 16.667,
            supersede_every: Some(2),
            ..Run::of(4_000)
        });
        let verdict = evaluate(&inputs);
        assert!(verdict.passed(), "{verdict}");
    }

    #[test]
    fn a_frame_that_vanishes_without_being_superseded_fails() {
        let mut inputs = healthy(4_000);
        // 400 frames never presented, but the slot says nothing was skipped:
        // they went missing somewhere the instrumentation cannot see.
        inputs.snapshot = snapshot_of(Run {
            supersede_every: Some(10),
            ..Run::of(4_000)
        });
        inputs.superseded = 0;
        inputs.rendered = 3_600;
        inputs.still_in_slot = 400;
        let verdict = evaluate(&inputs);
        let check = verdict
            .checks
            .iter()
            .find(|check| check.name == "drops explained")
            .unwrap();
        assert!(check.passed, "held frames explain them: {}", check.detail);

        inputs.still_in_slot = 0;
        inputs.rendered = 4_000;
        let verdict = evaluate(&inputs);
        let check = verdict
            .checks
            .iter()
            .find(|check| check.name == "drops explained")
            .unwrap();
        assert!(!check.passed, "{}", check.detail);
    }

    fn starvation_check(inputs: &GateInputs) -> Check {
        let verdict = evaluate(inputs);
        verdict
            .checks
            .into_iter()
            .find(|check| check.name == "no decoder starvation")
            .expect("starvation check")
    }

    #[test]
    fn a_source_that_outruns_the_panel_is_not_starvation() {
        // 240 fps into a 120 Hz display: half the frames are thrown away by
        // design, and the decoder is nowhere near behind.
        let mut inputs = healthy(4_800);
        inputs.target_fps = 240.0;
        inputs.run_seconds = 20.0;
        inputs.rendered = 2_386;
        inputs.superseded = 2_414;
        inputs.empty_ticks = 36;
        inputs.snapshot = snapshot_of(Run {
            supersede_every: Some(2),
            ..Run::of(4_800)
        });
        let check = starvation_check(&inputs);
        assert!(check.passed, "{}", check.detail);
    }

    #[test]
    fn a_source_slower_than_the_panel_leaves_refreshes_empty_by_arithmetic() {
        // 60 fps into a 120 Hz display: half the refreshes have nothing new,
        // and every source frame still reaches the screen.
        let mut inputs = healthy(3_600);
        inputs.target_fps = 60.0;
        inputs.run_seconds = 60.0;
        inputs.rendered = 3_591;
        inputs.superseded = 9;
        inputs.empty_ticks = 3_626;
        let check = starvation_check(&inputs);
        assert!(check.passed, "{}", check.detail);
    }

    #[test]
    fn a_slow_source_is_not_judged_against_the_refresh_period() {
        // 60 fps on a 120 Hz panel presents every 16.7 ms by design. Measuring
        // those intervals against the 8.3 ms refresh would report a stall on
        // every single frame.
        let mut inputs = healthy(3_600);
        inputs.target_fps = 60.0;
        inputs.run_seconds = 60.0;
        inputs.rendered = 3_591;
        inputs.superseded = 9;
        inputs.empty_ticks = 3_626;
        inputs.snapshot = snapshot_of(Run {
            period_ms: 16.667,
            ..Run::of(3_600)
        });
        let verdict = evaluate(&inputs);
        let check = verdict
            .checks
            .iter()
            .find(|check| check.name == "no present stalls")
            .unwrap();
        assert!(check.passed, "{}", check.detail);
    }

    #[test]
    fn a_decoder_that_falls_behind_the_panel_is_starvation() {
        let mut inputs = healthy(4_000);
        // 4000 frames over 60 s is 67/s against a 120 Hz panel.
        inputs.run_seconds = 60.0;
        let check = starvation_check(&inputs);
        assert!(!check.passed, "{}", check.detail);
    }

    #[test]
    fn empty_ticks_beyond_phase_noise_still_fail() {
        let mut inputs = healthy(4_000);
        inputs.empty_ticks = 1_000;
        let check = starvation_check(&inputs);
        assert!(!check.passed, "{}", check.detail);
    }

    #[test]
    fn a_short_run_can_pass_but_is_marked_unsoaked() {
        let verdict = evaluate(&healthy(600));
        assert!(verdict.passed(), "{verdict}");
        assert!(!verdict.soaked);
        assert!(verdict.to_string().contains("soak"));
    }
}
