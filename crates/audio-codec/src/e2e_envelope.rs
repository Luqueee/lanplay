//! The sender's half of one A6 run, as the gate document `xtask verdict`
//! decides on.
//!
//! The keyed block next door stays exactly as it is and this is emitted beside
//! it rather than instead of it: a person whose gate failed reads the block, the
//! evaluator reads this, and replacing either with the other trades one audience
//! for the other. What is gone is the third thing, the regular expression that
//! turned the first back into the second and once read 6001 captured packets as
//! none.
//!
//! Every criterion here is about the sending end alone. The receiver emits its
//! own document, and the continuity that decides the phase is decided there,
//! because only the far end knows what it played. What this end can settle is
//! everything upstream of the radio: that the endpoint delivered audio, that
//! every captured sample reached the encoder, that the split left nothing over,
//! that each frame became one datagram whose timestamp advanced by the frame's
//! own sample count, and that the encoder stayed far enough inside the frame to
//! be irrelevant to the budget. A run that fails one of those has told the
//! receiver's numbers what they are about.
//!
//! Nothing in this file decides anything. The bounds are parameters and the
//! reasons are the sentences a reviewer checks them against; `xtask`'s parser is
//! the authority on the shape and refuses a document that omits a reason or a
//! population rather than reading one.

use std::time::{SystemTime, UNIX_EPOCH};

use lanplay_tone_source::tone::CONTRACT;
use serde_json::{Map, Value, json};

use crate::e2e_sender::{Measurement, Options};

/// The fraction of a frame the encoder has to stay under, as A2 stated it.
///
/// A tenth, because "much less than the frame" has to be a number to be a
/// criterion at all. A2 measured 40 microseconds at p99 against the 500 this
/// allows, so the bound is not tight and is not meant to be: it is the line
/// above which the encoder stops being irrelevant to a budget the rest of this
/// project measures in whole milliseconds.
const BUDGET_FRACTION: f64 = 10.0;

/// How far the captured tone may sit from the one the source is playing.
///
/// Five hertz, against a window whose bin spacing is two, and two hundredths of
/// the thousand-hertz gap between the two contract tones: wide enough that a
/// correct path cannot fail it, far too narrow for a channel carrying the other
/// channel's tone to pass.
const TONE_TOLERANCE_HZ: f64 = 5.0;

/// What this end of the gate covers.
const DECLARED: [&str; 3] = ["loopback capture", "opus encoder", "rtp sender"];

pub fn document(
    measurement: &Measurement,
    options: Options,
    started: SystemTime,
    arm: &str,
    commit: Option<&str>,
) -> String {
    let frame_ms = measurement.config.frame.millis();
    let budget_us = f64::from(frame_ms) * 1_000.0 / BUDGET_FRACTION;
    let counts = measurement.carried.counts;
    let totals = measurement.carried.totals;

    let mut observations = Map::new();
    // A count is written as an integer even though every observation is read
    // back as a number, because a person reads this too and a packet count of
    // `12000.0` invites its reader to wonder what the tenths were.
    let mut observe = |name: &str, value: f64| {
        let stated = if value.fract() == 0.0 && value.abs() < 9e15 {
            json!(value as i64)
        } else {
            json!(value)
        };
        observations.insert(name.to_string(), stated);
    };

    observe("span_s", measurement.span_s);
    observe("capture_packets", totals.packets as f64);
    observe("capture_frames", totals.frames as f64);
    observe("capture_packets_per_s", measurement.capture_packets_per_s());
    observe("frames_encoded", counts.frames_encoded as f64);
    observe("frames_encoded_per_s", measurement.frames_encoded_per_s());
    observe("datagrams_sent", counts.datagrams_sent as f64);
    observe("datagram_bytes", counts.datagram_bytes as f64);
    observe("wire_kbps", measurement.wire_kbps());

    // The pair the split is proved by, and their difference stated as its own
    // number so that no reader has to subtract two counts to find out whether
    // audio went missing between the endpoint and the encoder.
    observe("samples_captured", measurement.samples_captured() as f64);
    observe("samples_encoded", measurement.samples_encoded() as f64);
    observe(
        "sample_accounting_disagreement",
        measurement.sample_disagreement() as f64,
    );
    observe("split_residue_frames", counts.residue_frames as f64);
    observe("unreadable_packets", counts.unreadable_packets as f64);

    observe("capture_gaps", totals.gaps as f64);
    observe("capture_gap_frames", totals.gap_frames as f64);
    observe("capture_rewinds", totals.rewinds as f64);
    observe(
        "discontinuities_in_flight",
        totals.discontinuities_in_flight() as f64,
    );
    observe("silent_packets", totals.silent_packets as f64);
    observe("timestamp_steps", counts.timestamp_steps as f64);
    observe("timestamp_steps_exact", counts.timestamp_steps_exact as f64);

    // A6.1's half of the pair audit, and one integer rather than a mechanism.
    // The receiving end can partition a run into the two frames of a captured
    // packet from the timestamps alone, and cannot tell which of the two is the
    // first without knowing where this stream's counter started. Nothing derived
    // and no clock read: the number is the packetiser's seed, and the join at the
    // far end is `(anchor - base) mod 480`.
    observe("rtp_base", f64::from(measurement.rtp_base.0));
    observe("encode_failures", counts.encode_failures as f64);
    observe("send_failures", counts.send_failures as f64);
    observe("buffer_errors", measurement.buffer_errors as f64);
    observe("wakeup_timeouts", measurement.wakeup_timeouts as f64);
    observe(
        "samples_dropped",
        measurement.carried.samples_dropped as f64,
    );

    // A7.1's half of the clock audit, and absent rather than zero when the run
    // had too few readings to state a rate. A criterion reading an absent
    // observation is a refusal in `xtask verdict`, which is what this end wants:
    // 0.000 ppm is what a perfect clock looks like and a run that measured no
    // clock at all must not be able to print it.
    if let Some(rate) = measurement.source_rate() {
        observe("source_ppm", rate.fitted_ppm);
        observe("source_ppm_endpoints", rate.endpoints_ppm);
        observe("source_ppm_error", rate.error_ppm);
        observe("source_rate_readings", rate.readings as f64);
        observe("source_rate_span_s", rate.seconds);
        observe("source_position_samples", rate.samples);
        observe("source_counter_scatter_samples", rate.scatter_samples);
    }
    observe(
        "source_counter_stalls",
        measurement.carried.drift.stalled() as f64,
    );

    // A percentile over no samples is not zero, it is absent, and a zero here
    // would let the encode budget pass on a run that encoded nothing.
    if let Some(encode) = measurement.carried.encode_us {
        observe("encode_p50_us", encode.p50 as f64);
        observe("encode_p95_us", encode.p95 as f64);
        observe("encode_p99_us", encode.p99 as f64);
        observe("encode_max_us", encode.max as f64);
    }
    if let Some(send) = measurement.carried.send_us {
        observe("send_p50_us", send.p50 as f64);
        observe("send_p95_us", send.p95 as f64);
        observe("send_p99_us", send.p99 as f64);
        observe("send_max_us", send.max as f64);
    }
    if let Some(bytes) = measurement.carried.packet_bytes {
        observe("packet_bytes_p50", bytes.p50 as f64);
        observe("packet_bytes_p95", bytes.p95 as f64);
        observe("packet_bytes_p99", bytes.p99 as f64);
        observe("packet_bytes_min", bytes.min as f64);
        observe("packet_bytes_max", bytes.max as f64);
    }
    if let Some(frames) = measurement.carried.packet_frames {
        observe("packet_frames_p50", frames.p50 as f64);
        observe("packet_frames_min", frames.min as f64);
        observe("packet_frames_max", frames.max as f64);
    }
    if let Some(wakeups) = measurement.wakeup_intervals_us {
        observe("wakeup_p50_us", wakeups.p50 as f64);
        observe("wakeup_p99_us", wakeups.p99 as f64);
        observe("wakeup_max_us", wakeups.max as f64);
    }

    let tone = &measurement.carried.tone;
    observe("tone_resolution_hz", tone.resolution_hz);
    observe("tone_analysed_frames", tone.analysed_frames as f64);
    if let Some(left) = tone.left {
        observe("tone_left_hz", left.frequency);
        observe("tone_left_dbfs", left.level_dbfs);
    }
    if let Some(right) = tone.right {
        observe("tone_right_hz", right.frequency);
        observe("tone_right_dbfs", right.level_dbfs);
    }

    let exercised: Vec<&str> = [
        ("loopback capture", totals.packets > 0),
        ("opus encoder", counts.frames_encoded > 0),
        ("rtp sender", counts.datagrams_sent > 0),
    ]
    .into_iter()
    .filter_map(|(subsystem, reached)| reached.then_some(subsystem))
    .collect();

    let checks = json!([
        {
            "name": "packets came off the endpoint",
            "kind": "must_not_be_zero",
            "reads": "capture_packets",
            "why": "every count and every percentile in this document is taken over these \
                    packets, so a run that collected none makes each of them an absence rather \
                    than a number, and loopback delivers nothing at all while the endpoint is \
                    idle",
        },
        {
            "name": "datagrams went onto the wire",
            "kind": "must_not_be_zero",
            "reads": "datagrams_sent",
            "why": "the receiving end's loss figure is a fraction of what this end sent, and a \
                    sender that sent nothing makes a receiver that received nothing look like a \
                    radio that lost everything",
        },
        {
            "name": "every captured sample was encoded",
            "kind": "must_equal",
            "reads": "samples_encoded",
            "equals": "samples_captured",
            "why": format!(
                "a {frame_ms} ms frame is {} samples and the endpoint delivers exactly two \
                 frames' worth in every packet, so the split is exact or it is not a split; the \
                 two counts agreeing is the proof, and audio lost here cannot be told apart by a \
                 receiver from audio lost on the air",
                measurement.config.frame_samples(),
            ),
        },
        {
            "name": "no packet left a residue",
            "kind": "must_be_zero",
            "reads": "split_residue_frames",
            "population": "capture_packets",
            "why": "there is no accumulator on this path because A1 measured that there is never \
                    a residue, so a residue is a finding about the endpoint rather than a \
                    tolerance; the population is the packets, because no packets leave nothing \
                    over just as convincingly",
        },
        {
            "name": "every encoded frame became a datagram",
            "kind": "must_equal",
            "reads": "datagrams_sent",
            "equals": "frames_encoded",
            "why": "one Opus frame is one datagram in this payload format, so the two counts are \
                    the same measurement taken at two boundaries, and a shortfall is a frame the \
                    socket refused rather than a frame the radio lost",
        },
        {
            "name": "the timestamp advanced by the frame's samples every time",
            "kind": "must_equal",
            "reads": "timestamp_steps_exact",
            "equals": "timestamp_steps",
            "why": "the RTP timestamp is a sample counter, and one that drifted with this \
                    sender's scheduling would leave a receiver unable to tell a late packet from \
                    a packet describing a later moment",
        },
        {
            "name": "the socket refused nothing",
            "kind": "must_be_zero",
            "reads": "send_failures",
            "population": "frames_encoded",
            "why": "a refused send is this host's own doing rather than the link's, and a run \
                    with any of them is measuring a machine that could not keep the socket fed; \
                    the population is the frames encoded, because nothing offered is never \
                    refused",
        },
        {
            "name": "no frames missing from the device position",
            "kind": "must_be_zero",
            "reads": "capture_gap_frames",
            "population": "capture_frames",
            "why": "the device position of a packet must equal the previous position plus the \
                    previous frame count, so a gap is audio the engine could not hand over and \
                    its size is known exactly; the population is the frames captured, because a \
                    stream that delivered none has no holes in it",
        },
        {
            "name": "the device position never went backwards",
            "kind": "must_be_zero",
            "reads": "capture_rewinds",
            "population": "capture_packets",
            "why": "a hole is audio that was lost and a rewind is a position stream that cannot \
                    be trusted at all, so they are counted apart and this one is not a tolerance",
        },
        {
            "name": "encode p99 under a tenth of the frame",
            "kind": "must_be_below",
            "reads": "encode_p99_us",
            "bound": budget_us,
            "why": format!(
                "a tenth of the {frame_ms} ms frame is {budget_us:.0} us, and an encoder under \
                 that cannot be the term that matters in a budget measured in whole \
                 milliseconds; p99 and not the mean, because a frame late once every hundred is \
                 a click a listener hears"
            ),
        },
        {
            "name": "the endpoint carried the left channel's tone",
            "kind": "must_be_within",
            "reads": "tone_left_hz",
            "target": CONTRACT.left_hz,
            "tolerance": TONE_TOLERANCE_HZ,
            "why": format!(
                "a packet count and a datagram count agree just as happily when the host was \
                 playing nothing, and a receiver that reports no underruns while playing \
                 silence has carried nothing either; {} Hz comes out of the captured samples \
                 themselves",
                CONTRACT.left_hz,
            ),
        },
        {
            "name": "the endpoint carried the right channel's tone",
            "kind": "must_be_within",
            "reads": "tone_right_hz",
            "target": CONTRACT.right_hz,
            "tolerance": TONE_TOLERANCE_HZ,
            "why": format!(
                "the two channels carry different frequencies so that channel order is provable \
                 rather than assumed, and a right channel reading {} Hz is what a fold to mono \
                 and a channel read twice both look like",
                CONTRACT.left_hz,
            ),
        },
    ]);

    let findings = json!([
        format!(
            "the endpoint delivered {} frames per packet at p50 and the encoder took {} of them \
             per frame, so one packet became {} datagrams of {} bytes",
            measurement
                .carried
                .packet_frames
                .map_or_else(|| "no".to_string(), |frames| frames.p50.to_string()),
            measurement.config.frame_samples(),
            measurement
                .carried
                .packet_frames
                .map_or(0, |frames| frames.p50 as usize
                    / measurement.config.frame_samples()),
            measurement
                .carried
                .packet_bytes
                .map_or_else(|| "no".to_string(), |bytes| bytes.p50.to_string()),
        ),
        format!(
            "{:.1} kbps on the wire including RTP headers, at {:.2} datagrams a second",
            measurement.wire_kbps(),
            measurement.datagrams_per_s(),
        ),
        format!("the sending thread ran as {}", measurement.scheduling),
    ]);

    // A clock behind the epoch is a machine whose time nobody set, and a stamp
    // of zero says that rather than a date in 1969.
    let started_unix_ms = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);

    let document = json!({
        "gate": "audio-e2e-gate",
        "run": {
            "started_unix_ms": started_unix_ms,
            "span_s": measurement.span_s,
            "args": {
                "seconds": options.seconds,
                "bitrate_kbps": options.bitrate_kbps,
                "frame_ms": frame_ms,
                "send_to": options.send_to.to_string(),
            },
            "commit": commit,
            "arm": arm,
        },
        "environment": {
            "end": "sender",
            "endpoint": measurement.endpoint,
            "mix_format": measurement.format.to_string(),
            "device_period_default_ms": measurement.default_period_ms,
            "device_period_minimum_ms": measurement.minimum_period_ms,
            "endpoint_buffer_frames": measurement.buffer_frames,
            "wakeup": measurement.wakeup.to_string(),
            "scheduling": measurement.scheduling.to_string(),
            "libopus": measurement.libopus,
            "ssrc": measurement.ssrc.0,
            "bind": measurement.bind.to_string(),
            "send_to": measurement.send_to.to_string(),
            "encoder_reports_bitrate_bps": measurement.settings.bitrate_bps,
            "application": measurement.settings.application,
            "vbr_constrained": measurement.settings.vbr_constrained,
            "dtx": measurement.settings.dtx,
            "inband_fec": measurement.settings.inband_fec,
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

    use lanplay_audio_capture::accounting::{Drift, Percentiles, Totals};
    use lanplay_audio_capture::analysis::ToneReport;
    use lanplay_audio_capture::format::{MixFormat, SUBTYPE_IEEE_FLOAT, SampleKind};
    use lanplay_audio_capture::goertzel::Tone;
    use lanplay_audio_capture::report::Wakeup;
    use lanplay_audio_capture::scheduling::{PRO_AUDIO, Scheduling};
    use lanplay_transport::{RtpTimestamp, Ssrc};

    use super::*;
    use crate::config::CodecConfig;
    use crate::e2e_sender::{Carried, Counts, FRAME};
    use crate::encoder::EncoderSettings;

    /// Sixty seconds of a device running slow, packet by packet.
    ///
    /// Generated rather than left empty, because an empty drift makes the rate
    /// observations absent and a test over a document with no rate in it cannot
    /// tell a correct emitter from one that never wrote the lines.
    fn drifted(ppm: f64) -> Drift {
        let mut drift = Drift::new(48_000.0);
        let rate = 48_000.0 * (1.0 + ppm / 1e6);
        for packet in 0..6_000u64 {
            let position = packet * 480;
            drift.record(position as f64, (position as f64 / rate * 1e9) as u64);
        }
        drift
    }

    /// A fixture rather than a run of the sender, so that what these tests
    /// defend is the document's shape rather than whatever this machine's
    /// endpoint was playing when they ran. The numbers are the ones A1, A2 and
    /// A3 measured, for sixty seconds: 6000 packets of 480 frames, 12000 frames
    /// of 81 bytes.
    fn measurement() -> Measurement {
        Measurement {
            endpoint: "LG ULTRAWIDE (NVIDIA High Definition Audio)".to_string(),
            format: MixFormat {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 32,
                valid_bits: 32,
                block_align: 8,
                kind: SampleKind::Float,
                channel_mask: 3,
                subformat: SUBTYPE_IEEE_FLOAT,
                extensible: true,
            },
            default_period_ms: 10.0,
            minimum_period_ms: 3.0,
            buffer_frames: 1_056,
            wakeup: Wakeup::Event,
            event_refused: None,
            scheduling: Scheduling::Mmcss {
                task: PRO_AUDIO,
                priority: 15,
            },
            config: CodecConfig::contract(FRAME, 128_000),
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
            bind: "0.0.0.0:52001".parse().expect("a literal address"),
            send_to: "192.168.1.108:5012".parse().expect("a literal address"),
            ssrc: Ssrc(0x1234_5678),
            rtp_base: RtpTimestamp(0x0BAD_0000),
            carried: Carried {
                totals: Totals {
                    packets: 6_000,
                    frames: 2_880_000,
                    first_packet_discontinuous: true,
                    discontinuities: 1,
                    ..Totals::default()
                },
                drift: drifted(-15.0),
                counts: Counts {
                    frames_encoded: 12_000,
                    datagrams_sent: 12_000,
                    datagram_bytes: 12_000 * 93,
                    timestamp_steps: 12_000,
                    timestamp_steps_exact: 12_000,
                    ..Counts::default()
                },
                packet_frames: percentiles(480, 480),
                encode_us: percentiles(18, 40),
                send_us: percentiles(9, 30),
                packet_bytes: percentiles(81, 81),
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
                first_encode_error: None,
                first_send_error: None,
                samples_dropped: 0,
            },
            buffer_errors: 0,
            first_buffer_error: None,
            wakeup_timeouts: 0,
            wakeup_intervals_us: percentiles(10_000, 11_000),
            span_s: 60.0,
        }
    }

    fn percentiles(p50: u64, p99: u64) -> Option<Percentiles> {
        Some(Percentiles {
            count: 12_000,
            min: p50,
            p50,
            p95: p99,
            p99,
            max: p99 * 3,
        })
    }

    fn options() -> Options {
        Options {
            send_to: "192.168.1.108:5012".parse().expect("a literal address"),
            bind: "0.0.0.0:0".parse().expect("a literal address"),
            seconds: 60.0,
            bitrate_kbps: 128,
        }
    }

    fn parsed(measurement: &Measurement) -> Value {
        let text = document(
            measurement,
            options(),
            UNIX_EPOCH + Duration::from_millis(1_755_212_345_678),
            "radio",
            Some("abc1234"),
        );
        serde_json::from_str(&text).expect("the document is JSON")
    }

    /// The document has to survive `xtask`'s parser, which refuses a check
    /// without a reason and a `must_be_zero` without a population. Nothing in
    /// this crate can call that parser, so what is defended here is the half a
    /// test on this side can see: every field it demands is present, and no
    /// check reads a name the run did not report.
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
        assert_eq!(checks.len(), 12);
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
            for parameter in ["population", "equals"] {
                if let Some(named) = check[parameter].as_str() {
                    assert!(
                        observations.contains_key(named),
                        "{named} is named as a {parameter} and reported by nothing",
                    );
                }
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
        assert_eq!(observations["capture_packets"], 6_000.0);
        assert_eq!(observations["capture_frames"], 2_880_000.0);
        assert_eq!(observations["frames_encoded"], 12_000.0);
        assert_eq!(observations["datagrams_sent"], 12_000.0);
        // 12000 frames of 240 samples is the 2880000 frames the endpoint
        // delivered, which is what makes the split exact rather than close.
        assert_eq!(observations["samples_captured"], 2_880_000.0);
        assert_eq!(observations["samples_encoded"], 2_880_000.0);
        assert_eq!(observations["sample_accounting_disagreement"], 0.0);
        assert_eq!(observations["split_residue_frames"], 0.0);
        assert_eq!(observations["capture_packets_per_s"], 100.0);
        assert_eq!(observations["frames_encoded_per_s"], 200.0);
        assert_eq!(observations["encode_p99_us"], 40.0);
        assert_eq!(observations["packet_bytes_p50"], 81.0);
        assert_eq!(observations["tone_left_hz"], 997.0);
        assert_eq!(observations["tone_right_hz"], 1_997.0);
        // The first packet's discontinuity is the loopback client attaching to
        // an engine that was already running, and it must not be counted as a
        // stream that broke.
        assert_eq!(observations["discontinuities_in_flight"], 0.0);
        assert_eq!(document["gate"], "audio-e2e-gate");
        assert_eq!(document["run"]["arm"], "radio");
        assert_eq!(document["run"]["commit"], "abc1234");
        assert_eq!(document["run"]["started_unix_ms"], 1_755_212_345_678u64);
        assert_eq!(document["declared"], json!(DECLARED));
        assert_eq!(document["exercised"], json!(DECLARED));
    }

    /// The two ways a run measures nothing, and neither may pass quietly: a
    /// sender that captured nothing reports no percentile for the encoder to be
    /// judged on, and one that captured only silence reports no tone.
    #[test]
    fn a_run_that_carried_nothing_claims_nothing() {
        let mut measured = measurement();
        measured.carried.totals = Totals::default();
        measured.carried.counts = Counts::default();
        measured.carried.encode_us = None;
        measured.carried.send_us = None;
        measured.carried.packet_bytes = None;
        measured.carried.packet_frames = None;
        measured.carried.tone = ToneReport::empty();

        let document = parsed(&measured);
        let observations = document["observations"]
            .as_object()
            .expect("observations are a map");
        for absent in [
            "encode_p99_us",
            "packet_bytes_p50",
            "tone_left_hz",
            "tone_right_hz",
        ] {
            assert!(
                !observations.contains_key(absent),
                "{absent} was reported by a run that measured none of it",
            );
        }
        assert_eq!(observations["capture_packets"], 0.0);
        assert_eq!(document["exercised"], json!([] as [&str; 0]));
    }

    /// A sender that dropped audio between the endpoint and the encoder says so
    /// in the one number the receiving end cannot see: 240 samples short is a
    /// frame that never became a datagram, and to a receiver that looks exactly
    /// like a datagram the radio lost.
    #[test]
    fn audio_lost_before_the_encoder_is_stated_as_its_own_disagreement() {
        let mut measured = measurement();
        measured.carried.counts.frames_encoded -= 1;
        measured.carried.counts.datagrams_sent -= 1;
        measured.carried.counts.unreadable_packets = 1;

        let document = parsed(&measured);
        let observations = &document["observations"];
        assert_eq!(observations["samples_encoded"], 2_879_760.0);
        assert_eq!(observations["sample_accounting_disagreement"], 240.0);
        assert_eq!(observations["unreadable_packets"], 1.0);
    }
}
