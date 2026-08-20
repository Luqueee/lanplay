//! The receive half: a few seconds of arrivals, counted where they arrive.
//!
//! The traffic is `net-bench send`'s, unchanged, because a probe whose traffic
//! does not look like the product's traffic measures the wrong link. What that
//! buys is stated rather than assumed: an access unit of 1080p video at 50 Mbps
//! is around forty datagrams handed to the kernel with no gap between them, and
//! the burst is the part a link fails at. A smooth stream of the same bitrate
//! never presents that burst to the air and comes back looking better than the
//! product ever will.
//!
//! Cadence comes out of `crates/link-metrics` and out of nothing else, so that a
//! figure from this probe and a figure from the running monitor are the same
//! quantity rather than two dialects of it. The two marks are placed exactly
//! where `macos/client/src/transport.rs` places them - `first_seen` on the
//! arrival that opened an access unit, `completed` on the arrival that finished
//! it, both on the arrival's own timestamp and never on a later `now()` - since
//! a stage measured through a later one is the defect that whole crate exists to
//! have retired.
//!
//! Per-datagram arrival percentiles are deliberately absent. The depacketiser's
//! own RFC 3550 jitter estimate is free and is reported; a second histogram of
//! arrival intervals here would be a second answer to "what was the cadence",
//! and the point of routing this through `link-metrics` is that there is one.
//!
//! Access units the sender produced come out of the frame id extension rather
//! than out of `fps` multiplied by the span. The sender states its own count in
//! band, one id per unit and contiguous, so the difference between the first and
//! the last id observed is exact where the multiplication is a rounding error
//! that would report one access unit lost on a run that lost nothing.

use std::io;
use std::net::UdpSocket;
use std::time::Duration;

use lanplay_link_metrics::{Delivery, Window};
use lanplay_telemetry::{Nanos, Timestamp};
use lanplay_transport::{
    Depacketizer, DepacketizerConfig, H264_PAYLOAD_TYPE, HEADER_OVERHEAD, RxStats, parse_packet,
};

/// One allocation for the whole probe, large enough for any datagram a
/// conforming sender can emit.
const RECV_BUFFER: usize = 65_536;

/// The depacketiser configuration `tools/net-bench` receives with. Identical on
/// purpose: a preflight figure measured behind a different reorder window is not
/// comparable with the arms already committed under `results/`.
const REORDER_WINDOW: usize = 256;
const MAX_ACCESS_UNIT_BYTES: usize = 4 * 1024 * 1024;

/// How long a blocked `recv_from` waits before the loop looks at its clocks.
/// A shutdown affordance only: it adds nothing to a datagram that has arrived.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

/// Silence after traffic started that ends the probe early. The sender is a
/// separate process on another machine and has no way to say it finished.
///
/// Shorter than the three seconds `tools/net-bench` waits, because a probe of
/// five cannot spend three of them deciding the sender is gone, and far longer
/// than any gap this link has been seen to produce: the worst complete-interval
/// across the four committed 120 s arms on this channel is 98.57 ms, and the
/// longest hold this gate's fault relay is told to apply is 120 ms. A run that
/// does end here reports the span it actually observed beside the span it asked
/// for, so a truncated arm is visible rather than quietly short.
const IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the probe's few seconds of arrivals were.
pub struct Measurement {
    /// The depacketiser's own sequence accounting, which is where loss comes
    /// from. Differencing two ends would need the sender's count, and one arm
    /// of `tools/audio-rtp-gate.sh` recorded what that costs: 2000 sent, 1740
    /// seen, no sequence gap at all, because the head of the stream left before
    /// the receiving socket was listening.
    pub rx: RxStats,
    pub jitter: Nanos,
    pub datagrams: u64,
    pub datagram_bytes: u64,
    /// Access units whose reassembled bytes would have fitted in one datagram.
    ///
    /// The shape assertion, and it is a count rather than a mean because the
    /// failure it catches is qualitative: a probe pointed at a tone, at a
    /// keepalive or at a synthetic trickle measures a link this product never
    /// presents to the air, and every number beside it would then be about the
    /// wrong traffic while looking perfectly reasonable.
    pub under_one_datagram: u64,
    /// The delivery tier over the whole probe, from `crates/link-metrics`.
    pub window: Window,
    /// First and last frame id of a *completed* access unit, which is how many
    /// units the sender produced over the interval this probe closed. `None`
    /// from a sender that emits no frame id extension, and then the count is
    /// absent rather than guessed.
    ///
    /// Completed rather than merely seen, and the difference is not pedantry: a
    /// probe stops in the middle of an access unit, so the newest id it saw is
    /// one it was never going to finish. The loopback self-test that found this
    /// reported 600 of 601 units on a run that lost nothing at all. Bounding the
    /// range by two units that did arrive makes every id inside it one that
    /// should have, and the count exact.
    pub completed_ids: Option<(u64, u64)>,
    /// First arrival to last arrival, which is what the counts are rates over.
    pub elapsed: Nanos,
    pub recv_error: Option<String>,
}

impl Measurement {
    /// Datagrams the sequence machine could account for: those it accepted plus
    /// the gaps it saw. The honest population under a loss count, and stated
    /// separately from the access units expected because those are a different
    /// quantity - `macos/client/src/report.rs` puts `packet_loss` (datagrams)
    /// over `expected` (access units), and the ratio of the two is not a loss.
    pub fn datagrams_accounted(&self) -> u64 {
        self.rx.packets + self.rx.lost
    }

    /// Access units the sender produced between the first and the last one that
    /// arrived, from its own contiguous ids.
    pub fn access_units_expected(&self) -> Option<u64> {
        self.completed_ids.map(|(first, last)| last - first + 1)
    }

    pub fn access_units_lost(&self) -> Option<u64> {
        self.access_units_expected()
            .map(|expected| expected.saturating_sub(self.window.delivered))
    }

    pub fn megabits_per_second(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return 0.0;
        }
        self.datagram_bytes as f64 * 8.0 / 1e6 / seconds
    }

    pub fn datagrams_per_access_unit(&self) -> f64 {
        if self.window.delivered == 0 {
            return 0.0;
        }
        self.datagrams as f64 / self.window.delivered as f64
    }

    pub fn mean_datagram_bytes(&self) -> f64 {
        if self.datagrams == 0 {
            return 0.0;
        }
        self.datagram_bytes as f64 / self.datagrams as f64
    }
}

/// A probe that measured something, or the reason there is nothing to report.
///
/// Two variants rather than a `Measurement` with zeros in it, for the reason
/// this whole harness exists to guard: zero datagrams lost out of zero sent is
/// the most common way an instrument here has lied, and a number that is never
/// written down cannot be quoted back.
// One of these is returned once, at the end of a run, and never held in a
// collection or passed on a hot path, so boxing the larger variant would buy a
// heap allocation and cost the reader an indirection.
#[allow(clippy::large_enum_variant)]
pub enum Outcome {
    Measured(Measurement),
    /// `datagrams` is kept even here, because "nothing arrived at all" and
    /// "datagrams arrived and none of them formed an access unit" are different
    /// faults, on different machines, with different next steps.
    Nothing {
        why: String,
        datagrams: u64,
    },
}

pub struct ProbeConfig {
    /// The probe's own length. Three to five seconds: long enough that a 120 Hz
    /// stream closes several hundred intervals, short enough that a user is not
    /// waiting on it.
    pub seconds: f64,
    /// The rate the sender was told to pace at, which is what every threshold
    /// in `crates/link-metrics` is a multiple of. Nothing in a received stream
    /// states it, so it is an argument and it is recorded as one.
    pub fps: f64,
    /// The sender's datagram budget, which fixes what "would have fitted in one
    /// datagram" means.
    pub mtu: usize,
    /// How long to wait for the first datagram before giving up. Separate from
    /// the probe's own length so that a host which never sent is refused for
    /// that reason rather than reported as a silence of the probe's length.
    pub wait: Duration,
}

pub fn run(socket: &UdpSocket, config: &ProbeConfig) -> io::Result<Outcome> {
    socket.set_read_timeout(Some(POLL_TIMEOUT))?;

    let period = Nanos::from_millis_f64(1000.0 / config.fps.max(1.0));
    let delivery = Delivery::new(period);
    let mut depacketizer = Depacketizer::new(DepacketizerConfig {
        payload_type: H264_PAYLOAD_TYPE,
        reorder_window: REORDER_WINDOW,
        max_access_unit_bytes: MAX_ACCESS_UNIT_BYTES,
    });

    let mut buffer = vec![0u8; RECV_BUFFER];
    let mut datagrams = 0u64;
    let mut datagram_bytes = 0u64;
    let mut under_one_datagram = 0u64;
    let mut recv_error = None;

    // An access unit that would have fitted in one datagram. The RTP header and
    // its extension come out of the datagram budget rather than sitting on top
    // of it, so the payload a sender can carry is the budget less the overhead.
    let one_datagram = config.mtu.saturating_sub(HEADER_OVERHEAD);

    let mut first_arrival: Option<Timestamp> = None;
    let mut last_arrival = Timestamp::now();
    let started_waiting = Timestamp::now();
    // Only an advancing frame id opens an access unit. A straggler from an
    // older one would otherwise reopen a unit the delivery series has already
    // closed, and on a link that reorders that is a second `first_seen` for a
    // unit that only started once.
    let mut newest_seen: Option<u64> = None;
    // The ids of the first and the last unit that actually completed, which is
    // the range every id inside should have arrived in.
    let mut completed_ids: Option<(u64, u64)> = None;

    loop {
        match socket.recv_from(&mut buffer) {
            Ok((len, _)) => {
                let at = Timestamp::now();
                datagrams += 1;
                datagram_bytes += len as u64;
                last_arrival = at;
                let started = *first_arrival.get_or_insert(at);

                let datagram = &buffer[..len];
                // The depacketiser does not expose per-packet frame ids and the
                // opening mark needs one before reassembly finishes, so the
                // header is read again. A handful of loads, off any deadline.
                let frame = parse_packet(datagram)
                    .ok()
                    .and_then(|packet| packet.header.frame_id)
                    .map(|frame| frame.get());
                if let Some(frame) = frame
                    && newest_seen.is_none_or(|seen| frame > seen)
                {
                    newest_seen = Some(frame);
                    delivery.first_seen(at);
                }

                if let Some(unit) = depacketizer.push(datagram, at) {
                    // Marked at the arrival that completed the unit rather than
                    // at a `now()` taken afterwards, which is what the client
                    // does: reassembly is bookkeeping and belongs to neither the
                    // link nor the stage after it.
                    delivery.completed(at);
                    if unit.data.len() <= one_datagram {
                        under_one_datagram += 1;
                    }
                    if !unit.id.is_none() {
                        let id = unit.id.get();
                        completed_ids = Some(match completed_ids {
                            Some((first, last)) => (first.min(id), last.max(id)),
                            None => (id, id),
                        });
                    }
                }

                if at.saturating_since(started).as_secs_f64() >= config.seconds.max(0.0) {
                    break;
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                let now = Timestamp::now();
                match first_arrival {
                    Some(_) if now.saturating_since(last_arrival).as_duration() >= IDLE_TIMEOUT => {
                        break;
                    }
                    None if now.saturating_since(started_waiting).as_duration() >= config.wait => {
                        return Ok(Outcome::Nothing {
                            why: format!(
                                "no datagram arrived in {:.0} s of waiting, so nothing here says \
                                 anything about the link: a loss of zero over a population of \
                                 zero is the shape of an instrument that was never presented \
                                 with any traffic",
                                config.wait.as_secs_f64()
                            ),
                            datagrams: 0,
                        });
                    }
                    _ => {}
                }
            }
            Err(err) => {
                recv_error = Some(err.to_string());
                break;
            }
        }
    }

    let window = delivery.cumulative();
    // Two marks close one interval; one mark closes none, and every percentile
    // of a histogram nobody recorded into is zero - which reads as a link
    // delivering exactly on time. So a run this short has no cadence to state
    // and says so, rather than stating a cadence of zeros.
    if window.delivered < 2 {
        return Ok(Outcome::Nothing {
            why: format!(
                "{} access units completed out of {datagrams} datagrams, and two are needed to \
                 close one interval, so this run has no cadence at all: a delivery window built \
                 from fewer would answer every percentile with zero, which is what a flawless \
                 link looks like",
                window.delivered
            ),
            datagrams,
        });
    }

    Ok(Outcome::Measured(Measurement {
        rx: *depacketizer.stats(),
        jitter: depacketizer.jitter(),
        datagrams,
        datagram_bytes,
        under_one_datagram,
        window,
        completed_ids,
        elapsed: match first_arrival {
            Some(first) => last_arrival.saturating_since(first),
            None => Nanos::ZERO,
        },
        recv_error,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measurement(completed_ids: Option<(u64, u64)>, delivered: u64) -> Measurement {
        Measurement {
            rx: RxStats::default(),
            jitter: Nanos::ZERO,
            datagrams: 0,
            datagram_bytes: 0,
            under_one_datagram: 0,
            window: Window {
                delivered,
                ..Default::default()
            },
            completed_ids,
            elapsed: Nanos::ZERO,
            recv_error: None,
        }
    }

    /// The boundary the loopback self-test found: it reported 600 of 601 access
    /// units on a run that lost nothing, because the probe stopped in the middle
    /// of the 601st. A range bounded by two units that arrived cannot do that,
    /// and an off-by-one in a loss counter is the kind of number nobody queries.
    #[test]
    fn a_closed_range_of_arrivals_loses_nothing() {
        let clean = measurement(Some((41, 640)), 600);
        assert_eq!(clean.access_units_expected(), Some(600));
        assert_eq!(clean.access_units_lost(), Some(0));
    }

    #[test]
    fn units_missing_inside_the_range_are_counted() {
        let holed = measurement(Some((41, 640)), 598);
        assert_eq!(holed.access_units_expected(), Some(600));
        assert_eq!(holed.access_units_lost(), Some(2));
    }

    /// A sender that states no frame id never said how many units it sent, and
    /// a zero here would be a loss figure invented out of that silence.
    #[test]
    fn a_sender_that_stated_no_id_yields_no_count() {
        let anonymous = measurement(None, 600);
        assert_eq!(anonymous.access_units_expected(), None);
        assert_eq!(anonymous.access_units_lost(), None);
    }
}
