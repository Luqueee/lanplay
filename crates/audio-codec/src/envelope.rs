//! One run of this probe as the gate document `xtask verdict` decides on.
//!
//! The prose report next door stays exactly as it was, and this is emitted
//! beside it rather than instead of it. Both audiences are real: a person whose
//! gate just failed reads the keyed block, and the evaluator reads this, and
//! replacing either with the other trades one for the other. What is gone is
//! the third thing, the regular expression that used to turn the first back
//! into the second.
//!
//! The criteria are stated here because this is what knows the frame duration
//! the bound is derived from, and only stated: every number in `checks` is a
//! parameter, none of them is a conclusion, and nothing in this file decides
//! whether the run passed. `xtask`'s parser is the authority on the shape, and
//! it refuses a document that omits a reason or a population rather than
//! reading one, so a mistake here fails the gate at once and loudly instead of
//! quietly weakening a criterion.

use std::time::{SystemTime, UNIX_EPOCH};

use lanplay_tone_source::tone::CONTRACT;
use serde_json::{Map, Value, json};

use crate::probe::{Measurement, Options};

/// The fraction of a frame the encoder has to stay under.
///
/// A tenth, because "much less than the frame" has to be a number to be a
/// criterion at all: an encoder at a tenth of the audio it is encoding cannot
/// be the term that matters in a budget the rest of this project measures in
/// whole milliseconds, and one above that has to be looked at.
const BUDGET_FRACTION: f64 = 10.0;

/// How far a decoded frequency may sit from the one that went in.
///
/// Five hertz against an analysis window that resolves two, so it is two bins
/// and a margin, and it is two hundredths of the thousand-hertz gap between the
/// two contract tones - wide enough that a correct codec cannot fail it and far
/// too narrow for a channel that carried the other channel's tone to pass.
const TONE_TOLERANCE_HZ: f64 = 5.0;

/// What the gate covers, and what a run has to reach to be allowed to claim it.
const DECLARED: [&str; 3] = ["encoder", "decoder", "tone analysis"];

pub fn document(
    measurement: &Measurement,
    options: Options,
    started: SystemTime,
    span_s: f64,
    commit: Option<&str>,
) -> String {
    let frame_ms = measurement.config.frame.millis();
    let budget_us = f64::from(frame_ms) * 1_000.0 / BUDGET_FRACTION;

    let mut observations = Map::new();
    // A count is written as an integer even though every observation is read
    // back as a number, because the document is also read by people and a
    // packet count of `2000.0` invites its reader to wonder what the tenths
    // were.
    let mut observe = |name: &str, value: f64| {
        let stated = if value.fract() == 0.0 && value.abs() < 9e15 {
            json!(value as i64)
        } else {
            json!(value)
        };
        observations.insert(name.to_string(), stated);
    };

    observe("frames_submitted", measurement.frames_submitted as f64);
    observe("frames_returned", measurement.frames_returned as f64);
    // Stated as its own number rather than left to the evaluator to subtract,
    // because a check names one observation and a difference nobody named is a
    // difference somebody computes twice.
    observe(
        "frame_count_disagreement",
        measurement
            .frames_submitted
            .abs_diff(measurement.frames_returned) as f64,
    );
    observe("packets", measurement.packets as f64);
    observe("total_packet_bytes", measurement.total_packet_bytes as f64);
    observe("effective_kbps", measurement.effective_kbps());

    // A percentile over no samples is not zero, it is absent, and a zero here
    // would let the encode budget pass on a run that encoded nothing.
    for (stem, percentiles) in [
        ("encode", measurement.encode_us),
        ("decode", measurement.decode_us),
    ] {
        if let Some(measured) = percentiles {
            observe(&format!("{stem}_p50_us"), measured.p50 as f64);
            observe(&format!("{stem}_p95_us"), measured.p95 as f64);
            observe(&format!("{stem}_p99_us"), measured.p99 as f64);
            observe(&format!("{stem}_max_us"), measured.max as f64);
        }
    }
    if let Some(bytes) = measurement.packet_bytes {
        observe("packet_bytes_p50", bytes.p50 as f64);
        observe("packet_bytes_p95", bytes.p95 as f64);
        observe("packet_bytes_p99", bytes.p99 as f64);
        observe("packet_bytes_max", bytes.max as f64);
        observe("packet_bytes_min", bytes.min as f64);
    }

    observe("tone_resolution_hz", measurement.tone.resolution_hz);
    observe(
        "tone_analysed_frames",
        measurement.tone.analysed_frames as f64,
    );
    if let Some(left) = measurement.tone.left {
        observe("tone_left_hz", left.frequency);
        observe("tone_left_dbfs", left.level_dbfs);
    }
    if let Some(right) = measurement.tone.right {
        observe("tone_right_hz", right.frequency);
        observe("tone_right_dbfs", right.level_dbfs);
    }
    // Only stated when both channels were found, so that a run which measured
    // no tone leaves the distinctness check unavailable rather than failing it:
    // a tone nobody could measure is not a tone that came back folded to mono,
    // and a gate that says the second when it means the first sends its reader
    // to the wrong subsystem.
    if measurement.tone.left.is_some() && measurement.tone.right.is_some() {
        observe(
            "tone_channels_distinct",
            f64::from(u8::from(measurement.tone.distinct())),
        );
    }

    let exercised: Vec<&str> = [
        ("encoder", measurement.packets > 0),
        ("decoder", measurement.frames_returned > 0),
        ("tone analysis", measurement.tone.analysed_frames > 0),
    ]
    .into_iter()
    .filter_map(|(subsystem, reached)| reached.then_some(subsystem))
    .collect();

    let checks = json!([
        {
            "name": "packets came out of the encoder",
            "kind": "must_not_be_zero",
            "reads": "packets",
            "why": "every percentile and the effective rate in this run is computed over these \
                    packets, so a run that produced none makes each of those figures an absence \
                    rather than a number",
        },
        {
            "name": "every submitted frame came back",
            "kind": "must_be_zero",
            "reads": "frame_count_disagreement",
            "population": "frames_submitted",
            "why": "Opus is lossy in amplitude and exact in length, so a decoded length that \
                    disagrees with the submitted one is a defect and not a tolerance; the \
                    population is the submitted frames, because two counts of nothing agree \
                    perfectly",
        },
        {
            "name": "encode p99 under a tenth of the frame",
            "kind": "must_be_below",
            "reads": "encode_p99_us",
            "bound": budget_us,
            "why": format!(
                "a tenth of the {frame_ms} ms frame is {budget_us:.0} us, and an encoder under \
                 that cannot be the term that matters in a budget measured in whole \
                 milliseconds; p99 rather than the mean, because a frame late once every \
                 hundred is a click a listener hears"
            ),
        },
        {
            "name": "the left channel decodes to its own tone",
            "kind": "must_be_within",
            "reads": "tone_left_hz",
            "target": CONTRACT.left_hz,
            "tolerance": TONE_TOLERANCE_HZ,
            "why": format!(
                "a byte count and a frame count agree just as happily when the decoder returns \
                 silence, and this project has read that agreement as success; {} Hz comes out \
                 of the decoded samples, at a window resolution of 2 Hz",
                CONTRACT.left_hz,
            ),
        },
        {
            "name": "the right channel decodes to its own tone",
            "kind": "must_be_within",
            "reads": "tone_right_hz",
            "target": CONTRACT.right_hz,
            "tolerance": TONE_TOLERANCE_HZ,
            "why": format!(
                "the two channels carry different frequencies so that channel order is provable \
                 rather than assumed, and a right channel reading {} Hz is what a swap looks \
                 like",
                CONTRACT.left_hz,
            ),
        },
        {
            "name": "the decoded channels stay distinct",
            "kind": "must_not_be_zero",
            "reads": "tone_channels_distinct",
            "why": "two channels reading one frequency is consistent with a fold to mono, with \
                    one channel encoded twice, and with a detector measuring its own scratch \
                    buffer, and a frame count is consistent with all three",
        },
    ]);

    let findings = json!([format!(
        "a {frame_ms} ms frame carries {} byte packets at {:.1} kbps effective against the \
             {} kbps asked for",
        measurement
            .packet_bytes
            .map_or_else(|| "no".to_string(), |bytes| bytes.p50.to_string()),
        measurement.effective_kbps(),
        measurement.config.bitrate_bps / 1_000,
    ),]);

    // A clock behind the epoch is a machine whose time nobody set, and a stamp
    // of zero says that rather than a date in 1969.
    let started_unix_ms = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);

    let document = json!({
        "gate": "codec-gate",
        "run": {
            "started_unix_ms": started_unix_ms,
            "span_s": span_s,
            "args": {
                "frame_ms": frame_ms,
                "seconds": options.seconds,
                "bitrate_kbps": options.bitrate_kbps,
            },
            "commit": commit,
            "arm": format!("{frame_ms} ms frames"),
        },
        "environment": {
            "libopus": measurement.libopus,
            "sample_rate_hz": measurement.config.sample_rate,
            "channels": measurement.config.channels,
            "frame_samples_per_channel": measurement.config.frame_samples(),
            "encoder_reports_bitrate_bps": measurement.settings.bitrate_bps,
            "application": measurement.settings.application,
            "vbr": measurement.settings.vbr,
            "vbr_constrained": measurement.settings.vbr_constrained,
            "dtx": measurement.settings.dtx,
            "inband_fec": measurement.settings.inband_fec,
            "complexity": measurement.settings.complexity,
            "lookahead_samples": measurement.settings.lookahead,
        },
        "declared": DECLARED,
        "exercised": exercised,
        "observations": Value::Object(observations),
        "checks": checks,
        "findings": findings,
    });

    format!("{document:#}\n")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use lanplay_audio_capture::{Percentiles, Tone, ToneReport};

    use super::*;
    use crate::config::CodecConfig;
    use crate::config::FrameDuration;
    use crate::encoder::EncoderSettings;

    /// A fixture rather than a run of the codec, so that what these tests
    /// defend is the document's shape and not whatever this machine's libopus
    /// happened to produce this morning.
    fn measurement() -> Measurement {
        Measurement {
            config: CodecConfig::contract(FrameDuration::Ms5, 128_000),
            settings: EncoderSettings {
                application: "OPUS_APPLICATION_RESTRICTED_LOWDELAY",
                bitrate_bps: 128_000,
                vbr: true,
                vbr_constrained: true,
                dtx: false,
                inband_fec: false,
                complexity: 9,
                lookahead: 120,
            },
            libopus: "libopus 1.6.1",
            frames_submitted: 480_000,
            frames_returned: 480_000,
            packets: 2_000,
            encode_us: percentiles(16, 29),
            decode_us: percentiles(4, 9),
            packet_bytes: percentiles(81, 81),
            total_packet_bytes: 162_000,
            tone: ToneReport {
                left: Some(Tone {
                    frequency: 997.0,
                    level_dbfs: -20.0,
                }),
                right: Some(Tone {
                    frequency: 1_997.0,
                    level_dbfs: -20.0,
                }),
                resolution_hz: 2.0,
                analysed_frames: 24_000,
            },
        }
    }

    fn percentiles(p50: u64, p99: u64) -> Option<Percentiles> {
        Some(Percentiles {
            count: 2_000,
            min: p50,
            p50,
            p95: p99,
            p99,
            max: p99 * 4,
        })
    }

    fn options() -> Options {
        Options {
            frame: FrameDuration::Ms5,
            seconds: 10.0,
            bitrate_kbps: 128,
        }
    }

    fn parsed(measurement: &Measurement) -> Value {
        let text = document(
            measurement,
            options(),
            UNIX_EPOCH + Duration::from_millis(1_755_212_345_678),
            10.0,
            Some("abc1234"),
        );
        serde_json::from_str(&text).expect("the document is JSON")
    }

    /// The document has to survive `xtask`'s parser, which refuses a check
    /// without a reason and a `must_be_zero` without a population. Nothing in
    /// this crate can call that parser, so what is defended here is the half a
    /// test on this side can see: every field that parser demands is present,
    /// and no check reads a name the run did not report — which is the defect
    /// that left a gate reading `target_ms` after it became `margin_ms`.
    #[test]
    fn every_check_reads_an_observation_the_run_reported() {
        let document = parsed(&measurement());

        let observations = document["observations"]
            .as_object()
            .expect("observations are a flat map of numbers by name");
        for (name, value) in observations {
            assert!(value.is_number(), "{name} is not a number: {value}");
        }

        let checks = document["checks"].as_array().expect("checks are a list");
        assert_eq!(checks.len(), 6);
        for check in checks {
            let reads = check["reads"].as_str().expect("a check reads by name");
            assert!(
                observations.contains_key(reads),
                "{reads} is read by a check and reported by nothing",
            );
            let why = check["why"].as_str().expect("a check states a reason");
            assert!(
                why.len() > 40,
                "a reason of {} characters: {why}",
                why.len()
            );
            if check["kind"] == "must_be_zero" {
                let population = check["population"]
                    .as_str()
                    .expect("a must_be_zero names its population");
                assert!(
                    observations.contains_key(population),
                    "{population} is named as a population and reported by nothing",
                );
            }
            assert!(
                check.get("verdict").is_none() && check.get("value").is_none(),
                "a probe states criteria and never conclusions: {check}",
            );
        }
    }

    #[test]
    fn the_numbers_the_gate_reports_are_the_ones_the_run_measured() {
        let document = parsed(&measurement());
        let observations = &document["observations"];
        assert_eq!(observations["packets"], 2_000.0);
        assert_eq!(observations["frames_submitted"], 480_000.0);
        assert_eq!(observations["frame_count_disagreement"], 0.0);
        assert_eq!(observations["packet_bytes_p50"], 81.0);
        assert_eq!(observations["encode_p99_us"], 29.0);
        assert_eq!(observations["tone_left_hz"], 997.0);
        assert_eq!(observations["tone_right_hz"], 1_997.0);
        assert_eq!(observations["tone_channels_distinct"], 1.0);
        // 162000 bytes over two thousand 5 ms packets is ten seconds of audio.
        assert_eq!(observations["effective_kbps"], 129.6);
        assert_eq!(document["run"]["started_unix_ms"], 1_755_212_345_678u64);
        assert_eq!(document["run"]["arm"], "5 ms frames");
        assert_eq!(document["run"]["commit"], "abc1234");
        assert_eq!(document["declared"], json!(DECLARED));
        assert_eq!(document["exercised"], json!(DECLARED));
    }

    /// The bound is derived from the frame rather than stated once, because the
    /// two arms encode different amounts of audio and a fixed number would hold
    /// one of them to the other's budget.
    #[test]
    fn the_encode_budget_is_a_tenth_of_whichever_frame_was_encoded() {
        for (frame, budget) in [(FrameDuration::Ms5, 500.0), (FrameDuration::Ms10, 1_000.0)] {
            let mut measured = measurement();
            measured.config = CodecConfig::contract(frame, 128_000);
            let document = parsed(&measured);
            let bound = document["checks"]
                .as_array()
                .expect("checks are a list")
                .iter()
                .find(|check| check["reads"] == "encode_p99_us")
                .expect("the encode budget is stated")["bound"]
                .as_f64()
                .expect("a bound is a number");
            assert_eq!(bound, budget);
        }
    }

    /// The failure this arrangement exists to prevent: a run that measured no
    /// tone leaves the three tone criteria undecidable rather than passing or
    /// failing them, and does not claim the subsystem it never reached.
    #[test]
    fn a_run_that_measured_no_tone_reports_neither_a_frequency_nor_the_subsystem() {
        let mut measured = measurement();
        measured.tone = ToneReport::empty();
        let document = parsed(&measured);
        let observations = document["observations"]
            .as_object()
            .expect("observations are a map");
        for absent in ["tone_left_hz", "tone_right_hz", "tone_channels_distinct"] {
            assert!(
                !observations.contains_key(absent),
                "{absent} was reported for a tone nobody measured",
            );
        }
        assert_eq!(document["exercised"], json!(["encoder", "decoder"]));
    }

    /// A run that encoded nothing reports no percentile at all, so the encode
    /// budget is unavailable rather than passing on a zero nobody measured.
    #[test]
    fn a_run_that_encoded_nothing_reports_no_percentile() {
        let mut measured = measurement();
        measured.packets = 0;
        measured.encode_us = None;
        measured.decode_us = None;
        measured.packet_bytes = None;
        measured.total_packet_bytes = 0;
        let document = parsed(&measured);
        let observations = document["observations"]
            .as_object()
            .expect("observations are a map");
        assert!(!observations.contains_key("encode_p99_us"));
        assert!(!observations.contains_key("packet_bytes_p50"));
        assert_eq!(observations["packets"], 0.0);
        assert_eq!(document["exercised"], json!(["decoder", "tone analysis"]));
    }
}
