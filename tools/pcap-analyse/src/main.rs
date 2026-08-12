//! Replays a packet capture through the receiver's own reassembly, so the
//! kernel's view of the link can be compared against the application's.
//!
//! The question is where bunching enters:
//!
//! ```text
//! AP / air
//!   -> Wi-Fi firmware and NIC
//!   -> driver and kernel
//!   -> BPF tap            <- the capture timestamps here
//!   -> socket
//!   -> lanplay receive    <- the live measurement happens here
//! ```
//!
//! A capture that is already bunched narrows the fault to everything above
//! the tap, which still includes the Mac's own driver and firmware: it does
//! not exonerate the machine. A capture that is regular while the
//! application sees bursts is the stronger result, and puts the fault
//! squarely in socket delivery or process scheduling.
//!
//! The comparison is only worth anything if both sides count the same way,
//! so this does not reimplement anything. It feeds the captured datagrams
//! through the same [`Depacketizer`] with the same configuration and the
//! same [`Delivery`] the receiver uses, with the capture's timestamps
//! substituted for the socket's. Any difference is therefore a difference in
//! timing and not in definition.
//!
//! usage:
//!   pcap-analyse <capture.pcap> [--port 5004] [--fps 120] [--json out.json]

use std::collections::VecDeque;
use std::path::PathBuf;

use lanplay_link_metrics::{Delivery, THRESHOLDS};
use lanplay_protocol::FrameId;
use lanplay_telemetry::{Nanos, Timestamp};
use lanplay_transport::{Depacketizer, DepacketizerConfig, parse_packet};

/// Frames whose first sighting has already been recorded. Matches the
/// receiver's own ring rather than a growing set: an unbounded set would
/// answer differently for a capture long enough to wrap the frame id.
const RECENT_FRAMES: usize = 256;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut port = 5004u16;
    let mut fps = 120.0f64;
    let mut json: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => port = args.next().and_then(|v| v.parse().ok()).unwrap_or(port),
            "--fps" => fps = args.next().and_then(|v| v.parse().ok()).unwrap_or(fps),
            "--json" => json = args.next().map(PathBuf::from),
            other => path = Some(PathBuf::from(other)),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: pcap-analyse <capture.pcap> [--port N] [--fps N] [--json out]");
        std::process::exit(2);
    };

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            std::process::exit(1);
        }
    };
    let capture = match Capture::parse(&bytes) {
        Ok(capture) => capture,
        Err(error) => {
            eprintln!("{}: {error}", path.display());
            std::process::exit(1);
        }
    };

    let delivery = Delivery::new(Nanos::from_millis_f64(1000.0 / fps.max(1.0)));
    let mut depacketizer = Depacketizer::new(DepacketizerConfig::default());
    let mut recent: VecDeque<FrameId> = VecDeque::with_capacity(RECENT_FRAMES);
    let mut datagrams = 0u64;
    let mut units = 0u64;

    for record in capture.records() {
        let Some((udp_port, payload)) = udp_payload(record.bytes, capture.link_type) else {
            continue;
        };
        if udp_port != port {
            continue;
        }
        datagrams += 1;
        let arrival = Timestamp::from_nanos(record.nanos);

        if let Ok(packet) = parse_packet(payload)
            && let Some(frame) = packet.header.frame_id
            && !recent.contains(&frame)
        {
            if recent.len() == RECENT_FRAMES {
                recent.pop_front();
            }
            recent.push_back(frame);
            delivery.first_seen(arrival);
        }
        if depacketizer.push(payload, arrival).is_some() {
            units += 1;
            delivery.completed(arrival);
        }
    }

    let window = delivery.cumulative();
    let stats = depacketizer.stats();
    let wait = depacketizer.reorder_wait();
    println!("capture          {}", path.display());
    println!("datagrams        {datagrams} on port {port}, {units} access units reassembled");
    println!("au delivery      {window}");
    println!(
        "au start         n={} p50 {:.2} ms p95 {:.2} ms p99 {:.2} ms max {:.2} ms",
        window.delivered,
        window.first_p50_ms,
        window.first_p95_ms,
        window.first_p99_ms,
        window.first_max_ms
    );
    print!("au late/min     ");
    for (index, multiple) in THRESHOLDS.iter().enumerate() {
        print!(
            "  >{multiple}T {:.1}",
            window.tail.per_minute(index, window.span_s)
        );
    }
    println!();
    println!(
        "au bunching      {:.1} clusters/min, catch-up {:.1} mean / {} max units, \
         stall gap p50 {:.0} ms p95 {:.0} ms",
        window.tail.clusters_per_minute(window.span_s),
        window.tail.mean_catch_up(),
        window.tail.catch_up_max,
        window.tail.stall_gap_p50_ms,
        window.tail.stall_gap_p95_ms
    );
    println!(
        "losses           {} lost, {} duplicates, {} reordered, depth max {}",
        stats.lost, stats.duplicates, stats.reordered, stats.max_reorder_depth
    );
    println!(
        "reordering       gap filled in {:.3} ms p50 / {:.3} ms p99 / {:.3} ms max",
        wait.p50_ns as f64 / 1e6,
        wait.p99_ns as f64 / 1e6,
        wait.max_ns as f64 / 1e6
    );

    if let Some(json) = json {
        // Shaped like the `delivery` section of the client's own report, so
        // the two can be put side by side without a translation step.
        let text = format!(
            concat!(
                "{{\n  \"delivered\": {},\n  \"au_interval_p50_ms\": {:.6},\n",
                "  \"au_interval_p95_ms\": {:.6},\n  \"au_interval_p99_ms\": {:.6},\n",
                "  \"au_interval_max_ms\": {:.6},\n  \"first_interval_p50_ms\": {:.6},\n",
                "  \"first_interval_p95_ms\": {:.6},\n  \"first_interval_p99_ms\": {:.6},\n",
                "  \"first_interval_max_ms\": {:.6},\n  \"span_s\": {:.6},\n",
                "  \"over_1_25t_per_min\": {:.4},\n  \"over_1_5t_per_min\": {:.4},\n",
                "  \"over_2t_per_min\": {:.4},\n  \"over_3t_per_min\": {:.4},\n",
                "  \"over_4t_per_min\": {:.4},\n  \"over_6t_per_min\": {:.4},\n",
                "  \"stall_clusters_per_min\": {:.4},\n  \"mean_catch_up_units\": {:.4},\n",
                "  \"max_catch_up_units\": {},\n  \"stall_gap_p50_ms\": {:.4},\n",
                "  \"stall_gap_p95_ms\": {:.4}\n}}\n"
            ),
            window.delivered,
            window.p50_ms,
            window.p95_ms,
            window.p99_ms,
            window.max_ms,
            window.first_p50_ms,
            window.first_p95_ms,
            window.first_p99_ms,
            window.first_max_ms,
            window.span_s,
            window.tail.per_minute(0, window.span_s),
            window.tail.per_minute(1, window.span_s),
            window.tail.per_minute(2, window.span_s),
            window.tail.per_minute(3, window.span_s),
            window.tail.per_minute(4, window.span_s),
            window.tail.per_minute(5, window.span_s),
            window.tail.clusters_per_minute(window.span_s),
            window.tail.mean_catch_up(),
            window.tail.catch_up_max,
            window.tail.stall_gap_p50_ms,
            window.tail.stall_gap_p95_ms,
        );
        if let Err(error) = std::fs::write(&json, text) {
            eprintln!("{}: {error}", json.display());
            std::process::exit(1);
        }
    }
}

/// A parsed capture file. Only what is needed to walk records: pcap is a
/// twenty-four byte header and a length-prefixed sequence, and a dependency
/// for that would be heavier than the format.
struct Capture<'a> {
    body: &'a [u8],
    big_endian: bool,
    /// True for the nanosecond-resolution variant, whose only difference is
    /// the units of the second field.
    nanosecond: bool,
    link_type: u32,
}

struct Record<'a> {
    nanos: u64,
    bytes: &'a [u8],
}

impl<'a> Capture<'a> {
    fn parse(bytes: &'a [u8]) -> Result<Capture<'a>, String> {
        if bytes.len() < 24 {
            return Err("shorter than a pcap header".into());
        }
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let (big_endian, nanosecond) = match magic {
            0xA1B2_C3D4 => (false, false),
            0xA1B2_3C4D => (false, true),
            0xD4C3_B2A1 => (true, false),
            0x4D3C_B2A1 => (true, true),
            // pcapng starts with a Section Header Block and is a different
            // format entirely. Saying so beats reading garbage.
            0x0A0D_0D0A => return Err("pcapng, not pcap: capture with -F pcap".into()),
            other => return Err(format!("unknown pcap magic {other:#010x}")),
        };
        let read32 = |at: usize| {
            let raw = [bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]];
            if big_endian {
                u32::from_be_bytes(raw)
            } else {
                u32::from_le_bytes(raw)
            }
        };
        Ok(Capture {
            body: &bytes[24..],
            big_endian,
            nanosecond,
            link_type: read32(20),
        })
    }

    fn records(&self) -> Records<'a> {
        Records {
            rest: self.body,
            big_endian: self.big_endian,
            nanosecond: self.nanosecond,
        }
    }
}

struct Records<'a> {
    rest: &'a [u8],
    big_endian: bool,
    nanosecond: bool,
}

impl<'a> Iterator for Records<'a> {
    type Item = Record<'a>;

    fn next(&mut self) -> Option<Record<'a>> {
        if self.rest.len() < 16 {
            return None;
        }
        let read32 = |at: usize| {
            let raw = [
                self.rest[at],
                self.rest[at + 1],
                self.rest[at + 2],
                self.rest[at + 3],
            ];
            if self.big_endian {
                u32::from_be_bytes(raw)
            } else {
                u32::from_le_bytes(raw)
            }
        };
        let seconds = u64::from(read32(0));
        let fraction = u64::from(read32(4));
        let captured = read32(8) as usize;
        if self.rest.len() < 16 + captured {
            return None;
        }
        let bytes = &self.rest[16..16 + captured];
        self.rest = &self.rest[16 + captured..];
        let nanos = seconds * 1_000_000_000
            + if self.nanosecond {
                fraction
            } else {
                fraction * 1000
            };
        Some(Record { nanos, bytes })
    }
}

/// Ethernet and loopback link types, which are the two a Mac produces.
const LINKTYPE_ETHERNET: u32 = 1;
const LINKTYPE_NULL: u32 = 0;

/// The UDP destination port and payload of an IPv4 datagram, if that is what
/// this frame is.
fn udp_payload(frame: &[u8], link_type: u32) -> Option<(u16, &[u8])> {
    let packet = match link_type {
        LINKTYPE_ETHERNET => {
            if frame.len() < 14 {
                return None;
            }
            // Only IPv4: the host addresses the receiver by IPv4 and a v6
            // frame here would be some other traffic.
            if u16::from_be_bytes([frame[12], frame[13]]) != 0x0800 {
                return None;
            }
            &frame[14..]
        }
        LINKTYPE_NULL => {
            if frame.len() < 4 {
                return None;
            }
            &frame[4..]
        }
        _ => return None,
    };

    if packet.len() < 20 || packet[0] >> 4 != 4 {
        return None;
    }
    let header_len = usize::from(packet[0] & 0x0F) * 4;
    // 17 is UDP.
    if packet[9] != 17 || packet.len() < header_len + 8 {
        return None;
    }
    let udp = &packet[header_len..];
    let destination = u16::from_be_bytes([udp[2], udp[3]]);
    let length = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
    // A snaplen shorter than the datagram is normal and wanted: capturing
    // payload would cost bandwidth the experiment is measuring. Take what is
    // there rather than rejecting the record.
    let end = length.max(8).min(udp.len());
    Some((destination, &udp[8..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ethernet_udp(port: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; 14];
        frame[12..14].copy_from_slice(&0x0800u16.to_be_bytes());
        let mut ip = vec![0u8; 20];
        ip[0] = 0x45;
        ip[9] = 17;
        let mut udp = vec![0u8; 8];
        udp[2..4].copy_from_slice(&port.to_be_bytes());
        udp[4..6].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        frame.extend(ip);
        frame.extend(udp);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn a_udp_datagram_is_found_inside_an_ethernet_frame() {
        let frame = ethernet_udp(5004, &[1, 2, 3, 4]);
        let (port, payload) = udp_payload(&frame, LINKTYPE_ETHERNET).expect("udp");
        assert_eq!(port, 5004);
        assert_eq!(payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn a_truncated_capture_yields_what_was_captured() {
        // The whole point of a small snaplen: the length field describes the
        // datagram on the wire, not the bytes on disk, and rejecting the
        // record would throw away every packet in the run.
        let mut frame = ethernet_udp(5004, &[1, 2, 3, 4, 5, 6, 7, 8]);
        frame.truncate(frame.len() - 4);
        let (_, payload) = udp_payload(&frame, LINKTYPE_ETHERNET).expect("udp");
        assert_eq!(payload, &[1, 2, 3, 4]);
    }

    #[test]
    fn pcapng_is_named_rather_than_misread() {
        let header = [0x0A, 0x0D, 0x0D, 0x0A, 0, 0, 0, 0];
        let mut bytes = header.to_vec();
        bytes.resize(64, 0);
        let Err(error) = Capture::parse(&bytes) else {
            panic!("pcapng must be refused");
        };
        assert!(error.contains("pcapng"), "{error}");
    }

    #[test]
    fn records_carry_microsecond_and_nanosecond_stamps_alike() {
        for (magic, fraction, expected) in [
            (0xA1B2_C3D4u32, 500_000u32, 1_000_500_000_000u64),
            (0xA1B2_3C4Du32, 500_000u32, 1_000_000_500_000u64),
        ] {
            let mut bytes = Vec::new();
            bytes.extend(magic.to_le_bytes());
            bytes.extend([0u8; 16]);
            bytes.extend(LINKTYPE_ETHERNET.to_le_bytes());
            bytes.extend(1000u32.to_le_bytes());
            bytes.extend(fraction.to_le_bytes());
            bytes.extend(4u32.to_le_bytes());
            bytes.extend(4u32.to_le_bytes());
            bytes.extend([1, 2, 3, 4]);
            let capture = Capture::parse(&bytes).expect("valid pcap");
            let record = capture.records().next().expect("one record");
            assert_eq!(record.nanos, expected);
        }
    }
}
