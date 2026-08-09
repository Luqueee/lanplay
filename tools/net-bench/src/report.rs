//! Everything the run measured, printed once at the end, plus the gate that
//! decides the exit code.

use lanplay_telemetry::{Nanos, Snapshot, Trend};
use lanplay_transport::MAX_UDP_PAYLOAD;

use crate::receiver::ReceiveReport;
use crate::sender::SendReport;
use crate::series::Series;

/// A run whose resident set grows faster than this is leaking, not warming up.
/// One 1200-byte packet per frame at 120 fps would be 8.6 MB/min, so 1 MB/min
/// is far below anything a real leak in this path could look like.
const MEMORY_TOLERANCE_BYTES_PER_MINUTE: f64 = 1_000_000.0;

/// Access units the depacketiser may legitimately hold outside its reorder
/// ring: the one being assembled, plus a ready queue that fills when a single
/// ring flush completes several at once. A reorder window holds at most a few
/// access units' worth of packets, so eight is generous and still catches a
/// queue that never drains.
const READY_HEADROOM_ACCESS_UNITS: usize = 8;

/// Everything the depacketiser is allowed to hold, derived from what it was
/// configured with rather than from what it happened to reach.
pub fn depacketizer_ceiling(receive: &ReceiveReport, largest_access_unit: usize) -> usize {
    receive.effective_reorder_window * MAX_UDP_PAYLOAD
        + READY_HEADROOM_ACCESS_UNITS * largest_access_unit
}

pub fn print_send(report: &SendReport) {
    println!("== tx ==");
    println!("{}", report.socket_buffer);
    println!("ssrc {}", report.ssrc);
    println!("{}", report.tx);
    println!(
        "wire: {} datagrams, {} bytes handed to send_to, {:.2} pkt/s, {:.2} Mbps",
        report.datagrams,
        report.datagram_bytes,
        report.packets_per_second(),
        report.wire_megabits_per_second(),
    );
    println!("faults: {}", report.faults);
}

pub fn print_receive(report: &ReceiveReport) {
    println!("== rx ==");
    println!("{}", report.socket_buffer);
    println!("{}", report.rx);
    println!(
        "wire: {} datagrams, {} bytes out of recv_from, {:.2} pkt/s",
        report.datagrams,
        report.datagram_bytes,
        report.packets_per_second(),
    );
    println!(
        "rfc 3550 jitter {:.2}µs, reorder window {} packets, stalls {}",
        report.jitter.get() as f64 / 1_000.0,
        report.effective_reorder_window,
        report.stalls,
    );
    println!(
        "depacketiser: peak {} B holding at most {} packets; second-half slope {}",
        report.depacketizer_peak_bytes,
        report.depacketizer_peak_packets,
        match steady_state(&report.depacketizer_memory).slope_per_minute() {
            Some(slope) => format!("{:+.1} kB/min", slope / 1e3),
            None => "unmeasured".to_string(),
        },
    );
    println!(
        "verify: {} matched, {} mismatched, {} without a digest, {} without a frame id",
        report.verified, report.verify_failures, report.unverifiable, report.anonymous,
    );
    if let Some(err) = &report.recv_error {
        println!("recv error: {err}");
    }
}

pub fn print_series(send: Option<&SendReport>, receive: Option<&ReceiveReport>) {
    println!("== per-packet series ==");
    println!("{}", Series::HEADER);
    let show = |series: &Series| {
        if !series.is_empty() {
            println!("{series}");
        }
    };
    if let Some(send) = send {
        show(&send.pacing_error);
        show(&send.send_syscall);
        show(&send.au_start_error);
        show(&send.rate_backlog);
    }
    if let Some(receive) = receive {
        show(&receive.inter_arrival);
        show(&receive.wire_first_packet);
        show(&receive.wire_access_unit);
    }
}

/// What the transport costs: bytes added, packets issued, and the pacing error
/// a CPU can actually see.
pub fn print_cost(send: &SendReport) {
    println!("== transport cost ==");
    if send.tx.access_units == 0 {
        println!("no access units were sent");
        return;
    }
    let units = send.tx.access_units as f64;
    let source_per_unit = send.source_bytes as f64 / units;
    let wire_per_unit = send.tx.bytes as f64 / units;
    println!(
        "access unit: {source_per_unit:.0} B of bitstream -> {wire_per_unit:.0} B of RTP \
         (+{:.2}%, {:.2} packets)",
        (send.wire_overhead_ratio() - 1.0) * 100.0,
        send.tx.packets as f64 / units,
    );
    println!(
        "largest access unit {} B in a {}-unit fixture; {} single-NAL and {} FU-A packets",
        send.largest_access_unit, send.fixture_access_units, send.tx.single_nal, send.tx.fu_a,
    );
    println!(
        "pacing error p50 {:.2}µs / p99 {:.2}µs / max {:.2}µs; send syscall p50 {:.2}µs / \
         p99 {:.2}µs",
        micros(send.pacing_error.quantile(0.50)),
        micros(send.pacing_error.quantile(0.99)),
        micros(send.pacing_error.max()),
        micros(send.send_syscall.quantile(0.50)),
        micros(send.send_syscall.quantile(0.99)),
    );
}

pub fn print_snapshot(snapshot: &Snapshot) {
    println!("== telemetry ==");
    println!("{snapshot}");
    // `transit` is NetworkSendFirst -> NetworkReceiveLast, which stays
    // positive however the send is spread; `serialisation` and `arrival` are
    // the overlapping halves and are reported as diagnostics, not summed.
    println!(
        "note: nothing decodes or presents here, so every frame retires as incomplete and \
         `frame age`, `present interval` and the decode/render segments are empty by \
         construction. `capture interval` is also unreliable: without a present mark, frames \
         retire when the collector's ring evicts them, and the final flush retires the last \
         ring's worth out of order."
    );
}

pub fn print_memory(trend: &Trend) {
    println!("== memory ==");
    match (trend.first(), trend.last(), trend.max()) {
        (Some(first), Some(last), Some(max)) => println!(
            "resident {:.1} -> {:.1} MB (peak {:.1} MB) over {} samples",
            first / 1e6,
            last / 1e6,
            max / 1e6,
            trend.count(),
        ),
        _ => println!("resident memory unavailable on this platform"),
    }
    let steady = steady_state(trend);
    match steady.slope_per_minute() {
        Some(slope) => println!(
            "second-half slope {:+.1} kB/min over {} samples: {}",
            slope / 1e3,
            steady.count(),
            if steady.is_stable(MEMORY_TOLERANCE_BYTES_PER_MINUTE) {
                "flat"
            } else {
                "GROWING"
            },
        ),
        None => println!("run too short to fit a slope"),
    }
}

/// The second half of a series.
///
/// A bounded buffer ramping to its ceiling is not a leak, and neither is a
/// fixture being paged in, but a line fitted from the first sample reads both
/// as one. A leak is growth that *continues*, so the fit starts halfway
/// through and lets everything that settles, settle.
fn steady_state(trend: &Trend) -> Trend {
    match trend.span() {
        Some(span) => trend.after_warmup(Nanos(span.get() / 2)),
        None => Trend::default(),
    }
}

/// Whether the run proved what it set out to prove.
pub struct Gate {
    pub failures: Vec<String>,
}

impl Gate {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Judges a loopback run, where the sender's count is known to the receiver.
pub fn gate(send: &SendReport, receive: &ReceiveReport, verifying: bool) -> Gate {
    let mut failures = Vec::new();

    if receive.verify_failures > 0 {
        failures.push(format!(
            "{} access units did not match their SHA-256",
            receive.verify_failures
        ));
    }
    if verifying && receive.unverifiable > 0 {
        failures.push(format!(
            "{} access units had no digest to compare against",
            receive.unverifiable
        ));
    }
    if verifying && receive.verified == 0 && receive.rx.access_units_completed > 0 {
        failures.push("--verify was requested but nothing was verified".to_string());
    }

    // Bounded memory is a contract, not an aspiration. The test is the bound
    // itself, not a zero slope: the footprint ratchets up to its ceiling as
    // rare bursts fill the reorder ring, so a least-squares fit over any
    // finite run reads an asymptote as growth. Exceeding the ceiling is the
    // bug; approaching it is the design.
    let ceiling = depacketizer_ceiling(receive, send.largest_access_unit);
    if receive.depacketizer_peak_bytes > ceiling {
        failures.push(format!(
            "the depacketiser peaked at {} B, past its {} B bound",
            receive.depacketizer_peak_bytes, ceiling,
        ));
    }

    let accounted = receive.rx.access_units_completed + receive.rx.access_units_dropped;
    let missing = send.tx.access_units.saturating_sub(accounted);
    // Loss explains a missing access unit; nothing else does. A frame that
    // vanished on an intact link is a transport bug.
    let lossless = receive.rx.lost == 0
        && send.faults.dropped == 0
        && send.tx.send_errors == 0
        && receive.rx.malformed == 0;
    if missing > 0 && lossless {
        failures.push(format!(
            "{missing} access units went missing with no loss to explain them \
             (sent {}, completed {}, dropped {})",
            send.tx.access_units,
            receive.rx.access_units_completed,
            receive.rx.access_units_dropped
        ));
    }

    Gate { failures }
}

pub fn print_gate(gate: &Gate) {
    println!("== gate ==");
    if gate.passed() {
        println!("PASS");
    } else {
        for failure in &gate.failures {
            println!("FAIL: {failure}");
        }
    }
}

fn micros(value: Nanos) -> f64 {
    value.get() as f64 / 1_000.0
}
