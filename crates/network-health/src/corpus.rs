//! Reading a committed session into the three tiers, or refusing to.
//!
//! `results/` was written by harnesses that had never heard of this contract,
//! which is what makes it worth classifying: the shape it is already in decides
//! nothing here, and a mapping that had to invent a quantity would be inventing
//! the answer.
//!
//! Three outcomes, and the middle one is the one this module exists for.
//!
//! A **session** is a file every tier could be filled from. A
//! [`Reading::Unreadable`] is a file that was read, is a session envelope, and
//! carries no delivery tier - so no [`StreamBehaviour`] can be built and nothing
//! may be concluded about the link. A [`Refusal`] is a file that cannot be
//! trusted at all: unparseable, or stating a population of zero so that every
//! number beside it is an absence. The first two are reported per session and the
//! run continues; the third makes the population the run would decide over
//! unknown, so it stops the run.
//!
//! The distinction between the last two is not pedantry. An arm with no delivery
//! tier is a permanent property of an instrument that has already been written
//! and will not change; a file stating that it fed nothing is a corpus that
//! cannot be relied on. `TASKS.md` section 0.2 keeps REFUSED apart from a finding
//! for the same reason, and an earlier draft of this module got it wrong in the
//! costliest direction: it filled the delivery tier with a "nobody counted this"
//! variant, which came out of the classifier as `UnknownDegradation`, so
//! `results/audio/e2e-clean/clean-600s` - 0 lost of 120005, no render underrun in
//! 112493 callbacks, the cleanest arm this project has recorded - was printed as
//! a degradation of unknown type. A reader skimming that learned something false.
//!
//! The video shape is `results/b3-channel/*.json`: `run`, `stream`, `network`,
//! `delivery`, `display`, `decode`, `environment`. `delivery` is
//! `crates/link-metrics`' own window written out field for field, so it maps onto
//! [`Window`] with nothing computed on the way. `display` is read into
//! [`Experience`] and is structurally unable to reach [`StreamBehaviour`]: the
//! two are built by different functions and the one that builds the middle tier
//! is never handed the section.
//!
//! The audio shape is `<arm>.receiver.json` from `macos/audio-render`. Its socket
//! counters are real and are reported, but `crates/link-metrics` is video-side
//! and no audio envelope carries a delivery tier of any kind, so every one of
//! them is unreadable here. That is a fact about the instrument and not about the
//! links those arms ran on.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lanplay_link_metrics::{THRESHOLDS, Tail, Window};
use serde_json::Value;

use crate::{Experience, Fraction, Incidence, NetworkObservation, RadioHint, StreamBehaviour};

/// The audio directories `NETWORK.md` names as the corpus whose answer is
/// already written down.
///
/// A list rather than every `*.receiver.json` under `results/audio`, because the
/// other five directories there are runs those three superseded - `e9f68ed`
/// retired the `jitter-target-sweep` design and `495adb1` labelled the earlier
/// `e2e` arms contaminated - and a session with no standing diagnosis is a row
/// that can only be guessed at.
pub const AUDIO_DIRECTORIES: [&str; 3] = [
    "audio/jitter-target-a8",
    "audio/e2e-clean",
    "audio/e2e-corrected",
];

/// The sections a video-shaped session must carry to be one.
const VIDEO_SECTIONS: [&str; 6] = [
    "network",
    "delivery",
    "stream",
    "display",
    "decode",
    "environment",
];

/// A file that cannot be trusted, which stops the run.
///
/// Nothing was measured, so nothing was decided, and reporting it as a pass would
/// be certifying an absence.
#[derive(Clone, Debug)]
pub struct Refusal {
    pub path: PathBuf,
    pub why: String,
}

impl Refusal {
    fn new(path: &Path, why: impl Into<String>) -> Self {
        Refusal {
            path: path.to_path_buf(),
            why: why.into(),
        }
    }
}

/// A session envelope with no delivery tier, so no middle tier and no verdict.
///
/// Carries the counters the envelope does state. A refusal that hides its
/// evidence is harder to act on than one that shows it, and the numbers here are
/// observations rather than a verdict - `t20-p3`'s 382 lost datagrams of 23997
/// are worth printing beside the reason they cannot be classified.
#[derive(Clone, Debug)]
pub struct Unreadable {
    pub name: String,
    /// The tier that is missing, and why it is missing rather than empty.
    pub missing: String,
    /// What the envelope did state, for a reader who wants to know how much was
    /// thrown away.
    pub evidence: String,
    pub span_s: f64,
    /// Present even here, because the radio tier is never what decides and an
    /// unreadable session can still say what the air was doing.
    pub radio: Option<RadioHint>,
}

/// Whether the numbers came from the video pipeline or from the audio one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Instrument {
    /// `crates/link-metrics` at the depacketiser, counting crossings.
    Delivery,
    /// `macos/audio-render`'s receive envelope, counting datagrams and playout.
    Playout,
}

/// One committed session, read into all three tiers.
#[derive(Clone, Debug)]
pub struct Session {
    /// Relative to the corpus root, so a row is quotable.
    pub name: String,
    pub instrument: Instrument,
    pub observation: NetworkObservation,
    /// Nominal run length less the span the delivery tier covered. Reported, read
    /// by nothing: it is the evidence that an access-unit shortfall was a
    /// truncated capture rather than anything the link did.
    pub shortfall_s: f64,
    /// Datagrams the depacketiser saw a sequence gap for, with no population
    /// anywhere in the envelope to divide by. Carried so a reader can see that a
    /// large access-unit shortfall sits beside zero datagram loss.
    pub datagrams_lost: u64,
    /// `stream.expected` less `stream.reconstructed`, in access units, over a
    /// nominal population that link loss, run truncation and host under-production
    /// all feed. Reported, read by no rule, and the general finding rather than
    /// three special cases is why.
    pub access_units_short: u64,
}

impl Session {
    pub fn span_s(&self) -> f64 {
        self.observation.stream.delivery.span_s
    }

    /// Crossings of two source periods a minute.
    pub fn crossings_per_min(&self) -> f64 {
        let window = &self.observation.stream.delivery;
        window.tail.per_minute(2, window.span_s)
    }

    pub fn clusters_per_min(&self) -> f64 {
        let window = &self.observation.stream.delivery;
        window.tail.clusters_per_minute(window.span_s)
    }

    pub fn stall_gap_p50_ms(&self) -> f64 {
        self.observation.stream.delivery.tail.stall_gap_p50_ms
    }
}

/// What reading one file produced.
///
/// `Session` is boxed because it carries a whole [`Window`] and `Unreadable`
/// carries three strings, so the two differ by enough that every element of the
/// walk's vector would otherwise be sized for the larger.
#[derive(Clone, Debug)]
pub enum Reading {
    Classifiable(Box<Session>),
    Unreadable(Unreadable),
}

impl Reading {
    pub fn name(&self) -> &str {
        match self {
            Reading::Classifiable(session) => &session.name,
            Reading::Unreadable(unreadable) => &unreadable.name,
        }
    }

    pub fn radio(&self) -> Option<RadioHint> {
        match self {
            Reading::Classifiable(session) => session.observation.radio,
            Reading::Unreadable(unreadable) => unreadable.radio,
        }
    }
}

/// Which files under the root count as the corpus.
///
/// The validation population has to be the committed one. A peer dropping a
/// scratch directory under `results/` silently widened this harness's population
/// once already - seventeen uncommitted sessions from another agent's run - and a
/// validation whose population drifts with whatever is on disk is not a
/// validation of anything stated. `NETWORK.md` asks for runs whose diagnosis is
/// already written down, and a file nobody has committed has nothing written down
/// about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tracking {
    /// Only what `git ls-files` reports.
    Committed,
    /// Every file on disk, for a corpus assembled by hand - which is what the
    /// negative controls do, and they say so rather than being quietly allowed.
    AsFound,
}

/// Every session under `root`, in a stable order, and every file that could not
/// be trusted.
///
/// Both halves are returned. A walk that quietly skipped what it could not parse
/// would make an unreliable corpus indistinguishable from a small one.
pub fn walk(root: &Path, tracking: Tracking) -> Result<(Vec<Reading>, Vec<Refusal>), Refusal> {
    let mut candidates: BTreeMap<String, PathBuf> = BTreeMap::new();
    match tracking {
        Tracking::Committed => committed(root, &mut candidates)?,
        Tracking::AsFound => collect(root, root, &mut candidates)?,
    }

    let mut readings = Vec::new();
    let mut refusals = Vec::new();
    for (name, path) in candidates {
        match read(&path, &name) {
            Ok(Some(reading)) => readings.push(reading),
            Ok(None) => {}
            Err(refusal) => refusals.push(refusal),
        }
    }
    Ok((readings, refusals))
}

/// Asks git which files under `root` are committed.
///
/// Refuses rather than falling back to the filesystem. A fallback would turn "git
/// is not here" into a silently larger population, which is the failure this
/// exists to prevent.
fn committed(root: &Path, into: &mut BTreeMap<String, PathBuf>) -> Result<(), Refusal> {
    let listing = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--full-name", "-z"])
        .output()
        .map_err(|error| {
            Refusal::new(
                root,
                format!(
                    "could not be listed by git, so which of its files are committed is \
                         unknown: {error}"
                ),
            )
        })?;
    if !listing.status.success() {
        return Err(Refusal::new(
            root,
            format!(
                "git ls-files refused it: {}",
                String::from_utf8_lossy(&listing.stderr).trim()
            ),
        ));
    }
    let text = String::from_utf8_lossy(&listing.stdout);
    // `--full-name` is relative to the repository root and `-C root` only sets
    // the working directory, so the names come back with the root's own prefix on
    // them. Stripped here so a row reads the same either way.
    let prefix = root.canonicalize().ok().and_then(|absolute| {
        absolute
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    });
    for entry in text.split('\0').filter(|entry| !entry.is_empty()) {
        if !entry.ends_with(".json") {
            continue;
        }
        let name = match &prefix {
            Some(prefix) => entry
                .strip_prefix(&format!("{prefix}/"))
                .unwrap_or(entry)
                .to_string(),
            None => entry.to_string(),
        };
        into.insert(name.clone(), root.join(&name));
    }
    if into.is_empty() {
        return Err(Refusal::new(
            root,
            "holds no committed .json at all, and a population of zero agrees with everything",
        ));
    }
    Ok(())
}

fn collect(
    root: &Path,
    directory: &Path,
    into: &mut BTreeMap<String, PathBuf>,
) -> Result<(), Refusal> {
    let entries = fs::read_dir(directory)
        .map_err(|error| Refusal::new(directory, format!("could not be listed: {error}")))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| Refusal::new(directory, format!("could not be read: {error}")))?;
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, into)?;
            continue;
        }
        let Some(name) = path.strip_prefix(root).ok().and_then(Path::to_str) else {
            continue;
        };
        if name.ends_with(".json") {
            into.insert(name.to_string(), path);
        }
    }
    Ok(())
}

/// `Ok(None)` for a file that is neither shape, which is most of `results/`.
pub fn read(path: &Path, name: &str) -> Result<Option<Reading>, Refusal> {
    let text = fs::read_to_string(path)
        .map_err(|error| Refusal::new(path, format!("could not be read: {error}")))?;
    let document: Value = match serde_json::from_str(&text) {
        Ok(document) => document,
        // Not every `.json` under `results/` is a session envelope, but one that
        // parses as nothing at all is a corrupt file rather than a different
        // shape, and the difference is worth reporting.
        Err(error) => return Err(Refusal::new(path, format!("is not JSON: {error}"))),
    };

    if VIDEO_SECTIONS
        .iter()
        .all(|section| document.get(section).is_some())
    {
        return read_video(path, name, &document).map(Some);
    }

    let audio = name.ends_with(".receiver.json")
        && AUDIO_DIRECTORIES
            .iter()
            .any(|directory| name.starts_with(directory));
    if audio {
        return read_audio(path, name, &document).map(Some);
    }

    Ok(None)
}

fn number(path: &Path, section: &Value, where_: &str, key: &str) -> Result<f64, Refusal> {
    section
        .get(key)
        .and_then(Value::as_f64)
        .ok_or_else(|| Refusal::new(path, format!("{where_} states no {key}")))
}

fn count(path: &Path, section: &Value, where_: &str, key: &str) -> Result<u64, Refusal> {
    let value = number(path, section, where_, key)?;
    if value < 0.0 {
        return Err(Refusal::new(
            path,
            format!("{where_} states {key} as {value}, which is not a count"),
        ));
    }
    Ok(value as u64)
}

fn read_video(path: &Path, name: &str, document: &Value) -> Result<Reading, Refusal> {
    let delivery = &document["delivery"];
    let stream = &document["stream"];

    // Access units, one per frame the host was asked to feed: `target_fps` times
    // `run.seconds`, computed by the client rather than reported by the sender.
    let expected = count(path, stream, "stream", "expected")?;
    if expected == 0 {
        return Err(Refusal::new(
            path,
            "states stream.expected as 0, so every zero beside it is an absence and this run \
             measured nothing",
        ));
    }

    let radio = radio_beside(path, "wifi.csv");
    let run_seconds = number(path, &document["run"], "run", "seconds")?;

    // Nothing in the committed envelopes states what the sender produced or sent,
    // and that single absence is why no loss rate is derivable from any of them.
    //
    // `stream.packet_loss`, `stream.reordered` and `stream.duplicates` are counted
    // in datagrams and no datagram population is stated anywhere -
    // `macos/client/src/session.rs` computes `rx.lost + rx.received` and
    // `rx.received` and prints them without writing them into the section. At
    // 40 Mbps and 120 fps an access unit is some forty-five datagrams, so dividing
    // `reordered` by `expected` read 30.8 per cent where the datagram figure is
    // nearer 0.69.
    //
    // `stream.au_loss` is `expected - reconstructed`, and `expected` is
    // `target_fps` times the nominal run length, so it is not a count of anything
    // produced, sent or observed. Three unrelated mechanisms feed it and all three
    // are measured in this repository: link loss, which would show in
    // `packet_loss`; run truncation, which is `pcap-parallel/parallel-r2`, 2528
    // units against a span 21.00 s short of its nominal 120 where 2528 units at
    // 120 fps is 21.07 s; and host under-production, refused below. A quotient
    // over a nominal population cannot separate those, so it is not formed.
    //
    // Both are carried as counts and no rule reads either.
    let reorder = Incidence::Bare(count(path, stream, "stream", "reordered")?);
    let access_units_short = count(path, stream, "stream", "au_loss")?;
    let datagrams_lost = count(path, stream, "stream", "packet_loss")?;

    // `None`, for every session in the committed corpus.
    // `macos/client/src/report.rs` now carries `loss_events` beside
    // `loss_population` - datagrams over datagrams - so a session recorded from the
    // passive monitor onward states one and this becomes `Some`. Read here rather
    // than assumed absent, so that the day one lands nothing has to change.
    //
    // That the defect is in the envelopes rather than in one reader's arithmetic is
    // worth stating: three agents reached it independently this session, one from
    // the field units, one from a reorder figure that could not be right, and one
    // from building the tier.
    let loss_ratio = match (
        stream.get("loss_events").and_then(Value::as_u64),
        stream.get("loss_population").and_then(Value::as_u64),
    ) {
        (Some(events), Some(population)) => {
            Some(Fraction::new(events, population).ok_or_else(|| {
                Refusal::new(
                    path,
                    "states loss_population as 0, so its loss ratio would be a zero over an \
                     absence",
                )
            })?)
        }
        _ => None,
    };

    let delivered = count(path, delivery, "delivery", "delivered")?;
    if delivered == 0 {
        return Err(Refusal::new(
            path,
            "delivery states delivered as 0, so no access unit ever completed and there is no \
             cadence in this file to read",
        ));
    }

    // The tail counters and the span arrived with `crates/link-metrics`, and the
    // arms in `results/b1-proximity` and `results/b5-datagram-size` predate them.
    // Percentiles alone cannot answer any rule here - `NETWORK.md` says why, a
    // p99 below a threshold says nothing about how many crossed it - so those arms
    // are unreadable rather than zero-filled, which would have read as a run that
    // crossed no threshold at all.
    if delivery.get("span_s").is_none() || delivery.get("over_2t_per_min").is_none() {
        return Ok(Reading::Unreadable(Unreadable {
            name: name.to_string(),
            missing: String::from(
                "delivery states percentiles and no counted crossing, no cluster count and no \
                 stall gap; it predates crates/link-metrics",
            ),
            evidence: format!(
                "au p99 {:.2} ms, au max {:.2} ms, {delivered} of {expected} delivered, \
                 {datagrams_lost} datagrams lost, {} reordered",
                number(path, delivery, "delivery", "au_interval_p99_ms")?,
                number(path, delivery, "delivery", "au_interval_max_ms")?,
                reorder.events(),
            ),
            span_s: run_seconds,
            radio,
        }));
    }

    let span_s = number(path, delivery, "delivery", "span_s")?;
    if span_s <= 0.0 {
        return Err(Refusal::new(
            path,
            format!(
                "delivery states span_s as {span_s}, and every rate in this file is a count \
                 divided by it"
            ),
        ));
    }

    // How much of the nominal run the delivery tier did not cover. Detected and
    // reported rather than acted on, because no rule reads a loss count and the
    // cadence rates are computed over `span_s`, which is honest whatever the
    // nominal says. `results/pcap-parallel/parallel-r2` is the one committed run
    // where this is large - 21.00 s short of a nominal 120, against at most 0.03 s
    // for every other - and it is exactly the evidence a reader needs to see why
    // its 2528 "lost" access units are 21.07 s of units that were never asked for
    // rather than a link fault. Refusing the session on it was tried and dropped:
    // its cadence is perfectly readable, 119.4 crossings a minute with stalls
    // 222 ms apart, and that is the diagnosis `e604ce1` recorded for it.
    let shortfall_s = run_seconds - span_s;

    // Whether the host held the cadence it was asked for. If it did not, the
    // delivery tier's threshold crossings are counted against a source period the
    // host was not hitting, so what they measure is the producer and not the link -
    // and the access-unit shortfall beside them is that under-production rather
    // than anything lost in the air.
    //
    // One per cent of `target_fps`. Twenty-one committed runs sit within 0.22 per
    // cent of it, the worst being `results/phase/lottery/1` at 119.73 of 120 a
    // second; the three that fall outside are `results/phase/sign-observe` at
    // 117.96, `results/phase/acting` at 109.94 and `results/phase/control` at
    // 105.32, which are 1.70, 8.38 and 12.23 per cent short. Disjoint by a factor
    // of 7.7, and 1 per cent of 120 fps is 1.2 frames a second, which is a figure
    // with a meaning rather than a fitted one.
    //
    // The three refused are the arms where `d35ed85` deliberately applied a 3.00 ms
    // draw to the producer, so the under-production there is the experiment. That
    // it also accounts for the whole of their access-unit shortfall - and that they
    // were the three sessions an earlier draft labelled SevereLoss - is the
    // evidence for refusing rather than classifying them.
    let target_fps = number(path, &document["run"], "run", "target_fps")?;
    let produced_per_s = delivered as f64 / span_s;
    if target_fps > 0.0 && produced_per_s < target_fps * 0.99 {
        return Ok(Reading::Unreadable(Unreadable {
            name: name.to_string(),
            missing: format!(
                "a source cadence to measure against: the host delivered {produced_per_s:.2} \
                 access units a second against a target of {target_fps:.0}, {:.2} per cent short, \
                 so the crossings below are counted against a period it was not holding",
                100.0 * (target_fps - produced_per_s) / target_fps
            ),
            evidence: format!(
                "{delivered} of {expected} delivered over {span_s:.2} s, au shortfall \
                 {access_units_short}, {datagrams_lost} datagrams lost, {} reordered",
                reorder.events(),
            ),
            span_s,
            radio,
        }));
    }

    // Rates back to counts, because `Tail` holds counts and `per_minute` divides
    // by the span the caller passes. Round-tripping through the rate the file
    // states keeps one definition of the division rather than two.
    let mut over = [0u64; THRESHOLDS.len()];
    for (index, threshold) in THRESHOLDS.iter().enumerate() {
        let key = rate_key(*threshold);
        let per_min = number(path, delivery, "delivery", &key)?;
        over[index] = (per_min * span_s / 60.0).round() as u64;
    }
    let clusters_per_min = number(path, delivery, "delivery", "stall_clusters_per_min")?;

    Ok(Reading::Classifiable(Box::new(Session {
        name: name.to_string(),
        instrument: Instrument::Delivery,
        observation: NetworkObservation {
            radio,
            stream: StreamBehaviour {
                delivery: Window {
                    delivered,
                    p50_ms: number(path, delivery, "delivery", "au_interval_p50_ms")?,
                    p95_ms: number(path, delivery, "delivery", "au_interval_p95_ms")?,
                    p99_ms: number(path, delivery, "delivery", "au_interval_p99_ms")?,
                    max_ms: number(path, delivery, "delivery", "au_interval_max_ms")?,
                    first_p50_ms: number(path, delivery, "delivery", "first_interval_p50_ms")?,
                    first_p95_ms: number(path, delivery, "delivery", "first_interval_p95_ms")?,
                    first_p99_ms: number(path, delivery, "delivery", "first_interval_p99_ms")?,
                    first_max_ms: number(path, delivery, "delivery", "first_interval_max_ms")?,
                    span_s,
                    tail: Tail {
                        over,
                        clusters: (clusters_per_min * span_s / 60.0).round() as u64,
                        catch_up_total: 0,
                        catch_up_max: count(path, delivery, "delivery", "max_catch_up_units")?,
                        stall_gap_p50_ms: number(path, delivery, "delivery", "stall_gap_p50_ms")?,
                        stall_gap_p95_ms: number(path, delivery, "delivery", "stall_gap_p95_ms")?,
                    },
                },
                loss_ratio,
                reorder,
            },
            experience: video_experience(document),
        },
        shortfall_s,
        datagrams_lost,
        access_units_short,
    })))
}

fn rate_key(threshold: f64) -> String {
    // `over_1_25t_per_min` and its five siblings: the multiple with its decimal
    // point spelled as an underscore, which is what the harnesses wrote.
    let spelled = format!("{threshold}").replace('.', "_");
    format!("over_{spelled}t_per_min")
}

/// Built from `display` alone, by a function the middle tier's builder does not
/// call and cannot reach.
fn video_experience(document: &Value) -> Experience {
    let display = &document["display"];
    let callbacks = display
        .get("callbacks")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Experience {
        // Stated as a percentage in the corpus and as a fraction in the
        // contract, and zero callbacks means there was no display rather than a
        // display that presented nothing.
        fresh_tick_ratio: (callbacks > 0)
            .then(|| display.get("fresh_tick_ratio").and_then(Value::as_f64))
            .flatten()
            .map(|percent| percent / 100.0),
        frame_age_p99_ms: (callbacks > 0)
            .then(|| display.get("frame_age_p99_ms").and_then(Value::as_f64))
            .flatten(),
        concealed_ratio: None,
        silence_events: None,
    }
}

/// Always unreadable, and the socket counters are reported rather than discarded.
///
/// `crates/link-metrics` is video-side. Nothing in a receive envelope counts an
/// arrival crossing against the frame grid: `rtp_late`, `jitter_underruns` and
/// every `arrival_delay` percentile are measured against a playout deadline whose
/// offset is a parameter of the jitter buffer, and A8 measured the same link
/// reading 0.196 to 7.442 per cent as that parameter moved, so they are
/// experience and barred from deciding. `macos/audio-render`'s excess curve does
/// count crossings independently of any target, and it landed after all forty
/// committed envelopes were written - 0 of 40 carry an `excess_*` key.
fn read_audio(path: &Path, name: &str, document: &Value) -> Result<Reading, Refusal> {
    let observations = document
        .get("observations")
        .ok_or_else(|| Refusal::new(path, "states no observations at all"))?;

    let expected = count(path, observations, "observations", "rtp_expected")?;
    let received = count(path, observations, "observations", "rtp_received")?;
    let lost = count(path, observations, "observations", "rtp_lost")?;
    let loss = Fraction::new(lost, expected).ok_or_else(|| {
        Refusal::new(
            path,
            "states rtp_expected as 0, so no datagram was ever accounted for and every zero \
             beside it is an absence",
        )
    })?;
    let reorder = Fraction::new(
        count(path, observations, "observations", "rtp_reordered")?,
        received,
    )
    .ok_or_else(|| {
        Refusal::new(
            path,
            "states rtp_received as 0, so nothing arrived and this run measured a path with \
             nothing on it",
        )
    })?;

    let concealed = count(path, observations, "observations", "plc_frames")?;
    let underruns = count(path, observations, "observations", "render_underruns")?;
    let span_s = document
        .get("run")
        .and_then(|run| run.get("span_s"))
        .and_then(Value::as_f64)
        .ok_or_else(|| Refusal::new(path, "states no run.span_s, so it covers an unknown time"))?;

    Ok(Reading::Unreadable(Unreadable {
        name: name.to_string(),
        missing: String::from(
            "no delivery tier at all; link-metrics is video-side and no receive envelope counts \
             an arrival crossing against the frame grid",
        ),
        evidence: format!(
            "{} of {} datagrams lost, {} reordered, {} concealed of {received} played, {underruns} \
             silence events",
            loss.events(),
            loss.population(),
            reorder.events(),
            concealed,
        ),
        span_s,
        radio: radio_beside(path, "radio.csv"),
    }))
}

/// The radio trace this session left beside itself, if it left one.
///
/// `None` is a first-class answer and not a failure: CoreWLAN may decline, an
/// arm may have been recorded before the sampler existed, and the classifier
/// never reads this tier. The arms in `results/audio/e2e-clean` and
/// `results/audio/e2e-corrected` carry a trace for the whole gate rather than one
/// per arm, so they come back empty here and are handled anyway.
fn radio_beside(path: &Path, suffix: &str) -> Option<RadioHint> {
    let file = path.file_name()?.to_str()?;
    let stem = file
        .strip_suffix(".receiver.json")
        .or_else(|| file.strip_suffix(".json"))?;
    let trace = path.with_file_name(format!("{stem}.{suffix}"));
    let text = fs::read_to_string(trace).ok()?;

    let mut lines = text.lines();
    let header: Vec<&str> = lines.next()?.split(',').collect();
    let column = |wanted: &str| header.iter().position(|name| *name == wanted);
    let wanted = [
        column("rssi_dbm")?,
        column("noise_dbm")?,
        column("tx_rate_mbps")?,
        column("channel")?,
        column("width_mhz")?,
    ];

    let mut columns: Vec<Vec<f64>> = vec![Vec::new(); wanted.len()];
    for line in lines {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() <= wanted.iter().copied().max()? {
            continue;
        }
        for (slot, index) in wanted.iter().enumerate() {
            if let Ok(value) = fields[*index].trim().parse::<f64>() {
                columns[slot].push(value);
            }
        }
    }
    if columns.iter().any(Vec::is_empty) {
        return None;
    }

    // Median per column, which is what every `report.txt` in `results/` already
    // summarises a trace with, so a row here and a row there agree.
    let median = |values: &mut Vec<f64>| -> f64 {
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    Some(RadioHint {
        rssi_dbm: median(&mut columns[0]) as i64,
        noise_dbm: median(&mut columns[1]) as i64,
        tx_rate_mbps: median(&mut columns[2]),
        channel: median(&mut columns[3]) as i64,
        width_mhz: median(&mut columns[4]) as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../results")
    }

    fn reading(name: &str) -> Reading {
        read(&root().join(name), name)
            .expect("the committed arm reads")
            .expect("and is a session envelope")
    }

    fn session(name: &str) -> Session {
        match reading(name) {
            Reading::Classifiable(session) => *session,
            Reading::Unreadable(unreadable) => {
                panic!("{name} was expected to classify: {}", unreadable.missing)
            }
        }
    }

    fn unreadable(name: &str) -> Unreadable {
        match reading(name) {
            Reading::Unreadable(unreadable) => unreadable,
            Reading::Classifiable(_) => panic!("{name} was expected to be unreadable"),
        }
    }

    #[test]
    fn the_rate_keys_are_the_ones_the_harnesses_wrote() {
        let keys: Vec<String> = THRESHOLDS.iter().map(|t| rate_key(*t)).collect();
        assert_eq!(
            keys,
            vec![
                "over_1_25t_per_min",
                "over_1_5t_per_min",
                "over_2t_per_min",
                "over_3t_per_min",
                "over_4t_per_min",
                "over_6t_per_min",
            ]
        );
    }

    #[test]
    fn a_video_session_round_trips_its_own_rates() {
        let session = session("b3-channel/ch36-return-r1.json");
        // 19.002794684164172 crossings a minute over 119.982351959 s is 38
        // crossings, and the classifier's ceiling was derived from that rate.
        let crossings = session.crossings_per_min();
        assert!(
            (crossings - 19.002_794_684_164_172).abs() < 1e-9,
            "read {crossings} where the file states 19.0027946841641"
        );
        let clusters = session.clusters_per_min();
        assert!(
            (clusters - 18.502_721_139_844_063).abs() < 1e-9,
            "{clusters}"
        );
    }

    #[test]
    fn no_committed_session_carries_a_loss_tier_and_none_is_fabricated() {
        // stream.expected is access units while stream.reordered and
        // stream.packet_loss are datagrams, so there is no population in the file
        // for either. An earlier reader divided them and printed 30.8 per cent
        // where the datagram figure is nearer 0.69.
        let session = session("b3-channel/ch36-return-r1.json");
        assert_eq!(session.observation.stream.reorder.events(), 4_441);
        assert_eq!(
            session.observation.stream.reorder.population(),
            None,
            "the datagram population is not in this envelope and must not be substituted"
        );
        assert_eq!(session.observation.stream.reorder.value(), None);
        assert_eq!(
            session.observation.stream.loss_ratio, None,
            "None is the absence of the tier; a zero here would claim the air lost nothing"
        );
        // The access-unit shortfall is carried where no rule can read it.
        assert_eq!(session.access_units_short, 0);
        assert_eq!(session.datagrams_lost, 0);
    }

    #[test]
    fn an_arm_recorded_before_the_tail_counters_is_refused_not_read_as_clean() {
        // This arm has a single 649.59 ms interval against a p99 of 17.76 ms, and
        // zero-filling its tail would have hidden exactly that.
        let unreadable = unreadable("b1-proximity/normal-r2.json");
        assert!(
            unreadable.missing.contains("counted crossing"),
            "the refusal must name the missing tier: {}",
            unreadable.missing
        );
        assert!(unreadable.evidence.contains("649.59"));
    }

    #[test]
    fn every_audio_arm_is_refused_and_keeps_its_counters() {
        // The one committed arm with loss. Its 382 of 23997 is real and is
        // reported, and it still cannot be classified, because a verdict needs
        // the tier that is missing rather than the one that is present.
        let unreadable = unreadable("audio/jitter-target-a8/t20-p3.receiver.json");
        assert!(unreadable.missing.contains("no delivery tier"));
        assert!(
            unreadable.evidence.contains("382 of 23997"),
            "the counters must survive the refusal: {}",
            unreadable.evidence
        );
    }

    #[test]
    fn the_cleanest_arm_this_project_recorded_is_not_called_a_degradation() {
        // The defect this module was rebuilt for. 0 lost of 120005 and no render
        // underrun, and it used to print as UnknownDegradation.
        let unreadable = unreadable("audio/e2e-clean/clean-600s.receiver.json");
        assert!(unreadable.evidence.contains("0 of 120005 datagrams lost"));
        assert!(unreadable.evidence.contains("0 silence events"));
    }

    #[test]
    fn the_display_never_reaches_the_middle_tier() {
        // The soak is the one committed run with a real display: 95.79 per cent
        // fresh ticks over 71648 callbacks. It has to appear in the experience
        // tier and nowhere else, and the middle tier has no field it could
        // appear in, so this checks the half that is not a compile error.
        let fresh = session("soak-1080p120/soak.json")
            .observation
            .experience
            .fresh_tick_ratio
            .expect("the soak presented frames");
        assert!((fresh - 0.957_919_271_996_427).abs() < 1e-9, "{fresh}");
    }

    #[test]
    fn a_link_only_arm_has_no_fresh_tick_ratio_rather_than_zero() {
        assert_eq!(
            session("b3-channel/ch36-r1.json")
                .observation
                .experience
                .fresh_tick_ratio,
            None,
            "no display ran, and 0.0 would say every tick was stale"
        );
    }

    #[test]
    fn a_radio_trace_beside_a_session_is_read_and_one_absent_is_not_invented() {
        let hint = session("b3-channel/ch36-r1.json")
            .observation
            .radio
            .expect("ch36-r1.wifi.csv sits beside it");
        assert_eq!(hint.channel, 36);
        assert_eq!(hint.width_mhz, 80);
        assert_eq!(hint.tx_rate_mbps, 1200.0);

        assert_eq!(
            unreadable("audio/e2e-clean/clean-600s.receiver.json").radio,
            None,
            "this arm's trace covers the whole gate rather than the arm, and its absence must \
             not stop it being read"
        );
    }

    #[test]
    fn the_corpus_splits_into_readable_and_refused_with_nothing_untrusted() {
        let (readings, refusals) = walk(&root(), Tracking::Committed).expect("the corpus lists");
        assert!(
            refusals.is_empty(),
            "a committed file can no longer be trusted: {refusals:?}"
        );
        let classifiable = readings
            .iter()
            .filter(|r| matches!(r, Reading::Classifiable(_)))
            .count();
        let unreadable = readings
            .iter()
            .filter(|r| matches!(r, Reading::Unreadable(_)))
            .count();
        // The partition and not the population, which is what the name promises. An
        // earlier version asserted 21 classifiable and 38 unreadable, and committing
        // N4's arms took the first to 104: a test that has to be edited every time
        // evidence lands is a test that stops being read, and the count was never the
        // contract. What must hold is that every session lands in exactly one side,
        // that neither side is empty - an all-refused corpus and an all-classified one
        // are both ways this reader could be broken while agreeing with itself - and
        // that nothing sits outside both.
        assert_eq!(
            classifiable + unreadable,
            readings.len(),
            "every reading is classifiable or unreadable and nothing is neither"
        );
        assert!(
            classifiable > 0,
            "no committed session carries the tail counters, so this reader would agree \
             with itself over an empty population"
        );
        assert!(
            unreadable > 0,
            "every committed session read cleanly, which has never been true here: the \
             audio envelopes carry no delivery tier and the older arms predate the tail \
             counters, so a zero means the reader stopped noticing"
        );
    }

    #[test]
    fn an_uncommitted_session_is_not_part_of_the_population() {
        // A peer's scratch directory under results/ widened this harness's
        // population once already: seventeen sessions from another agent's run,
        // uncommitted, silently classified. A corpus nothing has committed must
        // refuse under Committed and read under AsFound, and the two answers being
        // different is the whole point.
        let temporary =
            std::env::temp_dir().join(format!("network-health-untracked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temporary);
        fs::create_dir_all(temporary.join("b3-channel")).expect("a scratch corpus");
        fs::copy(
            root().join("b3-channel/ch36-r1.json"),
            temporary.join("b3-channel/ch36-r1.json"),
        )
        .expect("one session copied");

        let (found, refusals) =
            walk(&temporary, Tracking::AsFound).expect("the filesystem lists it");
        assert!(refusals.is_empty());
        assert_eq!(found.len(), 1, "as-found reads what is on disk");

        let refused = walk(&temporary, Tracking::Committed)
            .expect_err("nothing here is committed, so the population is empty");
        assert!(
            refused.why.contains("no committed .json") || refused.why.contains("git ls-files"),
            "the refusal must name the empty population or git's own complaint rather than \
             passing over it: {}",
            refused.why
        );

        let _ = fs::remove_dir_all(&temporary);
    }
}
