//! The client's JSON report, printed as a block a human can read, and the
//! gate that decides whether the numbers in it mean anything.

use serde::Deserialize;

/// How far one ten second window's callback rate may drift from the window
/// before it. A stall that lasts a few seconds barely moves a ten minute
/// average, but it cannot hide from its own neighbour.
const WINDOW_DRIFT: f64 = 0.05;

/// Decoder backlog growth a run may show and still be called steady, in
/// frames per minute.
const BACKLOG_SLOPE_LIMIT: f64 = 0.5;

const RULE: &str = "───────────────────────";

#[derive(Deserialize)]
pub struct Report {
    pub run: Run,
    pub stream: Stream,
    pub network: Network,
    pub decode: Decode,
    pub display: DisplayBlock,
    pub environment: EnvironmentBlock,
    pub windows: Vec<Window>,
}

#[derive(Deserialize)]
pub struct Run {
    pub seconds: f64,
    pub target_fps: f64,
    pub invalidated: bool,
    pub invalidating_events: Vec<String>,
}

#[derive(Deserialize)]
pub struct Stream {
    pub expected: u64,
    pub reconstructed: u64,
    pub packet_loss: u64,
    pub au_loss: u64,
    pub corruption: u64,
    pub reordered: u64,
    pub duplicates: u64,
}

#[derive(Deserialize)]
pub struct Network {
    pub arrival_p50_ms: f64,
    pub arrival_p95_ms: f64,
    pub arrival_p99_ms: f64,
    pub arrival_max_ms: f64,
    pub rtp_jitter_us: f64,
}

#[derive(Deserialize)]
pub struct Decode {
    pub decoded: u64,
    pub errors: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub backlog_slope_per_min: f64,
}

#[derive(Deserialize)]
pub struct DisplayBlock {
    pub nominal_hz: f64,
    pub callbacks: u64,
    pub rendered: u64,
    pub superseded: u64,
    pub empty_refreshes: u64,
    pub callback_interval_p50_ms: f64,
    pub callback_interval_p95_ms: f64,
    pub callback_interval_p99_ms: f64,
    pub callback_interval_max_ms: f64,
    pub frame_age_p50_ms: f64,
    pub frame_age_p95_ms: f64,
    pub frame_age_p99_ms: f64,
}

#[derive(Deserialize)]
pub struct EnvironmentBlock {
    pub occlusion_changes: u64,
    pub space_changes: u64,
    pub miniaturise_events: u64,
    pub display_changes: u64,
    pub link_pauses: u64,
    pub app_nap_protection: bool,
}

#[derive(Deserialize)]
pub struct Window {
    pub from_s: f64,
    pub to_s: f64,
    pub callback_hz: f64,
    pub render_hz: f64,
    pub superseded_pct: f64,
    pub frame_age_p99_ms: f64,
}

/// What the sending machine said about its own side of the wire. Read out of
/// its log because the sender writes no JSON.
#[derive(Default)]
pub struct SenderTotals {
    pub datagrams: Option<u64>,
    pub send_errors: Option<u64>,
}

impl SenderTotals {
    fn show(value: Option<u64>) -> String {
        match value {
            Some(value) => value.to_string(),
            None => "unknown".to_string(),
        }
    }
}

/// Pulls the two figures the report needs out of net-bench's own output.
pub fn sender_totals(log: &str) -> SenderTotals {
    let mut totals = SenderTotals::default();
    for line in log.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("wire: ")
            && let Some(count) = rest.split_whitespace().next()
            && let Ok(count) = count.parse::<u64>()
            && rest.contains("handed to send_to")
        {
            totals.datagrams = Some(count);
        }
        if let Some(index) = line.find("send errors ") {
            let rest = &line[index + "send errors ".len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(count) = digits.parse::<u64>() {
                totals.send_errors = Some(count);
            }
        }
    }
    totals
}

fn row(label: &str, value: &str) {
    println!("{label:<24}{value:>5}");
}

fn count(label: &str, value: u64) {
    row(label, &value.to_string());
}

fn fixed(label: &str, value: f64, decimals: usize) {
    row(label, &format!("{value:.decimals$}"));
}

fn heading(name: &str) {
    println!("\n{name}");
    println!("{RULE}");
}

pub fn print(report: &Report, sender: &SenderTotals) {
    heading("STREAM");
    count("AU sent", report.stream.expected);
    count("AU reconstructed", report.stream.reconstructed);
    count("packet loss", report.stream.packet_loss);
    count("AU loss", report.stream.au_loss);
    count("corruption", report.stream.corruption);
    count("reordered", report.stream.reordered);
    count("duplicates", report.stream.duplicates);

    heading("NETWORK");
    fixed("arrival p50 ms", report.network.arrival_p50_ms, 2);
    fixed("arrival p95 ms", report.network.arrival_p95_ms, 2);
    fixed("arrival p99 ms", report.network.arrival_p99_ms, 2);
    fixed("arrival max ms", report.network.arrival_max_ms, 2);
    fixed("rtp jitter us", report.network.rtp_jitter_us, 2);
    row("datagrams sent", &SenderTotals::show(sender.datagrams));
    row("send errors", &SenderTotals::show(sender.send_errors));

    heading("DECODE");
    count("decoded", report.decode.decoded);
    count("decode errors", report.decode.errors);
    fixed("decode p50 ms", report.decode.p50_ms, 2);
    fixed("decode p95 ms", report.decode.p95_ms, 2);
    fixed("decode p99 ms", report.decode.p99_ms, 2);
    fixed("backlog slope /min", report.decode.backlog_slope_per_min, 2);

    heading("DISPLAY");
    fixed("nominal hz", report.display.nominal_hz, 1);
    count("callbacks", report.display.callbacks);
    count("rendered", report.display.rendered);
    count("superseded", report.display.superseded);
    count("empty refreshes", report.display.empty_refreshes);
    fixed(
        "callback p50 ms",
        report.display.callback_interval_p50_ms,
        2,
    );
    fixed(
        "callback p95 ms",
        report.display.callback_interval_p95_ms,
        2,
    );
    fixed(
        "callback p99 ms",
        report.display.callback_interval_p99_ms,
        2,
    );
    fixed(
        "callback max ms",
        report.display.callback_interval_max_ms,
        2,
    );
    fixed("frame age p50 ms", report.display.frame_age_p50_ms, 2);
    fixed("frame age p95 ms", report.display.frame_age_p95_ms, 2);
    fixed("frame age p99 ms", report.display.frame_age_p99_ms, 2);

    heading("ENVIRONMENT");
    count("occlusion changes", report.environment.occlusion_changes);
    count("space changes", report.environment.space_changes);
    count("miniaturise events", report.environment.miniaturise_events);
    count("display changes", report.environment.display_changes);
    count("link pauses", report.environment.link_pauses);
    row(
        "app nap protection",
        if report.environment.app_nap_protection {
            "yes"
        } else {
            "no"
        },
    );

    heading("RUN");
    fixed("seconds", report.run.seconds, 1);
    fixed("target fps", report.run.target_fps, 1);
    row(
        "invalidated",
        if report.run.invalidated { "yes" } else { "no" },
    );
    count(
        "invalidating events",
        report.run.invalidating_events.len() as u64,
    );
    for event in &report.run.invalidating_events {
        println!("  {event}");
    }

    heading("WINDOWS");
    println!(
        "{:>8}{:>10}{:>13}{:>11}{:>13}{:>18}",
        "from_s", "to_s", "callback_hz", "render_hz", "superseded_%", "frame_age_p99_ms"
    );
    for window in &report.windows {
        println!(
            "{:>8.1}{:>10.1}{:>13.1}{:>11.1}{:>13.2}{:>18.2}",
            window.from_s,
            window.to_s,
            window.callback_hz,
            window.render_hz,
            window.superseded_pct,
            window.frame_age_p99_ms
        );
    }
}

/// Every reason the run may not be trusted. Empty means the gate passes.
///
/// `rendered == expected` is deliberately absent: a variable refresh display
/// is under no obligation to present every frame, and the ratio between the
/// two is exactly the thing this baseline exists to measure.
pub fn evaluate(report: &Report) -> Vec<String> {
    let mut reasons = Vec::new();

    if report.run.invalidated {
        reasons.push("the client invalidated the run".to_string());
    }
    for event in &report.run.invalidating_events {
        reasons.push(format!("invalidating event: {event}"));
    }

    let environment = &report.environment;
    for (count, what) in [
        (environment.occlusion_changes, "occlusion change"),
        (environment.space_changes, "Space change"),
        (environment.miniaturise_events, "miniaturise event"),
        (environment.display_changes, "display change"),
    ] {
        if count > 0 {
            reasons.push(format!(
                "{count} {what}{} during the run",
                if count == 1 { "" } else { "s" }
            ));
        }
    }

    if report.stream.reconstructed != report.stream.expected {
        reasons.push(format!(
            "reconstructed {} of {} access units",
            report.stream.reconstructed, report.stream.expected
        ));
    }
    if report.stream.corruption > 0 {
        reasons.push(format!("{} corrupt access units", report.stream.corruption));
    }
    if report.decode.errors > 0 {
        reasons.push(format!("{} decode errors", report.decode.errors));
    }
    if report.decode.backlog_slope_per_min > BACKLOG_SLOPE_LIMIT {
        reasons.push(format!(
            "decoder backlog grew {:.2}/min, over the {BACKLOG_SLOPE_LIMIT:.1}/min limit",
            report.decode.backlog_slope_per_min
        ));
    }

    if report.windows.is_empty() {
        reasons.push("no ten second windows were recorded".to_string());
    }
    for pair in report.windows.windows(2) {
        let (previous, current) = (&pair[0], &pair[1]);
        // A previous window of zero is not a baseline to compare against; it
        // is itself the stall this check exists to find.
        if previous.callback_hz <= 0.0 {
            reasons.push(format!(
                "window {:.0}-{:.0} s delivered no callbacks",
                previous.from_s, previous.to_s
            ));
            continue;
        }
        let drift = (current.callback_hz - previous.callback_hz).abs() / previous.callback_hz;
        if drift > WINDOW_DRIFT {
            reasons.push(format!(
                "window {:.0}-{:.0} s ran at {:.1} Hz, {:.1}% off the previous window's {:.1} Hz",
                current.from_s,
                current.to_s,
                current.callback_hz,
                drift * 100.0,
                previous.callback_hz
            ));
        }
    }

    reasons
}

pub fn print_verdict(reasons: &[String]) {
    println!();
    if reasons.is_empty() {
        println!("gate: PASS");
        return;
    }
    println!("gate: FAIL");
    for reason in reasons {
        println!("  - {reason}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape the client slice promised, with two clean windows.
    fn clean() -> serde_json::Value {
        serde_json::json!({
            "run": { "seconds": 20.0, "target_fps": 120.0, "invalidated": false,
                     "invalidating_events": [] },
            "stream": { "expected": 2400, "reconstructed": 2400, "packet_loss": 0,
                        "au_loss": 0, "corruption": 0, "reordered": 0, "duplicates": 0 },
            "network": { "arrival_p50_ms": 0.4, "arrival_p95_ms": 0.9, "arrival_p99_ms": 1.4,
                         "arrival_max_ms": 3.0, "rtp_jitter_us": 40.0 },
            "decode": { "decoded": 2400, "errors": 0, "p50_ms": 1.0, "p95_ms": 1.8,
                        "p99_ms": 2.4, "backlog_slope_per_min": 0.0 },
            "display": { "nominal_hz": 120.0, "callbacks": 2400, "rendered": 2380,
                         "superseded": 20, "empty_refreshes": 0,
                         "callback_interval_p50_ms": 8.33, "callback_interval_p95_ms": 8.4,
                         "callback_interval_p99_ms": 8.6, "callback_interval_max_ms": 16.7,
                         "frame_age_p50_ms": 4.1, "frame_age_p95_ms": 7.9,
                         "frame_age_p99_ms": 9.4 },
            "environment": { "occlusion_changes": 0, "space_changes": 0,
                             "miniaturise_events": 0, "display_changes": 0,
                             "link_pauses": 0, "app_nap_protection": true },
            "windows": [
                { "from_s": 0.0, "to_s": 10.0, "callback_hz": 119.9, "render_hz": 119.8,
                  "superseded_pct": 0.2, "frame_age_p99_ms": 9.4 },
                { "from_s": 10.0, "to_s": 20.0, "callback_hz": 119.7, "render_hz": 119.5,
                  "superseded_pct": 0.2, "frame_age_p99_ms": 9.3 }
            ]
        })
    }

    fn parse(value: &serde_json::Value) -> Report {
        serde_json::from_value(value.clone()).expect("the contract shape parses")
    }

    #[test]
    fn the_contract_shape_parses_and_a_clean_run_passes() {
        assert!(evaluate(&parse(&clean())).is_empty());
    }

    #[test]
    fn a_stalled_window_fails_even_though_the_average_is_fine() {
        let mut value = clean();
        // Eight good minutes and two bad ones average out; the window pair
        // does not.
        value["windows"][1]["callback_hz"] = serde_json::json!(60.0);
        let reasons = evaluate(&parse(&value));
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert!(reasons[0].contains("60.0 Hz"), "{reasons:?}");
    }

    #[test]
    fn drift_inside_five_percent_is_tolerated() {
        let mut value = clean();
        value["windows"][1]["callback_hz"] = serde_json::json!(119.9 * 0.96);
        assert!(evaluate(&parse(&value)).is_empty());
    }

    #[test]
    fn a_single_occlusion_change_invalidates_the_run() {
        let mut value = clean();
        value["environment"]["occlusion_changes"] = serde_json::json!(1);
        let reasons = evaluate(&parse(&value));
        assert_eq!(
            reasons,
            vec!["1 occlusion change during the run".to_string()]
        );
    }

    #[test]
    fn losing_one_access_unit_of_seventy_two_thousand_fails() {
        let mut value = clean();
        value["stream"]["reconstructed"] = serde_json::json!(2399);
        assert!(
            evaluate(&parse(&value))
                .iter()
                .any(|reason| reason.contains("2399 of 2400"))
        );
    }

    #[test]
    fn a_growing_backlog_fails_but_a_flat_one_does_not() {
        let mut value = clean();
        value["decode"]["backlog_slope_per_min"] = serde_json::json!(0.5);
        assert!(evaluate(&parse(&value)).is_empty());
        value["decode"]["backlog_slope_per_min"] = serde_json::json!(0.51);
        assert!(
            evaluate(&parse(&value))
                .iter()
                .any(|reason| reason.contains("backlog"))
        );
    }

    #[test]
    fn presenting_fewer_frames_than_were_sent_is_not_a_failure() {
        let mut value = clean();
        // A variable refresh panel dropping a tenth of the frames is the
        // measurement, not a fault.
        value["display"]["rendered"] = serde_json::json!(2160);
        value["display"]["superseded"] = serde_json::json!(240);
        assert!(evaluate(&parse(&value)).is_empty());
    }

    #[test]
    fn an_invalidated_run_never_passes() {
        let mut value = clean();
        value["run"]["invalidated"] = serde_json::json!(true);
        value["run"]["invalidating_events"] = serde_json::json!(["window occluded at 412.0 s"]);
        let reasons = evaluate(&parse(&value));
        assert_eq!(reasons.len(), 2, "{reasons:?}");
    }

    #[test]
    fn sender_totals_come_out_of_net_benchs_own_lines() {
        let log = "== tx ==\n\
             socket send buffer 2097152 B\n    \
             single-nal 240, fu-a 71760, send errors 0\n\
             wire: 72000 datagrams, 59400000 bytes handed to send_to, 7200.00 pkt/s, 50.10 Mbps\n\
             faults: none\n";
        let totals = sender_totals(log);
        assert_eq!(totals.datagrams, Some(72000));
        assert_eq!(totals.send_errors, Some(0));
    }

    #[test]
    fn a_receive_side_wire_line_is_not_mistaken_for_the_sender() {
        let log = "wire: 71999 datagrams, 59399000 bytes out of recv_from, 7199.90 pkt/s\n";
        assert_eq!(sender_totals(log).datagrams, None);
    }

    #[test]
    fn absent_sender_lines_stay_unknown_rather_than_zero() {
        let totals = sender_totals("ssh: connect to host windows port 22: Host is down\n");
        assert_eq!(SenderTotals::show(totals.datagrams), "unknown");
        assert_eq!(SenderTotals::show(totals.send_errors), "unknown");
    }
}
