//! The harness index, read as data rather than as eighteen shell scripts.
//!
//! Nothing here touches the machine. The index is parsed, the gates are ranked
//! against a set of satisfactions somebody else measured, and the answer is
//! printed. Keeping the measuring in `environment` is what makes this file
//! testable at all: a verdict computed from a map can be exercised on any
//! machine, at any hour, with the lab host switched off.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Abort;

/// A `negative_control` beginning with this has never been observed failing.
/// The index says so honestly rather than leaving the field out, and `--debt`
/// counts them.
pub const DEBT_MARKER: &str = "none yet";

/// What a probe found out about one requirement.
///
/// The third case is the whole point. A requirement nobody could check is not
/// a requirement that is missing: a suite that reads those as absent shrinks
/// without anyone deciding it should, and one that reads them as present fails
/// for reasons that have nothing to do with the code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Satisfaction {
    Present(String),
    Absent(String),
    Unknown(String),
}

impl Satisfaction {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Present(_) => "present",
            Self::Absent(_) => "absent",
            Self::Unknown(_) => "unknown",
        }
    }

    pub fn why(&self) -> &str {
        match self {
            Self::Present(why) | Self::Absent(why) | Self::Unknown(why) => why,
        }
    }
}

/// What this machine can satisfy, one entry per requirement word.
pub type Environment = Vec<(String, Satisfaction)>;

pub fn look_up<'a>(environment: &'a Environment, requirement: &str) -> Option<&'a Satisfaction> {
    environment
        .iter()
        .find(|(name, _)| name == requirement)
        .map(|(_, satisfaction)| satisfaction)
}

/// Whether a gate can run now, and if not, which requirement decided that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Runnable,
    Blocked { requirement: String, why: String },
    Unknown { requirement: String, why: String },
}

impl Verdict {
    pub fn state(&self) -> &'static str {
        match self {
            Self::Runnable => "runnable",
            Self::Blocked { .. } => "blocked",
            Self::Unknown { .. } => "unknown",
        }
    }

    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::Runnable)
    }
}

pub struct Gate {
    pub name: String,
    pub script: String,
    pub proves: String,
    pub phase: String,
    pub requires: Vec<String>,
    /// Arms the harness runs when it can and declares unavailable when it
    /// cannot. These never exclude a gate: a harness that reports an arm as
    /// unavailable is still worth running for the arms it did exercise.
    pub optional_requires: Vec<String>,
    pub minutes: u32,
    pub negative_control: String,
    pub human_attention: Option<String>,
    pub notes: Option<String>,
}

impl Gate {
    /// A gate whose failure mode has never been observed is a gate nobody has
    /// grounds to trust; the two false passes in this project came from the
    /// one harness that lacked a control.
    pub fn owes_a_negative_control(&self) -> bool {
        self.negative_control
            .trim_start()
            .to_ascii_lowercase()
            .starts_with(DEBT_MARKER)
    }

    /// The first requirement that stops the gate. An absent requirement beats
    /// an unknown one even when the unknown comes first, because a measured
    /// fact is a better answer than the absence of one - a gate needing both a
    /// host that did not answer and an encoder on that host is blocked on the
    /// host, and saying so names something a person can act on.
    pub fn verdict(&self, environment: &Environment) -> Verdict {
        let mut unknown = None;
        for requirement in &self.requires {
            match look_up(environment, requirement) {
                Some(Satisfaction::Present(_)) => {}
                Some(Satisfaction::Absent(why)) => {
                    return Verdict::Blocked {
                        requirement: requirement.clone(),
                        why: why.clone(),
                    };
                }
                Some(Satisfaction::Unknown(why)) => {
                    unknown.get_or_insert_with(|| (requirement.clone(), why.clone()));
                }
                // A requirement word with no probe behind it. Treating it as
                // met would be the silent shrink from the other direction:
                // gates would be offered as runnable on the strength of a
                // word nothing in this program understands.
                None => {
                    unknown.get_or_insert_with(|| {
                        (
                            requirement.clone(),
                            "nothing in xtask detects this requirement".to_string(),
                        )
                    });
                }
            }
        }
        match unknown {
            Some((requirement, why)) => Verdict::Unknown { requirement, why },
            None => Verdict::Runnable,
        }
    }
}

pub struct Index {
    pub schema: u32,
    pub docs: String,
    /// The scripts in `tools/` that are deliberately not gates. Stated in the
    /// index rather than known by a test, so that adding a script forces the
    /// question - harness or plumbing - to be answered in the same diff.
    pub plumbing: Vec<String>,
    pub gates: Vec<Gate>,
}

impl Index {
    /// Where the index lives, resolved against this crate rather than against
    /// the current directory, so that `cargo xtask gates` answers the same
    /// from anywhere in the tree.
    pub fn default_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tools/gates.toml")
    }

    pub fn load(path: &Path) -> Result<Self, Abort> {
        let text = fs::read_to_string(path)
            .map_err(|err| Abort::new(format!("could not read {}: {err}", path.display())))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, Abort> {
        let document: Document =
            toml::from_str(text).map_err(|err| Abort::new(format!("gate index: {err}")))?;

        let mut gates = Vec::with_capacity(document.gates.len());
        // Each gate is deserialised from its own value rather than the whole
        // table at once, because a typed map would be sorted by name and the
        // file states its gates grouped by subsystem, which is the order a
        // person reads them in.
        for (name, value) in document.gates {
            let entry: Entry = value
                .try_into()
                .map_err(|err| Abort::new(format!("gate index: {name}: {err}")))?;
            gates.push(Gate {
                name,
                script: entry.script,
                proves: entry.proves,
                phase: entry.phase,
                requires: entry.requires,
                optional_requires: entry.optional_requires,
                minutes: entry.minutes,
                negative_control: entry.negative_control,
                human_attention: entry.human_attention,
                notes: entry.notes,
            });
        }
        Ok(Self {
            schema: document.meta.schema,
            docs: document.meta.docs,
            plumbing: document.meta.plumbing,
            gates,
        })
    }

    /// Every requirement word the index uses, optional arms included, so that
    /// detection runs once for the whole listing and probes nothing the index
    /// never mentions.
    pub fn requirements(&self) -> BTreeSet<&str> {
        self.gates
            .iter()
            .flat_map(|gate| gate.requires.iter().chain(&gate.optional_requires))
            .map(String::as_str)
            .collect()
    }

    pub fn debt(&self) -> impl Iterator<Item = &Gate> {
        self.gates
            .iter()
            .filter(|gate| gate.owes_a_negative_control())
    }
}

/// The document as the file states it. Unknown keys are refused rather than
/// ignored: a misspelt `negative_controls` that parses quietly is an index
/// that describes something other than what its author wrote.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    meta: Meta,
    gates: toml::Table,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Meta {
    schema: u32,
    docs: String,
    #[serde(default)]
    plumbing: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    script: String,
    proves: String,
    phase: String,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    optional_requires: Vec<String>,
    minutes: u32,
    negative_control: String,
    #[serde(default)]
    human_attention: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

/// Which gates a listing states at all. Runnability is not a filter here:
/// `--runnable` reports the excluded gates too, because the useful half of
/// that answer is which requirement excluded them, and the printing separates
/// the two. Whether it reports at all follows from whether anything was
/// detected, which is the only way it can be reported honestly.
pub struct Selection {
    pub debt: bool,
}

impl Selection {
    fn keeps(&self, gate: &Gate) -> bool {
        !self.debt || gate.owes_a_negative_control()
    }
}

pub fn human(index: &Index, selection: &Selection, environment: Option<&Environment>) -> String {
    let mut out = String::new();
    let total = index.gates.len();
    let owed = index.debt().count();

    if let Some(environment) = environment {
        out.push_str("environment\n");
        for (requirement, satisfaction) in environment {
            let _ = writeln!(
                out,
                "  {requirement:<16} {:<8} {}",
                satisfaction.state(),
                satisfaction.why()
            );
        }
        out.push('\n');
    }

    let verdicts: Vec<Option<Verdict>> = index
        .gates
        .iter()
        .map(|gate| environment.map(|env| gate.verdict(env)))
        .collect();
    let kept: Vec<(&Gate, Option<&Verdict>)> = index
        .gates
        .iter()
        .zip(&verdicts)
        .map(|(gate, verdict)| (gate, verdict.as_ref()))
        .filter(|(gate, _)| selection.keeps(gate))
        .collect();

    if selection.debt {
        let _ = writeln!(
            out,
            "debt      {owed} of {total} gates have no negative control yet",
        );
        out.push('\n');
    }

    if environment.is_some() {
        let runnable: Vec<_> = kept
            .iter()
            .filter(|(_, verdict)| verdict.is_some_and(Verdict::is_runnable))
            .collect();
        let _ = writeln!(out, "runnable  {} of {}", runnable.len(), kept.len());
        for (gate, _) in &runnable {
            describe(&mut out, gate, !selection.debt);
        }
        let excluded: Vec<_> = kept
            .iter()
            .filter(|(_, verdict)| !verdict.is_some_and(Verdict::is_runnable))
            .collect();
        if !excluded.is_empty() {
            let _ = writeln!(out, "\nexcluded  {}", excluded.len());
            // The requirement and its state, and not the reason: the
            // environment block above states each reason once, and thirteen
            // gates excluded by one unreachable host would otherwise print
            // the same sentence thirteen times, which is how a listing
            // teaches its reader to skim.
            for (gate, verdict) in &excluded {
                let (requirement, state) = match verdict {
                    Some(Verdict::Blocked { requirement, .. }) => (requirement, "absent"),
                    Some(Verdict::Unknown { requirement, .. }) => (requirement, "unknown"),
                    _ => continue,
                };
                let _ = writeln!(out, "  {:<22} {requirement} {state}", gate.name);
            }
        }
        return out;
    }

    let _ = writeln!(
        out,
        "gates     {} of {total}, described in {}",
        kept.len(),
        index.docs
    );
    for (gate, _) in &kept {
        describe(&mut out, gate, !selection.debt);
    }
    out
}

/// `owed` is false when every gate in the listing owes a control anyway, so
/// that a `--debt` listing does not say so nine times.
fn describe(out: &mut String, gate: &Gate, owed: bool) {
    let requires = if gate.requires.is_empty() {
        "nothing".to_string()
    } else {
        gate.requires.join(", ")
    };
    let _ = writeln!(
        out,
        "  {:<22} {:<10} {:>3} min   {requires}",
        gate.name, gate.phase, gate.minutes
    );
    let _ = writeln!(out, "  {:22} {}", "", gate.proves);
    if !gate.optional_requires.is_empty() {
        let _ = writeln!(
            out,
            "  {:22} optional arms: {}",
            "",
            gate.optional_requires.join(", ")
        );
    }
    if let Some(attention) = &gate.human_attention {
        // Not in `requires`, because only one arm of the harness needs it and
        // the rest run unattended. Printed anyway: an agent that starts this
        // one alone gets an arm nobody was there for.
        let _ = writeln!(out, "  {:22} needs a person: {attention}", "");
    }
    if owed && gate.owes_a_negative_control() {
        let _ = writeln!(out, "  {:22} no negative control yet", "");
    }
}

/// One document, whatever the flags were, so that an agent parses the same
/// shape every time and filters on `status` rather than on which command it
/// happened to run.
pub fn json(index: &Index, selection: &Selection, environment: Option<&Environment>) -> String {
    let verdicts: Vec<Option<Verdict>> = index
        .gates
        .iter()
        .map(|gate| environment.map(|env| gate.verdict(env)))
        .collect();

    let gates: Vec<serde_json::Value> = index
        .gates
        .iter()
        .zip(&verdicts)
        .filter(|(gate, _)| selection.keeps(gate))
        .map(|(gate, verdict)| {
            let mut entry = serde_json::json!({
                "name": gate.name,
                "script": gate.script,
                "proves": gate.proves,
                "phase": gate.phase,
                "requires": gate.requires,
                "optional_requires": gate.optional_requires,
                "minutes": gate.minutes,
                "negative_control": gate.negative_control,
                "owes_negative_control": gate.owes_a_negative_control(),
                "human_attention": gate.human_attention,
                "notes": gate.notes,
            });
            if let Some(verdict) = verdict {
                let status = match verdict {
                    Verdict::Runnable => serde_json::json!({ "state": "runnable" }),
                    Verdict::Blocked { requirement, why } | Verdict::Unknown { requirement, why } => {
                        serde_json::json!({
                            "state": verdict.state(),
                            "requirement": requirement,
                            "why": why,
                        })
                    }
                };
                entry["status"] = status;
            }
            entry
        })
        .collect();

    let mut document = serde_json::json!({
        "schema": index.schema,
        "docs": index.docs,
        "counts": {
            "gates": index.gates.len(),
            "listed": gates.len(),
            "owing_a_negative_control": index.debt().count(),
            "runnable": verdicts
                .iter()
                .filter(|verdict| verdict.as_ref().is_some_and(Verdict::is_runnable))
                .count(),
        },
        // Stated so that a reader looking at `tools/` can tell a harness this
        // program forgot from a helper it was never meant to list.
        "plumbing": index.plumbing,
        "gates": gates,
    });
    if let Some(environment) = environment {
        let map: serde_json::Map<String, serde_json::Value> = environment
            .iter()
            .map(|(requirement, satisfaction)| {
                (
                    requirement.clone(),
                    serde_json::json!({
                        "state": satisfaction.state(),
                        "why": satisfaction.why(),
                    }),
                )
            })
            .collect();
        document["environment"] = serde_json::Value::Object(map);
    } else {
        // Nobody looked, and a document that omits the key says so less
        // clearly than one that states it.
        document["counts"]["runnable"] = serde_json::Value::Null;
    }
    serde_json::to_string_pretty(&document).expect("a document built from owned values serialises")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture rather than the real index, because a test written against
    /// `tools/gates.toml` measures whatever that file says today and starts
    /// failing for reasons that have nothing to do with this code.
    const FIXTURE: &str = r#"
[meta]
schema = 1
docs = "docs/testing.md"

[gates.local-gate]
script = "tools/local-gate.sh"
proves = "something that needs no hardware"
phase = "F1"
requires = []
minutes = 1
negative_control = "the empty arm, which must fail"

[gates.host-gate]
script = "tools/host-gate.sh"
proves = "something that needs the host"
phase = "F2"
requires = ["windows-host"]
optional_requires = ["radio"]
minutes = 2
negative_control = "none yet"

[gates.encoder-gate]
script = "tools/encoder-gate.sh"
proves = "something that needs the host's encoder"
phase = "F3"
requires = ["nvidia-nvenc"]
minutes = 3
negative_control = "none yet: the arm exists but is not wired"
notes = "a note"

[gates.attended-gate]
script = "tools/attended-gate.sh"
proves = "something a person has to watch"
phase = "F4"
requires = ["human-attention"]
minutes = 1
negative_control = "not applicable"
human_attention = "somebody has to look at it"
"#;

    fn fixture() -> Index {
        Index::parse(FIXTURE).expect("the fixture index parses")
    }

    fn environment() -> Environment {
        vec![
            (
                "windows-host".to_string(),
                Satisfaction::Present("ssh windows answered".to_string()),
            ),
            (
                "radio".to_string(),
                Satisfaction::Present("en0 has an address".to_string()),
            ),
            (
                "nvidia-nvenc".to_string(),
                Satisfaction::Unknown("the host did not answer, so nobody looked".to_string()),
            ),
            (
                "human-attention".to_string(),
                Satisfaction::Absent("a person has to be here".to_string()),
            ),
        ]
    }

    #[test]
    fn a_gate_whose_requirements_are_all_met_is_runnable() {
        let index = fixture();
        let environment = environment();
        let local = &index.gates[0];
        assert_eq!(local.name, "local-gate");
        assert_eq!(local.verdict(&environment), Verdict::Runnable);

        let host = &index.gates[1];
        assert_eq!(host.verdict(&environment), Verdict::Runnable);
    }

    #[test]
    fn an_unmet_requirement_excludes_a_gate_and_is_named() {
        let index = fixture();
        let environment = environment();
        let attended = index
            .gates
            .iter()
            .find(|gate| gate.name == "attended-gate")
            .expect("the fixture has an attended gate");
        assert_eq!(
            attended.verdict(&environment),
            Verdict::Blocked {
                requirement: "human-attention".to_string(),
                why: "a person has to be here".to_string(),
            }
        );
    }

    #[test]
    fn an_unknown_requirement_is_reported_unknown_and_never_runnable() {
        let index = fixture();
        let environment = environment();
        let encoder = index
            .gates
            .iter()
            .find(|gate| gate.name == "encoder-gate")
            .expect("the fixture has an encoder gate");
        let verdict = encoder.verdict(&environment);
        assert_eq!(
            verdict,
            Verdict::Unknown {
                requirement: "nvidia-nvenc".to_string(),
                why: "the host did not answer, so nobody looked".to_string(),
            }
        );
        assert!(!verdict.is_runnable());
    }

    #[test]
    fn a_measured_absence_beats_an_unmeasured_one() {
        let index = fixture();
        let mut environment = environment();
        environment[2] = (
            "nvidia-nvenc".to_string(),
            Satisfaction::Unknown("nobody looked".to_string()),
        );
        let gate = Gate {
            name: "both".to_string(),
            script: "tools/both.sh".to_string(),
            proves: "nothing".to_string(),
            phase: "F5".to_string(),
            // The unknown one first, so an order-driven answer would name it.
            requires: vec![
                "nvidia-nvenc".to_string(),
                "human-attention".to_string(),
            ],
            optional_requires: vec![],
            minutes: 1,
            negative_control: "none".to_string(),
            human_attention: None,
            notes: None,
        };
        assert_eq!(
            gate.verdict(&environment),
            Verdict::Blocked {
                requirement: "human-attention".to_string(),
                why: "a person has to be here".to_string(),
            }
        );
        let _ = index;
    }

    #[test]
    fn a_requirement_nothing_detects_is_unknown_rather_than_met() {
        let gate = Gate {
            name: "novel".to_string(),
            script: "tools/novel.sh".to_string(),
            proves: "nothing".to_string(),
            phase: "F6".to_string(),
            requires: vec!["a-word-nobody-taught-xtask".to_string()],
            optional_requires: vec![],
            minutes: 1,
            negative_control: "none".to_string(),
            human_attention: None,
            notes: None,
        };
        let verdict = gate.verdict(&environment());
        assert_eq!(verdict.state(), "unknown");
        assert!(!verdict.is_runnable());
    }

    #[test]
    fn an_optional_requirement_never_excludes_a_gate() {
        let index = fixture();
        let environment = vec![
            (
                "windows-host".to_string(),
                Satisfaction::Present("answered".to_string()),
            ),
            (
                "radio".to_string(),
                Satisfaction::Absent("en0 has no address".to_string()),
            ),
        ];
        let host = index
            .gates
            .iter()
            .find(|gate| gate.name == "host-gate")
            .expect("the fixture has a host gate");
        assert_eq!(host.verdict(&environment), Verdict::Runnable);
        let _ = &environment;
    }

    #[test]
    fn debt_counts_exactly_the_gates_whose_control_begins_none_yet() {
        let index = fixture();
        let owed: Vec<&str> = index.debt().map(|gate| gate.name.as_str()).collect();
        assert_eq!(owed, ["host-gate", "encoder-gate"]);
        assert!(
            !index.gates[0].owes_a_negative_control(),
            "a stated control is not a debt"
        );
        assert!(
            !index
                .gates
                .iter()
                .find(|gate| gate.name == "attended-gate")
                .expect("attended gate")
                .owes_a_negative_control(),
            "'not applicable' is a decision, not a debt"
        );
    }

    #[test]
    fn the_json_is_one_document_holding_every_gate() {
        let index = fixture();
        let selection = Selection { debt: false };
        let text = json(&index, &selection, None);
        let document: serde_json::Value =
            serde_json::from_str(&text).expect("the whole output is one JSON document");
        let gates = document["gates"].as_array().expect("gates is an array");
        assert_eq!(gates.len(), index.gates.len());
        let names: Vec<&str> = gates
            .iter()
            .map(|gate| gate["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(
            names,
            ["local-gate", "host-gate", "encoder-gate", "attended-gate"],
            "the document states the gates in the order the index does"
        );
        assert_eq!(document["counts"]["owing_a_negative_control"], 2);
        assert!(gates[0].get("status").is_none(), "nobody detected anything");
    }

    #[test]
    fn the_runnable_json_carries_a_status_and_a_reason_for_every_gate() {
        let index = fixture();
        let selection = Selection { debt: false };
        let environment = environment();
        let text = json(&index, &selection, Some(&environment));
        let document: serde_json::Value = serde_json::from_str(&text).expect("one document");
        let gates = document["gates"].as_array().expect("gates is an array");
        assert_eq!(gates.len(), 4);
        assert_eq!(document["counts"]["runnable"], 2);
        assert_eq!(document["environment"]["nvidia-nvenc"]["state"], "unknown");

        let encoder = gates
            .iter()
            .find(|gate| gate["name"] == "encoder-gate")
            .expect("the encoder gate is present");
        assert_eq!(encoder["status"]["state"], "unknown");
        assert_eq!(encoder["status"]["requirement"], "nvidia-nvenc");
        assert!(
            encoder["status"]["why"]
                .as_str()
                .expect("a reason")
                .contains("nobody looked")
        );
    }

    #[test]
    fn the_debt_selection_lists_only_what_is_owed() {
        let index = fixture();
        let selection = Selection { debt: true };
        let document: serde_json::Value =
            serde_json::from_str(&json(&index, &selection, None)).expect("one document");
        let names: Vec<&str> = document["gates"]
            .as_array()
            .expect("gates")
            .iter()
            .map(|gate| gate["name"].as_str().expect("a name"))
            .collect();
        assert_eq!(names, ["host-gate", "encoder-gate"]);
        assert_eq!(document["counts"]["gates"], 4);
        assert_eq!(document["counts"]["listed"], 2);
    }

    #[test]
    fn the_human_listing_names_the_requirement_that_excluded_each_gate() {
        let index = fixture();
        let selection = Selection { debt: false };
        let environment = environment();
        let text = human(&index, &selection, Some(&environment));
        assert!(text.contains("runnable  2 of 4"), "{text}");
        assert!(
            text.contains("attended-gate          human-attention absent"),
            "{text}"
        );
        assert!(
            text.contains("encoder-gate           nvidia-nvenc unknown"),
            "{text}"
        );
    }

    #[test]
    fn a_key_the_schema_does_not_know_is_refused() {
        let mistyped = r#"
[meta]
schema = 1
docs = "docs/testing.md"

[gates.typo-gate]
script = "tools/typo-gate.sh"
proves = "nothing"
phase = "F0"
minutes = 1
negative_controls = "plural, and therefore silently ignored by a lenient parser"
"#;
        assert!(Index::parse(mistyped).is_err());
    }

    /// The one test here that reads the real index, because it is the only
    /// thing standing between the index and rot. A gate renamed on disk and
    /// not in the index, or a harness written and never described, would
    /// otherwise be found by an agent planning a session around a listing
    /// that is quietly wrong - and it would be found as a missing file at
    /// minute three of a twenty minute sweep.
    #[test]
    fn the_real_index_and_the_tools_directory_agree() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let index = Index::load(&Index::default_path()).expect("the real index parses");

        let mut named: BTreeSet<String> = BTreeSet::new();
        for gate in &index.gates {
            assert!(
                root.join(&gate.script).is_file(),
                "{} names {}, which does not exist",
                gate.name,
                gate.script
            );
            assert!(
                named.insert(gate.script.clone()),
                "two gates claim {}",
                gate.script
            );
        }
        for script in &index.plumbing {
            assert!(
                root.join(script).is_file(),
                "{script} is listed as plumbing but does not exist"
            );
            named.insert(script.clone());
        }

        let mut undescribed: Vec<String> = Vec::new();
        for entry in fs::read_dir(root.join("tools")).expect("tools/ is readable") {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_none_or(|kind| kind != "sh") {
                continue;
            }
            let relative = format!(
                "tools/{}",
                path.file_name().expect("a file name").to_string_lossy()
            );
            if !named.contains(&relative) {
                undescribed.push(relative);
            }
        }
        undescribed.sort();
        assert!(
            undescribed.is_empty(),
            "these scripts are in tools/ but in neither the index nor its plumbing list: {}",
            undescribed.join(", ")
        );
    }
}
