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

#[derive(Debug)]
pub struct Accounting {
    totals: Totals,
    /// The endpoint's own rate, from the position and the counter every packet
    /// carries together. Here rather than beside it because the pair this reads
    /// is the pair the identity above is checked with, and a second reader of
    /// the same two fields is a second chance to pair them wrongly.
    drift: Drift,
    expected_next: Option<u64>,
    started: bool,
}

impl Accounting {
    /// `nominal_hz` is the rate the endpoint claims. The drift is measured
    /// against it rather than fitted from the data, which is what keeps the
    /// residual small enough to be summed exactly; see [`Drift`].
    pub fn new(nominal_hz: f64) -> Self {
        Accounting {
            totals: Totals::default(),
            drift: Drift::new(nominal_hz),
            expected_next: None,
            started: false,
        }
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

        // A packet the engine flagged is a packet whose performance counter it
        // will not stand behind, so it is left out of the rate rather than
        // averaged into it. A gap is not excluded and must not be: a gap is
        // audio the engine could not deliver, and the position it resumes at is
        // still the device's own count of its own frames at the counter it
        // reports for them, so the pair stays true straight across one.
        if !packet.timestamp_error {
            self.drift
                .record(packet.device_position as f64, packet.qpc_100ns * 100);
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

    /// The endpoint's rate against nominal, as its own position and counter
    /// state it.
    pub fn drift(&self) -> Drift {
        self.drift
    }
}

/// A rate against nominal, and what the window it was taken over is worth.
///
/// Parts per million throughout, because that is the size of the thing: two
/// nominally identical 48000 Hz crystals differ by tens of ppm, which is
/// milliseconds of accumulated audio over minutes, and no coarser unit can
/// express it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rate {
    /// Readings the figures below rest on. A rate over two readings is a rate
    /// with no error bar, which is why this is stated beside every figure.
    pub readings: u64,
    /// Between the first and last readings, on the clock that took them.
    pub seconds: f64,
    /// Sample positions the device advanced over that span.
    pub samples: f64,
    /// From the first and last readings alone, which is the quantity A7.1
    /// names, and which carries the timestamp noise of exactly two readings.
    pub endpoints_ppm: f64,
    /// From a least-squares line through every reading, which carries the noise
    /// of all of them and is therefore the better estimate of the same slope.
    pub fitted_ppm: f64,
    /// The standard error of that slope. This is the window's resolution, and
    /// it is the number that says whether the window was long enough: a rate
    /// quoted without it cannot be told from the scatter it was drawn out of.
    pub error_ppm: f64,
    /// Scatter of the readings about the line, in samples of the nominal rate.
    ///
    /// Reported because it is what an endpoints-only estimate is exposed to,
    /// and the two estimates disagreeing by more than this over the window is
    /// the shape of a timestamp nobody should have trusted.
    pub scatter_samples: f64,
}

impl Rate {
    /// Whether the two estimates agree inside what the scatter allows.
    ///
    /// The endpoints estimate spends the scatter of two readings across the
    /// whole span, so its own error is about `sqrt(2)` scatters over the span;
    /// three of those is the band an honest instrument stays inside. A run
    /// where it does not has a timestamp stream with structure in it, and the
    /// fitted figure is then the only one of the two worth reading.
    pub fn estimates_agree(&self) -> bool {
        if self.seconds <= 0.0 || self.readings < 3 {
            return false;
        }
        let allowed = 3.0 * self.scatter_samples * core::f64::consts::SQRT_2 / self.seconds;
        let nominal = self.samples / self.seconds;
        (self.endpoints_ppm - self.fitted_ppm).abs() <= allowed / nominal * 1e6
    }
}

/// A clock's rate, accumulated from the pairs of sample position and timestamp
/// that one endpoint reports together.
///
/// The pairing is the whole of the instrument. `IAudioCaptureClient::GetBuffer`
/// reports a packet's device position and a performance counter for the same
/// frame, and a CoreAudio IO cycle's timestamp carries `mSampleTime` and
/// `mHostTime` for the same cycle, so every reading is two views of one instant
/// on one machine. Nothing here ever subtracts a timestamp taken on one machine
/// from one taken on another, and it cannot: it never sees a second machine.
///
/// Two estimates come out, and the second is what makes the first readable. The
/// endpoints alone give the rate directly and carry the timestamp noise of
/// exactly two readings, which over a ten-minute window is 1.7 ppm for every
/// millisecond of scatter -- enough to swallow the twenty ppm this phase is
/// arguing about. A least-squares line through every reading gives the same
/// slope with a standard error, and that error is the only thing that says
/// whether the window was long enough. Neither is knowable from a pair of
/// readings, which is why both are kept.
///
/// The sums are taken on the residual against nominal rather than on the
/// position itself, and the reason is arithmetic rather than taste. A position
/// squared reaches 1e20 over ten minutes at 48 kHz, where a double resolves to
/// ten thousand, and the residual sum of squares this needs is a few million:
/// computed as a difference of two 1e20 terms it is gone entirely. Against
/// nominal the residual stays in the hundreds and every sum is exact to a part
/// in 1e13.
///
/// Nothing here allocates, branches on data it has not been handed, or reads a
/// clock of its own. It is five multiply-adds per reading, which is what lets
/// it sit inside a render callback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drift {
    nominal_hz: f64,
    /// The first reading. Every later one is expressed against it, so that a
    /// count of nanoseconds since an arbitrary epoch never reaches a double at
    /// full width.
    anchor: Option<(f64, u64)>,
    last_seconds: f64,
    last_residual: f64,
    readings: u64,
    /// Readings whose timestamp did not advance on the one before it.
    ///
    /// Counted and excluded rather than admitted, because a repeated timestamp
    /// is a clock that did not tick and not a rate of infinity, and a reader
    /// who cannot see how many there were cannot tell a clean stream from one
    /// this quietly thinned out.
    stalled: u64,
    s_t: f64,
    s_r: f64,
    s_tt: f64,
    s_tr: f64,
    s_rr: f64,
}

impl Drift {
    pub fn new(nominal_hz: f64) -> Drift {
        Drift {
            nominal_hz,
            anchor: None,
            last_seconds: 0.0,
            last_residual: 0.0,
            readings: 0,
            stalled: 0,
            s_t: 0.0,
            s_r: 0.0,
            s_tt: 0.0,
            s_tr: 0.0,
            s_rr: 0.0,
        }
    }

    /// Files one reading: a sample position and the timestamp the same endpoint
    /// reported for it, in nanoseconds on that machine's own monotonic clock.
    pub fn record(&mut self, position: f64, nanos: u64) {
        let Some((first_position, first_nanos)) = self.anchor else {
            self.anchor = Some((position, nanos));
            self.readings = 1;
            return;
        };
        // Subtracted as integers and only then widened, which is what keeps the
        // full resolution of a counter whose absolute value is meaningless.
        let seconds = (nanos.saturating_sub(first_nanos)) as f64 / 1e9;
        // One test and not two: a reading at or behind the anchor gives an
        // elapsed interval of zero, which is at or behind the last one, so the
        // anchor needs no separate case.
        if seconds <= self.last_seconds {
            self.stalled += 1;
            return;
        }
        let residual = (position - first_position) - self.nominal_hz * seconds;

        self.readings += 1;
        self.last_seconds = seconds;
        self.last_residual = residual;
        self.s_t += seconds;
        self.s_r += residual;
        self.s_tt += seconds * seconds;
        self.s_tr += seconds * residual;
        self.s_rr += residual * residual;
    }

    pub fn readings(&self) -> u64 {
        self.readings
    }

    pub fn stalled(&self) -> u64 {
        self.stalled
    }

    /// The rate, or nothing when there is not enough to state one.
    ///
    /// Three readings, because a line through two has no residual and would
    /// report a standard error of zero -- a criterion that cannot fail, and the
    /// one shape of answer this project treats as worse than none.
    pub fn rate(&self) -> Option<Rate> {
        if self.readings < 3 || self.last_seconds <= 0.0 || self.nominal_hz <= 0.0 {
            return None;
        }
        // The anchor is reading one and contributes (0, 0) to every sum, so it
        // is in the population without being in the accumulators.
        let n = self.readings as f64;
        let s_tt = self.s_tt - self.s_t * self.s_t / n;
        let s_tr = self.s_tr - self.s_t * self.s_r / n;
        let s_rr = self.s_rr - self.s_r * self.s_r / n;
        if s_tt <= 0.0 {
            return None;
        }

        let slope = s_tr / s_tt;
        // Clamped at zero rather than allowed negative: the residual sum of
        // squares is a sum of squares, and a rounding error that took it below
        // zero would come back as a NaN standard error rather than as a small
        // one.
        let residual_ss = (s_rr - slope * s_tr).max(0.0);
        let variance = residual_ss / (n - 2.0);
        let per_million = 1e6 / self.nominal_hz;

        Some(Rate {
            readings: self.readings,
            seconds: self.last_seconds,
            samples: self.last_residual + self.nominal_hz * self.last_seconds,
            endpoints_ppm: self.last_residual / self.last_seconds * per_million,
            fitted_ppm: slope * per_million,
            error_ppm: (variance / s_tt).sqrt() * per_million,
            scatter_samples: variance.sqrt(),
        })
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
        assert_eq!(account.record(&packet(9_999_999, 480)), None);
        assert_eq!(account.totals().gaps, 0);
    }

    /// A loopback client joins an engine that has been running, so the packet
    /// it joins on is flagged discontinuous whatever the stream goes on to do.
    /// Reading that one flag as a glitch would fail every clean run.
    #[test]
    fn a_discontinuity_on_the_first_packet_is_not_a_glitch_in_flight() {
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
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
        let mut account = Accounting::new(48_000.0);
        account.record(&Packet {
            qpc_100ns: 10_000_000,
            ..packet(0, 480)
        });
        assert_eq!(account.totals().qpc_span_seconds(), 0.0);
    }

    /// A device running at `ppm` against nominal, sampled every `packet_frames`
    /// frames for `seconds`, with `jitter_ns` of a deterministic sawtooth on
    /// every timestamp so that a residual exists to be measured.
    fn drifting(ppm: f64, seconds: f64, packet_frames: u64, jitter_ns: i64) -> Drift {
        let mut drift = Drift::new(48_000.0);
        let rate = 48_000.0 * (1.0 + ppm / 1e6);
        let packets = (seconds * rate / packet_frames as f64) as u64;
        for index in 0..packets {
            let position = index * packet_frames;
            let ideal = position as f64 / rate * 1e9;
            // A sawtooth rather than a random draw: a test whose scatter comes
            // from an unseeded generator fails once a month and teaches its
            // reader to re-run it.
            let wobble = jitter_ns * (index as i64 % 5 - 2);
            drift.record(position as f64, (ideal as i64 + wobble).max(0) as u64);
        }
        drift
    }

    #[test]
    fn a_device_running_fast_reads_positive_parts_per_million() {
        let rate = drifting(20.0, 600.0, 480, 0).rate().expect("many readings");
        assert!(
            (rate.fitted_ppm - 20.0).abs() < 0.05,
            "fitted {} ppm",
            rate.fitted_ppm
        );
        assert!(
            (rate.endpoints_ppm - 20.0).abs() < 0.05,
            "endpoints {} ppm",
            rate.endpoints_ppm
        );
    }

    #[test]
    fn a_device_running_slow_reads_negative_parts_per_million() {
        let rate = drifting(-15.0, 600.0, 480, 0).rate().expect("many readings");
        assert!(
            (rate.fitted_ppm + 15.0).abs() < 0.05,
            "fitted {} ppm",
            rate.fitted_ppm
        );
    }

    /// The claim the arm length rests on: scatter on the timestamps degrades the
    /// resolution, and the resolution is what the standard error reports. A
    /// millisecond of scatter over ten minutes cannot resolve twenty ppm.
    #[test]
    fn the_standard_error_grows_with_the_scatter_on_the_timestamps() {
        let clean = drifting(20.0, 600.0, 480, 1_000)
            .rate()
            .expect("many readings");
        let noisy = drifting(20.0, 600.0, 480, 1_000_000)
            .rate()
            .expect("many readings");
        assert!(
            noisy.error_ppm > clean.error_ppm * 100.0,
            "clean {} ppm, noisy {} ppm",
            clean.error_ppm,
            noisy.error_ppm
        );
        assert!(
            noisy.scatter_samples > clean.scatter_samples * 100.0,
            "clean {} samples, noisy {} samples",
            clean.scatter_samples,
            noisy.scatter_samples
        );
    }

    /// And the other half of the same claim: a window too short to resolve the
    /// rate says so in its own error bar rather than reporting a tidy number.
    #[test]
    fn a_shorter_window_resolves_the_same_rate_worse() {
        let long = drifting(20.0, 600.0, 480, 100_000)
            .rate()
            .expect("many readings");
        let short = drifting(20.0, 60.0, 480, 100_000)
            .rate()
            .expect("many readings");
        assert!(
            short.error_ppm > long.error_ppm * 5.0,
            "long {} ppm, short {} ppm",
            long.error_ppm,
            short.error_ppm
        );
    }

    #[test]
    fn two_readings_state_no_rate_because_a_line_through_them_has_no_residual() {
        let mut drift = Drift::new(48_000.0);
        drift.record(0.0, 0);
        drift.record(48_000.0, 1_000_000_000);
        assert_eq!(drift.readings(), 2);
        assert_eq!(drift.rate(), None);
    }

    #[test]
    fn a_timestamp_that_did_not_advance_is_counted_and_left_out() {
        let mut drift = Drift::new(48_000.0);
        for index in 0..10u64 {
            drift.record((index * 480) as f64, index * 10_000_000);
        }
        let clean = drift.readings();
        drift.record(4_800.0, 90_000_000);
        drift.record(4_800.0, 50_000_000);
        assert_eq!(drift.readings(), clean);
        assert_eq!(drift.stalled(), 2);
    }

    /// A gap is audio the engine could not deliver and not a clock that moved,
    /// so a stream full of holes reports the same rate as one without them.
    #[test]
    fn a_hole_in_the_delivery_does_not_move_the_rate() {
        let mut whole = Drift::new(48_000.0);
        let mut holed = Drift::new(48_000.0);
        let rate = 48_000.0 * (1.0 + 20.0 / 1e6);
        for index in 0..12_000u64 {
            let position = index * 480;
            let nanos = (position as f64 / rate * 1e9) as u64;
            whole.record(position as f64, nanos);
            // Every seventh packet never arrives, so the position jumps by 960
            // and the counter jumps with it.
            if index % 7 != 0 {
                holed.record(position as f64, nanos);
            }
        }
        let whole = whole.rate().expect("many readings");
        let holed = holed.rate().expect("many readings");
        assert!(
            (whole.fitted_ppm - holed.fitted_ppm).abs() < 0.01,
            "whole {} ppm, holed {} ppm",
            whole.fitted_ppm,
            holed.fitted_ppm
        );
    }

    #[test]
    fn a_packet_whose_timestamp_the_engine_flagged_is_left_out_of_the_rate() {
        let mut account = Accounting::new(48_000.0);
        for index in 0..10u64 {
            account.record(&Packet {
                qpc_100ns: index * 100_000,
                timestamp_error: index == 5,
                ..packet(index * 480, 480)
            });
        }
        assert_eq!(account.drift().readings(), 9);
        assert_eq!(account.totals().packets, 10);
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
