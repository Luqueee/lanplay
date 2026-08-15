//! Which members are not portable, according to the members themselves, and
//! whether the Windows job's exclude list says the same.
//!
//! The list in `.github/workflows/ci.yml` used to claim that naming the
//! macOS-only crates by hand made a missing one "fail here loudly". It did
//! fail, but as `cannot find wifi in lanplay_capabilities` inside
//! `tools/radio-sample`, which reads as a broken crate rather than as a crate
//! that belongs on the other side of a platform boundary, and it cost a wrong
//! diagnosis. This is the mechanism that comment described and did not have.
//!
//! A heuristic was tried first and rejected. Grepping manifests for
//! `target_os = "macos"` or for an objc2 dependency also matches
//! `crates/telemetry`, which is genuinely cross-platform: it has a Windows
//! module and refuses its scheduling request by naming the platform rather
//! than by failing to compile. What a crate supports is a decision its author
//! makes, so the crate declares it in its own manifest under
//! `[package.metadata.lanplay] platforms`, which cargo carries into
//! `cargo metadata` untouched. Saying nothing means portable, because that is
//! the common case and the cost of a silent default here is a crate excluded
//! from Windows for no stated reason, which is the opposite failure and a
//! louder one.
//!
//! Both directions are checked. A declared macOS-only crate missing from the
//! list is the defect above. A listed name that is not such a crate is how the
//! list rots: rename or delete a crate and its stale `--exclude` keeps
//! excluding nothing at all, which cargo accepts in silence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{fs, str};

use serde::Deserialize;

use crate::Abort;

/// A platform a crate can claim. Closed rather than open on purpose: an
/// unrecognised word is far more likely to be a typo than a port, and a typo
/// that parses would quietly change which crates this check demands be
/// excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Platform {
    MacOs,
    Windows,
}

impl Platform {
    fn parse(word: &str) -> Option<Platform> {
        match word {
            "macos" => Some(Platform::MacOs),
            "windows" => Some(Platform::Windows),
            _ => None,
        }
    }
}

/// One workspace member and what it says about where it builds.
///
/// `None` is the absence of a declaration, not an empty set, and the two must
/// not collapse into each other: absent means portable, whereas an empty list
/// would mean a crate that builds nowhere, which is refused when read.
pub struct Member {
    pub name: String,
    pub platforms: Option<BTreeSet<Platform>>,
}

impl Member {
    /// Whether the Windows job should be able to build this crate. A member
    /// that declares nothing is expected to, which is what makes the second
    /// direction of the check bite.
    pub fn supports_windows(&self) -> bool {
        match &self.platforms {
            None => true,
            Some(platforms) => platforms.contains(&Platform::Windows),
        }
    }
}

/// `cargo metadata` as far as this needs it. `--no-deps` is what makes
/// `packages` exactly the workspace members, so nothing here filters by id.
#[derive(Deserialize)]
struct Document {
    packages: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    /// Named in every complaint, because a crate is easier to find by path
    /// than by package name when the two differ, and here they usually do.
    manifest_path: String,
    /// Null for the overwhelming majority of members, and null is the answer
    /// that means portable.
    metadata: Option<PackageMetadata>,
}

#[derive(Deserialize)]
struct PackageMetadata {
    lanplay: Option<Declaration>,
}

/// Unknown keys are refused rather than ignored, on the same grounds the gate
/// index refuses them: a misspelt `platform` that parsed would leave the crate
/// looking portable while its author believed it had said otherwise.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    platforms: Vec<String>,
}

/// Runs `cargo metadata` and returns what it printed. Separate from parsing so
/// that every test works from a fixture and none of them shells out.
pub fn metadata(root: &Path) -> Result<String, Abort> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(&cargo)
        .current_dir(root)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .map_err(|err| Abort::new(format!("could not run {cargo} metadata: {err}")))?;
    if !output.status.success() {
        let complaint = String::from_utf8_lossy(&output.stderr);
        return Err(Abort::new(format!(
            "cargo metadata failed: {}",
            complaint.trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|err| Abort::new(format!("cargo metadata did not print UTF-8: {err}")))
}

/// The members, in the order cargo listed them, with their declarations
/// validated.
pub fn members(json: &str) -> Result<Vec<Member>, Abort> {
    let document: Document =
        serde_json::from_str(json).map_err(|err| Abort::new(format!("cargo metadata: {err}")))?;
    let mut members = Vec::with_capacity(document.packages.len());
    for package in document.packages {
        let declaration = package.metadata.and_then(|metadata| metadata.lanplay);
        let platforms = match declaration {
            None => None,
            Some(declaration) => {
                let mut platforms = BTreeSet::new();
                for word in &declaration.platforms {
                    let platform = Platform::parse(word).ok_or_else(|| {
                        Abort::new(format!(
                            "{}: platforms lists {word:?}, which is not a platform this \
                             repository has a half for; expected any of \"macos\", \"windows\"",
                            package.manifest_path
                        ))
                    })?;
                    platforms.insert(platform);
                }
                if platforms.is_empty() {
                    return Err(Abort::new(format!(
                        "{}: platforms is empty, which claims the crate builds nowhere; \
                         leave the table out to say it is portable",
                        package.manifest_path
                    )));
                }
                Some(platforms)
            }
        };
        members.push(Member {
            name: package.name,
            platforms,
        });
    }
    Ok(members)
}

/// Where the workflow lives, resolved against this crate rather than against
/// the current directory, so that the answer does not depend on where in the
/// tree the command was run.
pub fn default_workflow() -> PathBuf {
    repo_root().join(".github/workflows/ci.yml")
}

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the xtask manifest lives one level under the workspace root")
        .to_path_buf()
}

/// The exclude names the Windows job passes, one entry per step that passes
/// any.
///
/// Read as text. There is no YAML parser in this tree and one was not added
/// for this, so the shape expected is stated here and departures from it are
/// refused rather than skipped, because a reader that quietly matches nothing
/// turns this whole check into a check of nothing.
///
/// The shape: a job whose key is `windows:` at two spaces of indent, ended by
/// the next key at that indent; inside it, steps beginning `- ` at six spaces;
/// and within a step, every exclusion on a line of its own whose first token
/// is `--exclude`, followed by the crate name. Anything after the name -
/// a line continuation, a redirection, a `-- -D warnings` - is the shell's
/// business and ignored. A line carrying two exclusions, or a name that is not
/// a crate name, is refused: both are shapes this reader would half-read.
pub fn exclusions(text: &str) -> Result<Vec<(String, BTreeSet<String>)>, Abort> {
    let mut lines = text.lines().skip_while(|line| !is_windows_job(line));
    if lines.next().is_none() {
        return Err(Abort::new(
            "no line reading `  windows:` in the workflow, so the job whose exclude list \
             this checks could not be found; either the job was renamed or its indent \
             changed, and this reader has to be taught the new shape"
                .to_string(),
        ));
    }

    let mut steps: Vec<(String, BTreeSet<String>)> = Vec::new();
    for line in lines {
        if is_job_key(line) {
            break;
        }
        if let Some(name) = step_name(line) {
            steps.push((name, BTreeSet::new()));
        }
        let Some(excluded) = excluded_on(line)? else {
            continue;
        };
        let Some(step) = steps.last_mut() else {
            return Err(Abort::new(format!(
                "`--exclude {excluded}` appears in the windows job before any step, \
                 which is not a shape this reader can attribute"
            )));
        };
        if !step.1.insert(excluded.clone()) {
            return Err(Abort::new(format!(
                "the windows job's `{}` step excludes {excluded} twice",
                step.0
            )));
        }
    }

    let carrying: Vec<(String, BTreeSet<String>)> = steps
        .into_iter()
        .filter(|(_, excluded)| !excluded.is_empty())
        .collect();
    if carrying.is_empty() {
        return Err(Abort::new(
            "the windows job passes no `--exclude` at all, which cannot be right while \
             any member is macOS-only; a check reading zero exclusions is a check of \
             nothing, so this is refused rather than reported as agreement"
                .to_string(),
        ));
    }
    Ok(carrying)
}

/// The two steps of the Windows job build and lint the same set, and a crate
/// excluded from one but not the other is a job that fails in its second half
/// having passed its first. Collapsing them into one set would hide exactly
/// that, so they are compared before being collapsed.
pub fn agreed(steps: &[(String, BTreeSet<String>)]) -> Result<BTreeSet<String>, Abort> {
    let (first_name, first) = &steps[0];
    for (name, excluded) in &steps[1..] {
        if excluded != first {
            let mut differences = excluded
                .symmetric_difference(first)
                .cloned()
                .collect::<Vec<_>>();
            differences.sort();
            return Err(Abort::new(format!(
                "the windows job's `{first_name}` and `{name}` steps exclude different \
                 sets, differing over {}; one of them will build a crate the other \
                 refuses to",
                differences.join(", ")
            )));
        }
    }
    Ok(first.clone())
}

/// The two lists against each other. `Ok(false)` is a disagreement, which is a
/// result; an `Abort` is the absence of one.
pub fn check(members: &[Member], excluded: &BTreeSet<String>) -> Result<(String, bool), Abort> {
    let declared: BTreeMap<&str, &BTreeSet<Platform>> = members
        .iter()
        .filter(|member| !member.supports_windows())
        .filter_map(|member| {
            member
                .platforms
                .as_ref()
                .map(|platforms| (member.name.as_str(), platforms))
        })
        .collect();
    if declared.is_empty() {
        return Err(Abort::new(
            "no member declares a platform set that excludes windows, so there is nothing \
             for the exclude list to agree with; the manifests or this reader are wrong, \
             and passing over an empty population would say neither"
                .to_string(),
        ));
    }

    let known: BTreeSet<&str> = members.iter().map(|member| member.name.as_str()).collect();
    let mut complaints = Vec::new();

    for (name, platforms) in &declared {
        if !excluded.contains(*name) {
            let mut words: Vec<&str> = platforms
                .iter()
                .map(|platform| match platform {
                    Platform::MacOs => "macos",
                    Platform::Windows => "windows",
                })
                .collect();
            words.sort_unstable();
            complaints.push(format!(
                "{name} declares platforms = [{}] and so cannot build on windows, but the \
                 windows job does not exclude it; add `--exclude {name}` to every step of \
                 that job",
                words
                    .iter()
                    .map(|word| format!("\"{word}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    for name in excluded {
        if !known.contains(name.as_str()) {
            complaints.push(format!(
                "the windows job excludes {name}, which is not a member of this workspace; \
                 a stale exclusion excludes nothing and cargo does not say so"
            ));
        } else if !declared.contains_key(name.as_str()) {
            complaints.push(format!(
                "the windows job excludes {name}, which declares no platform set that \
                 leaves windows out; either it is portable and the exclusion is hiding a \
                 real failure, or its manifest owes a \
                 `[package.metadata.lanplay] platforms` saying why not"
            ));
        }
    }

    if complaints.is_empty() {
        let mut report = format!(
            "{} members declare a platform set without windows, and the windows job \
             excludes exactly those:\n",
            declared.len()
        );
        for name in declared.keys() {
            let _ = writeln!(report, "  {name}");
        }
        return Ok((report, true));
    }

    let mut report = String::new();
    for complaint in &complaints {
        let _ = writeln!(report, "{complaint}");
    }
    Ok((report, false))
}

/// Reads both lists and compares them, which is what the subcommand is.
pub fn audit(workflow: &Path) -> Result<(String, bool), Abort> {
    let text = fs::read_to_string(workflow)
        .map_err(|err| Abort::new(format!("could not read {}: {err}", workflow.display())))?;
    let steps = exclusions(&text)?;
    let excluded = agreed(&steps)?;
    let members = members(&metadata(&repo_root())?)?;
    check(&members, &excluded)
}

fn is_windows_job(line: &str) -> bool {
    is_job_key(line) && line.trim_end() == "  windows:"
}

/// A job key: two spaces of indent, then a name and a colon. Comments and list
/// items at the same indent are not keys, and anything deeper belongs to a job
/// rather than starting one.
fn is_job_key(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("  ") else {
        return false;
    };
    if rest.starts_with([' ', '#', '-']) {
        return false;
    }
    let trimmed = rest.trim_end();
    trimmed.ends_with(':') && !trimmed.is_empty()
}

/// A step: `- ` at six spaces of indent. Its name is whatever `name:` says, or
/// the key it leads with, which is enough to quote back in a complaint.
fn step_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("      - ")?;
    if rest.starts_with(' ') {
        return None;
    }
    let rest = rest.trim();
    Some(match rest.strip_prefix("name:") {
        Some(name) => name.trim().to_string(),
        None => rest.split(':').next().unwrap_or(rest).to_string(),
    })
}

/// The crate name one line excludes, if it excludes one.
fn excluded_on(line: &str) -> Result<Option<String>, Abort> {
    let trimmed = line.trim();
    if !trimmed.contains("--exclude") {
        return Ok(None);
    }
    if trimmed.matches("--exclude").count() > 1 {
        return Err(Abort::new(format!(
            "two exclusions on one line of the windows job: {trimmed:?}. This reader \
             expects one `--exclude <crate>` per line, so that a diff adding or removing \
             a crate is one line and this check reads all of them"
        )));
    }
    let Some(rest) = trimmed.strip_prefix("--exclude") else {
        return Err(Abort::new(format!(
            "`--exclude` is not the first token of {trimmed:?}. This reader expects each \
             exclusion on a line of its own beginning `--exclude <crate>`, and refuses \
             rather than reading the one exclusion it can see out of a line that may hold \
             more"
        )));
    };
    let name = rest.split_whitespace().next().ok_or_else(|| {
        Abort::new("`--exclude` with no crate name after it in the windows job".to_string())
    })?;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Abort::new(format!(
            "{name:?} follows `--exclude` in the windows job and is not a crate name"
        )));
    }
    Ok(Some(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture rather than the real workflow, because a test written against
    /// `.github/workflows/ci.yml` passes or fails on whatever that file says
    /// today, and the point here is the reader rather than the current list.
    const WORKFLOW: &str = "\
name: ci

jobs:
  macos:
    runs-on: macos-15
    steps:
      - run: cargo test --workspace --locked

  # A comment inside the job, at the indent of a key and not one.
  windows:
    runs-on: windows-latest
    steps:
      - name: test what Windows can build
        run: |
          cargo test --workspace --locked \\
            --exclude lanplay-client \\
            --exclude lanplay-mouse-mover
      - name: clippy the same set
        run: |
          cargo clippy --workspace --all-targets --locked \\
            --exclude lanplay-client \\
            --exclude lanplay-mouse-mover \\
            -- -D warnings

  msrv:
    runs-on: macos-15
    steps:
      - run: cargo check --workspace --all-targets --locked
";

    fn metadata_json(declarations: &[(&str, Option<&str>)]) -> String {
        let packages: Vec<String> = declarations
            .iter()
            .map(|(name, platforms)| {
                let metadata = match platforms {
                    None => "null".to_string(),
                    Some(platforms) => {
                        format!(r#"{{"lanplay":{{"platforms":[{platforms}]}}}}"#)
                    }
                };
                format!(
                    r#"{{"name":"{name}","manifest_path":"/w/{name}/Cargo.toml","metadata":{metadata}}}"#
                )
            })
            .collect();
        format!(r#"{{"packages":[{}]}}"#, packages.join(","))
    }

    fn agreeing() -> Vec<Member> {
        members(&metadata_json(&[
            ("lanplay-client", Some(r#""macos""#)),
            ("lanplay-mouse-mover", Some(r#""macos""#)),
            ("lanplay-capture", None),
            ("xtask", None),
        ]))
        .expect("the fixture metadata parses")
    }

    fn excluded_by(text: &str) -> BTreeSet<String> {
        agreed(&exclusions(text).expect("the fixture workflow is readable"))
            .expect("its steps agree")
    }

    #[test]
    fn a_crate_that_declares_nothing_is_portable() {
        let members = agreeing();
        let capture = members
            .iter()
            .find(|member| member.name == "lanplay-capture")
            .expect("the fixture has it");
        assert!(capture.platforms.is_none());
        assert!(capture.supports_windows());
    }

    #[test]
    fn a_crate_declaring_macos_alone_does_not_support_windows() {
        let members = agreeing();
        let client = members
            .iter()
            .find(|member| member.name == "lanplay-client")
            .expect("the fixture has it");
        assert!(!client.supports_windows());
    }

    #[test]
    fn a_misspelt_key_under_the_lanplay_table_is_refused() {
        let json = r#"{"packages":[{"name":"a","manifest_path":"/w/a/Cargo.toml",
            "metadata":{"lanplay":{"platform":["macos"]}}}]}"#;
        let Err(Abort(why)) = members(json) else {
            panic!("a misspelt key must not parse as an absent declaration");
        };
        assert!(why.contains("platform"), "{why}");
    }

    #[test]
    fn a_platform_word_this_repository_has_no_half_for_is_refused() {
        let json = metadata_json(&[("a", Some(r#""mac""#))]);
        let Err(Abort(why)) = members(&json) else {
            panic!("a typo must not decide which crates get excluded");
        };
        assert!(why.contains("\"mac\""), "{why}");
        assert!(why.contains("/w/a/Cargo.toml"), "{why}");
    }

    #[test]
    fn an_empty_platform_list_is_refused() {
        let json = metadata_json(&[("a", Some(""))]);
        let Err(Abort(why)) = members(&json) else {
            panic!("a crate that builds nowhere is not a declaration");
        };
        assert!(why.contains("builds nowhere"), "{why}");
    }

    #[test]
    fn the_two_lists_agreeing_passes_and_names_the_crates() {
        let (report, passed) = check(&agreeing(), &excluded_by(WORKFLOW)).expect("readable");
        assert!(passed, "{report}");
        assert!(report.contains("lanplay-client"), "{report}");
        assert!(report.contains("lanplay-mouse-mover"), "{report}");
    }

    #[test]
    fn a_macos_only_crate_missing_from_the_exclude_list_is_named() {
        let members = members(&metadata_json(&[
            ("lanplay-client", Some(r#""macos""#)),
            ("lanplay-mouse-mover", Some(r#""macos""#)),
            // The crate whose absence arrived as a compile error about `wifi`.
            ("lanplay-radio-sample", Some(r#""macos""#)),
            ("lanplay-capture", None),
        ]))
        .expect("the fixture metadata parses");
        let (report, passed) = check(&members, &excluded_by(WORKFLOW)).expect("readable");
        assert!(!passed, "{report}");
        assert!(report.contains("lanplay-radio-sample"), "{report}");
        assert!(
            report.contains("--exclude lanplay-radio-sample"),
            "{report}"
        );
        assert!(!report.contains("lanplay-client"), "{report}");
    }

    #[test]
    fn an_excluded_name_that_is_not_a_member_is_named() {
        let members = members(&metadata_json(&[
            ("lanplay-client", Some(r#""macos""#)),
            ("lanplay-capture", None),
        ]))
        .expect("the fixture metadata parses");
        let (report, passed) = check(&members, &excluded_by(WORKFLOW)).expect("readable");
        assert!(!passed, "{report}");
        assert!(report.contains("lanplay-mouse-mover"), "{report}");
        assert!(report.contains("not a member"), "{report}");
    }

    #[test]
    fn an_excluded_member_that_declares_no_platforms_is_named() {
        let members = members(&metadata_json(&[
            ("lanplay-client", Some(r#""macos""#)),
            ("lanplay-mouse-mover", None),
        ]))
        .expect("the fixture metadata parses");
        let (report, passed) = check(&members, &excluded_by(WORKFLOW)).expect("readable");
        assert!(!passed, "{report}");
        assert!(report.contains("lanplay-mouse-mover"), "{report}");
        assert!(report.contains("platforms"), "{report}");
    }

    #[test]
    fn a_workspace_where_nothing_is_macos_only_is_refused_rather_than_passed() {
        let members = members(&metadata_json(&[("lanplay-capture", None)]))
            .expect("the fixture metadata parses");
        let Err(Abort(why)) = check(&members, &excluded_by(WORKFLOW)) else {
            panic!("agreement over an empty population is not agreement");
        };
        assert!(why.contains("empty population"), "{why}");
    }

    #[test]
    fn the_reader_finds_the_windows_job_and_ignores_its_neighbours() {
        let steps = exclusions(WORKFLOW).expect("the fixture is readable");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].0, "test what Windows can build");
        assert_eq!(steps[1].0, "clippy the same set");
        assert_eq!(
            steps[0].1,
            BTreeSet::from([
                "lanplay-client".to_string(),
                "lanplay-mouse-mover".to_string()
            ])
        );
    }

    #[test]
    fn a_workflow_with_no_windows_job_is_refused() {
        let text = WORKFLOW.replace("  windows:", "  win32:");
        let Err(Abort(why)) = exclusions(&text) else {
            panic!("a job this reader cannot find must not read as no exclusions");
        };
        assert!(why.contains("windows:"), "{why}");
    }

    #[test]
    fn a_windows_job_that_excludes_nothing_is_refused() {
        let text = WORKFLOW
            .lines()
            .filter(|line| !line.trim().starts_with("--exclude"))
            .collect::<Vec<_>>()
            .join("\n");
        let Err(Abort(why)) = exclusions(&text) else {
            panic!("zero exclusions is a check of nothing");
        };
        assert!(why.contains("no `--exclude` at all"), "{why}");
    }

    #[test]
    fn two_exclusions_on_one_line_are_refused() {
        let text = WORKFLOW.replace(
            "--exclude lanplay-client \\",
            "--exclude lanplay-client --exclude lanplay-mouse-mover \\",
        );
        let Err(Abort(why)) = exclusions(&text) else {
            panic!("a line this reader half-reads must be refused");
        };
        assert!(why.contains("one `--exclude <crate>` per line"), "{why}");
    }

    #[test]
    fn an_exclusion_that_does_not_begin_its_line_is_refused() {
        let text = WORKFLOW.replace(
            "cargo test --workspace --locked \\",
            "cargo test --workspace --locked --exclude lanplay-client \\",
        );
        let Err(Abort(why)) = exclusions(&text) else {
            panic!("an exclusion sharing a line with the command must be refused");
        };
        assert!(why.contains("first token"), "{why}");
    }

    #[test]
    fn steps_excluding_different_sets_are_refused() {
        let text = WORKFLOW.replace("            --exclude lanplay-mouse-mover \\\n", "");
        let steps = exclusions(&text).expect("the reader still reads it");
        let Err(Abort(why)) = agreed(&steps) else {
            panic!("a crate built by one step and refused by the other is not agreement");
        };
        assert!(why.contains("lanplay-mouse-mover"), "{why}");
        assert!(why.contains("different"), "{why}");
    }
}
