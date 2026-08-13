//! What a run observed, and the text it turns into.
//!
//! Separate from the window so that the report can be rendered, read and
//! tested on a machine with no display at all. Everything here is plain data;
//! nothing in this module knows what a swap chain is.

use std::fmt::Write as _;

use hdrhistogram::Histogram;
use lanplay_telemetry::Nanos;

/// Widest interval the histogram holds, matching the rest of the project. An
/// input-to-present interval longer than this is a stall, not a latency.
const MAX_NANOS: u64 = 10_000_000_000;

/// The display the window covered, named rather than numbered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Display {
    /// The DXGI index the name resolved to on this boot. Recorded because it
    /// is what the capture side will be given, and worthless on the next one.
    pub index: u32,
    pub device_name: String,
    pub monitor_name: String,
    pub adapter_name: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

/// One input path's counters and its own histogram.
///
/// Two of these exist and they are never added together. `SendInput` reaching
/// the raw input queue is not proof that it reached the window message queue,
/// and a total would hide exactly the case this target was built to expose.
pub struct Tally {
    pub label: &'static str,
    /// Every event this path delivered, whether or not it could be timed.
    pub seen: u64,
    /// Events that arrived while white was already up. Real input, but with
    /// no colour change of their own to be measured against.
    pub during_flash: u64,
    /// Handled-to-presented intervals for the events on this path that caused
    /// a transition. Its length is how many of `seen` were timed.
    pub latency: Histogram<u64>,
}

impl Tally {
    pub fn new(label: &'static str) -> Tally {
        Tally {
            label,
            seen: 0,
            during_flash: 0,
            latency: Histogram::new_with_bounds(1, MAX_NANOS, 3).expect("valid histogram bounds"),
        }
    }

    /// Events that caused a transition and were therefore timed.
    pub fn timed(&self) -> u64 {
        self.latency.len()
    }
}

/// Everything one run has to say.
pub struct Observed {
    pub display: Display,
    pub flash_presents: u32,
    /// Whether the swap chain was allowed to tear, which is the only way a
    /// present can reach the display without waiting for vertical blank.
    pub tearing: bool,
    /// Whether the window held the foreground. Recorded because the ordinary
    /// window messages only reach the focused window while raw input, being
    /// registered as an input sink, arrives either way: a run with no
    /// foreground explains its own empty message column.
    pub foreground: bool,
    pub elapsed: Nanos,
    pub presents: u64,
    pub present_failures: u64,
    /// The first failing `Present` result, so a failure count has something
    /// to be diagnosed from.
    pub first_present_failure: Option<i32>,
    /// Presents the compositor reported as invisible. Not a failure, but a
    /// present nothing could have captured.
    pub occluded_presents: u64,
    pub raw: Tally,
    pub messages: Tally,
    /// The window message path split by what carried the event. Raw input is
    /// not split the same way: telling a raw mouse from a raw keyboard needs
    /// `GetRawInputData`, and that call would land inside the interval being
    /// measured.
    pub key_messages: u64,
    pub mouse_messages: u64,
}

impl Observed {
    pub fn new(display: Display, flash_presents: u32) -> Observed {
        Observed {
            display,
            flash_presents,
            tearing: false,
            foreground: false,
            elapsed: Nanos::ZERO,
            presents: 0,
            present_failures: 0,
            first_present_failure: None,
            occluded_presents: 0,
            raw: Tally::new("raw input"),
            messages: Tally::new("window messages"),
            key_messages: 0,
            mouse_messages: 0,
        }
    }

    /// Whether either path delivered anything.
    ///
    /// Asked rather than assumed. A run that received nothing still has a
    /// well-formed report with a tidy zero in every column, and a gate reading
    /// that as a pass has already happened twice on this project. Absence of
    /// evidence is not evidence.
    pub fn saw_input(&self) -> bool {
        self.raw.seen > 0 || self.messages.seen > 0
    }

    /// Whether the two paths disagree about how many events arrived.
    pub fn paths_disagree(&self) -> bool {
        self.raw.seen != self.messages.seen
    }
}

/// Percentile summary of one series, in the shape the rest of the project
/// reports percentiles.
///
/// A copy rather than [`lanplay_telemetry::Percentiles`] because that type is
/// built from a histogram by a crate-private constructor, and a segment
/// measured by a standalone target is not one of the pipeline's stages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Summary {
    pub label: &'static str,
    pub count: u64,
    pub p50: Nanos,
    pub p95: Nanos,
    pub p99: Nanos,
    pub max: Nanos,
}

impl Summary {
    pub fn of(label: &'static str, histogram: &Histogram<u64>) -> Summary {
        Summary {
            label,
            count: histogram.len(),
            p50: Nanos(histogram.value_at_quantile(0.50)),
            p95: Nanos(histogram.value_at_quantile(0.95)),
            p99: Nanos(histogram.value_at_quantile(0.99)),
            max: Nanos(histogram.max()),
        }
    }
}

/// The whole report, ready to be written to the file named on the command
/// line.
pub fn render(observed: &Observed) -> String {
    let mut text = String::new();
    let display = &observed.display;

    let _ = writeln!(
        text,
        "input-latency-target: input message handled -> the present that first showed the change"
    );
    // Stated at the top rather than left to be inferred. Both instants are
    // read on this machine, so the figure below is a host-local interval and
    // subtracting anything the client timestamped from it would produce a
    // clock offset dressed as a latency.
    let _ = writeln!(
        text,
        "clock: QueryPerformanceCounter on this host; a host-local interval, never to be \
         differenced against a client timestamp"
    );
    let _ = writeln!(text);

    let _ = writeln!(
        text,
        "display   index {} {} {}x{} at ({},{}) monitor {:?} adapter {:?}",
        display.index,
        display.device_name,
        display.width,
        display.height,
        display.left,
        display.top,
        display.monitor_name,
        display.adapter_name
    );
    let _ = writeln!(
        text,
        "window    borderless popup, topmost, covering the whole output, foreground {}",
        yes_no(observed.foreground)
    );
    // The sync interval is the measurement, not an oversight. Waiting for
    // vertical blank would fold up to a whole refresh period of doing nothing
    // into the figure, which is a property of the panel rather than of the
    // software. The number this run produces is therefore a lower bound: a
    // vsynced game does everything measured here and then waits.
    let _ = writeln!(
        text,
        "present   sync interval 0, tearing {}, white held for {} presents; not waiting for \
         vertical blank makes this a lower bound on a vsynced game",
        yes_no(observed.tearing),
        observed.flash_presents
    );
    let _ = writeln!(
        text,
        "run       {:.2} s, {} presents, {} failed, {} occluded",
        observed.elapsed.as_secs_f64(),
        observed.presents,
        observed.present_failures,
        observed.occluded_presents
    );
    if let Some(hresult) = observed.first_present_failure {
        let _ = writeln!(
            text,
            "          first Present failure 0x{:08X}",
            hresult as u32
        );
    }
    let _ = writeln!(text);

    // Two rows, never a total. The whole reason both paths are counted is
    // that they are different queues.
    let _ = writeln!(
        text,
        "{:<18} {:>7} {:>7} {:>7}",
        "path", "seen", "timed", "in-flash"
    );
    for tally in [&observed.raw, &observed.messages] {
        let _ = writeln!(
            text,
            "{:<18} {:>7} {:>7} {:>7}",
            tally.label,
            tally.seen,
            tally.timed(),
            tally.during_flash
        );
    }
    let _ = writeln!(
        text,
        "  window messages by kind: keys {}, mouse {}",
        observed.key_messages, observed.mouse_messages
    );
    let _ = writeln!(text);

    // Microseconds, not the project's usual milliseconds. This segment is the
    // small one: at two decimal places in milliseconds a 60 microsecond p50
    // and a 30 microsecond one both read as noise.
    let _ = writeln!(
        text,
        "{:<18} {:>7} {:>10} {:>10} {:>10} {:>10}",
        "handled to present", "count", "p50", "p95", "p99", "max"
    );
    write_summary(
        &mut text,
        &Summary::of(observed.raw.label, &observed.raw.latency),
    );
    write_summary(
        &mut text,
        &Summary::of(observed.messages.label, &observed.messages.latency),
    );
    let _ = writeln!(text);

    write_findings(&mut text, observed);
    text
}

fn write_summary(text: &mut String, summary: &Summary) {
    if summary.count == 0 {
        let _ = writeln!(text, "{:<18} {:>7}   nothing timed", summary.label, 0);
        return;
    }
    let _ = writeln!(
        text,
        "{:<18} {:>7} {:>9.1}µ {:>9.1}µ {:>9.1}µ {:>9.1}µ",
        summary.label,
        summary.count,
        micros(summary.p50),
        micros(summary.p95),
        micros(summary.p99),
        micros(summary.max),
    );
}

/// Everything that makes this run something other than a clean measurement.
///
/// Always written, including the line that says there was nothing to say. A
/// section that only appears when something went wrong cannot be told apart
/// from a process that died before reaching it.
fn write_findings(text: &mut String, observed: &Observed) {
    let mut any = false;

    if !observed.saw_input() {
        any = true;
        let _ = writeln!(
            text,
            "NO INPUT: no WM_INPUT and no window input message arrived in {:.2} s. Nothing was \
             measured, the histogram above is empty because it is empty, and this run is not a \
             pass.",
            observed.elapsed.as_secs_f64()
        );
    }

    if observed.paths_disagree() {
        any = true;
        let _ = writeln!(
            text,
            "FINDING: raw input delivered {} events and the window message queue delivered {}. \
             These are different queues and the difference is the point; they are not summed \
             anywhere in this report.",
            observed.raw.seen, observed.messages.seen
        );
        // Named so that an operator can tell a known asymmetry from a real
        // one before going looking for a driver bug.
        let _ = writeln!(
            text,
            "         Known innocent causes: a held key auto-repeats into the message queue and \
             not into raw input; pointer motion is coalesced per frame for the message queue and \
             delivered per HID report to raw input; and with no foreground, only raw input \
             arrives at all."
        );
    }

    if !observed.foreground {
        any = true;
        let _ = writeln!(
            text,
            "FINDING: this window never held the foreground, so the window message counts are a \
             floor and not a measurement. Raw input was registered as an input sink and arrived \
             regardless."
        );
    }

    if observed.present_failures > 0 {
        any = true;
        let _ = writeln!(
            text,
            "FINDING: {} of {} presents failed. A failed present put nothing on the display, so \
             any interval it terminated is fiction.",
            observed.present_failures, observed.presents
        );
    }

    if observed.occluded_presents > 0 {
        any = true;
        let _ = writeln!(
            text,
            "FINDING: {} presents were reported occluded. Something was on top of this window and \
             the capture side saw that instead.",
            observed.occluded_presents
        );
    }

    if !any {
        let _ = writeln!(
            text,
            "no findings: both paths delivered the same number of events, the window held the \
             foreground, and every present succeeded."
        );
    }
}

fn micros(nanos: Nanos) -> f64 {
    nanos.get() as f64 / 1_000.0
}

fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::{Display, Observed, Summary};
    use hdrhistogram::Histogram;
    use lanplay_telemetry::Nanos;

    fn display() -> Display {
        Display {
            index: 2,
            device_name: "\\\\.\\DISPLAY3".into(),
            monitor_name: String::new(),
            adapter_name: "LanPlay IDD-LAB 1080p120".into(),
            left: 1920,
            top: 0,
            width: 1920,
            height: 1080,
        }
    }

    fn observed() -> Observed {
        let mut observed = Observed::new(display(), 8);
        observed.foreground = true;
        observed.tearing = true;
        observed.elapsed = Nanos::from_millis(30_000);
        observed.presents = 41_233;
        observed
    }

    #[test]
    fn a_summary_reports_the_percentiles_of_what_it_was_given() {
        let mut histogram = Histogram::<u64>::new_with_bounds(1, 10_000_000_000, 3).unwrap();
        for value in 1..=100u64 {
            histogram.saturating_record(value * 1_000);
        }
        let summary = Summary::of("raw input", &histogram);
        assert_eq!(summary.count, 100);
        // Three significant figures, so a percentile is the bucket the sample
        // landed in and not the sample. Asserting the exact nanosecond would
        // be asserting the histogram's rounding.
        for (quantile, wanted) in [
            (summary.p50, 50_000u64),
            (summary.p95, 95_000),
            (summary.p99, 99_000),
            (summary.max, 100_000),
        ] {
            let error = quantile.get().abs_diff(wanted);
            assert!(
                error * 1_000 <= wanted,
                "{quantile:?} is not within 0.1% of {wanted}"
            );
        }
    }

    #[test]
    fn an_empty_series_is_named_as_empty_rather_than_printed_as_zeroes() {
        let observed = observed();
        let text = super::render(&observed);
        assert!(text.contains("nothing timed"), "{text}");
        // A row of 0.0µ percentiles would read as an impossibly fast run.
        assert!(!text.contains("0.0µ"), "{text}");
    }

    #[test]
    fn a_run_that_saw_no_input_says_so_instead_of_reporting_a_clean_empty_histogram() {
        let observed = observed();
        assert!(!observed.saw_input());
        let text = super::render(&observed);
        assert!(text.contains("NO INPUT"), "{text}");
        assert!(text.contains("is not a pass"), "{text}");
    }

    #[test]
    fn a_run_that_saw_input_does_not_claim_it_saw_none() {
        let mut observed = observed();
        observed.raw.seen = 240;
        observed.messages.seen = 240;
        observed.key_messages = 240;
        let text = super::render(&observed);
        assert!(!text.contains("NO INPUT"), "{text}");
        assert!(text.contains("no findings"), "{text}");
    }

    #[test]
    fn paths_that_disagree_are_reported_side_by_side_and_never_summed() {
        let mut observed = observed();
        observed.raw.seen = 240;
        observed.messages.seen = 242;
        assert!(observed.paths_disagree());
        let text = super::render(&observed);
        assert!(
            text.contains("FINDING: raw input delivered 240 events"),
            "{text}"
        );
        assert!(text.contains("delivered 242"), "{text}");
        // 482 is the sum, and it must appear nowhere.
        assert!(!text.contains("482"), "{text}");
    }

    #[test]
    fn a_failed_present_is_visible_with_the_result_that_caused_it() {
        let mut observed = observed();
        observed.raw.seen = 10;
        observed.messages.seen = 10;
        observed.present_failures = 3;
        observed.first_present_failure = Some(0x887A_0005u32 as i32);
        let text = super::render(&observed);
        assert!(text.contains("first Present failure 0x887A0005"), "{text}");
        assert!(text.contains("3 of 41233 presents failed"), "{text}");
    }

    #[test]
    fn losing_the_foreground_explains_an_empty_message_column_rather_than_hiding_it() {
        let mut observed = observed();
        observed.foreground = false;
        observed.raw.seen = 240;
        let text = super::render(&observed);
        assert!(text.contains("never held the foreground"), "{text}");
    }

    #[test]
    fn the_report_names_the_display_it_used_with_its_dimensions() {
        let observed = observed();
        let text = super::render(&observed);
        assert!(text.contains("1920x1080 at (1920,0)"), "{text}");
        assert!(text.contains("LanPlay IDD-LAB 1080p120"), "{text}");
    }

    #[test]
    fn the_report_says_the_interval_is_host_local() {
        let text = super::render(&observed());
        assert!(text.contains("host-local interval"), "{text}");
        assert!(text.contains("QueryPerformanceCounter"), "{text}");
    }

    #[test]
    fn timed_counts_come_from_the_histogram_rather_than_a_separate_counter() {
        let mut observed = observed();
        observed.raw.seen = 3;
        observed.raw.during_flash = 1;
        observed.raw.latency.saturating_record(120_000);
        observed.raw.latency.saturating_record(240_000);
        observed.messages.seen = 3;
        assert_eq!(observed.raw.timed(), 2);
        let text = super::render(&observed);
        let row = text
            .lines()
            .skip_while(|line| !line.starts_with("path "))
            .nth(1)
            .expect("the path table names raw input first");
        assert_eq!(
            row.split_whitespace().collect::<Vec<_>>(),
            ["raw", "input", "3", "2", "1"]
        );
    }
}
