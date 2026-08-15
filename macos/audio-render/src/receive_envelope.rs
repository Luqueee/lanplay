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

/// What the gate covers, and what a run has to reach to be allowed to claim it.
const DECLARED: [&str; 4] = [
    "rtp receive",
    "jitter buffer",
    "opus decode",
    "coreaudio render",
];

pub fn document(receipt: &Receipt, started: SystemTime, arm: &str, commit: Option<&str>) -> String {
    let frame_ms = receipt.config.frame.millis();
    let decode_budget_us = f64::from(frame_ms) * 1_000.0 / DECODE_BUDGET_FRACTION;
    let continuity = receipt.continuity();
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

    observe("samples_expected", continuity.expected as f64);
    observe("samples_played", continuity.played as f64);
    observe("continuity_hole", continuity.hole() as f64);
    observe("worst_window_hole", receipt.worst_window_hole() as f64);
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

    let exercised: Vec<&str> = [
        ("rtp receive", counts.received > 0),
        ("jitter buffer", continuity.expected > 0),
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
                    so a callback count, an underrun count of zero and a continuity figure are \
                    all consistent with a path that carried nothing; a decoded frame is not",
        },
        {
            "name": "every sample the stream produced reached the device",
            "kind": "must_be_zero",
            "reads": "continuity_hole",
            "population": "samples_expected",
            "why": "this is the criterion the phase turns on: expected counts every per-channel \
                    sample the playout cursor travelled and played counts the ones the producer \
                    deposited, with a gap concealment credited because the waveform continued \
                    across it and an underrun refused because nothing of the stream was there. \
                    The population is the expected samples, because a hole of zero over an \
                    expectation of zero is a run that carried nothing",
        },
        {
            "name": "the device was never handed silence",
            "kind": "must_be_zero",
            "reads": "render_underruns",
            "population": "render_callbacks",
            "why": "a cycle the ring could not fill is a whole IO buffer of silence sent to a \
                    device in place of audio, which is audible however small the sample count \
                    beside it looks; the population is the cycles, because no cycles means no \
                    silence for a trivial reason",
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
    let findings = json!([
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
            "the ring sat at {} frames of {} at p50, having been primed to {}; the difference is \
             the {:.1} ms `AudioDeviceStart` spent getting the device going, during which the \
             producer kept depositing and the device consumed nothing, so it is the ring's \
             contribution to latency and it belongs to the device rather than to the link",
            render.occupancy_frames.map_or(0, |o| o.p50),
            render.ring_frames,
            render.ring_prime_frames,
            render.start_latency_ms,
        ),
    ]);

    // A clock behind the epoch is a machine whose time nobody set, and a stamp
    // of zero says that rather than a date in 1969.
    let started_unix_ms = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);

    let windows: Vec<Value> = receipt
        .windows
        .iter()
        .map(|row| {
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
                "continuity_hole": row.hole(),
            })
        })
        .collect();

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
        },
        "declared": DECLARED,
        "exercised": exercised,
        "observations": Value::Object(observations),
        "checks": checks,
        "findings": findings,
    });

    format!("{document:#}\n")
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

/// A biased arrival delay as signed milliseconds, positive when late.
fn delay_ms(biased: u64) -> f64 {
    unbias_micros(biased) as f64 / 1_000.0
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl fmt::Display for Receipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let counts = self.counts;
        let continuity = self.continuity();
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
        // The line the phase is decided on, and the one a reader should look
        // at first when everything above it looks healthy.
        writeln!(
            f,
            "continuity expected {} played {} unbroken {}",
            continuity.expected,
            continuity.played,
            yes_no(continuity.unbroken())
        )?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(producer.tone.left),
            hertz(producer.tone.right)
        )?;

        // Everything below is for a person reading the run rather than for the
        // harness parsing it, in the order somebody asking "why those numbers"
        // would want it.
        writeln!(
            f,
            "continuity hole {} samples over {} frame periods",
            continuity.hole(),
            continuity.expected / self.frame_samples().max(1)
        )?;
        writeln!(f, "worst window hole {} samples", self.worst_window_hole())?;
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
            "device {} at {} with a {} frame io buffer",
            render.device, render.format, render.buffer_frames
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
             underruns {} overruns {} expected {} played {} hole {}",
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
            self.hole()
        )
    }
}
