//! Deciding one gate run, which is the only place in this repository where a
//! verdict is decided.
//!
//! Nothing here measures anything and nothing here parses prose. An envelope
//! goes in, a block of text and a pass or a fail come out, and the whole of it
//! is exercised over fixture documents on any machine at any hour, which is
//! what a harness that shelled out to Python for the same judgement never was.
//!
//! Two rules live here rather than in each probe, because both were already
//! implemented several times and each implementation was a chance to differ.
//! The first is that a zero over a population of zero is unavailable and never
//! a pass. The second is that a gate whose declared subsystems were not all
//! exercised fails, whatever its checks said, since a run that reports coverage
//! it did not reach is the one failure mode a criterion cannot catch.

use std::fmt::Write as _;

use crate::envelope::{Check, Criterion, Envelope};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Pass,
    Fail,
    /// The check could not be decided. Not a pass: a gate holding one of these
    /// says which and why, and does not claim what it did not test.
    Unavailable,
}

pub struct Judgement<'a> {
    pub check: &'a Check,
    pub outcome: Outcome,
    /// The numbers, as the fragment that goes next to the check's name. A
    /// failure that does not carry its numbers sends its reader back to the run
    /// to find them, and one gate's failure named the wrong thing for a whole
    /// session because of it.
    pub detail: String,
}

/// Columns wide enough for the names the migrated gate uses, so that the
/// numbers line up down the block and a reader compares them by eye.
const STATUS: usize = 6;
const NAME: usize = 40;

pub fn judge<'a>(check: &'a Check, envelope: &Envelope) -> Judgement<'a> {
    let Some(value) = envelope.observation(&check.reads) else {
        return unavailable(
            check,
            format!(
                "nothing named {} was observed, so there is no number to decide on",
                check.reads
            ),
        );
    };

    match &check.criterion {
        Criterion::MustBeZero { population } => {
            let Some(count) = envelope.observation(population) else {
                return unavailable(
                    check,
                    format!(
                        "nothing named {population} was observed, so nothing says whether there \
                         was anything to be zero about"
                    ),
                );
            };
            if count == 0.0 {
                return unavailable(
                    check,
                    format!("{population} is zero, so a zero here is an absence and not a result"),
                );
            }
            let detail = format!("{} over {} {population}", number(value), number(count));
            decide(check, value == 0.0, detail)
        }
        Criterion::MustNotBeZero => decide(check, value > 0.0, number(value)),
        Criterion::MustEqual { equals } => {
            let Some(other) = envelope.observation(equals) else {
                return unavailable(
                    check,
                    format!("nothing named {equals} was observed, so there is nothing to compare"),
                );
            };
            let detail = format!("{} against {} {equals}", number(value), number(other));
            decide(check, value == other, detail)
        }
        Criterion::MustBeBelow { bound } => {
            let detail = format!("{} against a bound of {}", number(value), number(*bound));
            decide(check, value < *bound, detail)
        }
        Criterion::MustBeWithin { target, tolerance } => {
            let detail = format!(
                "{} against {} give or take {}",
                number(value),
                number(*target),
                number(*tolerance)
            );
            decide(check, (value - target).abs() <= *tolerance, detail)
        }
    }
}

fn decide<'a>(check: &'a Check, held: bool, detail: String) -> Judgement<'a> {
    Judgement {
        check,
        outcome: if held { Outcome::Pass } else { Outcome::Fail },
        detail,
    }
}

fn unavailable<'a>(check: &'a Check, detail: String) -> Judgement<'a> {
    Judgement {
        check,
        outcome: Outcome::Unavailable,
        detail,
    }
}

/// The block a person reads, and whether the run passed.
pub fn report(envelope: &Envelope) -> (String, bool) {
    let judgements: Vec<Judgement<'_>> = envelope
        .checks
        .iter()
        .map(|check| judge(check, envelope))
        .collect();

    let mut out = String::new();
    let mut header = format!(
        "gate      {}   arm {}   span {:.1} s",
        envelope.gate, envelope.run.arm, envelope.run.span_s
    );
    if let Some(commit) = &envelope.run.commit {
        let _ = write!(header, "   commit {commit}");
    }
    let _ = writeln!(out, "{header}");
    // The provenance, and not decoration: an arm nobody can date, whose
    // arguments are not stated, and whose seed is unknown is a result nobody
    // can re-run, and re-running one is how two of this project's harness
    // defects were found.
    let _ = writeln!(out, "started   {} unix ms", envelope.run.started_unix_ms);
    if !envelope.run.args.is_empty() {
        let stated: Vec<String> = envelope
            .run
            .args
            .iter()
            .map(|(name, value)| format!("{name} {}", plain(value)))
            .collect();
        let _ = writeln!(out, "args      {}", stated.join(", "));
    }
    if let Some(seed) = envelope.run.seed {
        let _ = writeln!(out, "seed      {seed}");
    }
    let _ = writeln!(out, "covers    {}", envelope.declared.join(", "));

    if !envelope.environment.is_empty() {
        out.push_str("\nenvironment\n\n");
        for (name, value) in &envelope.environment {
            let _ = writeln!(out, "  {name:<NAME$} {}", plain(value));
        }
    }

    if !envelope.observations.is_empty() {
        out.push_str("\nobservations\n\n");
        for (name, value) in &envelope.observations {
            let _ = writeln!(out, "  {name:<NAME$} {}", number(*value));
        }
    }

    // Above the verdict on purpose: a failing arm does not make its measurement
    // uninteresting, and the exchange rate a phase exists to produce has to
    // survive the arm that failed to meet a criterion beside it.
    if !envelope.findings.is_empty() {
        out.push_str("\nfindings\n\n");
        for finding in &envelope.findings {
            let _ = writeln!(out, "  FINDING {finding}");
        }
    }

    section(
        &mut out,
        "what must not be zero",
        &judgements,
        |criterion| matches!(criterion, Criterion::MustNotBeZero),
    );
    section(&mut out, "what must be zero", &judgements, |criterion| {
        matches!(criterion, Criterion::MustBeZero { .. })
    });
    section(&mut out, "what else must hold", &judgements, |criterion| {
        matches!(
            criterion,
            Criterion::MustEqual { .. }
                | Criterion::MustBeBelow { .. }
                | Criterion::MustBeWithin { .. }
        )
    });

    let untested: Vec<&Judgement<'_>> = judgements
        .iter()
        .filter(|judged| judged.outcome == Outcome::Unavailable)
        .collect();
    if !untested.is_empty() {
        out.push_str("\nwhat could not be tested\n\n");
        for judged in &untested {
            let _ = writeln!(
                out,
                "  {:<STATUS$}{:<NAME$} {}",
                "", judged.check.name, judged.detail
            );
            let _ = writeln!(out, "  {:<STATUS$}{}", "", judged.check.why);
        }
    }

    let mut failures: Vec<String> = judgements
        .iter()
        .filter(|judged| judged.outcome == Outcome::Fail)
        .map(|judged| {
            format!(
                "{}: {}\n     {}",
                judged.check.name, judged.detail, judged.check.why
            )
        })
        .collect();
    for subsystem in envelope.unexercised() {
        failures.push(format!(
            "{subsystem} was declared and nothing exercised it, so this run would be claiming \
             what it did not test"
        ));
    }
    if envelope.checks.is_empty() {
        failures.push(
            "the run states no check at all, so there is no invocation of it that could fail"
                .to_string(),
        );
    }

    out.push('\n');
    if failures.is_empty() {
        let held = judgements.len() - untested.len();
        let _ = writeln!(
            out,
            "PASS {held} of {} checks hold and every declared subsystem was exercised",
            judgements.len()
        );
        if !untested.is_empty() {
            let _ = writeln!(
                out,
                "     {} could not be tested and is named above, so this run does not claim it",
                untested.len()
            );
        }
        return (out, true);
    }
    for failure in &failures {
        let _ = writeln!(out, "FAIL {failure}");
    }
    (out, false)
}

fn section(
    out: &mut String,
    heading: &str,
    judgements: &[Judgement<'_>],
    wanted: impl Fn(&Criterion) -> bool,
) {
    // An unavailable check is reported once, in its own section, rather than
    // here as well: printing it twice invites a reader to count it as tested.
    let mine: Vec<&Judgement<'_>> = judgements
        .iter()
        .filter(|judged| judged.outcome != Outcome::Unavailable && wanted(&judged.check.criterion))
        .collect();
    if mine.is_empty() {
        return;
    }
    let _ = write!(out, "\n{heading}\n\n");
    for judged in mine {
        let status = match judged.outcome {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Unavailable => unreachable!("filtered above"),
        };
        let _ = writeln!(
            out,
            "  {status:<STATUS$}{:<NAME$} {}",
            judged.check.name, judged.detail
        );
        let _ = writeln!(out, "  {:<STATUS$}{}", "", judged.check.why);
    }
}

/// Observations are numbers, and a packet count printed as `6001` and a bitrate
/// printed as `129.707` are both the number somebody wants to read. Three
/// decimals is where a kbps figure stops carrying information and a float's
/// last bits start.
fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// A string from the environment reads as itself rather than as a quoted JSON
/// string, since the block is for a person.
fn plain(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures rather than envelopes from a run, so that what these tests
    /// defend cannot drift with a probe's output.
    fn envelope(observations: &str, checks: &str) -> Envelope {
        document(
            observations,
            checks,
            r#""declared": [], "exercised": [], "findings": []"#,
        )
    }

    fn document(observations: &str, checks: &str, rest: &str) -> Envelope {
        let text = format!(
            r#"{{
  "gate": "fixture-gate",
  "run": {{ "started_unix_ms": 1755212345678, "span_s": 10.0, "arm": "clean" }},
  "observations": {{ {observations} }},
  "checks": [{checks}],
  {rest}
}}"#
        );
        Envelope::parse(&text).expect("the fixture parses")
    }

    const ZERO_OVER_PACKETS: &str = r#"{ "name": "no position gaps", "kind": "must_be_zero",
        "reads": "gaps", "population": "packets",
        "why": "a device position advancing by other than the previous packet's frame count is a gap of a known size" }"#;

    #[test]
    fn a_zero_over_a_population_of_zero_is_unavailable_and_never_a_pass() {
        let envelope = envelope(r#""gaps": 0, "packets": 0"#, ZERO_OVER_PACKETS);
        let judged = judge(&envelope.checks[0], &envelope);
        assert_eq!(judged.outcome, Outcome::Unavailable);
        assert!(
            judged.detail.contains("packets is zero"),
            "the reason names the empty population: {}",
            judged.detail
        );

        let (text, passed) = report(&envelope);
        assert!(
            passed,
            "an unavailable check does not fail the run on its own"
        );
        assert!(
            text.contains("what could not be tested"),
            "and it is reported: {text}"
        );
        assert!(
            !text.contains("PASS  no position gaps"),
            "it is never printed as a pass: {text}"
        );
    }

    #[test]
    fn a_zero_over_a_real_population_passes() {
        let envelope = envelope(r#""gaps": 0, "packets": 6001"#, ZERO_OVER_PACKETS);
        let judged = judge(&envelope.checks[0], &envelope);
        assert_eq!(judged.outcome, Outcome::Pass);
        assert_eq!(judged.detail, "0 over 6001 packets");

        let (text, passed) = report(&envelope);
        assert!(passed);
        assert!(text.contains("what must be zero"), "{text}");
        assert!(!text.contains("what could not be tested"), "{text}");
    }

    #[test]
    fn a_non_zero_over_a_real_population_fails_and_the_failure_carries_its_numbers() {
        let envelope = envelope(r#""gaps": 3, "packets": 6001"#, ZERO_OVER_PACKETS);
        let (text, passed) = report(&envelope);
        assert!(!passed);
        assert!(
            text.contains("FAIL no position gaps: 3 over 6001 packets"),
            "{text}"
        );
        assert!(
            text.contains("a gap of a known size"),
            "the reason travels with the failure: {text}"
        );
    }

    #[test]
    fn a_check_reading_an_observation_nobody_reported_is_unavailable() {
        let envelope = envelope(
            r#""packets": 6001"#,
            r#"{ "name": "margin", "kind": "must_be_below", "reads": "margin_ms", "bound": 4.0,
                 "why": "a renamed field is what a bespoke parser reads as a zero" }"#,
        );
        let judged = judge(&envelope.checks[0], &envelope);
        assert_eq!(judged.outcome, Outcome::Unavailable);
        assert!(
            judged
                .detail
                .contains("nothing named margin_ms was observed"),
            "{}",
            judged.detail
        );
        assert!(report(&envelope).1, "it does not fail the run on its own");
    }

    #[test]
    fn the_evidence_check_fails_on_a_run_that_produced_nothing() {
        let envelope = envelope(
            r#""packets": 0"#,
            r#"{ "name": "packets were captured", "kind": "must_not_be_zero", "reads": "packets",
                 "why": "every percentile below is computed over these" }"#,
        );
        let judged = judge(&envelope.checks[0], &envelope);
        assert_eq!(judged.outcome, Outcome::Fail);
        assert_eq!(judged.detail, "0");
    }

    #[test]
    fn an_equality_holds_exactly_and_refuses_when_one_side_is_absent() {
        let both = envelope(
            r#""submitted": 480000, "returned": 480000"#,
            r#"{ "name": "lengths agree", "kind": "must_equal", "reads": "submitted",
                 "equals": "returned", "why": "Opus is exact in length" }"#,
        );
        assert_eq!(judge(&both.checks[0], &both).outcome, Outcome::Pass);

        let one = envelope(
            r#""submitted": 480000"#,
            r#"{ "name": "lengths agree", "kind": "must_equal", "reads": "submitted",
                 "equals": "returned", "why": "Opus is exact in length" }"#,
        );
        assert_eq!(judge(&one.checks[0], &one).outcome, Outcome::Unavailable);
    }

    #[test]
    fn a_bound_is_strict_and_a_tolerance_is_inclusive() {
        let below = envelope(
            r#""encode_p99_us": 499"#,
            r#"{ "name": "under a tenth of the frame", "kind": "must_be_below",
                 "reads": "encode_p99_us", "bound": 500.0,
                 "why": "a tenth of the 5 ms frame, past which the codec is in the latency path" }"#,
        );
        assert_eq!(judge(&below.checks[0], &below).outcome, Outcome::Pass);

        let at = envelope(
            r#""encode_p99_us": 500"#,
            r#"{ "name": "under a tenth of the frame", "kind": "must_be_below",
                 "reads": "encode_p99_us", "bound": 500.0,
                 "why": "a tenth of the 5 ms frame, past which the codec is in the latency path" }"#,
        );
        assert_eq!(judge(&at.checks[0], &at).outcome, Outcome::Fail);

        let edge = envelope(
            r#""tone_left_hz": 1002"#,
            r#"{ "name": "the left tone", "kind": "must_be_within", "reads": "tone_left_hz",
                 "target": 997.0, "tolerance": 5.0,
                 "why": "the window resolves 2 Hz, so five is two bins and a margin" }"#,
        );
        assert_eq!(judge(&edge.checks[0], &edge).outcome, Outcome::Pass);

        let outside = envelope(
            r#""tone_left_hz": 1003"#,
            r#"{ "name": "the left tone", "kind": "must_be_within", "reads": "tone_left_hz",
                 "target": 997.0, "tolerance": 5.0,
                 "why": "the window resolves 2 Hz, so five is two bins and a margin" }"#,
        );
        assert_eq!(judge(&outside.checks[0], &outside).outcome, Outcome::Fail);
    }

    #[test]
    fn a_declared_subsystem_that_was_not_exercised_fails_the_run() {
        let envelope = document(
            r#""packets": 6001"#,
            r#"{ "name": "packets were captured", "kind": "must_not_be_zero", "reads": "packets",
                 "why": "every percentile below is computed over these" }"#,
            r#""declared": ["encoder", "decoder"], "exercised": ["encoder"]"#,
        );
        let (text, passed) = report(&envelope);
        assert!(!passed, "every check held, and the run still fails: {text}");
        assert!(
            text.contains("FAIL decoder was declared and nothing exercised it"),
            "{text}"
        );
        assert!(
            text.contains("claiming what it did not test"),
            "and it says why that matters: {text}"
        );
    }

    #[test]
    fn a_run_that_states_no_check_cannot_pass() {
        let envelope = envelope(r#""packets": 6001"#, "");
        let (text, passed) = report(&envelope);
        assert!(!passed);
        assert!(
            text.contains("FAIL the run states no check at all"),
            "{text}"
        );
    }

    #[test]
    fn findings_are_reported_even_when_a_check_failed() {
        let envelope = document(
            r#""gaps": 3, "packets": 6001"#,
            ZERO_OVER_PACKETS,
            r#""declared": [], "exercised": [],
               "findings": ["the endpoint is 48 kHz stereo, so the path to Opus needs no resampler"]"#,
        );
        let (text, passed) = report(&envelope);
        assert!(!passed);
        let finding = text
            .find("FINDING the endpoint is 48 kHz stereo")
            .expect("the finding is reported");
        let failure = text
            .find("FAIL no position gaps")
            .expect("so is the failure");
        assert!(
            finding < failure,
            "and the finding is above the verdict, so it survives it: {text}"
        );
    }

    #[test]
    fn the_numbers_are_printed_as_a_reader_states_them() {
        assert_eq!(number(6001.0), "6001");
        assert_eq!(number(129.70666666666666), "129.707");
        assert_eq!(number(997.0002), "997");
        assert_eq!(number(0.6), "0.6");
    }
}
