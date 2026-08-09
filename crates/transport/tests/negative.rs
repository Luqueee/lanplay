//! What the transport does when the network misbehaves.
//!
//! Every test here drives the real [`Packetizer`] and [`Depacketizer`]; none
//! of them asserts on an intermediate that only exists for the test. The
//! happy path is covered by the unit tests next to the code. These are the
//! cases that decide whether a stream survives a real LAN: a counter that
//! wraps, a packet that never arrives, one that arrives twice, one that
//! arrives early, and a sender that has lost its mind.

use std::path::PathBuf;

use lanplay_protocol::FrameId;
use lanplay_telemetry::Timestamp;
use lanplay_transport::{
    Depacketizer, DepacketizerConfig, H264_CLOCK_RATE, H264_PAYLOAD_TYPE, MAX_UDP_PAYLOAD,
    NAL_LENGTH_SIZE, Packetizer, RtpClock, SequenceNumber, Ssrc, TxStats, parse_packet,
};
use lanplay_video_core::{
    EncodedAccessUnit, VideoTimestamp, avcc_nal_units, parse_stream, to_avcc,
};
use sha2::{Digest, Sha256};

const FIXTURE: &str = "motion-1920x1080@120-10s-50M.h264";

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// A NAL unit with a deterministic pseudo-random body, so comparing the
/// reassembled bytes actually compares something.
fn nal(kind: u8, len: usize, seed: u64) -> Vec<u8> {
    assert!(len >= 1);
    let mut out = Vec::with_capacity(len);
    out.push(0x60 | (kind & 0x1F));
    let mut state = seed | 1;
    while out.len() < len {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn access_unit(id: u64, index: u64, nals: &[Vec<u8>]) -> EncodedAccessUnit {
    EncodedAccessUnit {
        id: FrameId::new(id),
        pts: VideoTimestamp::from_frame_index(index, 120, 1),
        is_idr: nals.iter().any(|n| n[0] & 0x1F == 5),
        data: to_avcc(nals.iter().map(Vec::as_slice), NAL_LENGTH_SIZE),
    }
}

fn packetizer(sequence: u16, base: u32, ssrc: u32) -> Packetizer {
    Packetizer::with_sequence(
        Ssrc(ssrc),
        RtpClock::new(H264_CLOCK_RATE, base),
        H264_PAYLOAD_TYPE,
        MAX_UDP_PAYLOAD,
        SequenceNumber(sequence),
    )
}

fn depacketizer(reorder_window: usize, max_access_unit_bytes: usize) -> Depacketizer {
    Depacketizer::new(DepacketizerConfig {
        payload_type: H264_PAYLOAD_TYPE,
        reorder_window,
        max_access_unit_bytes,
    })
}

fn datagrams(packetizer: &mut Packetizer, unit: &EncodedAccessUnit) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    packetizer
        .packetize(unit, |datagram| out.push(datagram.to_vec()))
        .expect("packetises");
    out
}

fn marker(datagram: &[u8]) -> bool {
    parse_packet(datagram).expect("valid").header.marker
}

fn sequence(datagram: &[u8]) -> u16 {
    parse_packet(datagram).expect("valid").header.sequence.0
}

fn timestamp(datagram: &[u8]) -> u32 {
    parse_packet(datagram).expect("valid").header.timestamp.0
}

#[test]
fn a_sequence_number_wrap_loses_nothing() {
    let mut tx = packetizer(65530, 0, 0xAAAA_0001);
    let mut rx = depacketizer(32, 4 << 20);

    let units: Vec<EncodedAccessUnit> = (0..1000)
        .map(|i| {
            access_unit(
                i + 1,
                i,
                &[
                    nal(1, 1500, i),
                    nal(1, 1500, i + 7_000),
                    nal(6, 900, i + 90_000),
                ],
            )
        })
        .collect();

    let mut sent = 0usize;
    let mut wrapped = false;
    let mut received = Vec::new();
    for unit in &units {
        for datagram in datagrams(&mut tx, unit) {
            wrapped |= sequence(&datagram) == 0;
            sent += 1;
            if let Some(done) = rx.push(&datagram, Timestamp::now()) {
                received.push(done);
            }
        }
    }

    assert!(sent >= 5000, "{sent} packets is not a soak");
    assert!(wrapped, "the sequence counter never crossed zero");
    let stats = rx.stats();
    assert_eq!(stats.lost, 0);
    assert_eq!(stats.malformed, 0);
    assert_eq!(stats.duplicates, 0);
    assert_eq!(stats.access_units_dropped, 0);
    assert_eq!(stats.access_units_completed, units.len() as u64);
    assert_eq!(received.len(), units.len());
    for (sent, got) in units.iter().zip(&received) {
        assert_eq!(digest(&sent.data), digest(&got.data));
        assert_eq!(sent.id, got.id);
    }
}

#[test]
fn a_timestamp_wrap_does_not_merge_or_split_access_units() {
    // Two frames' worth of ticks short of the wrap, so it lands mid-stream.
    let base = u32::MAX - 1_000;
    let mut tx = packetizer(0, base, 0xAAAA_0002);
    let mut rx = depacketizer(32, 4 << 20);

    let units: Vec<EncodedAccessUnit> = (0..20)
        .map(|i| access_unit(i + 1, i, &[nal(1, 2_400, i), nal(6, 64, i + 500)]))
        .collect();

    let mut wrapped = false;
    let mut previous = base;
    let mut received = Vec::new();
    for unit in &units {
        for datagram in datagrams(&mut tx, unit) {
            let ticks = timestamp(&datagram);
            wrapped |= ticks < previous;
            previous = ticks;
            if let Some(done) = rx.push(&datagram, Timestamp::now()) {
                received.push(done);
            }
        }
    }

    assert!(wrapped, "the RTP timestamp never wrapped");
    assert_eq!(rx.stats().access_units_completed, units.len() as u64);
    assert_eq!(rx.stats().access_units_dropped, 0);
    assert_eq!(received.len(), units.len());
    for (sent, got) in units.iter().zip(&received) {
        assert_eq!(digest(&sent.data), digest(&got.data));
    }
    // Ticks since the first access unit must keep climbing across the wrap,
    // 750 per frame at 120 fps, or the media timeline jumps backwards.
    for (index, got) in received.iter().enumerate() {
        assert_eq!(got.pts.value, index as i64 * 750);
        assert_eq!(got.pts.timescale, H264_CLOCK_RATE);
    }
}

#[test]
fn ten_slices_are_one_access_unit_with_one_marker() {
    let nals: Vec<Vec<u8>> = (0..10).map(|i| nal(1, 300 + i as usize, i)).collect();
    let unit = access_unit(1, 0, &nals);
    let mut tx = packetizer(4000, 0, 0xAAAA_0003);
    let mut rx = depacketizer(32, 4 << 20);

    let packets = datagrams(&mut tx, &unit);
    assert_eq!(packets.len(), 10, "each slice fits one packet");
    assert_eq!(packets.iter().filter(|p| marker(p)).count(), 1);
    assert!(marker(packets.last().expect("packets")));
    let stamps: Vec<u32> = packets.iter().map(|p| timestamp(p)).collect();
    assert!(stamps.windows(2).all(|w| w[0] == w[1]));

    let mut received = Vec::new();
    for datagram in &packets {
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received.push(done);
        }
    }

    assert_eq!(received.len(), 1, "ten slices are one frame, not ten");
    let got = &received[0];
    assert_eq!(avcc_nal_units(&got.data, NAL_LENGTH_SIZE).count(), 10);
    assert_eq!(digest(&got.data), digest(&unit.data));
}

#[test]
fn a_sixty_kilobyte_nal_survives_fragmentation_byte_for_byte() {
    let unit = access_unit(9, 3, &[nal(5, 60_000, 0xBEEF)]);
    let mut tx = packetizer(100, 0, 0xAAAA_0004);
    let mut rx = depacketizer(32, 4 << 20);

    let mut packets = Vec::new();
    let report = tx
        .packetize(&unit, |d| packets.push(d.to_vec()))
        .expect("packetises");

    assert!(packets.len() > 50, "60 KB must fragment");
    assert_eq!(report.packets as usize, packets.len());
    assert_eq!(report.single_nal, 0, "a 60 KB NAL cannot ride one packet");
    assert_eq!(report.fu_a, report.packets);
    assert_eq!(packets.iter().filter(|p| marker(p)).count(), 1);

    let mut received = None;
    for datagram in &packets {
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received = Some(done);
        }
    }
    let got = received.expect("access unit completes");
    assert_eq!(digest(&got.data), digest(&unit.data));
    assert!(got.is_idr);
    assert_eq!(rx.stats().missing_fragments, 0);
}

#[test]
fn the_whole_fixture_round_trips_byte_for_byte() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(FIXTURE);
    let Ok(stream) = std::fs::read(&path) else {
        eprintln!(
            "skipping: fixture {} is missing; generate it with the ffmpeg command in the phase 1 brief",
            path.display()
        );
        return;
    };

    let (sets, units) = parse_stream(&stream).expect("fixture parses");
    assert_eq!(sets.nal_length_size, NAL_LENGTH_SIZE);
    assert!(units.len() > 1000, "fixture is shorter than expected");

    let mut tx = packetizer(65000, 0, 0xAAAA_0005);
    let mut rx = depacketizer(32, 4 << 20);
    let mut totals = TxStats::default();
    let mut max_packets = 0u32;
    let mut received = Vec::with_capacity(units.len());
    let mut buffer = Vec::new();

    for (index, raw) in units.iter().enumerate() {
        let unit = EncodedAccessUnit {
            id: FrameId::new(index as u64 + 1),
            pts: VideoTimestamp::from_frame_index(index as u64, 120, 1),
            is_idr: raw.is_idr,
            data: raw.data.clone(),
        };
        buffer.clear();
        let report = tx
            .packetize(&unit, |datagram| buffer.push(datagram.to_vec()))
            .expect("packetises");
        totals.record(&report);
        max_packets = max_packets.max(report.packets);
        for datagram in &buffer {
            if let Some(done) = rx.push(datagram, Timestamp::now()) {
                received.push(done);
            }
        }
    }

    let stats = *rx.stats();
    println!("{totals}");
    println!("{stats}");
    println!(
        "packets per access unit: mean {:.1}, max {max_packets}",
        totals.packets_per_access_unit()
    );
    let nals_in: usize = units
        .iter()
        .map(|raw| avcc_nal_units(&raw.data, NAL_LENGTH_SIZE).count())
        .sum();
    let nals_out: usize = received
        .iter()
        .map(|got| avcc_nal_units(&got.data, NAL_LENGTH_SIZE).count())
        .sum();
    println!(
        "nal units: {nals_in} in, {nals_out} out; {} idr access units",
        units.iter().filter(|raw| raw.is_idr).count()
    );
    assert_eq!(nals_in, nals_out, "a NAL unit was dropped or invented");

    assert_eq!(stats.lost, 0);
    assert_eq!(stats.malformed, 0);
    assert_eq!(stats.access_units_dropped, 0);
    assert_eq!(received.len(), units.len());
    for (raw, got) in units.iter().zip(&received) {
        assert_eq!(digest(&raw.data), digest(&got.data));
        assert_eq!(raw.is_idr, got.is_idr);
    }
}

#[test]
fn one_lost_fragment_costs_its_own_access_unit_and_no_other() {
    let damaged = access_unit(1, 0, &[nal(1, 20_000, 11)]);
    let intact = access_unit(2, 1, &[nal(1, 1_400, 22), nal(6, 200, 33)]);
    let mut tx = packetizer(7, 0, 0xAAAA_0006);
    let mut rx = depacketizer(8, 4 << 20);

    let first = datagrams(&mut tx, &damaged);
    let second = datagrams(&mut tx, &intact);
    assert!(first.len() > 10, "the NAL must fragment past the window");

    let mut received = Vec::new();
    for (index, datagram) in first.iter().chain(&second).enumerate() {
        // A middle fragment never makes it out of the switch.
        if index == 4 {
            continue;
        }
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received.push(done);
        }
    }

    let stats = *rx.stats();
    assert_eq!(stats.lost, 1);
    assert!(stats.missing_fragments >= 1);
    assert_eq!(stats.access_units_dropped, 1);
    assert_eq!(stats.access_units_completed, 1);
    assert_eq!(received.len(), 1);
    assert_eq!(digest(&received[0].data), digest(&intact.data));
    assert_eq!(received[0].id, FrameId::new(2));
}

#[test]
fn a_duplicated_packet_is_counted_and_changes_nothing() {
    let unit = access_unit(1, 0, &[nal(1, 4_000, 44), nal(6, 100, 55)]);
    let mut tx = packetizer(500, 0, 0xAAAA_0007);
    let mut rx = depacketizer(32, 4 << 20);

    let packets = datagrams(&mut tx, &unit);
    assert!(packets.len() >= 3);

    let mut received = Vec::new();
    for (index, datagram) in packets.iter().enumerate() {
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received.push(done);
        }
        if index == 1 {
            // The same datagram again, as a retransmitting switch would.
            if let Some(done) = rx.push(datagram, Timestamp::now()) {
                received.push(done);
            }
        }
    }

    let stats = *rx.stats();
    assert_eq!(stats.duplicates, 1);
    assert_eq!(stats.lost, 0);
    assert_eq!(received.len(), 1);
    assert_eq!(digest(&received[0].data), digest(&unit.data));
}

#[test]
fn packets_reordered_inside_an_access_unit_are_recovered() {
    let unit = access_unit(1, 0, &[nal(5, 7_000, 66)]);
    let mut tx = packetizer(9, 0, 0xAAAA_0008);
    let mut rx = depacketizer(32, 4 << 20);

    let packets = datagrams(&mut tx, &unit);
    assert!(packets.len() >= 6);

    // Swap adjacent pairs after the first packet, which must still lead so the
    // receiver latches the right starting sequence number.
    let mut order: Vec<usize> = (0..packets.len()).collect();
    let mut index = 1;
    while index + 1 < order.len() {
        order.swap(index, index + 1);
        index += 2;
    }

    let mut received = None;
    for &position in &order {
        assert!(rx.buffered_packets() <= rx.reorder_window());
        if let Some(done) = rx.push(&packets[position], Timestamp::now()) {
            received = Some(done);
        }
    }

    let stats = *rx.stats();
    assert!(stats.reordered > 0, "nothing went through the reorder ring");
    assert_eq!(stats.lost, 0);
    assert_eq!(stats.access_units_dropped, 0);
    let got = received.expect("access unit completes");
    assert_eq!(digest(&got.data), digest(&unit.data));
}

#[test]
fn interleaving_two_access_units_blocks_neither_and_grows_nothing() {
    let first = access_unit(1, 0, &[nal(5, 3_400, 77)]);
    let second = access_unit(2, 1, &[nal(1, 3_400, 88)]);
    let mut tx = packetizer(0, 0, 0xAAAA_0009);
    let mut rx = depacketizer(32, 4 << 20);

    let a = datagrams(&mut tx, &first);
    let b = datagrams(&mut tx, &second);
    assert_eq!(a.len(), b.len());

    let ceiling = rx.memory_bytes() + 2 * (4 << 10);
    let mut delivered: Vec<(usize, FrameId)> = Vec::new();
    let mut received = Vec::new();
    let mut step = 0usize;
    for (one, two) in a.iter().zip(&b) {
        for datagram in [one, two] {
            if let Some(done) = rx.push(datagram, Timestamp::now()) {
                delivered.push((step, done.id));
                received.push(done);
            }
            assert!(rx.buffered_packets() <= rx.reorder_window());
            assert!(
                rx.memory_bytes() <= ceiling,
                "held {} bytes, ceiling {ceiling}",
                rx.memory_bytes()
            );
            step += 1;
        }
    }

    assert_eq!(received.len(), 2);
    assert!(rx.stats().reordered > 0);
    assert_eq!(rx.stats().lost, 0);
    assert_eq!(rx.stats().access_units_dropped, 0);
    assert_eq!(digest(&received[0].data), digest(&first.data));
    assert_eq!(digest(&received[1].data), digest(&second.data));
    // The older access unit came out on the packet that completed it, not
    // after the younger one had finished arriving.
    assert_eq!(delivered[0].1, FrameId::new(1));
    assert_eq!(delivered[0].0, 2 * (a.len() - 1));
}

#[test]
fn nothing_a_stranger_sends_is_accepted_and_the_stream_recovers() {
    let first = access_unit(1, 0, &[nal(5, 2_000, 99)]);
    let second = access_unit(2, 1, &[nal(1, 2_000, 111)]);
    let mut tx = packetizer(300, 0, 0xAAAA_000A);
    let mut rx = depacketizer(32, 4 << 20);

    let a = datagrams(&mut tx, &first);
    let b = datagrams(&mut tx, &second);
    let mut received = Vec::new();
    for datagram in &a {
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received.push(done);
        }
    }

    let good = *rx.stats();
    let mut garbage: Vec<Vec<u8>> = Vec::new();
    // Something that is not RTP at all.
    let mut noise = nal(0, 37, 0xDEAD);
    noise[0] = 0x00;
    garbage.push(noise);
    // A packet cut off inside its fixed header.
    garbage.push(a[0][..8].to_vec());
    // Version 1: a different protocol, or a corrupted first byte.
    let mut wrong_version = a[0].clone();
    wrong_version[0] = (1 << 6) | (wrong_version[0] & 0x3F);
    garbage.push(wrong_version);
    // The extension bit is set and the extension is not there.
    garbage.push(a[0][..14].to_vec());
    // Longer than any datagram we ever emit.
    garbage.push(vec![0x80; MAX_UDP_PAYLOAD + 1]);

    for datagram in &garbage {
        assert!(rx.push(datagram, Timestamp::now()).is_none());
    }
    assert_eq!(rx.stats().malformed, garbage.len() as u64);
    assert_eq!(rx.stats().packets, good.packets);

    // Arbitrary datagrams must never be mistaken for our stream.
    let mut fuzz = 0u64;
    for seed in 0..256u64 {
        let length = 12 + (seed as usize * 37) % 300;
        let datagram = nal(0, length, seed.wrapping_mul(0x9E37_79B9));
        assert!(rx.push(&datagram, Timestamp::now()).is_none());
        fuzz += 1;
    }
    let after = *rx.stats();
    assert_eq!(after.packets, good.packets, "a stray datagram was accepted");
    assert_eq!(
        after.malformed + after.unknown_ssrc + after.unknown_payload_type,
        garbage.len() as u64 + fuzz
    );

    for datagram in &b {
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received.push(done);
        }
    }
    assert_eq!(received.len(), 2);
    assert_eq!(digest(&received[1].data), digest(&second.data));
    assert_eq!(rx.stats().access_units_dropped, 0);
}

#[test]
fn a_second_ssrc_is_counted_and_dropped_without_disturbing_the_first() {
    let first = access_unit(1, 0, &[nal(5, 3_000, 121)]);
    let second = access_unit(2, 1, &[nal(1, 3_000, 122)]);
    let stranger = access_unit(1, 0, &[nal(5, 3_000, 123)]);

    let mut ours = packetizer(1000, 0, 0xAAAA_000B);
    let mut theirs = packetizer(1000, 0, 0xBBBB_000B);
    let mut rx = depacketizer(32, 4 << 20);

    let a = datagrams(&mut ours, &first);
    let b = datagrams(&mut ours, &second);
    let intruder = datagrams(&mut theirs, &stranger);

    let mut received = Vec::new();
    for datagram in a.iter().chain(&intruder).chain(&b) {
        if let Some(done) = rx.push(datagram, Timestamp::now()) {
            received.push(done);
        }
    }

    let stats = *rx.stats();
    assert_eq!(stats.unknown_ssrc, intruder.len() as u64);
    assert_eq!(stats.packets, (a.len() + b.len()) as u64);
    assert_eq!(stats.lost, 0);
    assert_eq!(stats.access_units_dropped, 0);
    assert_eq!(received.len(), 2);
    assert_eq!(digest(&received[0].data), digest(&first.data));
    assert_eq!(digest(&received[1].data), digest(&second.data));
}

#[test]
fn a_sender_that_never_marks_a_frame_cannot_make_the_receiver_grow() {
    const MAX_BYTES: usize = 64 << 10;
    let filler = access_unit(1, 0, &[nal(1, 6_000, 131), nal(1, 6_000, 132)]);
    let mut tx = packetizer(2000, 0, 0xAAAA_000C);
    let mut rx = depacketizer(8, MAX_BYTES);

    // The same timestamp forever and not one marker bit: exactly the shape a
    // wedged encoder produces, and the shape a naive receiver buffers until it
    // is killed.
    let mut peak = rx.memory_bytes();
    for _ in 0..40 {
        for mut datagram in datagrams(&mut tx, &filler) {
            datagram[1] &= 0x7F;
            assert!(rx.push(&datagram, Timestamp::now()).is_none());
            peak = peak.max(rx.memory_bytes());
        }
    }

    let stats = *rx.stats();
    assert!(stats.oversized_access_units >= 1);
    assert!(stats.access_units_dropped >= 1);
    assert_eq!(stats.access_units_completed, 0);
    assert!(
        peak <= 8 * MAX_UDP_PAYLOAD + 4 * MAX_BYTES,
        "receiver grew to {peak} bytes"
    );

    // A well-behaved frame after the flood must still get through.
    let good = access_unit(2, 500, &[nal(5, 2_000, 141)]);
    let mut received = None;
    for datagram in datagrams(&mut tx, &good) {
        if let Some(done) = rx.push(&datagram, Timestamp::now()) {
            received = Some(done);
        }
    }
    let got = received.expect("the receiver resynchronises");
    assert_eq!(digest(&got.data), digest(&good.data));
    assert_eq!(rx.stats().access_units_completed, 1);
}
