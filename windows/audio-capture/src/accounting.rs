//! Every frame accounted for, or said to be missing.
//!
//! `IAudioCaptureClient::GetBuffer` reports the device position of the first
//! frame of each packet, so consecutive packets carry an arithmetic identity:
//! the position of a packet must equal the position of the one before it plus
//! that one's frame count. Anything else is a hole whose size is known exactly,
//! which is a very different report from a count of packets that looked odd.
//! Counting packets can only ever say that something happened; this says how
//! much audio was not there.
//!
//! A position that moves backwards is counted apart from a hole. Both break the
//! identity, but a hole is audio the engine could not give and a rewind is a
//! position stream that cannot be trusted at all, and averaging the two into
//! one number would let the second hide inside the first.
//!
//! Discontinuity and silence are also kept apart, for the reason the flags are
//! separate in the first place: a glitch means the engine lost data, while
//! silence means the host was playing nothing. A run that conflated them would
//! answer the question this phase exists to ask with the wrong word.

/// What `GetBuffer` said about one packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Packet {
    /// Device position of the packet's first frame, in frames from the start
    /// of the stream.
    pub device_position: u64,
    pub frames: u32,
    /// The performance counter at the moment the endpoint recorded that first
    /// frame, in 100-nanosecond units, exactly as `GetBuffer` converts it.
    pub qpc_100ns: u64,
    pub discontinuity: bool,
    pub silent: bool,
    pub timestamp_error: bool,
}

/// How a packet's device position broke the identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deviation {
    /// Frames the stream skipped over.
    Gap(u64),
    /// Frames the position went backwards by.
    Rewind(u64),
}

/// The running account, and the totals it produces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub packets: u64,
    pub frames: u64,
    pub discontinuities: u64,
    /// Whether the very first packet carried the discontinuity flag.
    ///
    /// It usually does, and it usually means nothing: a loopback client
    /// attaches to an engine that has been running for a while, so the first
    /// packet genuinely does not continue anything. Kept apart from the rest
    /// so that a run reporting one glitch is not read as a stream that broke
    /// once when it is a stream that started.
    pub first_packet_discontinuous: bool,
    pub silent_packets: u64,
    pub timestamp_errors: u64,
    pub gaps: u64,
    pub gap_frames: u64,
    pub rewinds: u64,
    pub rewind_frames: u64,
    pub first_position: u64,
    pub last_position: u64,
    pub first_qpc_100ns: u64,
    pub last_qpc_100ns: u64,
}

impl Totals {
    /// Seconds between the first frame of the first packet and the first frame
    /// of the last one.
    ///
    /// Deliberately not a capture duration: it stops at the last packet's first
    /// frame, so it is short by that packet's length. Both ends are timestamps
    /// the endpoint produced, and mixing in a duration computed from a frame
    /// count would make it a hybrid of two clocks.
    pub fn qpc_span_seconds(&self) -> f64 {
        if self.packets < 2 {
            return 0.0;
        }
        self.last_qpc_100ns.saturating_sub(self.first_qpc_100ns) as f64 / 10_000_000.0
    }

    /// Glitches that cannot be explained by the stream having started
    /// mid-flight.
    pub fn discontinuities_in_flight(&self) -> u64 {
        self.discontinuities - u64::from(self.first_packet_discontinuous)
    }
}

#[derive(Debug, Default)]
pub struct Accounting {
    totals: Totals,
    expected_next: Option<u64>,
    started: bool,
}

impl Accounting {
    pub fn new() -> Self {
        Self::default()
    }

    /// Files one packet, reporting how it broke the position identity if it
    /// did.
    pub fn record(&mut self, packet: &Packet) -> Option<Deviation> {
        self.totals.packets += 1;
        self.totals.frames += u64::from(packet.frames);
        if packet.discontinuity {
            self.totals.discontinuities += 1;
        }
        if packet.silent {
            self.totals.silent_packets += 1;
        }
        if packet.timestamp_error {
            self.totals.timestamp_errors += 1;
        }

        if !self.started {
            self.started = true;
            self.totals.first_position = packet.device_position;
            self.totals.first_qpc_100ns = packet.qpc_100ns;
            self.totals.first_packet_discontinuous = packet.discontinuity;
        }
        self.totals.last_position = packet.device_position;
        self.totals.last_qpc_100ns = packet.qpc_100ns;

        let deviation = match self.expected_next {
            Some(expected) if packet.device_position > expected => {
                let missing = packet.device_position - expected;
                self.totals.gaps += 1;
                self.totals.gap_frames += missing;
                Some(Deviation::Gap(missing))
            }
            Some(expected) if packet.device_position < expected => {
                let backwards = expected - packet.device_position;
                self.totals.rewinds += 1;
                self.totals.rewind_frames += backwards;
                Some(Deviation::Rewind(backwards))
            }
            _ => None,
        };
        self.expected_next = Some(packet.device_position + u64::from(packet.frames));
        deviation
    }

    pub fn totals(&self) -> Totals {
        self.totals
    }
}

/// A fixed-capacity store of measurements, for distributions computed after
/// the run.
///
/// Fixed because the capture loop must not allocate: a `Vec` that grew inside
/// it would put a heap allocation on the path whose timing is the measurement.
/// Overflow is counted rather than silently dropped, so a distribution can
/// never quietly describe a prefix of the run while claiming to describe all
/// of it.
#[derive(Debug)]
pub struct Samples {
    values: Vec<u64>,
    dropped: u64,
}

impl Samples {
    pub fn with_capacity(capacity: usize) -> Self {
        Samples {
            values: Vec::with_capacity(capacity),
            dropped: 0,
        }
    }

    pub fn record(&mut self, value: u64) {
        if self.values.len() < self.values.capacity() {
            self.values.push(value);
        } else {
            self.dropped += 1;
        }
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Nearest-rank percentiles. Sorts in place, so it is for use after the
    /// stream has stopped.
    pub fn percentiles(&mut self) -> Option<Percentiles> {
        if self.values.is_empty() {
            return None;
        }
        self.values.sort_unstable();
        let rank = |q: f64| {
            let n = self.values.len();
            let index = ((q * n as f64).ceil() as usize).clamp(1, n) - 1;
            self.values[index]
        };
        Some(Percentiles {
            count: self.values.len(),
            min: self.values[0],
            p50: rank(0.50),
            p95: rank(0.95),
            p99: rank(0.99),
            max: self.values[self.values.len() - 1],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Percentiles {
    pub count: usize,
    pub min: u64,
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(device_position: u64, frames: u32) -> Packet {
        Packet {
            device_position,
            frames,
            qpc_100ns: 0,
            discontinuity: false,
            silent: false,
            timestamp_error: false,
        }
    }

    #[test]
    fn a_contiguous_stream_has_no_gaps() {
        let mut account = Accounting::new();
        let mut position = 1_000;
        for _ in 0..10 {
            assert_eq!(account.record(&packet(position, 480)), None);
            position += 480;
        }
        let totals = account.totals();
        assert_eq!(totals.packets, 10);
        assert_eq!(totals.frames, 4_800);
        assert_eq!(totals.gaps, 0);
        assert_eq!(totals.gap_frames, 0);
        assert_eq!(totals.first_position, 1_000);
        assert_eq!(totals.last_position, 1_000 + 9 * 480);
    }

    #[test]
    fn the_first_packet_is_never_a_gap() {
        // The stream may well start at a position far from zero, because the
        // engine has been running since something else opened it.
        let mut account = Accounting::new();
        assert_eq!(account.record(&packet(9_999_999, 480)), None);
        assert_eq!(account.totals().gaps, 0);
    }

    /// A loopback client joins an engine that has been running, so the packet
    /// it joins on is flagged discontinuous whatever the stream goes on to do.
    /// Reading that one flag as a glitch would fail every clean run.
    #[test]
    fn a_discontinuity_on_the_first_packet_is_not_a_glitch_in_flight() {
        let mut account = Accounting::new();
        account.record(&Packet {
            discontinuity: true,
            ..packet(106_560, 480)
        });
        for index in 1..5u64 {
            account.record(&packet(106_560 + index * 480, 480));
        }
        let totals = account.totals();
        assert!(totals.first_packet_discontinuous);
        assert_eq!(totals.discontinuities, 1);
        assert_eq!(totals.discontinuities_in_flight(), 0);
    }

    #[test]
    fn a_later_discontinuity_is_a_glitch_in_flight() {
        let mut account = Accounting::new();
        account.record(&packet(0, 480));
        account.record(&Packet {
            discontinuity: true,
            ..packet(480, 480)
        });
        let totals = account.totals();
        assert!(!totals.first_packet_discontinuous);
        assert_eq!(totals.discontinuities_in_flight(), 1);
    }

    #[test]
    fn a_hole_is_measured_in_frames() {
        let mut account = Accounting::new();
        account.record(&packet(0, 480));
        // 480 expected, 1_920 delivered: 1_440 frames of audio were not there.
        assert_eq!(
            account.record(&packet(1_920, 480)),
            Some(Deviation::Gap(1_440))
        );
        let totals = account.totals();
        assert_eq!(totals.gaps, 1);
        assert_eq!(totals.gap_frames, 1_440);
        assert_eq!(totals.frames, 960);
    }

    #[test]
    fn several_holes_accumulate() {
        let mut account = Accounting::new();
        account.record(&packet(0, 100));
        account.record(&packet(150, 100));
        account.record(&packet(250, 100));
        account.record(&packet(400, 100));
        let totals = account.totals();
        assert_eq!(totals.gaps, 2);
        assert_eq!(totals.gap_frames, 100);
        assert_eq!(totals.frames, 400);
    }

    #[test]
    fn a_position_going_backwards_is_not_a_hole() {
        let mut account = Accounting::new();
        account.record(&packet(1_000, 480));
        assert_eq!(
            account.record(&packet(1_000, 480)),
            Some(Deviation::Rewind(480))
        );
        let totals = account.totals();
        assert_eq!(totals.gaps, 0);
        assert_eq!(totals.gap_frames, 0);
        assert_eq!(totals.rewinds, 1);
        assert_eq!(totals.rewind_frames, 480);
    }

    #[test]
    fn silence_and_discontinuity_are_counted_apart() {
        let mut account = Accounting::new();
        account.record(&Packet {
            silent: true,
            ..packet(0, 480)
        });
        account.record(&Packet {
            discontinuity: true,
            ..packet(480, 480)
        });
        account.record(&Packet {
            silent: true,
            discontinuity: true,
            timestamp_error: true,
            ..packet(960, 480)
        });
        let totals = account.totals();
        assert_eq!(totals.silent_packets, 2);
        assert_eq!(totals.discontinuities, 2);
        assert_eq!(totals.timestamp_errors, 1);
    }

    #[test]
    fn the_span_is_between_the_first_frames_of_the_first_and_last_packets() {
        let mut account = Accounting::new();
        account.record(&Packet {
            qpc_100ns: 10_000_000,
            ..packet(0, 480)
        });
        account.record(&Packet {
            qpc_100ns: 20_000_000,
            ..packet(480, 480)
        });
        assert!((account.totals().qpc_span_seconds() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_single_packet_spans_nothing() {
        let mut account = Accounting::new();
        account.record(&Packet {
            qpc_100ns: 10_000_000,
            ..packet(0, 480)
        });
        assert_eq!(account.totals().qpc_span_seconds(), 0.0);
    }

    #[test]
    fn percentiles_are_nearest_rank() {
        let mut samples = Samples::with_capacity(100);
        for value in 1..=100u64 {
            samples.record(value);
        }
        let percentiles = samples.percentiles().expect("a hundred samples");
        assert_eq!(percentiles.count, 100);
        assert_eq!(percentiles.min, 1);
        assert_eq!(percentiles.p50, 50);
        assert_eq!(percentiles.p95, 95);
        assert_eq!(percentiles.p99, 99);
        assert_eq!(percentiles.max, 100);
    }

    #[test]
    fn samples_past_capacity_are_counted_not_taken() {
        let mut samples = Samples::with_capacity(2);
        samples.record(5);
        samples.record(6);
        samples.record(7);
        assert_eq!(samples.len(), 2);
        assert_eq!(samples.dropped(), 1);
        assert_eq!(samples.percentiles().expect("two samples").max, 6);
    }

    #[test]
    fn no_samples_means_no_distribution() {
        assert_eq!(Samples::with_capacity(4).percentiles(), None);
    }
}
