//! The one document a gate emits, and the only statement of its shape.
//!
//! Eight harnesses each printed keyed prose and then read it back with a
//! bespoke regular expression, and three defects that cost this project a
//! measurement came out of exactly that: a pattern anchored with `^` and no
//! multiline flag that read 6001 captured packets as none, a rate whose count
//! and whose span were taken over different intervals, and a gate still reading
//! a field a probe had renamed. One reader, tested, retires the whole family.
//!
//! The parsing refuses rather than repairs. Every field a criterion depends on
//! is mandatory, so a document that omits one fails to deserialise instead of
//! deserialising into a check that cannot fail. A `why` defaulting to empty and
//! a `population` defaulting to absent would each hand the two rules this file
//! exists to enforce back to whoever wrote the probe, which is where they were
//! when the same defect appeared in five subsystems.
//!
//! One deliberate departure from the illustration in `docs/testing.md`, which
//! showed a check carrying `value` and `verdict`: both are refused here. A
//! probe reports observations and states criteria; the evaluator reads the
//! value out of the observation the check names and decides. A value restated
//! beside the name of the observation it came from is a second copy that can
//! disagree with the first, and a verdict written by the measured party is not
//! a verdict.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde::de::{self, Deserializer};
use serde_json::Value;

use crate::Abort;

/// One gate run, as the probe that performed it states it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub gate: String,
    pub run: Run,
    /// Whatever the run depended on and could read. Defaulted, because a run
    /// that depended on nothing outside itself has nothing to put here and an
    /// empty table forced on every probe would say less than nothing.
    #[serde(default)]
    pub environment: BTreeMap<String, Value>,
    /// What the gate claims to cover, and what it reached. The evaluator fails
    /// a run whose declaration was not met, because that rule held in exactly
    /// the harnesses that happened to implement it and eight probes enforcing
    /// it separately is eight chances to enforce it differently.
    pub declared: Vec<String>,
    pub exercised: Vec<String>,
    /// Numbers by name, flat on purpose: every check names one of these, and a
    /// nesting nobody needs is a path expression somebody has to get right.
    pub observations: BTreeMap<String, f64>,
    pub checks: Vec<Check>,
    /// What the run established that no criterion votes on. Defaulted, because
    /// a run can honestly find nothing worth stating.
    #[serde(default)]
    pub findings: Vec<String>,
}

impl Envelope {
    pub fn load(path: &Path) -> Result<Self, Abort> {
        let text = fs::read_to_string(path)
            .map_err(|err| Abort::new(format!("could not read {}: {err}", path.display())))?;
        Self::parse(&text).map_err(|Abort(why)| Abort::new(format!("{}: {why}", path.display())))
    }

    pub fn parse(text: &str) -> Result<Self, Abort> {
        serde_json::from_str(text).map_err(|err| Abort::new(err.to_string()))
    }

    pub fn observation(&self, name: &str) -> Option<f64> {
        self.observations.get(name).copied()
    }

    /// Subsystems the gate declared and nothing reached. A run that reports
    /// these as covered is claiming what it did not test, which is the failure
    /// the `--expect` rule elsewhere in this repository already refuses.
    pub fn unexercised(&self) -> Vec<&str> {
        self.declared
            .iter()
            .filter(|subsystem| !self.exercised.contains(subsystem))
            .map(String::as_str)
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub started_unix_ms: u64,
    pub span_s: f64,
    /// Required when any fault is injected, and nothing in the document says
    /// whether one was, so this is the one conditionally mandatory field and
    /// the parser cannot enforce it. A gate that injects faults without stating
    /// its seed produces a result nobody can reproduce.
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub args: BTreeMap<String, Value>,
    #[serde(default)]
    pub commit: Option<String>,
    /// Which arm this is. Mandatory even when a gate has one, because a gate
    /// with a negative control has two and a result filed under neither is a
    /// result nobody can place.
    pub arm: String,
}

/// One criterion. `reads` names the observation it is about, `why` is the
/// sentence a reviewer checks the criterion against, and the criterion itself
/// carries only the parameters its kind reads.
#[derive(Clone, Debug, PartialEq)]
pub struct Check {
    pub name: String,
    pub reads: String,
    pub why: String,
    pub criterion: Criterion,
}

#[derive(Clone, Debug, PartialEq)]
// The shared prefix is the point rather than an accident: these are the check kinds named
// in docs/testing.md, and a call site reading `Criterion::MustBeZero` states the criterion
// in the words a reviewer will use for it. Shortening them to `Zero` and `NotZero` would
// save four characters and lose the sentence.
#[allow(clippy::enum_variant_names)]
pub enum Criterion {
    /// `population` names the observation that proves the check had something
    /// to be zero about. It is the mechanism against the defect that recurred
    /// five times: zero discontinuities over zero packets reads as a clean
    /// sweep, so a zero population makes the check unavailable and never a
    /// pass.
    MustBeZero {
        population: String,
    },
    /// The evidence check, which is what a `must_be_zero` leans on.
    MustNotBeZero,
    MustEqual {
        equals: String,
    },
    MustBeBelow {
        bound: f64,
    },
    MustBeWithin {
        target: f64,
        tolerance: f64,
    },
}

/// Deserialised through a flat intermediate so that a document stating a
/// criterion it cannot support is refused while it is being read, rather than
/// parsing into something an evaluator has to validate afterwards. A validation
/// step is a step something can be run without.
impl<'de> Deserialize<'de> for Check {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        RawCheck::deserialize(deserializer)?
            .into_check()
            .map_err(de::Error::custom)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    name: String,
    kind: Kind,
    reads: String,
    why: String,
    #[serde(default)]
    population: Option<String>,
    #[serde(default)]
    equals: Option<String>,
    #[serde(default)]
    bound: Option<f64>,
    #[serde(default)]
    target: Option<f64>,
    #[serde(default)]
    tolerance: Option<f64>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
// The shared prefix is the point rather than an accident: these are the check kinds named
// in docs/testing.md, and a call site reading `Criterion::MustBeZero` states the criterion
// in the words a reviewer will use for it. Shortening them to `Zero` and `NotZero` would
// save four characters and lose the sentence.
#[allow(clippy::enum_variant_names)]
enum Kind {
    MustBeZero,
    MustNotBeZero,
    MustEqual,
    MustBeBelow,
    MustBeWithin,
}

impl Kind {
    fn word(self) -> &'static str {
        match self {
            Kind::MustBeZero => "must_be_zero",
            Kind::MustNotBeZero => "must_not_be_zero",
            Kind::MustEqual => "must_equal",
            Kind::MustBeBelow => "must_be_below",
            Kind::MustBeWithin => "must_be_within",
        }
    }

    /// What this kind reads besides `reads`, so that a parameter nothing reads
    /// is refused by name. A bound on a `must_not_be_zero` is a bound its
    /// author believes is being enforced.
    fn parameters(self) -> &'static [&'static str] {
        match self {
            Kind::MustBeZero => &["population"],
            Kind::MustNotBeZero => &[],
            Kind::MustEqual => &["equals"],
            Kind::MustBeBelow => &["bound"],
            Kind::MustBeWithin => &["target", "tolerance"],
        }
    }
}

impl RawCheck {
    fn into_check(self) -> Result<Check, String> {
        let Self {
            name,
            kind,
            reads,
            why,
            population,
            equals,
            bound,
            target,
            tolerance,
        } = self;

        for (field, stated) in [
            ("population", population.is_some()),
            ("equals", equals.is_some()),
            ("bound", bound.is_some()),
            ("target", target.is_some()),
            ("tolerance", tolerance.is_some()),
        ] {
            if stated && !kind.parameters().contains(&field) {
                return Err(format!(
                    "the check named {name:?} is a {kind} and states a {field}, which no {kind} \
                     reads: a parameter nothing reads is a criterion its author believes is \
                     enforced",
                    kind = kind.word(),
                ));
            }
        }

        if why.trim().is_empty() {
            return Err(format!(
                "the check named {name:?} states no reason: a criterion whose reason cannot be \
                 written down is one nobody can review, and four of this project's five criterion \
                 defects would have been visible in the writing"
            ));
        }

        let criterion = match kind {
            Kind::MustBeZero => Criterion::MustBeZero {
                population: population.ok_or_else(|| missing(&name, kind, "population"))?,
            },
            Kind::MustNotBeZero => Criterion::MustNotBeZero,
            Kind::MustEqual => Criterion::MustEqual {
                equals: equals.ok_or_else(|| missing(&name, kind, "equals"))?,
            },
            Kind::MustBeBelow => Criterion::MustBeBelow {
                bound: bound.ok_or_else(|| missing(&name, kind, "bound"))?,
            },
            Kind::MustBeWithin => Criterion::MustBeWithin {
                target: target.ok_or_else(|| missing(&name, kind, "target"))?,
                tolerance: tolerance.ok_or_else(|| missing(&name, kind, "tolerance"))?,
            },
        };

        Ok(Check {
            name,
            reads,
            why,
            criterion,
        })
    }
}

/// The reason each parameter is mandatory, said at the point of refusal rather
/// than left to a reader to look up: a probe author sees this message and not
/// this file.
fn missing(name: &str, kind: Kind, field: &str) -> String {
    let because = match field {
        "population" => {
            "a zero is only evidence beside the count that could have been something else, and a \
             check that names no population passes hardest when nothing happened"
        }
        "equals" => "an equality with one side is not a comparison",
        "bound" => {
            "a bound stated nowhere is a comparison against whatever its reader assumes, and the \
             derivation belongs in the reason next to it"
        }
        "target" => "a value within a tolerance of nothing is not a criterion",
        "tolerance" => {
            "a target with no tolerance is either an exact equality, which floating point does not \
             give, or a bound nobody stated"
        }
        _ => "the kind cannot be decided without it",
    };
    format!(
        "the check named {name:?} is a {} without a {field}: {because}",
        kind.word()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture rather than an envelope a probe produced, because a test
    /// written against a real run measures whatever that run happened to
    /// contain and starts failing for reasons that have nothing to do with
    /// this code.
    fn document(checks: &str) -> String {
        format!(
            r#"{{
  "gate": "fixture-gate",
  "run": {{ "started_unix_ms": 1755212345678, "span_s": 10.0, "arm": "clean" }},
  "declared": ["thing"],
  "exercised": ["thing"],
  "observations": {{ "packets": 6001, "gaps": 0 }},
  "checks": [{checks}]
}}"#
        )
    }

    #[test]
    fn a_must_be_zero_check_without_a_population_is_refused_while_it_is_read() {
        let refused = Envelope::parse(&document(
            r#"{ "name": "no gaps", "kind": "must_be_zero", "reads": "gaps",
                 "why": "a gap of a known size is stronger than a packet count" }"#,
        ))
        .expect_err("a must_be_zero with no population is not a document");
        assert!(
            refused.0.contains("without a population"),
            "the refusal names the missing field: {}",
            refused.0
        );
        assert!(
            refused.0.contains("passes hardest when nothing happened"),
            "the refusal says why the field is mandatory: {}",
            refused.0
        );
    }

    #[test]
    fn a_check_without_a_reason_is_refused_while_it_is_read() {
        let absent = Envelope::parse(&document(
            r#"{ "name": "some packets", "kind": "must_not_be_zero", "reads": "packets" }"#,
        ))
        .expect_err("a check with no reason is not a document");
        assert!(
            absent.0.contains("missing field `why`"),
            "serde names the absent field: {}",
            absent.0
        );

        let empty = Envelope::parse(&document(
            r#"{ "name": "some packets", "kind": "must_not_be_zero", "reads": "packets",
                 "why": "   " }"#,
        ))
        .expect_err("a reason of whitespace is not a reason");
        assert!(
            empty.0.contains("states no reason"),
            "an empty reason is refused as an absent one: {}",
            empty.0
        );
    }

    #[test]
    fn a_verdict_written_by_the_probe_is_refused_rather_than_ignored() {
        let refused = Envelope::parse(&document(
            r#"{ "name": "some packets", "kind": "must_not_be_zero", "reads": "packets",
                 "why": "every rate below is computed over these", "verdict": "pass" }"#,
        ))
        .expect_err("a probe does not decide");
        assert!(
            refused.0.contains("unknown field `verdict`"),
            "the refusal names the field: {}",
            refused.0
        );
    }

    #[test]
    fn a_parameter_the_kind_never_reads_is_refused_by_name() {
        let refused = Envelope::parse(&document(
            r#"{ "name": "some packets", "kind": "must_not_be_zero", "reads": "packets",
                 "why": "every rate below is computed over these", "bound": 10.0 }"#,
        ))
        .expect_err("a bound nothing reads is not a criterion");
        assert!(
            refused
                .0
                .contains("states a bound, which no must_not_be_zero reads"),
            "the refusal names the kind and the parameter: {}",
            refused.0
        );
    }

    #[test]
    fn an_absent_mandatory_field_of_the_run_is_refused_rather_than_defaulted() {
        let refused = Envelope::parse(
            r#"{
  "gate": "fixture-gate",
  "run": { "started_unix_ms": 1, "span_s": 1.0 },
  "declared": [],
  "exercised": [],
  "observations": {},
  "checks": []
}"#,
        )
        .expect_err("a run with no arm is not a document");
        assert!(
            refused.0.contains("missing field `arm`"),
            "the refusal names the absent field: {}",
            refused.0
        );
    }

    #[test]
    fn the_shape_docs_testing_states_parses_with_every_kind_present() {
        let envelope = Envelope::parse(&document(
            r#"{ "name": "some packets", "kind": "must_not_be_zero", "reads": "packets",
                 "why": "every rate below is computed over these" },
               { "name": "no gaps", "kind": "must_be_zero", "reads": "gaps",
                 "population": "packets", "why": "a gap of a known size is the stronger check" },
               { "name": "counts agree", "kind": "must_equal", "reads": "packets",
                 "equals": "packets", "why": "a length that disagrees is a defect" },
               { "name": "under the frame", "kind": "must_be_below", "reads": "gaps",
                 "bound": 500.0, "why": "a tenth of the frame, so it cannot be the term" },
               { "name": "near the tone", "kind": "must_be_within", "reads": "packets",
                 "target": 6000.0, "tolerance": 1.0, "why": "two Hz resolution over the window" }"#,
        ))
        .expect("the fixture parses");
        assert_eq!(envelope.gate, "fixture-gate");
        assert_eq!(envelope.run.arm, "clean");
        assert_eq!(envelope.observation("packets"), Some(6001.0));
        assert_eq!(envelope.checks.len(), 5);
        assert_eq!(
            envelope.checks[1].criterion,
            Criterion::MustBeZero {
                population: "packets".to_string()
            }
        );
        assert_eq!(
            envelope.checks[4].criterion,
            Criterion::MustBeWithin {
                target: 6000.0,
                tolerance: 1.0
            }
        );
        assert!(envelope.findings.is_empty());
        assert!(envelope.unexercised().is_empty());
    }

    #[test]
    fn a_declared_subsystem_missing_from_exercised_is_named() {
        let envelope = Envelope::parse(
            r#"{
  "gate": "fixture-gate",
  "run": { "started_unix_ms": 1, "span_s": 1.0, "arm": "clean" },
  "declared": ["encoder", "decoder", "tone analysis"],
  "exercised": ["encoder", "tone analysis"],
  "observations": {},
  "checks": []
}"#,
        )
        .expect("the fixture parses");
        assert_eq!(envelope.unexercised(), vec!["decoder"]);
    }
}
