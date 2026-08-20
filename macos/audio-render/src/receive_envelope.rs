//! One run of the receiver, in the two forms it has to be read in.
//!
//! [`document`] is the gate envelope `xtask verdict` decides on, and the
//! [`fmt::Display`] on [`Receipt`] below it is the keyed block a person reads
//! when the gate has just failed. Both, not either: an evaluator handed prose
//! has to parse it, and a person handed JSON has to find the four lines that
//! matter in ninety. What is gone is the third thing, the regular expression
//! that used to turn the first back into the second, and which is where three
//! of this project's instrument failures came from.
//!
//! Every number in `checks` is a parameter and none of them is a conclusion.
//! Nothing in this file decides whether the run passed.

use core::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use lanplay_audio_capture::Percentiles;
use lanplay_audio_capture::analysis::hertz;
use lanplay_tone_source::tone::CONTRACT;
use serde_json::{Map, Value, json};

use crate::excess::{self, ExcessCurve, ExcessReport, Threshold};
use crate::pairs::{BUCKETS, Spread, bucket_floor_us};
use crate::receive::{DELAY_BIAS_US, Receipt, WindowRow, unbias_micros};

/// How far a decoded frequency may sit from the one that went in.
///
/// Five hertz against an analysis window that resolves two, so it is two bins
/// and a margin, and it is two hundredths of the thousand-hertz gap between the
/// two contract tones - wide enough that a correct path cannot fail it and far
/// too narrow for a channel that carried the other channel's tone to pass.
const TONE_TOLERANCE_HZ: f64 = 5.0;

/// The fraction of a frame a decode has to stay under.
///
/// A tenth. The decoder runs inside the producer's period and the producer has
/// to deposit a frame before the device asks for one, so a decode that is a
/// tenth of the audio it decodes cannot be the term that matters; the phase
/// before this one measured p99 at 10 microseconds against a 5 ms frame, which
/// is a fiftieth of this bound, so a run near it has changed something.
const DECODE_BUDGET_FRACTION: f64 = 10.0;

/// What A7 measured this pair of machines at, referred to the Mac's timebase.
///
/// Here so that A8.1's own fit is reported against a figure taken by a different
/// instrument rather than against nothing. A7 closed +238 samples predicted
/// against +238 +-75 observed at this rate, which is why it is the reference and
/// not merely an earlier reading.
const A7_PPM: f64 = 9.29;

/// What the gate covers, and what a run has to reach to be allowed to claim it.
const DECLARED: [&str; 4] = [
    "rtp receive",
    "jitter buffer",
    "opus decode",
    "coreaudio render",
];

pub fn document(receipt: &Receipt, started: SystemTime, arm: &str, commit: Option<&str>) -> String {
    let frame_ms = receipt.config.frame.millis();
    // The same duration as a float, because a cluster's length and the gap
    // between two clusters are counted in timeline positions and reported in
    // milliseconds, and one frame is one position.
    let frame_ms_f = f64::from(frame_ms);
    let decode_budget_us = f64::from(frame_ms) * 1_000.0 / DECODE_BUDGET_FRACTION;
    let concealment = receipt.concealment();
    let producer = &receipt.producer;
    let render = &receipt.render;
    let counts = receipt.counts;

    let mut observations = Map::new();
    // A count is written as an integer even though every observation is read
    // back as a number, because the document is also read by people and a
    // packet count of `12000.0` invites its reader to wonder what the tenths
    // were.
    let mut observe = |name: &str, value: f64| {
        let stated = if value.fract() == 0.0 && value.abs() < 9e15 {
            json!(value as i64)
        } else {
            json!(value)
        };
        observations.insert(name.to_string(), stated);
    };

    observe("rtp_received", counts.received as f64);
    observe("rtp_unique", receipt.loss.unique() as f64);
    observe("rtp_lost", receipt.loss.lost() as f64);
    // Stated rather than left to the evaluator to add, because a denominator
    // nobody named is a denominator two readers compute differently.
    observe("rtp_expected", receipt.loss.expected() as f64);
    observe("rtp_duplicate", counts.duplicate as f64);
    observe("rtp_reordered", counts.reordered as f64);
    observe("rtp_late", counts.late as f64);
    observe("rtp_off_grid", counts.off_grid as f64);
    observe("rtp_oversize", counts.oversize as f64);
    observe("rtp_foreign_ssrc", receipt.foreign_ssrc as f64);

    // Signed, and in milliseconds because that is the unit the target is in:
    // this is the distribution the target has to cover, and a reader comparing
    // it against a 10 ms target should not have to divide.
    if let Some(delay) = receipt.arrival_delay_us {
        observe("arrival_delay_min_ms", delay_ms(delay.min));
        observe("arrival_delay_p50_ms", delay_ms(delay.p50));
        observe("arrival_delay_p95_ms", delay_ms(delay.p95));
        observe("arrival_delay_p99_ms", delay_ms(delay.p99));
        observe("arrival_delay_max_ms", delay_ms(delay.max));
    }

    // A6.1, split by which of a captured packet's two Opus frames a datagram
    // carried. The classes are counted from the first frame this receiver
    // accepted and are labelled by their offset from it and never by the words
    // first and second: the RTP base is random by RFC 3550's requirement and this
    // end joins a stream already running, so which offset is a packet's first
    // frame is a bit that lives in the sender's envelope and nowhere here. The
    // anchor is stated so that join is arithmetic somebody else can check.
    //
    // Both step distributions are stated, not one. A step is the lateness of the
    // frame one position later less this frame's, and exactly one of the two
    // classes steps within a captured packet while the other steps across the
    // boundary between two of them. The intra-packet step is the quantity with a
    // predicted value of -4.956 ms and the cross-packet step is its mirror, so a
    // run where the two are near -5 and +5 has confirmed the cadence before
    // anything is labelled, and a run where both are near zero has found a sender
    // that spaced its pair.
    let pair = &receipt.pair;
    if let Some(anchor) = pair.anchor {
        observe("pair_anchor_rtp", f64::from(anchor));
    }
    observe("pair_frame_samples", f64::from(pair.frame_samples));
    observe("pair_samples_dropped", pair.samples_dropped as f64);
    observe("pair_unanchored", pair.unanchored as f64);
    for (index, class) in pair.classes.iter().enumerate() {
        let key = |name: &str| format!("pair_class{index}_{name}");
        observe(&key("offset_samples"), f64::from(class.offset_samples));
        observe(&key("frames"), class.frames as f64);
        observe(&key("late"), class.late as f64);
        observe(&key("underruns"), class.underruns as f64);
        if let Some(delay) = class.delay_us {
            observe(&key("arrival_p50_ms"), signed_ms(delay.p50));
            observe(&key("arrival_p95_ms"), signed_ms(delay.p95));
            observe(&key("arrival_p99_ms"), signed_ms(delay.p99));
            observe(&key("arrival_min_ms"), signed_ms(delay.min));
            observe(&key("arrival_max_ms"), signed_ms(delay.max));
        }
        if let Some(step) = class.step_us {
            observe(&key("step_pairs"), step.count as f64);
            observe(&key("step_p50_ms"), signed_ms(step.p50));
            observe(&key("step_p95_ms"), signed_ms(step.p95));
            observe(&key("step_p99_ms"), signed_ms(step.p99));
            observe(&key("step_min_ms"), signed_ms(step.min));
            observe(&key("step_max_ms"), signed_ms(step.max));
        }
    }

    observe("frames_played", counts.played as f64);
    observe("plc_frames", counts.concealed as f64);
    observe("jitter_underruns", counts.underruns as f64);
    observe("jitter_overruns", counts.overruns as f64);
    observe("jitter_overrun_frames", counts.overrun_frames as f64);
    observe("decode_failures", producer.decode_failures as f64);

    // A percentile over no samples is not zero, it is absent, and a zero here
    // would let the decode budget pass on a run that decoded nothing.
    if let Some(occupancy) = producer.occupancy_us {
        observe("jitter_occupancy_p50_ms", millis(occupancy.p50));
        observe("jitter_occupancy_p95_ms", millis(occupancy.p95));
        observe("jitter_occupancy_p99_ms", millis(occupancy.p99));
        observe("jitter_occupancy_max_ms", millis(occupancy.max));
    }
    if let Some(decode) = producer.decode_us {
        observe("decode_p50_us", decode.p50 as f64);
        observe("decode_p99_us", decode.p99 as f64);
        observe("decode_max_us", decode.max as f64);
    }
    if let Some(interval) = producer.interval_us {
        observe("producer_interval_p50_us", interval.p50 as f64);
        observe("producer_interval_p99_us", interval.p99 as f64);
    }

    observe("render_callbacks", render.callbacks as f64);
    observe("render_underruns", render.underruns as f64);
    observe("render_underrun_frames", render.underrun_frames as f64);
    observe("render_overruns", render.overruns as f64);
    observe("render_overrun_frames", render.overrun_frames as f64);
    observe("render_odd_cycles", render.odd_cycles as f64);
    observe("render_frames_consumed", render.frames_consumed as f64);
    observe("device_start_latency_ms", render.start_latency_ms);
    if let Some(interval) = render.interval_us {
        observe("render_interval_p50_us", interval.p50 as f64);
        observe("render_interval_p99_us", interval.p99 as f64);
    }
    if let Some(occupancy) = render.occupancy_frames {
        observe("ring_occupancy_p50_frames", occupancy.p50 as f64);
        observe("ring_occupancy_min_frames", occupancy.min as f64);
        observe("ring_occupancy_max_frames", occupancy.max as f64);
    }

    // A7.1's Mac half and its third measurement. Every one of these is absent
    // rather than zero when the run could not state it: a rate of 0.000 ppm and a
    // growth of 0 samples are both exactly what a perfect run looks like, so a
    // run that measured no clock at all must not be able to print either.
    if let Some(rate) = render.sink_rate {
        observe("sink_ppm", rate.fitted_ppm);
        observe("sink_ppm_endpoints", rate.endpoints_ppm);
        observe("sink_ppm_error", rate.error_ppm);
        observe("sink_rate_readings", rate.readings as f64);
        observe("sink_rate_span_s", rate.seconds);
        observe("sink_sample_time_samples", rate.samples);
        observe("sink_host_time_scatter_samples", rate.scatter_samples);
    }
    observe("sink_invalid_timestamps", render.invalid_timestamps as f64);

    if let Some(invariant) = receipt.invariant() {
        observe("samples_produced", invariant.produced as f64);
        observe("samples_consumed", invariant.consumed as f64);
        observe("samples_discarded", invariant.discarded as f64);
        observe("samples_concealed_in", invariant.inserted as f64);
        observe("buffer_growth_samples", invariant.growth() as f64);
        observe("buffer_held_samples", invariant.held as f64);
        // Zero is the only value that says the eight counters and the two
        // occupancies agree, so this is the one number here that is a check on the
        // instrument rather than a measurement of the link.
        observe("buffer_invariant_residual", invariant.residual() as f64);
    }

    observe("samples_expected", concealment.expected as f64);
    observe("samples_played", concealment.played as f64);
    observe("concealed_samples", concealment.concealed() as f64);
    observe(
        "worst_window_concealed",
        receipt.worst_window_concealed() as f64,
    );
    observe("windows", receipt.windows.len() as f64);
    observe(
        "deadlines_granted",
        f64::from(u8::from(receipt.deadlines_were_granted())),
    );

    observe("tone_resolution_hz", producer.tone.resolution_hz);
    observe("tone_analysed_frames", producer.tone.analysed_frames as f64);
    // Stated only when both channels were found, so that a run which measured
    // no tone leaves the frequency checks unavailable rather than failing them:
    // a tone nobody could measure is not a tone that came back folded to mono,
    // and a gate that says the second when it means the first sends its reader
    // to the wrong subsystem.
    if let (Some(left), Some(right)) = (producer.tone.left, producer.tone.right) {
        observe("tone_left_hz", left.frequency);
        observe("tone_left_dbfs", left.level_dbfs);
        observe("tone_right_hz", right.frequency);
        observe("tone_right_dbfs", right.level_dbfs);
        observe(
            "tone_channels_distinct",
            f64::from(u8::from(producer.tone.distinct())),
        );
    }

    // A8.1. Stated as observations and nowhere in `checks`, deliberately: this
    // document is also the end-to-end gate's, and a criterion added here would
    // be evaluated by a gate that never asked for it - including on the runs
    // where the curve is legitimately absent, which `xtask verdict` reads as a
    // refusal. The excess criteria live in `tools/jitter-excess.sh`, which is
    // the harness that turns on them.
    //
    // The three counts outside the curve are stated whether or not there is a
    // curve, because they are the reasons there might not be one.
    observe("excess_arrivals", receipt.excess.arrivals as f64);
    observe("excess_arrivals_dropped", receipt.excess.dropped as f64);
    observe("excess_repeated_frames", receipt.excess.repeated as f64);
    observe("excess_blocks", receipt.excess.blocks as f64);
    if let Some(curve) = &receipt.excess.curve {
        observe("excess_population", curve.population as f64);
        // Stream time, from the RTP timestamps alone, so every rate below
        // crosses no clock. Beside the run's span rather than instead of it: the
        // two disagreeing by more than the drift would mean one of them is
        // measuring something else.
        observe("excess_stream_s", curve.stream_seconds);
        observe("excess_frames_missing", curve.frames_missing as f64);
        observe("excess_sequence_breaks", curve.sequence_breaks as f64);
        // Two quantities under names that say which is which. The delay slope is
        // what the correction subtracts; the source rate is its negation and is
        // the one comparable with A7's figure. Reading one as the other cost this
        // gate its first radio run's finding, and a key called `excess_drift_ppm`
        // is exactly the name that invited it.
        observe("excess_delay_slope_ppm", curve.drift.delay_ppm);
        observe(
            "excess_delay_slope_ppm_all_points",
            curve.drift.delay_ppm_all_points,
        );
        observe("excess_source_ppm", curve.drift.source_ppm());
        observe(
            "excess_source_ppm_all_points",
            curve.drift.source_ppm_all_points(),
        );
        observe("excess_drift_blocks", curve.drift.blocks as f64);
        observe("excess_drift_accumulated_ms", curve.drift.accumulated_ms);
        observe("excess_reference_source_ppm", A7_PPM);
        observe(
            "excess_drift_agrees_with_a7",
            f64::from(u8::from(curve.drift_agrees_with(A7_PPM))),
        );
        // The pair cadence, as the threshold that carries its signature or as an
        // absence. Absent rather than zero: a threshold of 0 ms is not a row.
        if let Some(cadence) = curve.pair_cadence() {
            observe("excess_pair_cadence_ms", f64::from(cadence.millis));
            observe("excess_pair_cadence_late", cadence.late as f64);
            observe("excess_pair_cadence_fraction", curve.fraction(cadence.late));
        }
        // Both curves, because the correction is only checkable beside the thing
        // it corrected. The difference at p99 and at the maximum is how many
        // milliseconds of the raw tail were the two clocks rather than the link.
        for (prefix, shape) in [("raw", &curve.raw), ("corrected", &curve.corrected)] {
            let spread = shape.spread;
            observe(&format!("excess_{prefix}_p50_ms"), signed_ms(spread.p50));
            observe(&format!("excess_{prefix}_p95_ms"), signed_ms(spread.p95));
            observe(&format!("excess_{prefix}_p99_ms"), signed_ms(spread.p99));
            observe(&format!("excess_{prefix}_max_ms"), signed_ms(spread.max));
            observe(
                &format!("excess_{prefix}_over_100ms"),
                shape.over_span() as f64,
            );
        }
        observe(
            "excess_drift_removed_p99_ms",
            signed_ms(curve.raw.spread.p99 - curve.corrected.spread.p99),
        );
        observe(
            "excess_drift_removed_max_ms",
            signed_ms(curve.raw.spread.max - curve.corrected.spread.max),
        );
        for threshold in &curve.thresholds {
            let key = |name: &str| format!("excess_{name}_{}ms", threshold.millis);
            observe(&key("late"), threshold.late as f64);
            observe(&key("late_raw"), threshold.late_raw as f64);
            observe(&key("late_fraction"), curve.fraction(threshold.late));
            observe(&key("late_per_min"), curve.per_minute(threshold.late));
            observe(&key("clusters"), threshold.clusters as f64);
            observe(
                &key("clusters_per_min"),
                curve.per_minute(threshold.clusters),
            );
            // A rate a reader may quote and a rate they may not are told apart
            // here rather than in the reader's head. Below thirty clusters the
            // fractional standard error is worse than a fifth and the rate above
            // is a number whose interval covers everything anybody would do with
            // it.
            observe(
                &key("rate_quotable"),
                f64::from(u8::from(threshold.rate_is_quotable())),
            );
            if let Some(frames) = threshold.cluster_frames {
                observe(&key("cluster_frames_p50"), frames.p50 as f64);
                observe(&key("cluster_frames_p95"), frames.p95 as f64);
                observe(&key("cluster_frames_max"), frames.max as f64);
                observe(&key("cluster_ms_max"), frames.max as f64 * frame_ms_f);
            }
            if let Some(worst) = threshold.cluster_worst_us {
                observe(&key("cluster_worst_p50_ms"), signed_ms(worst.p50));
                observe(&key("cluster_worst_max_ms"), signed_ms(worst.max));
            }
            if let Some(gap) = threshold.cluster_gap_frames {
                observe(&key("cluster_gap_min_ms"), gap.min as f64 * frame_ms_f);
                observe(&key("cluster_gap_p50_ms"), gap.p50 as f64 * frame_ms_f);
            }
            // The spread of a rate, from blocks of stream time and never from a
            // binomial over frames: the frames inside a cluster are one event,
            // so a binomial would overstate the precision by the mean cluster
            // size.
            if let Some(blocks) = threshold.block_clusters {
                let rate = 60.0 / excess::BLOCK_SECONDS;
                observe(&key("block_clusters_per_min_min"), blocks.min as f64 * rate);
                observe(&key("block_clusters_per_min_p50"), blocks.p50 as f64 * rate);
                observe(&key("block_clusters_per_min_max"), blocks.max as f64 * rate);
            }
        }
    }

    let exercised: Vec<&str> = [
        ("rtp receive", counts.received > 0),
        ("jitter buffer", concealment.expected > 0),
        ("opus decode", counts.played > 0),
        ("coreaudio render", render.callbacks > 0),
    ]
    .into_iter()
    .filter_map(|(subsystem, reached)| reached.then_some(subsystem))
    .collect();

    let checks = json!([
        {
            "name": "the stream arrived",
            "kind": "must_not_be_zero",
            "reads": "rtp_received",
            "why": "every figure in this document is computed over these datagrams, so a run \
                    that received none makes each of them an absence rather than a number",
        },
        {
            "name": "audio was decoded and not merely concealed",
            "kind": "must_not_be_zero",
            "reads": "frames_played",
            "why": "the concealer produces plausible samples from an empty stream indefinitely, \
                    so a callback count, an underrun count of zero and a concealment figure are \
                    all consistent with a path that carried nothing; a decoded frame is not",
        },
        {
            "name": "source concealment: none of the source was replaced",
            "kind": "must_be_zero",
            "reads": "concealed_samples",
            "population": "samples_expected",
            "why": "expected counts every per-channel sample the playout cursor travelled and \
                    played counts the ones the producer deposited, with a gap concealment \
                    credited because the source's own audio sits either side of it and an \
                    underrun refused because nothing of the source was there. The difference is \
                    source audio the listener was handed an invention in place of, which is a \
                    fidelity loss and not a playout failure - the concealer keeps the device fed \
                    throughout, and forty of the forty envelopes committed under results/audio \
                    report zero render underruns, so this project has never once handed a device \
                    silence. The population is the expected samples, because zero concealed out \
                    of zero expected is a run that carried nothing",
        },
        {
            "name": "playout continuity: the device was never handed silence",
            "kind": "must_be_zero",
            "reads": "render_underruns",
            "population": "render_callbacks",
            "why": "the other half of the pair above, and stated as its own criterion because a \
                    concealment figure quoted alone invites its reader to believe the device was \
                    starved. A cycle the ring could not fill is a whole IO buffer of silence \
                    sent to a device in place of audio, which is audible however small the \
                    sample count beside it looks; the population is the cycles, because a run \
                    in which the device never ran must not pass this by having had no chance to \
                    fail it",
        },
        {
            "name": "no frame arrived in time and failed to decode",
            "kind": "must_be_zero",
            "reads": "decode_failures",
            "population": "frames_played",
            "why": "a payload the decoder refuses is a disagreement between the two ends about \
                    the format, which no amount of buffering repairs and which concealment \
                    hides completely in every other counter here",
        },
        {
            "name": "no packet carried a timestamp off the frame grid",
            "kind": "must_be_zero",
            "reads": "rtp_off_grid",
            "population": "rtp_received",
            "why": "a timestamp that is not a whole number of frames from the one that anchored \
                    the stream is a sender running at another frame duration, and the symptom \
                    is a run that conceals everything and plays nothing",
        },
        {
            "name": "decode p99 under a tenth of the frame",
            "kind": "must_be_below",
            "reads": "decode_p99_us",
            "bound": decode_budget_us,
            "why": format!(
                "a tenth of the {frame_ms} ms frame is {decode_budget_us:.0} us, and the \
                 producer has to decode and deposit inside its period or the device finds an \
                 empty ring; p99 rather than the mean, because a frame late once every hundred \
                 is a click a listener hears"
            ),
        },
        {
            "name": "the left channel arrives as its own tone",
            "kind": "must_be_within",
            "reads": "tone_left_hz",
            "target": CONTRACT.left_hz,
            "tolerance": TONE_TOLERANCE_HZ,
            "why": format!(
                "a packet count and a callback count agree just as happily when the path \
                 carries digital silence, and this project has read that agreement as success; \
                 {} Hz comes out of the samples that were played, at a window resolution of \
                 2 Hz",
                CONTRACT.left_hz,
            ),
        },
        {
            "name": "the right channel arrives as its own tone",
            "kind": "must_be_within",
            "reads": "tone_right_hz",
            "target": CONTRACT.right_hz,
            "tolerance": TONE_TOLERANCE_HZ,
            "why": format!(
                "the two channels carry different frequencies so that channel order is provable \
                 across the whole link rather than assumed, and a right channel reading {} Hz \
                 is what a swap looks like",
                CONTRACT.left_hz,
            ),
        },
        {
            "name": "the played channels stay distinct",
            "kind": "must_not_be_zero",
            "reads": "tone_channels_distinct",
            "why": "two channels reading one frequency is consistent with a fold to mono, with \
                    one channel encoded twice, and with an analysis reading its own scratch \
                    buffer, and every count in this document is consistent with all three",
        },
    ]);

    let loss_percent = if receipt.loss.expected() > 0 {
        receipt.loss.lost() as f64 * 100.0 / receipt.loss.expected() as f64
    } else {
        0.0
    };
    // Across the windows that measured an occupancy at all, so a run whose last
    // window caught no pull does not drag the span down to an absence.
    let window_p50s = || {
        receipt
            .windows
            .iter()
            .filter_map(|row| row.occupancy_us)
            .map(|held| held.p50)
    };
    let lowest_window_p50 = optional_millis(window_p50s().min());
    let highest_window_p50 = optional_millis(window_p50s().max());
    // A8.1's own prose. The findings list is where a run says what it
    // established that no criterion votes on, and the drift comparison belongs
    // here for exactly that reason: A7 measured a pair of crystals directly and
    // this measures the same pair through a radio, so a disagreement is a result
    // about one of the two instruments rather than a defect in this run - and it
    // must not pass unremarked either.
    let excess_findings: Vec<String> = match &receipt.excess.curve {
        None => vec![format!(
            "no excess curve: {} arrivals were filed, {} were dropped for want of room, {} \
             claimed a timeline position twice, and the run covered {} blocks of {:.0} s against \
             the {} a line needs; every figure a curve would have carried is absent rather than \
             zero",
            receipt.excess.arrivals,
            receipt.excess.dropped,
            receipt.excess.repeated,
            receipt.excess.blocks,
            excess::BLOCK_SECONDS,
            3,
        )],
        Some(curve) => {
            let mut findings = vec![
                format!(
                    "the source clock runs {:+.2} ppm referred to this Mac's timebase, as this \
                     run's own per-block minima fit it, against A7's {A7_PPM:+.2} ppm for the \
                     same pair, and {}. That is a delay slope of {:+.2} ppm - a fast source \
                     makes the subtracted RTP term outrun arrival time, so the two have opposite \
                     signs and both are stated because reading one as the other is how this \
                     reported a disagreement on its first radio run. Over the {:.0} s of stream \
                     time it is {:+.2} ms of accumulated skew, which took {:+.3} ms off the p99 \
                     of the raw curve and {:+.3} ms off its maximum. The same fit over every \
                     arrival rather than the minima gives {:+.2} ppm, and the two differing is \
                     the burst sensitivity that chose the estimator",
                    curve.drift.source_ppm(),
                    if curve.drift_agrees_with(A7_PPM) {
                        "the two agree to within a factor of two"
                    } else {
                        "THEY DISAGREE BY MORE THAN A FACTOR OF TWO, so one of the two \
                         measurements is wrong and neither may be cited until that is settled"
                    },
                    curve.drift.delay_ppm,
                    curve.stream_seconds,
                    curve.drift.accumulated_ms,
                    signed_ms(curve.raw.spread.p99 - curve.corrected.spread.p99),
                    signed_ms(curve.raw.spread.max - curve.corrected.spread.max),
                    curve.drift.source_ppm_all_points(),
                ),
                format!(
                    "excess above this run's own best case reached {:.2} ms at p50, {:.2} at \
                     p95, {:.2} at p99 and {:.2} at worst over {} arrivals, with {} of them past \
                     the 100 ms the curve reports out to; the reference is the minimum over the \
                     run and not the first packet, so this is comparable with another run's and \
                     an arrival delay anchored on one datagram is not",
                    signed_ms(curve.corrected.spread.p50),
                    signed_ms(curve.corrected.spread.p95),
                    signed_ms(curve.corrected.spread.p99),
                    signed_ms(curve.corrected.spread.max),
                    curve.population,
                    curve.corrected.over_span(),
                ),
            ];
            // Before the per-threshold rows, because a reader who meets the 5 ms
            // row first will read half the population late as a broken link.
            if let Some(cadence) = curve.pair_cadence() {
                findings.push(format!(
                    "the {} ms row is the sender's pair cadence and not the link: {:.2} per cent \
                     of the population is late there, in clusters of one frame separated by gaps \
                     of one frame, and that alternation is not something a radio can do because \
                     a burst is consecutive by definition. Two Opus frames ride in one captured \
                     packet, so both arrive at one instant while the second sits a frame later \
                     in stream time and its excess is exactly a frame lower - A6.1 measured the \
                     same thing from the other side at -4.996 ms per pair at p50, with 96 per \
                     cent of pairs inside the [-5,-4) ms bucket over 8998, 9000 and 120004 \
                     pairs, and found the first member is the one that goes late in practice at \
                     524 against 384, 476 against 354 and 8594 against 6391. What this \
                     establishes is a floor: a target below the pair spacing cannot hold both \
                     members of a pair, so {} ms is structurally unreachable on this sender for \
                     a reason that has nothing to do with the air. What it does not establish is \
                     that spacing the pair in the sender would be an improvement - that would \
                     also delay the second frame by a frame in real time, and whether the floor \
                     it removes is worth the delay it adds is arithmetic nobody has done",
                    cadence.millis,
                    curve.fraction(cadence.late) * 100.0,
                    cadence.millis,
                ));
            }
            for millis in [5, 10, 20] {
                if let Some(threshold) = curve.at(millis) {
                    findings.push(threshold_finding(curve, threshold, frame_ms_f));
                }
            }
            findings
        }
    };
    let mut findings: Vec<String> = vec![
        format!(
            "{:.3} per cent of the stream was lost over the link, {} packets of the {} the \
             sequence numbers describe",
            loss_percent,
            receipt.loss.lost(),
            receipt.loss.expected(),
        ),
        format!(
            "frames arrived {:.1} ms past their moment at p50, {:.1} at p95, {:.1} at p99 and \
             {:.1} at worst, against a target of {} ms; negative is margin in hand, and the \
             shape is what says whether the target is short by a fixed amount every frame pays \
             or long enough for all but a tail",
            receipt.arrival_delay_us.map_or(0.0, |d| delay_ms(d.p50)),
            receipt.arrival_delay_us.map_or(0.0, |d| delay_ms(d.p95)),
            receipt.arrival_delay_us.map_or(0.0, |d| delay_ms(d.p99)),
            receipt.arrival_delay_us.map_or(0.0, |d| delay_ms(d.max)),
            receipt.target.as_millis_f64().round() as u64,
        ),
        format!(
            "the jitter buffer held {:.1} ms at p50 and {:.1} ms at p99 against a {} ms target, \
             measured after serving each frame",
            producer.occupancy_us.map_or(0.0, |o| millis(o.p50)),
            producer.occupancy_us.map_or(0.0, |o| millis(o.p99)),
            receipt.target.as_millis_f64().round() as u64,
        ),
        format!(
            "window by window that occupancy went from {} ms at p50 in the first of {} windows \
             to {} in the last, staying between {} and {}; a run-wide percentile cannot be \
             separated back into these, so whether the buffer grew during the arm is readable \
             here and nowhere above",
            window_p50(receipt.windows.first()),
            receipt.windows.len(),
            window_p50(receipt.windows.last()),
            lowest_window_p50,
            highest_window_p50,
        ),
        format!(
            "the ring sat at {} frames of {} at p50, having been primed to {}; the difference is \
             the {:.1} ms `AudioDeviceStart` spent getting the device going, during which the \
             producer kept depositing and the device consumed nothing, so it is the ring's \
             contribution to latency and it belongs to the device rather than to the link",
            render.occupancy_frames.map_or(0, |o| o.p50),
            render.ring_frames,
            render.ring_prime_frames,
            render.start_latency_ms,
        ),
    ];
    findings.extend(excess_findings);

    // A clock behind the epoch is a machine whose time nobody set, and a stamp
    // of zero says that rather than a date in 1969.
    let started_unix_ms = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);

    let windows: Vec<Value> = receipt
        .windows
        .iter()
        .map(|row| {
            // Null rather than zero for a window nothing was pulled in, and the
            // pull count beside the figures so that a reader fitting a slope
            // across windows can see which of them were measured over a full
            // ten seconds and which over the remainder at the end of a run.
            let held = row.occupancy_us;
            json!({
                "seconds": row.seconds,
                "rtp_received": row.rtp_received,
                "rtp_lost": row.rtp_lost,
                "plc_frames": row.plc_frames,
                "frames_played": row.frames_played,
                "jitter_underruns": row.jitter_underruns,
                "render_callbacks": row.render_callbacks,
                "render_underruns": row.render_underruns,
                "render_overruns": row.render_overruns,
                "samples_expected": row.expected_samples,
                "samples_played": row.played_samples,
                "concealed_samples": row.concealed(),
                "jitter_occupancy_pulls": held.map_or(0, |held| held.count),
                "jitter_occupancy_min_ms": held.map(|held| millis(held.min)),
                "jitter_occupancy_p50_ms": held.map(|held| millis(held.p50)),
                "jitter_occupancy_p95_ms": held.map(|held| millis(held.p95)),
                "jitter_occupancy_p99_ms": held.map(|held| millis(held.p99)),
                "jitter_occupancy_max_ms": held.map(|held| millis(held.max)),
            })
        })
        .collect();

    // The whole table, structured, beside the flat observations rather than
    // instead of them. Every number a criterion turns on is above as its own
    // observation, because that is where `xtask verdict` reads; this is for the
    // reader who wants the shape - a histogram is 401 numbers and a flat map is
    // no place for it, and a curve nobody can plot is a curve nobody checks.
    //
    // Under `environment` because that is the one part of the document schema
    // that takes free-form values, which is also where the window rows live.
    let excess = excess_table(&receipt.excess, frame_ms_f);

    let document = json!({
        "gate": "audio-e2e-gate",
        "run": {
            "started_unix_ms": started_unix_ms,
            "span_s": receipt.span_seconds,
            "args": {
                "bind": receipt.bind.to_string(),
                "frame_ms": frame_ms,
                "target_ms": receipt.target.as_millis_f64(),
                "buffer_frames": render.buffer_frames,
                "ring_frames": render.ring_frames,
            },
            "commit": commit,
            "arm": arm,
        },
        "environment": {
            "device": render.device,
            // Beside the name rather than instead of it: a run that inherited
            // its device measured whatever the system was pointing at, which is
            // a different provenance from one that asked for this endpoint by
            // name, and no reader can recover that from the name alone.
            "device_chosen": render.chosen.to_string(),
            "sample_rate_hz": render.format.sample_rate,
            "channels": render.format.channels,
            "io_buffer_frames": render.buffer_frames,
            "ring_frames": render.ring_frames,
            "ring_prime_frames": render.ring_prime_frames,
            "jitter_target_ms": receipt.target.as_millis_f64(),
            "jitter_ceiling_ms": receipt.ceiling.as_millis_f64(),
            "jitter_slots": receipt.slots,
            "frame_ms": frame_ms,
            "ssrc": receipt.ssrc.map(|ssrc| ssrc.0),
            // Ahead of the counters in the keyed block and stated here too,
            // because a counter means one thing under a deadline and another
            // without one, and a reader who takes the numbers before reading
            // this has already been misled.
            "producer_scheduled_as": producer.scheduled_as.to_string(),
            "receiver_scheduled_as": receipt.receiver_scheduled_as.to_string(),
            "device_span_s": render.span_seconds,
            "measurements_dropped": render.samples_dropped,
            "windows": windows,
            "excess": excess,
        },
        "declared": DECLARED,
        "exercised": exercised,
        "observations": Value::Object(observations),
        "checks": checks,
        "findings": findings,
    });

    format!("{document:#}\n")
}

/// One threshold in prose, for a reader who will not open the table.
///
/// Both halves, always. The fraction says how much of the stream a target of
/// this size would have discarded and the cluster figures say what that would
/// have sounded like, and neither is recoverable from the other: 100 isolated
/// late frames and 20 bursts of five are the same fraction and are not the same
/// experience.
fn threshold_finding(curve: &ExcessCurve, threshold: &Threshold, frame_ms: f64) -> String {
    let rate = if threshold.rate_is_quotable() {
        format!(
            "{:.2} clusters a minute",
            curve.per_minute(threshold.clusters)
        )
    } else {
        format!(
            "{} clusters in the whole run, which is under the {} a rate needs, so no rate is \
             quoted",
            threshold.clusters,
            excess::MINIMUM_CLUSTERS
        )
    };
    let shape = match (threshold.cluster_frames, threshold.cluster_gap_frames) {
        (Some(frames), gap) => format!(
            "clusters of {} frames at p50, {} at p95 and {} at worst - {:.0} ms of timeline in \
             the worst one - separated by {}",
            frames.p50,
            frames.p95,
            frames.max,
            frames.max as f64 * frame_ms,
            gap.map_or_else(
                || "nothing, because one cluster has no gap beside it".to_string(),
                |gap| format!(
                    "{:.0} ms at p50 and {:.0} ms at closest",
                    gap.p50 as f64 * frame_ms,
                    gap.min as f64 * frame_ms
                )
            ),
        ),
        (None, _) => "no cluster at all, so nothing to describe the shape of".to_string(),
    };
    format!(
        "a {} ms target would have left {} frames of {} past their moment, {:.4} per cent, at \
         {:.2} a minute and {rate}: {shape}",
        threshold.millis,
        threshold.late,
        curve.population,
        curve.fraction(threshold.late) * 100.0,
        curve.per_minute(threshold.late),
    )
}

/// The whole curve as a structure, or the reasons there is not one.
fn excess_table(report: &ExcessReport, frame_ms: f64) -> Value {
    // The counts that explain an absent curve are outside the curve, so they are
    // stated either way. A table holding `"curve": null` and nothing else would
    // send its reader looking for a reason that is not in the document.
    let mut table = Map::new();
    table.insert("arrivals".into(), json!(report.arrivals));
    table.insert("arrivals_dropped".into(), json!(report.dropped));
    table.insert("repeated_frames".into(), json!(report.repeated));
    table.insert("blocks".into(), json!(report.blocks));
    table.insert("block_seconds".into(), json!(excess::BLOCK_SECONDS));
    table.insert("bin_us".into(), json!(excess::BIN_US));
    table.insert("minimum_clusters".into(), json!(excess::MINIMUM_CLUSTERS));
    let Some(curve) = &report.curve else {
        return Value::Object(table);
    };

    table.insert("population".into(), json!(curve.population));
    table.insert("stream_s".into(), json!(curve.stream_seconds));
    table.insert("frames_missing".into(), json!(curve.frames_missing));
    table.insert("sequence_breaks".into(), json!(curve.sequence_breaks));
    table.insert(
        "drift".into(),
        json!({
            // The source clock's rate first, because it is the one a reader
            // compares with anything else, and the delay slope beside it because
            // it is what the correction actually subtracts. Opposite signs, and
            // both named, for the reason `excess::Drift` derives.
            "source_ppm": curve.drift.source_ppm(),
            "source_ppm_all_points": curve.drift.source_ppm_all_points(),
            "delay_slope_ppm": curve.drift.delay_ppm,
            "delay_slope_ppm_all_points": curve.drift.delay_ppm_all_points,
            "blocks_fitted": curve.drift.blocks,
            "accumulated_ms": curve.drift.accumulated_ms,
            "reference_source_ppm": A7_PPM,
            "agrees_with_reference": curve.drift_agrees_with(A7_PPM),
        }),
    );
    table.insert(
        "pair_cadence_ms".into(),
        json!(curve.pair_cadence().map(|threshold| threshold.millis)),
    );
    for (name, shape) in [("raw", &curve.raw), ("corrected", &curve.corrected)] {
        table.insert(
            name.into(),
            json!({
                "p50_ms": signed_ms(shape.spread.p50),
                "p95_ms": signed_ms(shape.spread.p95),
                "p99_ms": signed_ms(shape.spread.p99),
                "max_ms": signed_ms(shape.spread.max),
                "over_100ms": shape.over_span(),
                // Occupied bins only, as `[floor_ms, count]`. Four hundred bins
                // of which nine are filled is a line whose shape nobody can see,
                // and the shape above 20 ms is the entire diagnostic this curve
                // exists to produce.
                "bins": shape
                    .bins
                    .iter()
                    .enumerate()
                    .filter(|&(_, &count)| count > 0)
                    .map(|(bin, &count)| json!([bin as f64 * excess::BIN_US as f64 / 1_000.0, count]))
                    .collect::<Vec<Value>>(),
            }),
        );
    }
    let thresholds: Vec<Value> = curve
        .thresholds
        .iter()
        .map(|threshold| {
            let frames = threshold.cluster_frames;
            let worst = threshold.cluster_worst_us;
            let gap = threshold.cluster_gap_frames;
            let blocks = threshold.block_clusters;
            let per_block = 60.0 / excess::BLOCK_SECONDS;
            json!({
                "ms": threshold.millis,
                "late": threshold.late,
                "late_raw": threshold.late_raw,
                "late_fraction": curve.fraction(threshold.late),
                "late_per_min": curve.per_minute(threshold.late),
                "clusters": threshold.clusters,
                "clusters_per_min": curve.per_minute(threshold.clusters),
                "rate_quotable": threshold.rate_is_quotable(),
                "cluster_frames_p50": frames.map(|f| f.p50),
                "cluster_frames_p95": frames.map(|f| f.p95),
                "cluster_frames_max": frames.map(|f| f.max),
                "cluster_ms_max": frames.map(|f| f.max as f64 * frame_ms),
                "cluster_worst_p50_ms": worst.map(|w| signed_ms(w.p50)),
                "cluster_worst_p95_ms": worst.map(|w| signed_ms(w.p95)),
                "cluster_worst_max_ms": worst.map(|w| signed_ms(w.max)),
                "cluster_gap_min_ms": gap.map(|g| g.min as f64 * frame_ms),
                "cluster_gap_p50_ms": gap.map(|g| g.p50 as f64 * frame_ms),
                "cluster_gap_max_ms": gap.map(|g| g.max as f64 * frame_ms),
                "block_clusters_per_min_min": blocks.map(|b| b.min as f64 * per_block),
                "block_clusters_per_min_p50": blocks.map(|b| b.p50 as f64 * per_block),
                "block_clusters_per_min_max": blocks.map(|b| b.max as f64 * per_block),
            })
        })
        .collect();
    table.insert("thresholds".into(), json!(thresholds));
    Value::Object(table)
}

const NOTHING: Percentiles = Percentiles {
    count: 0,
    min: 0,
    p50: 0,
    p95: 0,
    p99: 0,
    max: 0,
};

/// The same absence in the biased units the arrival delay is kept in, so a run
/// that measured none prints zero rather than minus ten seconds.
const UNBIASED_ZERO: Percentiles = Percentiles {
    count: 0,
    min: DELAY_BIAS_US as u64,
    p50: DELAY_BIAS_US as u64,
    p95: DELAY_BIAS_US as u64,
    p99: DELAY_BIAS_US as u64,
    max: DELAY_BIAS_US as u64,
};

fn millis(micros: u64) -> f64 {
    micros as f64 / 1_000.0
}

/// Milliseconds, or the word for a figure a run did not measure.
///
/// Written out rather than printed as zero, because zero occupancy is a buffer
/// that ran dry and this is a window in which nothing was pulled at all.
fn optional_millis(micros: Option<u64>) -> String {
    micros.map_or_else(
        || "none".to_string(),
        |micros| format!("{:.1}", millis(micros)),
    )
}

/// One window's median occupancy, in milliseconds.
fn window_p50(row: Option<&WindowRow>) -> String {
    optional_millis(row.and_then(|row| row.occupancy_us).map(|held| held.p50))
}

/// A biased arrival delay as signed milliseconds, positive when late.
fn delay_ms(biased: u64) -> f64 {
    unbias_micros(biased) as f64 / 1_000.0
}

/// Microseconds that are already signed, as milliseconds.
///
/// Beside [`delay_ms`] rather than folded into it, because the two take their
/// input in different units: the arrival delay lives in a store that could not
/// hold a negative number and the pair figures come back already unbiased. One
/// function taking both would be a function whose caller has to remember which
/// kind it is holding.
fn signed_ms(micros: i64) -> f64 {
    micros as f64 / 1_000.0
}

/// One signed distribution, or the word for a run that measured none.
///
/// Absent rather than a row of zeros, because zero is the answer a sender that
/// spaced its pair would give and a run that closed no pair at all must not be
/// able to print it.
fn spread(f: &mut fmt::Formatter<'_>, key: &str, values: Option<Spread>) -> fmt::Result {
    match values {
        Some(held) => writeln!(
            f,
            "{key} p50 {:.3} p95 {:.3} p99 {:.3} min {:.3} max {:.3} over {}",
            signed_ms(held.p50),
            signed_ms(held.p95),
            signed_ms(held.p99),
            signed_ms(held.min),
            signed_ms(held.max),
            held.count
        ),
        None => writeln!(f, "{key} none measured"),
    }
}

/// The occupied buckets of a step histogram, as a floor in milliseconds against
/// its count.
///
/// Only the occupied ones. Twenty-eight buckets of which two are filled is a
/// line whose shape nobody can see, and the shape is the whole reason this is
/// printed beside percentiles that already say where the middle is. The two
/// tails are named rather than numbered, because a tail has no floor and a run
/// with anything in one is a run whose steps left the span this audit expected.
fn buckets(f: &mut fmt::Formatter<'_>, key: &str, counts: &[u64; BUCKETS]) -> fmt::Result {
    write!(f, "{key}")?;
    for (index, count) in counts.iter().enumerate() {
        if *count == 0 {
            continue;
        }
        match bucket_floor_us(index) {
            Some(floor) => write!(f, " {}:{count}", floor / 1_000)?,
            None if index == 0 => write!(f, " under:{count}")?,
            None => write!(f, " over:{count}")?,
        }
    }
    writeln!(f)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// The curve and the cluster table, in the form a person reads when a gate has
/// just refused.
///
/// Every row carries both halves of what a threshold costs, and a rate is
/// printed as a dash rather than as a number when the run did not see enough
/// clusters to state one. A rate from four events printed to two decimals is a
/// precision the measurement does not have, and this project has quoted one
/// before.
fn excess_block(f: &mut fmt::Formatter<'_>, report: &ExcessReport, frame_ms: f64) -> fmt::Result {
    writeln!(
        f,
        "excess arrivals {} dropped {} repeated {} blocks {}",
        report.arrivals, report.dropped, report.repeated, report.blocks
    )?;
    let Some(curve) = &report.curve else {
        return writeln!(
            f,
            "excess curve absent, so no threshold below has been measured either way"
        );
    };
    writeln!(
        f,
        "excess population {} over {:.1} s of stream time, {} frames missing, {} sequence breaks",
        curve.population, curve.stream_seconds, curve.frames_missing, curve.sequence_breaks
    )?;
    writeln!(
        f,
        "excess source ppm {:+.2} against A7 {A7_PPM:+.2} agrees {} all points {:+.2}",
        curve.drift.source_ppm(),
        yes_no(curve.drift_agrees_with(A7_PPM)),
        curve.drift.source_ppm_all_points()
    )?;
    writeln!(
        f,
        "excess delay slope ppm {:+.2} over {} blocks, {:+.2} ms accumulated",
        curve.drift.delay_ppm, curve.drift.blocks, curve.drift.accumulated_ms
    )?;
    writeln!(
        f,
        "excess pair cadence {}",
        curve.pair_cadence().map_or_else(
            || "none: no row alternates, so no row is the sender's packing".to_string(),
            |cadence| format!(
                "{} ms, {:.2} per cent of the population in clusters of one frame separated by \
                 one, which is two frames per captured packet and not the link",
                cadence.millis,
                curve.fraction(cadence.late) * 100.0
            )
        )
    )?;
    for (name, shape) in [("raw", &curve.raw), ("corrected", &curve.corrected)] {
        writeln!(
            f,
            "excess {name} ms p50 {:.2} p95 {:.2} p99 {:.2} max {:.2} over 100 ms {}",
            signed_ms(shape.spread.p50),
            signed_ms(shape.spread.p95),
            signed_ms(shape.spread.p99),
            signed_ms(shape.spread.max),
            shape.over_span()
        )?;
    }
    writeln!(
        f,
        "excess table    T  late    frac%   /min  clusters  /min  frames p50/p95/max  worst ms  \
         gap ms p50/min"
    )?;
    for threshold in &curve.thresholds {
        let rate = if threshold.rate_is_quotable() {
            format!("{:5.2}", curve.per_minute(threshold.clusters))
        } else {
            "    -".to_string()
        };
        let frames = threshold.cluster_frames;
        let worst = threshold.cluster_worst_us;
        let gap = threshold.cluster_gap_frames;
        writeln!(
            f,
            "excess row  {:4}  {:6}  {:6.3}  {:6.2}  {:8}  {rate}  {:>18}  {:8}  {:>14}",
            threshold.millis,
            threshold.late,
            curve.fraction(threshold.late) * 100.0,
            curve.per_minute(threshold.late),
            threshold.clusters,
            frames.map_or_else(
                || "-".to_string(),
                |frames| format!("{}/{}/{}", frames.p50, frames.p95, frames.max)
            ),
            worst.map_or_else(
                || "-".to_string(),
                |worst| format!("{:.1}", signed_ms(worst.max))
            ),
            gap.map_or_else(
                || "-".to_string(),
                |gap| format!(
                    "{:.0}/{:.0}",
                    gap.p50 as f64 * frame_ms,
                    gap.min as f64 * frame_ms
                )
            ),
        )?;
    }
    Ok(())
}

impl fmt::Display for Receipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let counts = self.counts;
        let concealment = self.concealment();
        let producer = &self.producer;
        let render = &self.render;
        let occupancy = producer.occupancy_us.unwrap_or(NOTHING);
        let decode = producer.decode_us.unwrap_or(NOTHING);
        let conceal = producer.conceal_us.unwrap_or(NOTHING);
        let pull_interval = producer.interval_us.unwrap_or(NOTHING);
        let cycle = render.interval_us.unwrap_or(NOTHING);
        let ring = render.occupancy_frames.unwrap_or(NOTHING);
        let delay = self.arrival_delay_us.unwrap_or(UNBIASED_ZERO);

        // The two policies come before every counter below them, because the
        // counters mean one thing under a deadline and another without one.
        writeln!(f, "producer scheduled as {}", producer.scheduled_as)?;
        writeln!(f, "receiver scheduled as {}", self.receiver_scheduled_as)?;
        writeln!(
            f,
            "every deadline granted {}",
            yes_no(self.deadlines_were_granted())
        )?;

        writeln!(f, "rtp received {}", counts.received)?;
        writeln!(
            f,
            "rtp lost {} of {} expected",
            self.loss.lost(),
            self.loss.expected()
        )?;
        writeln!(f, "rtp duplicate {}", counts.duplicate)?;
        writeln!(f, "rtp reordered {}", counts.reordered)?;
        writeln!(f, "rtp late {}", counts.late)?;
        writeln!(
            f,
            "arrival delay ms p50 {:.1} p95 {:.1} p99 {:.1} max {:.1} min {:.1}",
            delay_ms(delay.p50),
            delay_ms(delay.p95),
            delay_ms(delay.p99),
            delay_ms(delay.max),
            delay_ms(delay.min)
        )?;

        // A6.1. The two classes of the stream, and the step out of each of them.
        //
        // Labelled by offset from the anchor and not as first and second: the
        // sender's RTP base is random and this end joined a stream already
        // running, so the position of a class inside a captured packet is a bit
        // that comes from the sender's envelope. `pair anchor rtp` is this end's
        // half of that join and `rtp base` is the other.
        writeln!(
            f,
            "pair anchor rtp {} frame samples {}",
            self.pair
                .anchor
                .map_or("none".to_string(), |anchor| anchor.to_string()),
            self.pair.frame_samples
        )?;
        for (index, class) in self.pair.classes.iter().enumerate() {
            writeln!(
                f,
                "pair class {index} offset samples {} frames {} late {} underruns {}",
                class.offset_samples, class.frames, class.late, class.underruns
            )?;
            spread(f, &format!("pair class {index} arrival ms"), class.delay_us)?;
            spread(f, &format!("pair class {index} step ms"), class.step_us)?;
            buckets(
                f,
                &format!("pair class {index} step histogram ms"),
                &class.step_buckets,
            )?;
        }
        writeln!(
            f,
            "pair measurements dropped {} unanchored {}",
            self.pair.samples_dropped, self.pair.unanchored
        )?;

        // A8.1, which is the table this whole run exists to produce and is
        // therefore printed rather than left to whoever opens the JSON. One row
        // per threshold, because the point of the population is that it answers
        // every threshold at once and a reader has to be able to see the curve
        // fall.
        excess_block(f, &self.excess, f64::from(self.config.frame.millis()))?;
        writeln!(f, "frames played {}", counts.played)?;
        writeln!(f, "plc frames {}", counts.concealed)?;
        writeln!(
            f,
            "jitter occupancy ms p50 {:.1} p95 {:.1} p99 {:.1} max {:.1}",
            millis(occupancy.p50),
            millis(occupancy.p95),
            millis(occupancy.p99),
            millis(occupancy.max)
        )?;
        writeln!(f, "jitter underruns {}", counts.underruns)?;
        writeln!(
            f,
            "jitter overruns {} dropping {} frames",
            counts.overruns, counts.overrun_frames
        )?;
        writeln!(f, "decode us p50 {} p99 {}", decode.p50, decode.p99)?;
        writeln!(f, "render callbacks {}", render.callbacks)?;
        writeln!(
            f,
            "render underruns {} over {} frames",
            render.underruns, render.underrun_frames
        )?;
        writeln!(
            f,
            "render overruns {} over {} frames",
            render.overruns, render.overrun_frames
        )?;
        // The pair the phase is decided on, and the two lines a reader should
        // look at first when everything above them looks healthy. Both, never
        // one: source concealment alone reads as a starved device to anybody
        // who has not been told that the concealer never stops feeding one.
        writeln!(
            f,
            "source expected {} played {} nothing concealed {}",
            concealment.expected,
            concealment.played,
            yes_no(concealment.nothing_concealed())
        )?;
        writeln!(
            f,
            "playout continuity {} render underruns over {} callbacks",
            render.underruns, render.callbacks
        )?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(producer.tone.left),
            hertz(producer.tone.right)
        )?;

        // A7.1. The device's own rate and the samples invariant, kept together
        // because neither means much without the other: the rate is a slope
        // against this machine's monotonic clock and the invariant is a count of
        // audio, and it is the second that says whether the first can be believed
        // across the two machines.
        match render.sink_rate {
            Some(rate) => {
                writeln!(
                    f,
                    "sink ppm {:+.3} error {:.3} over {} readings and {:.3} s",
                    rate.fitted_ppm, rate.error_ppm, rate.readings, rate.seconds
                )?;
                writeln!(f, "sink ppm endpoints {:+.3}", rate.endpoints_ppm)?;
                writeln!(f, "sink sample time samples {:.0}", rate.samples)?;
                writeln!(
                    f,
                    "sink host time scatter {:.2} samples estimates agree {}",
                    rate.scatter_samples,
                    yes_no(rate.estimates_agree())
                )?;
            }
            None => writeln!(f, "sink ppm unavailable")?,
        }
        writeln!(f, "sink invalid timestamps {}", render.invalid_timestamps)?;
        match self.invariant() {
            Some(invariant) => {
                writeln!(
                    f,
                    "samples produced {} consumed {} discarded {} concealed in {}",
                    invariant.produced, invariant.consumed, invariant.discarded, invariant.inserted
                )?;
                writeln!(
                    f,
                    "buffer growth {} samples held {} residual {}",
                    invariant.growth(),
                    invariant.held,
                    invariant.residual()
                )?;
            }
            None => writeln!(f, "samples produced unavailable, no frame was admitted")?,
        }

        // Everything below is for a person reading the run rather than for the
        // harness parsing it, in the order somebody asking "why those numbers"
        // would want it.
        writeln!(
            f,
            "concealed {} samples over {} frame periods",
            concealment.concealed(),
            concealment.expected / self.frame_samples().max(1)
        )?;
        writeln!(
            f,
            "worst window concealed {} samples",
            self.worst_window_concealed()
        )?;
        writeln!(
            f,
            "tone channels distinct {}",
            yes_no(producer.tone.distinct())
        )?;
        writeln!(
            f,
            "tone resolution {:.2} hz over {} frames",
            producer.tone.resolution_hz, producer.tone.analysed_frames
        )?;
        writeln!(f, "rtp off grid {}", counts.off_grid)?;
        writeln!(f, "rtp oversize {}", counts.oversize)?;
        writeln!(f, "rtp foreign ssrc {}", self.foreign_ssrc)?;
        writeln!(f, "decode failures {}", producer.decode_failures)?;
        writeln!(f, "conceal us p50 {} p99 {}", conceal.p50, conceal.p99)?;
        writeln!(
            f,
            "producer interval us p50 {} p99 {} max {}",
            pull_interval.p50, pull_interval.p99, pull_interval.max
        )?;
        writeln!(
            f,
            "render interval us p50 {} p99 {} max {}",
            cycle.p50, cycle.p99, cycle.max
        )?;
        writeln!(
            f,
            "ring occupancy frames min {} p50 {} max {} of {} primed to {} after a {:.1} ms \
             device start",
            ring.min,
            ring.p50,
            ring.max,
            render.ring_frames,
            render.ring_prime_frames,
            render.start_latency_ms
        )?;
        writeln!(
            f,
            "ring frames refused {} consumed {}",
            producer.refused_frames, render.frames_consumed
        )?;
        writeln!(
            f,
            "jitter target ms {} ceiling ms {} over {} slots",
            self.target.as_millis_f64().round() as u64,
            self.ceiling.as_millis_f64().round() as u64,
            self.slots
        )?;
        writeln!(f, "frame ms {}", self.config.frame.millis())?;
        writeln!(
            f,
            "device {} ({}) at {} with a {} frame io buffer",
            render.device, render.chosen, render.format, render.buffer_frames
        )?;
        writeln!(
            f,
            "bind {} ssrc {}",
            self.bind,
            self.ssrc
                .map_or("none".to_string(), |ssrc| ssrc.0.to_string())
        )?;
        writeln!(f, "odd cycles {}", render.odd_cycles)?;
        writeln!(f, "measurements dropped {}", render.samples_dropped)?;
        writeln!(
            f,
            "span s {:.1} device span s {:.1}",
            self.span_seconds, render.span_seconds
        )?;

        // The windows last, because a reader who needs them is looking for
        // when something started rather than whether it did.
        for (index, row) in self.windows.iter().enumerate() {
            writeln!(f, "window {index} {row}")?;
        }
        Ok(())
    }
}

impl fmt::Display for WindowRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let rate = |count: u64| {
            if self.seconds > 0.0 {
                count as f64 / self.seconds
            } else {
                0.0
            }
        };
        write!(
            f,
            "s {:.1} rtp/s {:.0} lost {} plc {} played {} jitter underruns {} callbacks {} \
             underruns {} overruns {} expected {} played {} concealed {}",
            self.seconds,
            rate(self.rtp_received),
            self.rtp_lost,
            self.plc_frames,
            self.frames_played,
            self.jitter_underruns,
            self.render_callbacks,
            self.render_underruns,
            self.render_overruns,
            self.expected_samples,
            self.played_samples,
            self.concealed()
        )?;
        // After the concealed count rather than beside the other jitter
        // figures, because it is what a reader asks next: concealment with the
        // buffer at its target is audio that arrived too late to be in it, with
        // the buffer empty it is audio that did not arrive, and with the buffer
        // at its ceiling it is audio this end threw away.
        //
        // Absent prints as an absence. Zeroes here would read as a buffer that
        // ran dry for ten seconds, which is the opposite of a window in which
        // nothing was pulled at all.
        match self.occupancy_us {
            Some(held) => write!(
                f,
                " occupancy ms p50 {:.1} p95 {:.1} p99 {:.1} min {:.1} max {:.1} over {} pulls",
                millis(held.p50),
                millis(held.p95),
                millis(held.p99),
                millis(held.min),
                millis(held.max),
                held.count
            )?,
            None => write!(f, " occupancy none")?,
        }
        // The same buffers in samples rather than in frame-quantised
        // milliseconds, and cumulative rather than this window's. A slope through
        // this column is A7.1's drift; the occupancy beside it is quantised to one
        // frame and cannot carry one at any run length.
        write!(f, " held {}", self.held_samples)
    }
}
