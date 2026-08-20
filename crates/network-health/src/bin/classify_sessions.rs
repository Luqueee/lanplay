//! Runs [`classify`] over the committed corpus and holds each label against the
//! diagnosis that was written down when the run was taken.
//!
//! A classifier that cannot label runs whose answer is already documented will
//! not label a live one, and finding that out costs no hardware time at all.
//! This binary is the whole of N3's validation, so it is careful about the
//! difference between three outcomes:
//!
//! ```text
//! 0  every session with a recorded diagnosis carries the label that diagnosis implies
//! 1  a label disagrees with a diagnosis
//! 2  refused: a file could not be trusted, or the population was empty or no
//!    longer matches the table
//! ```
//!
//! The third is not a softer first. Nothing was measured, so nothing was decided,
//! and a harness that reported that as agreement would be certifying an absence.
//!
//! A session the reader cannot build a middle tier from is a fourth thing and is
//! none of the above: it is reported per session as `REFUSED` with the missing
//! tier named, expected as such in the table, and counted apart from the
//! classified ones. Calling it a degradation of unknown type would convert "this
//! cannot be read" into "something is wrong with the network".

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lanplay_network_health::corpus::{self, Reading, Session, Tracking, Unreadable};
use lanplay_network_health::{NetworkCondition, RadioHint, classify};

/// What the record says this session was found to be.
enum Expectation {
    Is(NetworkCondition),
    /// No middle tier can be built from it, so it must not be classified at all.
    Refused,
    /// Read and classified and printed, but not checked, because nothing written
    /// down establishes what it was. Guessing a label and then matching it would
    /// be a criterion that cannot fail.
    Unestablished,
}

struct Row {
    expectation: Expectation,
    note: String,
}

/// What standing the corpus gives a condition.
///
/// Four states rather than a count, because "reached by sessions whose diagnosis
/// is written down", "reached by a session nobody has diagnosed", "wired and
/// waiting on an instrument that now exists" and "waiting on a session that has
/// never occurred" are four different kinds of evidence, and reporting them as
/// one number would flatter the classifier.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    Confirmed,
    Undiagnosed,
    /// Wired and unreached, and what it waits on is stated per condition.
    Owed(&'static str),
}

fn main() -> ExitCode {
    let mut root = PathBuf::from("results");
    let mut table = PathBuf::new();
    let mut tracking = Tracking::Committed;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--results" => match arguments.next() {
                Some(value) => root = PathBuf::from(value),
                None => return refuse("--results names no directory"),
            },
            "--expect" => match arguments.next() {
                Some(value) => table = PathBuf::from(value),
                None => return refuse("--expect names no file"),
            },
            // The negative controls assemble corpora by hand, outside any
            // repository, and have to say so. The default is the committed corpus
            // because a validation whose population drifts with whatever is on
            // disk is not a validation of anything stated - a peer's uncommitted
            // scratch directory under results/ widened this one once already.
            "--as-found" => tracking = Tracking::AsFound,
            other => return refuse(&format!("{other} is not an argument this reads")),
        }
    }
    if table.as_os_str().is_empty() {
        return refuse(
            "--expect was not given, and a run with no table of recorded diagnoses checks \
             nothing while printing as though it had",
        );
    }

    match run(&root, &table, tracking) {
        Outcome::Agreed(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Outcome::Disagreed(report) => {
            print!("{report}");
            ExitCode::from(1)
        }
        Outcome::Refused(why) => refuse(&why),
    }
}

fn refuse(why: &str) -> ExitCode {
    eprintln!("REFUSE {why}");
    ExitCode::from(2)
}

enum Outcome {
    Agreed(String),
    Disagreed(String),
    Refused(String),
}

fn run(root: &Path, table: &Path, tracking: Tracking) -> Outcome {
    let (readings, refusals) = match corpus::walk(root, tracking) {
        Ok(both) => both,
        Err(refusal) => {
            return Outcome::Refused(format!("{} {}", refusal.path.display(), refusal.why));
        }
    };
    if !refusals.is_empty() {
        let mut why = format!(
            "{} of the files under {} look like sessions and could not be trusted, so the \
             population this would have decided over is unknown:",
            refusals.len(),
            root.display()
        );
        for refusal in &refusals {
            let _ = write!(why, "\n         {} {}", refusal.path.display(), refusal.why);
        }
        return Outcome::Refused(why);
    }
    if readings.is_empty() {
        return Outcome::Refused(format!(
            "no session under {} carries either shape this reads, and a population of zero \
             agrees with everything",
            root.display()
        ));
    }

    let rows = match read_table(table) {
        Ok(rows) => rows,
        Err(why) => return Outcome::Refused(why),
    };
    if rows.is_empty() {
        return Outcome::Refused(format!("{} states no expectation at all", table.display()));
    }

    // The table and the corpus have to name the same sessions. A session the table
    // has never heard of would be printed and silently unchecked, and a row naming
    // a session that is gone would leave the check quietly smaller than it reads.
    let present: BTreeSet<&str> = readings.iter().map(Reading::name).collect();
    let named: BTreeSet<&str> = rows.keys().map(String::as_str).collect();
    let unnamed: Vec<&&str> = present.difference(&named).collect();
    let missing: Vec<&&str> = named.difference(&present).collect();
    if !unnamed.is_empty() || !missing.is_empty() {
        let mut why = format!("{} no longer describes this corpus:", table.display());
        for name in unnamed {
            let _ = write!(
                why,
                "\n         {name} is a session and the table says nothing about it"
            );
        }
        for name in missing {
            let _ = write!(
                why,
                "\n         {name} is in the table and not in the corpus"
            );
        }
        return Outcome::Refused(why);
    }

    let mut report = String::new();
    let _ = writeln!(
        report,
        "{} sessions under {}, {}.\n",
        readings.len(),
        root.display(),
        match tracking {
            Tracking::Committed => "as git ls-files reports them",
            Tracking::AsFound => "as found on disk, which is not the committed corpus",
        }
    );
    let _ = writeln!(
        report,
        "{:<19} {:<47} {:>6} {:>7} {:>7} {:>8} {:>6} {:>7} {:>7}  radio",
        "condition", "session", "span", "2T/min", "clu/min", "sgapp50", "loss", "aushort", "reord"
    );

    let mut disagreements = Vec::new();
    let mut reached: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
    let mut classified = 0usize;
    let mut refused_rows = 0usize;
    let mut checked = 0usize;
    let mut undiagnosed = 0usize;
    let mut without_radio = 0usize;
    let mut without_loss_tier = 0usize;

    for reading in &readings {
        let row = &rows[reading.name()];
        if reading.radio().is_none() {
            without_radio += 1;
        }
        match reading {
            Reading::Classifiable(session) => {
                classified += 1;
                if session.observation.stream.loss_ratio.is_none() {
                    without_loss_tier += 1;
                }
                let condition = classify(&session.observation.stream);
                let counts = reached.entry(condition.name()).or_default();
                let mark = match &row.expectation {
                    Expectation::Is(expected) if *expected == condition => {
                        checked += 1;
                        counts.0 += 1;
                        " "
                    }
                    Expectation::Is(expected) => {
                        checked += 1;
                        disagreements.push((
                            session.name.clone(),
                            condition.name(),
                            expected.name(),
                            row.note.clone(),
                        ));
                        "X"
                    }
                    Expectation::Refused => {
                        checked += 1;
                        disagreements.push((
                            session.name.clone(),
                            condition.name(),
                            "REFUSED",
                            row.note.clone(),
                        ));
                        "X"
                    }
                    Expectation::Unestablished => {
                        undiagnosed += 1;
                        counts.1 += 1;
                        "~"
                    }
                };
                let _ = writeln!(report, "{mark}{}", session_line(session, condition));
            }
            Reading::Unreadable(unreadable) => {
                refused_rows += 1;
                let mark = match &row.expectation {
                    Expectation::Refused => {
                        checked += 1;
                        " "
                    }
                    Expectation::Is(expected) => {
                        checked += 1;
                        disagreements.push((
                            unreadable.name.clone(),
                            "REFUSED",
                            expected.name(),
                            row.note.clone(),
                        ));
                        "X"
                    }
                    Expectation::Unestablished => {
                        undiagnosed += 1;
                        "~"
                    }
                };
                let _ = writeln!(report, "{mark}{}", refused_line(unreadable));
            }
        }
    }

    let _ = writeln!(
        report,
        "\n  X disagrees with the recorded diagnosis, ~ no diagnosis is recorded, \
         blank agrees\n"
    );

    let _ = writeln!(
        report,
        "{classified} sessions could be turned into a middle tier and were classified. \
         {refused_rows} could not,"
    );
    let _ = writeln!(
        report,
        "and were refused with the missing tier named, which is not a verdict about their links."
    );
    let _ = writeln!(
        report,
        "{checked} of the {} were held against a recorded diagnosis{}. The other {undiagnosed} \
         are read and",
        readings.len(),
        if checked == readings.len() {
            String::new()
        } else {
            format!(
                ", so the check is over {checked} and not {}",
                readings.len()
            )
        }
    );
    let _ = writeln!(
        report,
        "printed with no diagnosis to check them against, which is a different reason for being \
         excluded"
    );
    let _ = writeln!(
        report,
        "than being unreadable. {without_radio} {} read with the radio tier absent, which is what",
        if without_radio == 1 { "was" } else { "were" }
    );
    let _ = writeln!(
        report,
        "NetworkObservation.radio being an Option is for. {without_loss_tier} of the \
         {classified} classified carry"
    );
    let _ = writeln!(
        report,
        "no loss tier at all, so their verdicts were taken on cadence alone - see the limit \
         below."
    );

    let standings = standings(&reached);
    let _ = writeln!(report, "\nwhat standing this corpus gives each condition");
    for (condition, standing, confirmed, seen) in &standings {
        let _ = writeln!(
            report,
            "  {:<19} {}",
            condition.name(),
            match standing {
                Standing::Confirmed =>
                    format!("CONFIRMED by {confirmed} session(s) whose recorded diagnosis says so"),
                Standing::Undiagnosed => format!(
                    "reached {seen} time(s), by no session anybody has diagnosed - UNCONFIRMED"
                ),
                Standing::Owed(waiting) => format!("UNCONFIRMED, waiting on {waiting}"),
            }
        );
    }
    let confirmed = standings
        .iter()
        .filter(|(_, standing, _, _)| *standing == Standing::Confirmed)
        .count();
    let _ = writeln!(
        report,
        "\n{confirmed} of the 6 conditions are confirmed against a recorded diagnosis and {} \
         are not. The two",
        standings.len() - confirmed
    );
    let _ = writeln!(
        report,
        "unconfirmed loss conditions are owed different debts and it matters which: SevereLoss is\n\
         wired and waits only on a session recorded with a loss tier that now exists, while\n\
         CapacityPressure waits on a session that has ever exhibited it, and a lab that has never\n\
         been throughput-limited may never produce one."
    );

    let _ = writeln!(
        report,
        "\nTHE LIMIT ON THIS PHASE, general rather than three special cases. No envelope in the\n\
         committed corpus states what the sender produced or sent, and that single absence is why\n\
         no loss rate is derivable from any of them. The datagram counters - packet_loss,\n\
         reordered, duplicates - have no datagram population anywhere; and au_loss is expected\n\
         minus reconstructed where expected is target_fps times the nominal run length, a number\n\
         nothing produced, fed by three unrelated mechanisms all measured here: link loss, which\n\
         would show in packet_loss; run truncation, which is pcap-parallel/parallel-r2 at 21.00 s\n\
         short of its nominal 120 with 2528 units that are 21.07 s at 120 fps; and host\n\
         under-production, which is why the three phase arms delivering 105.32 to 117.96 access\n\
         units a second against a target of 120 are refused rather than classified. Dividing by\n\
         the nearest available number is what once read a 0.69 per cent reorder figure as 30.8.\n\
         So those counts are carried, printed above as loss, aushort and reord, and read by no\n\
         rule. Three agents reached this independently in one session - from the field units,\n\
         from a reorder figure that could not be right, and from building the tier - which is why\n\
         it is a property of the envelopes rather than one reader's arithmetic."
    );

    // The Option is load-bearing and a run in which every session happened to
    // carry a radio trace would leave it untested while reading as a pass.
    if without_radio == 0 {
        return Outcome::Refused(
            "every session carried a radio trace, so nothing here exercised an absent radio \
             tier and the one property this contract turns on went unchecked"
                .to_string(),
        );
    }
    if checked == 0 {
        return Outcome::Refused(
            "no session was held against a recorded diagnosis, so this run printed labels and \
             checked none of them"
                .to_string(),
        );
    }

    // Each condition the table asks for has to be reached by something, or the
    // rule that produces it is a branch nobody has exercised.
    let mut wanted: BTreeSet<&'static str> = BTreeSet::new();
    for row in rows.values() {
        if let Expectation::Is(condition) = row.expectation {
            wanted.insert(condition.name());
        }
    }
    let unreached: Vec<&&str> = wanted
        .iter()
        .filter(|name| reached.get(**name).map(|c| c.0 + c.1).unwrap_or(0) == 0)
        .collect();
    if !unreached.is_empty() {
        let mut why = String::from(
            "the table expects conditions no session reached, so those rules went unexercised:",
        );
        for name in unreached {
            let _ = write!(why, " {name}");
        }
        return Outcome::Refused(why);
    }

    if disagreements.is_empty() {
        return Outcome::Agreed(report);
    }
    let _ = writeln!(
        report,
        "\n{}:",
        match disagreements.len() {
            1 => String::from("1 session carries a label its recorded diagnosis does not support"),
            many =>
                format!("{many} sessions carry a label their recorded diagnosis does not support"),
        }
    );
    for (name, got, expected, note) in &disagreements {
        let _ = writeln!(
            report,
            "  {name}\n    classified {got}, recorded as {expected}\n    {note}"
        );
    }
    Outcome::Disagreed(report)
}

/// Standing for all six, in the order the taxonomy states them.
///
/// The two loss conditions are named here rather than inferred from a zero count,
/// because "nothing reached it" and "what it is waiting for" are different
/// claims and only a human can state the second.
fn standings(
    reached: &BTreeMap<&'static str, (usize, usize)>,
) -> Vec<(NetworkCondition, Standing, usize, usize)> {
    [
        NetworkCondition::Healthy,
        NetworkCondition::CapacityPressure,
        NetworkCondition::CadenceDegraded,
        NetworkCondition::SevereLoss,
        NetworkCondition::TransientStall,
        NetworkCondition::UnknownDegradation,
    ]
    .into_iter()
    .map(|condition| {
        let (confirmed, seen) = reached.get(condition.name()).copied().unwrap_or((0, 0));
        let standing = if confirmed > 0 {
            Standing::Confirmed
        } else if seen > 0 {
            Standing::Undiagnosed
        } else {
            match condition {
                NetworkCondition::SevereLoss => Standing::Owed(
                    "a session recorded with a loss tier; the committed corpus has none, and \
                     macos/client now emits one",
                ),
                NetworkCondition::CapacityPressure => Standing::Owed(
                    "a session that ever exhibited it, and tools/bitrate-sweep.sh output that \
                     NETWORK.md records as not in results/",
                ),
                _ => Standing::Owed("a committed session of this shape; unit tests exercise it"),
            }
        };
        (condition, standing, confirmed, seen)
    })
    .collect()
}

fn session_line(session: &Session, condition: NetworkCondition) -> String {
    format!(
        "{:<18} {:<47} {:>6.1} {:>7.1} {:>7.1} {:>8.0} {:>6} {:>7} {:>7}  {}{}",
        condition.name(),
        session.name,
        session.span_s(),
        session.crossings_per_min(),
        session.clusters_per_min(),
        session.stall_gap_p50_ms(),
        match session.observation.stream.loss_ratio {
            Some(ratio) => format!("{:.3}%", 100.0 * ratio.value()),
            // Not zero. The tier is not there.
            None => String::from("absent"),
        },
        session.access_units_short,
        session.observation.stream.reorder.events(),
        radio_column(session.observation.radio),
        // The one committed run whose nominal and measured spans disagree by more
        // than a rounding error, annotated so its access-unit shortfall is legible
        // as a truncation rather than as a fault.
        if session.shortfall_s.abs() > 1.0 {
            format!(" span {:.2} s short of nominal", session.shortfall_s)
        } else {
            String::new()
        }
    )
}

fn refused_line(unreadable: &Unreadable) -> String {
    format!(
        "{:<18} {:<47} {:>6.1} {:>7} {:>7} {:>8} {:>6} {:>7} {:>7}  {}\n{:20}{} - {}",
        "REFUSED",
        unreadable.name,
        unreadable.span_s,
        "-",
        "-",
        "-",
        "-",
        "-",
        "-",
        radio_column(unreadable.radio),
        "",
        unreadable.missing,
        unreadable.evidence
    )
}

fn radio_column(radio: Option<RadioHint>) -> String {
    match radio {
        Some(hint) => format!(
            "{:>4} dBm {:>6.0} Mbps ch{}/{}",
            hint.rssi_dbm, hint.tx_rate_mbps, hint.channel, hint.width_mhz
        ),
        None => String::from("absent"),
    }
}

fn read_table(path: &Path) -> Result<BTreeMap<String, Row>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
    let mut rows = BTreeMap::new();
    for (number, line) in text.lines().enumerate() {
        let number = number + 1;
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let name = fields.next().unwrap_or("").trim();
        let verdict = fields.next().unwrap_or("").trim();
        let note = fields.next().unwrap_or("").trim();
        if name.is_empty() || verdict.is_empty() {
            return Err(format!(
                "{}:{number} is neither blank, a comment, nor a session and a verdict separated \
                 by a tab",
                path.display()
            ));
        }
        if note.is_empty() {
            return Err(format!(
                "{}:{number} states {verdict} for {name} with nothing after it; a verdict with \
                 no citation cannot be checked against the record it claims to come from",
                path.display()
            ));
        }
        let expectation = match verdict {
            "UNESTABLISHED" => Expectation::Unestablished,
            "REFUSED" => Expectation::Refused,
            word => match NetworkCondition::parse(word) {
                Some(condition) => Expectation::Is(condition),
                None => {
                    return Err(format!(
                        "{}:{number} states {word}, which is not a condition in the taxonomy, \
                         nor REFUSED, nor UNESTABLISHED",
                        path.display()
                    ));
                }
            },
        };
        if rows
            .insert(
                name.to_string(),
                Row {
                    expectation,
                    note: note.to_string(),
                },
            )
            .is_some()
        {
            return Err(format!(
                "{}:{number} states {name} a second time, and two expectations for one session \
                 mean one of them is never read",
                path.display()
            ));
        }
    }
    Ok(rows)
}
