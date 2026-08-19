//! One run of the RTP probe as the gate document `xtask verdict` decides on.
//!
//! The keyed block on stdout stays exactly as it was and this is emitted beside
//! it rather than instead of it. Both audiences are real: a person whose gate
//! just failed reads the block, the evaluator reads this, and neither is served
//! by being handed the other's form. What is gone is the third thing, the
//! regular expression that turned the first back into the second - three of the
//! eight instrument failures this project has recorded were that mechanism, and
//! one of them read 6001 captured packets as none.
//!
//! Which criteria a run states follows from its arm, and the arm is the one
//! thing about a run this probe cannot measure. A receive-only process cannot
//! tell a relay on 127.0.0.1 from a peer across the air, and a sending process
//! cannot tell an arm whose far end does the counting from one that lost
//! everything; both differences decide what the numbers mean. Over loopback
//! nothing may be lost, duplicated or reordered, because there is no wire to
//! blame and a defect there is this code's. Over the radio those same figures
//! are the measurement the phase exists to produce, reported and not failed on:
//! a gate demanding zero loss over Wi-Fi would be demanding a different radio,
//! and the plan is explicit that loss gets measured before anything is built to
//! hide it. What must hold either way is the stream's own arithmetic and the
//! audio that came out of it.
//!
//! The negative control is [`Arm::Control`], and it states the loopback arm's
//! criteria rather than a threshold of its own. A control judged by a rule
//! nothing else is judged by would be evidence about that rule; judged by the
//! criteria it is aimed at, its failure is evidence those criteria can fail.
//!
//! Nothing here decides anything. Every number in `checks` is a parameter and
//! every `why` is the sentence a reviewer checks the criterion against;
//! `xtask`'s parser is the authority on the shape, and it refuses a document
//! that omits a reason or a population rather than reading one, so a mistake
//! here fails the gate at once and loudly instead of quietly weakening a
//! criterion.

use std::time::{SystemTime, UNIX_EPOCH};

use lanplay_tone_source::tone::CONTRACT;
use lanplay_transport::{MAX_UDP_PAYLOAD, OPUS_PAYLOAD_TYPE};
use serde_json::{Map, Value, json};

use crate::rtp_probe::{Measurement, Options, TONE_TOLERANCE_HZ, Verification};

/// Which arm of the gate a run is, and with it what the run is in a position to
/// claim.
///
/// Stated by whoever invoked the probe rather than inferred from the flags,
/// because the two properties that decide the criteria - whether a lost packet
/// is a defect, and whether this end's arrivals are the measurement at all -
/// are both invisible from inside the process.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Arm {
    /// Both halves in one process over 127.0.0.1, where a lost packet, a
    /// duplicate or a reordering is a defect in this code because there is
    /// nowhere else for it to have come from.
    Loopback,
    /// This end sends across the radio and the far end accounts for what
    /// arrived, so this document settles only what left this host.
    RadioSender,
    /// This end accounts for a peer's stream off the air, where loss is the
    /// measurement rather than a criterion.
    RadioReceiver,
    /// The loopback path relayed through `udp-fault`: the negative control,
    /// judged against the loopback criteria.
    Control,
}

impl Arm {
    /// The line a document read on its own is filed under. A negative control
    /// whose report is indistinguishable from a measuring arm's invites
    /// somebody to quote its failure as the gate's.
    fn stated(self) -> &'static str {
        match self {
            Arm::Loopback => "loopback, both halves in one process",
            Arm::RadioSender => "radio, the sending end",
            Arm::RadioReceiver => "radio, the receiving end",
            Arm::Control => "the loopback path relayed through udp-fault: the negative control",
        }
    }

    /// Whether the arrivals this end counted are the measurement. False on a
    /// sending end whose peer does the counting, where a received packet count
    /// of zero is the arrangement working rather than a path that carried
    /// nothing.
    fn accounts(self) -> bool {
        !matches!(self, Arm::RadioSender)
    }

    /// Whether a packet that never arrived is a figure to report rather than a
    /// criterion to fail.
    fn may_lose(self) -> bool {
        matches!(self, Arm::RadioSender | Arm::RadioReceiver)
    }
}

pub fn document(
    measurement: &Measurement,
    options: Options,
    arm: Arm,
    started: SystemTime,
    span_s: f64,
    seed: Option<u64>,
    commit: Option<&str>,
) -> String {
    let frame_ms = measurement.config.frame.millis();
    let receive = &measurement.receive;
    let send = measurement.send.as_ref();

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

    observe("packets_received", receive.packets as f64);
    observe("bytes_received", receive.bytes as f64);
    observe("effective_kbps", measurement.effective_kbps());
    observe("timestamp_pairs", receive.timestamp_pairs as f64);
    observe("timestamp_exact", receive.timestamp_exact as f64);
    observe("sequence_gaps", receive.sequence_gaps as f64);
    observe("packets_missing", receive.packets_missing as f64);
    observe("reordered", receive.reordered as f64);
    observe("duplicates", receive.duplicates as f64);
    observe("frames_decoded", receive.frames_decoded as f64);
    observe("decode_failures", receive.decode_failures as f64);
    observe("datagrams_not_rtp", receive.not_rtp as f64);
    observe(
        "datagrams_wrong_payload_type",
        receive.wrong_payload_type as f64,
    );
    observe("datagrams_empty_payload", receive.empty_payload as f64);
    observe("datagrams_foreign_ssrc", receive.foreign_ssrc as f64);

    // The datagram this arm is about: what arrived, where arrivals are the
    // measurement, and what went out where the far end does the counting. One
    // name for one meaning per arm, and the environment says which end wrote
    // the document.
    let largest = if arm.accounts() {
        receive.largest_datagram
    } else {
        send.map_or(0, |send| send.largest_datagram)
    };
    observe("largest_datagram_bytes", largest as f64);
    // Stated as its own number rather than left to the evaluator to subtract a
    // constant it would first have to be told, because a check names one
    // observation and a difference nobody named is a difference somebody
    // computes twice.
    observe(
        "datagram_bytes_over_mtu",
        largest.saturating_sub(MAX_UDP_PAYLOAD) as f64,
    );

    // A percentile over no samples is not zero, it is absent.
    if let Some(arrival) = receive.arrival_us {
        observe("arrival_p50_us", arrival.p50 as f64);
        observe("arrival_p95_us", arrival.p95 as f64);
        observe("arrival_p99_us", arrival.p99 as f64);
        observe("arrival_max_us", arrival.max as f64);
    }

    if let Some(send) = send {
        observe("packets_sent", send.packets as f64);
        observe("bytes_sent", send.wire_bytes as f64);
        observe("send_failures", send.send_failures as f64);
        observe("largest_datagram_sent", send.largest_datagram as f64);
        if let Some(cost) = send.send_us {
            observe("send_p50_us", cost.p50 as f64);
            observe("send_p95_us", cost.p95 as f64);
            observe("send_p99_us", cost.p99 as f64);
            observe("send_max_us", cost.max as f64);
        }
    }

    // Loss by subtraction only where one process holds both ends. Across two
    // machines the two counts are taken over different intervals by different
    // clocks, and a difference between them is arithmetic rather than a
    // measurement; what the far end can see there is the sequence total.
    if arm.accounts()
        && let (Some(lost), Some(percent)) = (measurement.lost(), measurement.loss_percent())
    {
        observe("packets_lost", lost as f64);
        observe("loss_percent", percent);
    }

    // Absent rather than zero when the sender is another machine. Zero verified
    // of N is indistinguishable from a path that corrupted everything, and what
    // it would mean is that the question does not apply.
    if let Verification::Digests {
        verified,
        mismatched,
        unverifiable,
    } = receive.verification
    {
        observe("payloads_verified", verified as f64);
        observe("payloads_mismatched", mismatched as f64);
        observe("payloads_unverifiable", unverifiable as f64);
    }

    observe("tone_resolution_hz", receive.tone.resolution_hz);
    observe("tone_analysed_frames", receive.tone.analysed_frames as f64);
    if let Some(left) = receive.tone.left {
        observe("tone_left_hz", left.frequency);
        observe("tone_left_dbfs", left.level_dbfs);
    }
    if let Some(right) = receive.tone.right {
        observe("tone_right_hz", right.frequency);
        observe("tone_right_dbfs", right.level_dbfs);
    }
    // Only stated when both channels were found, so that a run which measured
    // no tone leaves the question unavailable rather than answering it: a tone
    // nobody could measure is not a tone that came back folded to mono.
    if receive.tone.left.is_some() && receive.tone.right.is_some() {
        observe(
            "tone_channels_distinct",
            f64::from(u8::from(receive.tone.distinct())),
        );
    }

    let (declared, exercised) = coverage(arm, measurement);
    let checks = checks(arm, measurement, frame_ms);
    let findings = findings(arm, measurement, frame_ms, largest);

    // A clock behind the epoch is a machine whose time nobody set, and a stamp
    // of zero says that rather than a date in 1969.
    let started_unix_ms = started
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64);

    let document = json!({
        "gate": "audio-rtp-gate",
        "run": {
            "started_unix_ms": started_unix_ms,
            "span_s": span_s,
            // Stated by the arm that injects faults and absent everywhere else.
            // A control whose seed nobody recorded is a control nobody can run
            // again, and this one has already had to be run again once.
            "seed": seed,
            "args": {
                "bind": options.bind.to_string(),
                "send_to": options.send_to.map(|target| target.to_string()),
                "seconds": options.seconds,
                "frame_ms": frame_ms,
            },
            "commit": commit,
            "arm": arm.stated(),
        },
        "environment": {
            // Which end of the arm wrote this, because two documents describing
            // one arm from its two ends are two different sets of facts.
            "end": if arm.accounts() { "receiver" } else { "sender" },
            "bind": measurement.bind.to_string(),
            "send_to": measurement.send_to.map(|target| target.to_string()),
            "ssrc": measurement.ssrc().map(|ssrc| ssrc.0),
            "payload_type": OPUS_PAYLOAD_TYPE,
            "sample_rate_hz": measurement.config.sample_rate,
            "channels": measurement.config.channels,
            "frame_samples_per_channel": measurement.config.frame_samples(),
            "bitrate_bps": measurement.config.bitrate_bps,
            "max_udp_payload_bytes": MAX_UDP_PAYLOAD,
            "payload_comparison": match receive.verification {
                Verification::Digests { .. } => "digests, one process holds both halves",
                Verification::NotApplicable => "none, the sender is another machine",
            },
            // A receive-only run that stopped early stopped because its peer
            // went quiet, and a run that ended any other way describes only a
            // prefix of the stream it was pointed at.
            "ended_on_silence": receive.ended_on_silence,
            "receive_error": receive.error.clone(),
        },
        "declared": declared,
        "exercised": exercised,
        "observations": Value::Object(observations),
        "checks": checks,
        "findings": findings,
    });

    format!("{document:#}\n")
}

/// What the arm covers and what it reached, split by end: a sending run that
/// declared a decoder would be claiming a subsystem running on another machine,
/// and a receiving one that declared the encoder would be claiming the peer's.
fn coverage(arm: Arm, measurement: &Measurement) -> (Vec<&'static str>, Vec<&'static str>) {
    let receive = &measurement.receive;
    let mut covers: Vec<(&'static str, bool)> = Vec::new();
    if let Some(send) = &measurement.send {
        // One evidence for both, and it is the right one: a datagram exists
        // only because a frame was encoded and then packetised, so a count of
        // them reached each in turn.
        covers.push(("opus encoder", send.packets > 0));
        covers.push(("rtp packetiser", send.packets > 0));
    }
    if arm.accounts() {
        covers.push(("udp transport", receive.packets > 0));
        covers.push(("opus decoder", receive.frames_decoded > 0));
        covers.push(("tone analysis", receive.tone.analysed_frames > 0));
    }
    (
        covers.iter().map(|(name, _)| *name).collect(),
        covers
            .iter()
            .filter(|(_, reached)| *reached)
            .map(|(name, _)| *name)
            .collect(),
    )
}

fn checks(arm: Arm, measurement: &Measurement, frame_ms: u32) -> Vec<Value> {
    let mut checks = Vec::new();

    // The population everything else in this document is counted over, and the
    // evidence the zeros below lean on.
    let population = if arm.accounts() {
        checks.push(json!({
            "name": "datagrams arrived",
            "kind": "must_not_be_zero",
            "reads": "packets_received",
            "why": "every count, every percentile and the tone in this document is taken over \
                    these packets, so a run that received none makes each of them an absence \
                    rather than a number, and a receiver pointed at a peer that never sent \
                    reports exactly the zeros a lossless path reports",
        }));
        "packets_received"
    } else {
        checks.push(json!({
            "name": "datagrams went onto the wire",
            "kind": "must_not_be_zero",
            "reads": "packets_sent",
            "why": "the far end's loss figure is a fraction of what this end sent, and a sender \
                    that sent nothing makes a receiver that received nothing look like a radio \
                    that lost everything",
        }));
        "packets_sent"
    };

    checks.push(json!({
        "name": "no datagram exceeded the MTU",
        "kind": "must_be_zero",
        "reads": "datagram_bytes_over_mtu",
        "population": population,
        "why": format!(
            "one Opus frame is one datagram in this payload format, and a {frame_ms} ms frame at \
             this bitrate is 81 payload bytes at p50 under a 12 byte RTP header; the \
             {MAX_UDP_PAYLOAD} byte budget is chosen so that the datagram clears the Ethernet \
             MTU with room for IPv6 and UDP, so anything over it means something fragmented, \
             which a datagram an order of magnitude inside the budget cannot have done"
        ),
    }));

    if measurement.send.is_some() {
        checks.push(json!({
            "name": "the socket refused nothing",
            "kind": "must_be_zero",
            "reads": "send_failures",
            "population": "packets_sent",
            "why": "a datagram the socket refused never reached the air, so counting it against \
                    the link would charge the radio for this host's own doing, and telling those \
                    two apart is what the loss figure this phase owes is worth; the population is \
                    what was offered, because nothing offered is never refused",
        }));
    }

    if !arm.accounts() {
        return checks;
    }

    checks.push(json!({
        "name": "the timestamp advanced by the frame's samples every time",
        "kind": "must_equal",
        "reads": "timestamp_exact",
        "equals": "timestamp_pairs",
        "why": format!(
            "the RTP timestamp is a sample counter, so it advances by the {} samples of the \
             frame in the packet carrying it whatever the network does to that packet, and the \
             pairs are normalised by sequence distance so that loss between two of them cannot \
             excuse a drift; one that drifted with the sender's scheduling would leave a \
             receiver unable to tell a late packet from a packet describing a later moment",
            measurement.config.frame_samples(),
        ),
    }));

    if !arm.may_lose() {
        checks.push(json!({
            "name": "no packet went missing",
            "kind": "must_be_zero",
            "reads": "packets_missing",
            "population": "packets_received",
            "why": "this path never touches a wire, so a sequence number that never arrived is a \
                    buffer this code sized and there is nowhere else for it to have come from; \
                    the population is what did arrive, because a stream nobody received has no \
                    holes in it",
        }));
        checks.push(json!({
            "name": "nothing was duplicated",
            "kind": "must_be_zero",
            "reads": "duplicates",
            "population": "packets_received",
            "why": "a copy of a datagram comes from a retransmitting driver or a bridged \
                    interface, and this path has neither, so a duplicate here is this code \
                    counting one arrival twice; over the radio it is a normal event, which is \
                    why the radio arm states no such criterion",
        }));
        checks.push(json!({
            "name": "nothing arrived out of order",
            "kind": "must_be_zero",
            "reads": "reordered",
            "population": "packets_received",
            "why": "there is one queue between the two halves and it cannot deliver its second \
                    entry first, so an arrival older than the furthest the stream has got is \
                    this code's doing; a radio reorders whenever a retry succeeds behind a \
                    fresher frame",
        }));
        if measurement.send.is_some() {
            checks.push(json!({
                "name": "every packet that was sent arrived",
                "kind": "must_be_zero",
                "reads": "packets_lost",
                "population": "packets_sent",
                "why": "the sending and receiving halves are one process on one clock here, so \
                        what went out and what came back are two counts of the same thing taken \
                        microseconds apart, and the difference is not a tolerance; the \
                        population is what was sent, because nothing sent is never lost",
            }));
        }
    }

    if matches!(
        measurement.receive.verification,
        Verification::Digests { .. }
    ) {
        checks.push(json!({
            "name": "every payload arrived byte for byte",
            "kind": "must_equal",
            "reads": "payloads_verified",
            "equals": "packets_received",
            "why": "a datagram arrives whole or does not arrive at all, since a UDP checksum \
                    failure is a drop rather than a delivery, so a payload whose bytes differ \
                    from the digest of what was sent is a fault in this code and never the \
                    link's; every arrival is compared exactly once, which is what makes the two \
                    counts the same count",
        }));
    }

    checks.push(json!({
        "name": "the left channel decodes to its own tone",
        "kind": "must_be_within",
        "reads": "tone_left_hz",
        "target": CONTRACT.left_hz,
        "tolerance": TONE_TOLERANCE_HZ,
        "why": format!(
            "a packet count and a byte count agree just as happily when every payload is \
             plausible rubbish, and this project has read that agreement as success four times; \
             {} Hz comes out of the samples the decoder returned, at a window resolution of 2 Hz",
            CONTRACT.left_hz,
        ),
    }));
    checks.push(json!({
        "name": "the right channel decodes to its own tone",
        "kind": "must_be_within",
        "reads": "tone_right_hz",
        "target": CONTRACT.right_hz,
        "tolerance": TONE_TOLERANCE_HZ,
        "why": format!(
            "the two channels carry frequencies a thousand hertz apart so that channel order is \
             provable rather than assumed, and a right channel reading {} Hz is what a fold to \
             mono and a channel encoded twice both look like",
            CONTRACT.left_hz,
        ),
    }));

    checks
}

/// What the run established that no criterion votes on, which on the radio arm
/// is the whole point of running it.
fn findings(arm: Arm, measurement: &Measurement, frame_ms: u32, largest: usize) -> Vec<String> {
    let receive = &measurement.receive;
    let mut findings = Vec::new();

    if arm.accounts() {
        findings.push(format!(
            "{frame_ms} ms frames arrived as datagrams of at most {largest} bytes, {:.1} kbps \
             effective over {} packets",
            measurement.effective_kbps(),
            receive.packets,
        ));
    } else {
        findings.push(format!(
            "{} datagrams of at most {largest} bytes left this host at {:.1} kbps effective; \
             what arrived is the far end's to state, and this end cannot subtract a count taken \
             on another machine over another interval from its own",
            measurement.send.as_ref().map_or(0, |send| send.packets),
            measurement.effective_kbps(),
        ));
    }

    // The measurement, and the reason the radio arm exists. Stated over what the
    // sequence numbers say rather than over a difference of two machines'
    // counts, because the two ends' windows do not begin or end together.
    if arm.may_lose() && arm.accounts() {
        let offered = receive.packets + receive.packets_missing;
        let percent = if offered == 0 {
            0.0
        } else {
            receive.packets_missing as f64 * 100.0 / offered as f64
        };
        findings.push(format!(
            "the radio lost {} of {offered} packets by sequence number, {percent:.3} %, with {} \
             reordered and {} duplicated - measured here and not failed on, because a gate \
             demanding zero loss over Wi-Fi would be demanding a different radio",
            receive.packets_missing, receive.reordered, receive.duplicates,
        ));
    }

    if matches!(receive.verification, Verification::NotApplicable) && arm.accounts() {
        findings.push(
            "payload bytes are unverifiable with the sender on another machine, and the decoded \
             tone stands in: comparing digests would put the verification on the link it is \
             trying to measure"
                .to_string(),
        );
    }

    findings
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::time::Duration;

    use lanplay_audio_capture::{Percentiles, Tone, ToneReport};
    use lanplay_transport::Ssrc;

    use super::*;
    use crate::config::{CodecConfig, FrameDuration};
    use crate::rtp_probe::{ReceiveReport, SendReport};

    fn address(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}")
            .parse()
            .expect("a literal address")
    }

    /// A fixture rather than a run of the probe, so that what these tests
    /// defend is the document's shape and not whatever this machine's loopback
    /// happened to do this morning. The numbers are ten seconds of 5 ms frames
    /// as the loopback arm measures them: 2000 packets averaging 93 bytes, the
    /// largest of them 127.
    fn measurement() -> Measurement {
        Measurement {
            config: CodecConfig::contract(FrameDuration::Ms5, 128_000),
            bind: address(5_008),
            send_to: Some(address(5_008)),
            send: Some(SendReport {
                packets: 2_000,
                wire_bytes: 186_000,
                largest_datagram: 127,
                send_us: percentiles(6, 41),
                send_failures: 0,
            }),
            receive: receive_report(),
        }
    }

    /// The same run with the sender on another machine, which is both the radio
    /// receiver and the control: neither has digests, and neither can subtract
    /// what it never sent.
    fn listening() -> Measurement {
        Measurement {
            config: CodecConfig::contract(FrameDuration::Ms5, 128_000),
            bind: address(5_008),
            send_to: None,
            send: None,
            receive: ReceiveReport {
                verification: Verification::NotApplicable,
                ..receive_report()
            },
        }
    }

    fn receive_report() -> ReceiveReport {
        ReceiveReport {
            ssrc: Some(Ssrc(0x1234_5678)),
            packets: 2_000,
            bytes: 186_000,
            largest_datagram: 127,
            timestamp_pairs: 1_999,
            timestamp_exact: 1_999,
            sequence_gaps: 0,
            packets_missing: 0,
            reordered: 0,
            duplicates: 0,
            verification: Verification::Digests {
                verified: 2_000,
                mismatched: 0,
                unverifiable: 0,
            },
            not_rtp: 0,
            wrong_payload_type: 0,
            empty_payload: 0,
            foreign_ssrc: 0,
            frames_decoded: 2_000,
            decode_failures: 0,
            arrival_us: percentiles(4_998, 5_400),
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
            ended_on_silence: false,
            error: None,
        }
    }

    fn percentiles(p50: u64, p99: u64) -> Option<Percentiles> {
        Some(Percentiles {
            count: 2_000,
            min: p50,
            p50,
            p95: p99,
            p99,
            max: p99 * 2,
        })
    }

    fn options(send_to: Option<SocketAddr>) -> Options {
        Options {
            bind: address(5_008),
            send_to,
            seconds: 10.0,
            frame: FrameDuration::Ms5,
        }
    }

    fn parsed(measurement: &Measurement, arm: Arm) -> Value {
        let text = document(
            measurement,
            options(measurement.send_to),
            arm,
            UNIX_EPOCH + Duration::from_millis(1_755_212_345_678),
            10.0,
            matches!(arm, Arm::Control).then_some(20_250_815),
            Some("abc1234"),
        );
        serde_json::from_str(&text).expect("the document is JSON")
    }

    fn names(document: &Value) -> Vec<String> {
        document["checks"]
            .as_array()
            .expect("checks are a list")
            .iter()
            .map(|check| {
                check["reads"]
                    .as_str()
                    .expect("a check reads a name")
                    .to_string()
            })
            .collect()
    }

    /// The document has to survive `xtask`'s parser, which refuses a check
    /// without a reason and a `must_be_zero` without a population. Nothing in
    /// this crate can call that parser, so what is defended here is the half a
    /// test on this side can see: every field that parser demands is present,
    /// and no check reads a name the run did not report - which is the defect
    /// that left a gate reading `target_ms` after it became `margin_ms`.
    #[test]
    fn every_check_reads_an_observation_the_run_reported() {
        for (measured, arm) in [
            (measurement(), Arm::Loopback),
            (measurement(), Arm::RadioSender),
            (listening(), Arm::RadioReceiver),
            (listening(), Arm::Control),
        ] {
            let document = parsed(&measured, arm);
            let observations = document["observations"]
                .as_object()
                .expect("observations are a flat map of numbers by name");
            for (name, value) in observations {
                assert!(value.is_number(), "{name} is not a number: {value}");
            }

            let checks = document["checks"].as_array().expect("checks are a list");
            assert!(!checks.is_empty(), "{arm:?} states no criterion at all");
            for check in checks {
                let reads = check["reads"].as_str().expect("a check reads by name");
                assert!(
                    observations.contains_key(reads),
                    "{arm:?} reads {reads} and reports it nowhere",
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
                        "{arm:?} names {population} as a population and reports it nowhere",
                    );
                }
                if check["kind"] == "must_equal" {
                    let equals = check["equals"]
                        .as_str()
                        .expect("a must_equal names its other side");
                    assert!(
                        observations.contains_key(equals),
                        "{arm:?} compares against {equals} and reports it nowhere",
                    );
                }
                assert!(
                    check.get("verdict").is_none() && check.get("value").is_none(),
                    "a probe states criteria and never conclusions: {check}",
                );
            }
        }
    }

    #[test]
    fn the_numbers_the_gate_reports_are_the_ones_the_run_measured() {
        let document = parsed(&measurement(), Arm::Loopback);
        let observations = &document["observations"];
        assert_eq!(observations["packets_sent"], 2_000.0);
        assert_eq!(observations["packets_received"], 2_000.0);
        assert_eq!(observations["packets_lost"], 0.0);
        assert_eq!(observations["timestamp_exact"], 1_999.0);
        assert_eq!(observations["largest_datagram_bytes"], 127.0);
        assert_eq!(observations["datagram_bytes_over_mtu"], 0.0);
        assert_eq!(observations["payloads_verified"], 2_000.0);
        assert_eq!(observations["tone_left_hz"], 997.0);
        assert_eq!(observations["tone_right_hz"], 1_997.0);
        // 186000 bytes over two thousand 5 ms packets is ten seconds of audio.
        assert_eq!(observations["effective_kbps"], 148.8);
        assert_eq!(document["run"]["started_unix_ms"], 1_755_212_345_678u64);
        assert_eq!(document["run"]["commit"], "abc1234");
        assert_eq!(document["run"]["seed"], Value::Null);
        assert_eq!(
            document["declared"],
            json!([
                "opus encoder",
                "rtp packetiser",
                "udp transport",
                "opus decoder",
                "tone analysis"
            ])
        );
        assert_eq!(document["declared"], document["exercised"]);
    }

    /// The whole worth of the negative control is that it is judged against the
    /// criteria the measuring arm is judged against. A control holding a rule
    /// nothing else holds would be evidence about that rule, and a control that
    /// cannot be compared to the arm it is aimed at is a control that tested
    /// nothing.
    #[test]
    fn the_control_states_the_loopback_criteria_and_names_itself() {
        let control = parsed(&listening(), Arm::Control);
        let loopback = parsed(&measurement(), Arm::Loopback);

        for criterion in ["packets_missing", "duplicates", "reordered"] {
            assert!(
                names(&control).contains(&criterion.to_string()),
                "the control states no criterion on {criterion}, so the loopback arm's strongest \
                 claim is as unproven as it was",
            );
            assert!(names(&loopback).contains(&criterion.to_string()));
        }

        let arm = control["run"]["arm"].as_str().expect("an arm is named");
        assert!(arm.contains("negative control"), "the arm reads {arm}");
        // A control whose seed is not in the document is a control nobody can
        // run again, and this one has already had to be.
        assert_eq!(control["run"]["seed"], 20_250_815u64);
    }

    /// The radio arm reports what it lost and states no criterion about it,
    /// which is the difference between this gate measuring the link and this
    /// gate demanding a different one.
    #[test]
    fn the_radio_arm_reports_loss_where_the_loopback_arm_forbids_it() {
        let mut measured = listening();
        measured.receive.packets = 1_960;
        measured.receive.packets_missing = 40;
        measured.receive.sequence_gaps = 37;
        measured.receive.reordered = 3;
        measured.receive.duplicates = 2;
        let document = parsed(&measured, Arm::RadioReceiver);

        for absent in ["packets_missing", "duplicates", "reordered", "packets_lost"] {
            assert!(
                !names(&document).contains(&absent.to_string()),
                "the radio arm states a criterion on {absent}, which fails a run for the radio \
                 being a radio",
            );
        }
        assert_eq!(document["observations"]["packets_missing"], 40.0);

        let findings = document["findings"]
            .as_array()
            .expect("findings are a list")
            .iter()
            .map(|finding| finding.as_str().expect("a finding is a sentence"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            findings.contains("40 of 2000 packets by sequence number, 2.000 %"),
            "the loss this arm exists to measure is not in its findings: {findings}",
        );
    }

    /// The sending end of a two-machine arm receives nothing by design, so a
    /// document from it that stated criteria about arrivals would fail every
    /// run of a working arrangement.
    #[test]
    fn the_sending_end_claims_only_what_left_this_host() {
        let mut measured = measurement();
        measured.send_to = Some(address(5_010));
        measured.receive = ReceiveReport {
            packets: 0,
            bytes: 0,
            largest_datagram: 0,
            timestamp_pairs: 0,
            timestamp_exact: 0,
            frames_decoded: 0,
            verification: Verification::Digests {
                verified: 0,
                mismatched: 0,
                unverifiable: 0,
            },
            arrival_us: None,
            tone: ToneReport::empty(),
            ..receive_report()
        };
        let document = parsed(&measured, Arm::RadioSender);

        assert_eq!(
            names(&document),
            vec!["packets_sent", "datagram_bytes_over_mtu", "send_failures"]
        );
        assert_eq!(
            document["declared"],
            json!(["opus encoder", "rtp packetiser"])
        );
        assert_eq!(document["declared"], document["exercised"]);
        assert_eq!(document["environment"]["end"], "sender");
        // The datagram this end can speak for is the one it sent, and the
        // receiving side of the same struct saw nothing at all.
        assert_eq!(document["observations"]["largest_datagram_bytes"], 127.0);
        assert!(
            document["observations"].get("packets_lost").is_none(),
            "the sending end subtracted a count taken on another machine",
        );
    }

    /// The failure this arrangement exists to prevent: a run that measured no
    /// tone leaves the two tone criteria undecidable rather than passing them,
    /// and does not claim the subsystem it never reached.
    #[test]
    fn a_run_that_measured_no_tone_reports_neither_a_frequency_nor_the_subsystem() {
        let mut measured = listening();
        measured.receive.tone = ToneReport::empty();
        measured.receive.frames_decoded = 0;
        let document = parsed(&measured, Arm::RadioReceiver);

        let observations = document["observations"]
            .as_object()
            .expect("observations are a map");
        for absent in ["tone_left_hz", "tone_right_hz", "tone_channels_distinct"] {
            assert!(
                !observations.contains_key(absent),
                "{absent} was reported for a tone nobody measured",
            );
        }
        assert_eq!(document["exercised"], json!(["udp transport"]));
    }

    /// A datagram over the MTU is stated as the amount it is over by, so that
    /// the criterion is a zero over a population rather than a comparison
    /// against a constant the evaluator would have to be told.
    #[test]
    fn a_fragmented_datagram_is_stated_as_the_bytes_it_is_over_by() {
        let mut measured = measurement();
        measured.receive.largest_datagram = MAX_UDP_PAYLOAD + 44;
        let document = parsed(&measured, Arm::Loopback);
        assert_eq!(document["observations"]["datagram_bytes_over_mtu"], 44.0);
    }
}
