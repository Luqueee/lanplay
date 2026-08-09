//! The send half: fixture -> RFC 6184 packetiser -> fault injector -> UDP.

use core::fmt;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;

use lanplay_telemetry::{Nanos, Recorder, Stage, Timestamp, wait_until};
use lanplay_transport::{
    H264_CLOCK_RATE, H264_PAYLOAD_TYPE, HEADER_OVERHEAD, PacketizeError, Packetizer, RtpClock,
    Ssrc, TxStats, random_u32,
};
use lanplay_video_core::{AccessUnitSource, FixtureError, FixtureSource};

use crate::faults::{FaultConfig, FaultInjector, FaultStats};
use crate::pacing::{Pacer, PacerKind};
use crate::series::Series;
use crate::socket::{self, SocketBuffer};
use crate::wire::WireTimes;

/// FU-A adds an indicator and a header byte to every fragment; the arena is
/// sized as if every packet paid both, plus the RTP header.
const PER_PACKET_OVERHEAD: usize = HEADER_OVERHEAD + 2;

pub struct SendConfig {
    pub fixture: PathBuf,
    pub fps: f64,
    pub seconds: f64,
    pub mtu: usize,
    pub pacer: PacerKind,
    pub micro_window: Nanos,
    pub bitrate_mbps: f64,
    pub faults: FaultConfig,
    pub socket_send_buffer: Option<usize>,
}

#[derive(Debug)]
pub enum SendError {
    Io(io::Error),
    Fixture(FixtureError),
    Packetize(PacketizeError),
    EmptyFixture(PathBuf),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::Io(err) => write!(f, "{err}"),
            SendError::Fixture(err) => write!(f, "{err}"),
            SendError::Packetize(err) => write!(f, "packetisation failed: {err:?}"),
            SendError::EmptyFixture(path) => write!(f, "{} holds no access units", path.display()),
        }
    }
}

impl core::error::Error for SendError {}

impl From<io::Error> for SendError {
    fn from(err: io::Error) -> Self {
        SendError::Io(err)
    }
}

impl From<FixtureError> for SendError {
    fn from(err: FixtureError) -> Self {
        SendError::Fixture(err)
    }
}

pub struct SendReport {
    pub tx: TxStats,
    pub faults: FaultStats,
    /// Difference between when a datagram was scheduled and when `send_to`
    /// returned.
    pub pacing_error: Series,
    /// The `send_to` syscall itself.
    pub send_syscall: Series,
    /// First datagram of an access unit against that unit's release deadline.
    pub au_start_error: Series,
    /// How far behind the media clock the rate limiter ran. Always zero for
    /// the pacers that impose no rate.
    pub rate_backlog: Series,
    pub socket_buffer: SocketBuffer,
    /// Datagrams actually handed to `send_to`, faults included.
    pub datagrams: u64,
    pub datagram_bytes: u64,
    /// Sum of `EncodedAccessUnit::data` lengths, i.e. the bitstream itself.
    pub source_bytes: u64,
    pub largest_access_unit: usize,
    pub fixture_access_units: usize,
    pub elapsed: Nanos,
    pub ssrc: Ssrc,
}

impl SendReport {
    pub fn packets_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        self.datagrams as f64 / seconds
    }

    pub fn wire_megabits_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        self.datagram_bytes as f64 * 8.0 / seconds / 1e6
    }

    /// Bytes on the wire divided by bytes of bitstream: everything RTP, UDP
    /// and IP add for the privilege of carrying the frame.
    pub fn wire_overhead_ratio(&self) -> f64 {
        if self.source_bytes == 0 {
            return 0.0;
        }
        self.tx.bytes as f64 / self.source_bytes as f64
    }
}

pub fn run(
    socket: &UdpSocket,
    target: SocketAddr,
    config: &SendConfig,
    recorder: &Recorder,
    wire: Option<&WireTimes>,
) -> Result<SendReport, SendError> {
    let socket_buffer = socket::send_buffer(socket, config.socket_send_buffer)?;

    let mut source = FixtureSource::load(&config.fixture, config.fps.round().max(1.0) as u32)?;
    if source.access_unit_count() == 0 {
        return Err(SendError::EmptyFixture(config.fixture.clone()));
    }
    source.set_looping(true);

    let ssrc = Ssrc(random_u32());
    let clock = RtpClock::new(H264_CLOCK_RATE, random_u32());
    let mut packetizer = Packetizer::new(ssrc, clock, H264_PAYLOAD_TYPE, config.mtu);
    let mut pacer = Pacer::new(config.pacer, config.micro_window, config.bitrate_mbps);
    let mut faults = FaultInjector::new(config.faults, config.mtu);

    // One arena and one index, reused for every access unit. The packetiser
    // hands out a slice that dies on the next call, and all three pacers need
    // the packet count before the first datagram goes out, so the datagrams
    // are staged once and then transmitted on schedule.
    let (mut arena, mut index) = staging(&source, config.mtu);

    let mut report = SendReport {
        tx: TxStats::default(),
        faults: FaultStats::default(),
        pacing_error: Series::new("pacing error"),
        send_syscall: Series::new("send syscall"),
        au_start_error: Series::new("au start error"),
        rate_backlog: Series::new("rate backlog"),
        socket_buffer,
        datagrams: 0,
        datagram_bytes: 0,
        source_bytes: 0,
        largest_access_unit: source.largest_access_unit(),
        fixture_access_units: source.access_unit_count(),
        elapsed: Nanos::ZERO,
        ssrc,
    };

    let period_nanos = 1e9 / config.fps.max(f64::MIN_POSITIVE);
    let total = (config.seconds * config.fps).round().max(0.0) as u64;
    let start = Timestamp::now();
    let mut send_error: Option<io::Error> = None;

    for ordinal in 0..total {
        let deadline = start.add(Nanos((ordinal as f64 * period_nanos) as u64));
        let Some(unit) = source.next_access_unit() else {
            break;
        };

        wait_until(deadline);
        recorder.mark(unit.id, Stage::FrameCreated);

        // The rate limiter's admission wait happens before packetisation, not
        // between it and the first datagram. Otherwise `packetization` — which
        // is PacketizationStart -> NetworkSendFirst — would absorb it, and the
        // one segment that should cost the same under all three pacers would
        // be the one that differs most. It is reported instead as `au start
        // error` and `rate backlog`.
        let release = pacer.admit(deadline);
        wait_until(release);

        recorder.mark(unit.id, Stage::PacketizationStart);
        arena.clear();
        index.clear();
        let packetized = packetizer
            .packetize(&unit, |datagram| {
                index.push((arena.len() as u32, datagram.len() as u32));
                arena.extend_from_slice(datagram);
            })
            .map_err(SendError::Packetize)?;

        pacer.start_access_unit(release, packetized.packets);
        // `Transit` is first byte out to last byte in, so this is the entry to
        // the first `send_to`, not the moment the datagrams were ready.
        let mut first_send: Option<Timestamp> = None;
        let mut last_send = Timestamp::now();

        for &(offset, len) in &index {
            let scheduled = pacer.packet_deadline(len as usize);
            wait_until(scheduled);

            let mut latest: Option<(Timestamp, Timestamp)> = None;
            let datagram = &arena[offset as usize..offset as usize + len as usize];
            faults.offer(datagram, |bytes| {
                let before = Timestamp::now();
                let outcome = socket.send_to(bytes, target);
                let after = Timestamp::now();
                report.send_syscall.record(after.saturating_since(before));
                match outcome {
                    Ok(sent) => {
                        report.datagrams += 1;
                        report.datagram_bytes += sent as u64;
                    }
                    Err(err) => {
                        report.tx.send_errors += 1;
                        send_error.get_or_insert(err);
                    }
                }
                latest = Some((before, after));
            });

            if let Some((entered, at)) = latest {
                report.pacing_error.record(at.saturating_since(scheduled));
                last_send = at;
                if first_send.is_none() {
                    first_send = Some(entered);
                    report.au_start_error.record(at.saturating_since(deadline));
                    if let Some(wire) = wire {
                        wire.begin(unit.id, at);
                    }
                } else if let Some(wire) = wire {
                    wire.extend(unit.id, at);
                }
            }
        }

        if let Some(entered) = first_send {
            recorder.mark_at(unit.id, Stage::NetworkSendFirst, entered);
            recorder.mark_at(unit.id, Stage::NetworkSendLast, last_send);
        }
        if pacer.kind() == PacerKind::Rate {
            report.rate_backlog.record(pacer.backlog(last_send));
        }
        report.tx.record(&packetized);
        report.source_bytes += unit.data.len() as u64;
    }

    faults.flush(|bytes| {
        if let Ok(sent) = socket.send_to(bytes, target) {
            report.datagrams += 1;
            report.datagram_bytes += sent as u64;
        } else {
            report.tx.send_errors += 1;
        }
    });

    report.elapsed = Timestamp::now().saturating_since(start);
    report.faults = *faults.stats();

    match send_error {
        // A single failed datagram is a measurement, not a crash; the run is
        // still worth reporting, and `send_errors` says how bad it was.
        Some(err) if report.datagrams == 0 => Err(SendError::Io(err)),
        _ => Ok(report),
    }
}

/// Preallocates the staging arena from the fixture's largest access unit, so
/// the steady-state path never reallocates.
fn staging(source: &FixtureSource, mtu: usize) -> (Vec<u8>, Vec<(u32, u32)>) {
    let payload = mtu.saturating_sub(PER_PACKET_OVERHEAD).max(1);
    let packets = source.largest_access_unit().div_ceil(payload) + 2;
    (
        Vec::with_capacity(source.largest_access_unit() + packets * PER_PACKET_OVERHEAD),
        Vec::with_capacity(packets),
    )
}
