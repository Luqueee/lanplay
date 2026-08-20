//! A8.1: one population that answers every jitter target at once, and the
//! cluster structure a survival curve cannot express.
//!
//! # Why the sweep had to be replaced by this
//!
//! A8 measured four candidate targets three times each and could not rank them.
//! The reason is measured rather than suspected: the target is fixed when the
//! buffer is built, so each candidate is its own arm minutes away from the
//! others, and the between-arm variance of this link's heavy tail is larger than
//! the 5 ms step being resolved. Ten arms on the steadiest link this project has
//! ever recorded - channel 36 at 80 MHz, every arm negotiating a median
//! 1200 Mbps, the effective margins ascending correctly in every pass - produced
//! concealment ratios from 0.196 to 7.442 per cent, with worst arrivals of 78,
//! 61, 76, 221, 24, 79, 19 and 91 ms across arms - and 0 render underruns in
//! 202431 callbacks, so the device was fed on every one of them and what varied
//! was fidelity rather than playout. What separated the arms was how many bursts
//! landed inside each 120 second window, not what target each was configured
//! with.
//!
//! No arrangement of separate arms survives that, and a longer arm does not fix
//! it: the anchor offset and the burst incidence are properties of an arm, so
//! they do not average out inside one. What does survive is refusing to run
//! separate arms at all.
//!
//! # The primitive
//!
//! For each admitted datagram, the excess delay above the run's best case:
//!
//! ```text
//! excess_i = (arrival_i - arrival_ref) - (rtp_i - rtp_ref) / 48000
//! ```
//!
//! It names no target, so `late(T)` is exactly `excess_i > T` for every
//! candidate `T` at once and one population decides the whole curve. The
//! receiver already computes this quantity: its arrival delay is
//! `arrival_i - playout_anchor - (rtp_i - rtp_anchor)/rate`, and the playout
//! anchor is a constant for the run, so excess is that same number with the
//! constant subtracted back off. There is no second timing path here and there
//! must not be one - two subtractions of the same two clocks are two chances to
//! disagree.
//!
//! ## The reference is the minimum over the run, not the first packet
//!
//! The first admitted packet anchors every playout deadline in a run, so a first
//! packet that landed badly shifts the entire distribution by a constant.
//! Measured on one link on one evening: a 90 s arm read p50 -10.9 ms and a
//! 1200 s arm -36.3 ms, a 25 ms difference that is entirely the two anchors and
//! nothing to do with either link. Referred to the minimum instead, the fastest
//! observed path defines zero and every excess is non-negative queueing delay
//! above best case - a quantity two runs can be compared on.
//!
//! ## The drift is removed, and it is not negligible
//!
//! `arrival` is on this Mac's monotonic clock and `rtp` is the far machine's
//! audio clock, so their difference accumulates the rate error between them. A7
//! measured this pair at +9.29 ppm referred to the Mac's timebase, closing to
//! +238 samples predicted against +238 +-75 observed. Over a 600 s run that is
//! 5.6 ms of accumulated skew, which is larger than the 5 ms spacing between the
//! targets the curve exists to separate: uncorrected, the curve would rank the
//! position of a frame in the run.
//!
//! So the drift is fitted from the run itself and the uncorrected curve is
//! reported beside the corrected one, because a correction whose size nobody can
//! see is a correction nobody can check.
//!
//! ## The fit is a line through per-block minima, not through every point
//!
//! Ordinary least squares over every arrival is the wrong estimator for this
//! variable and the arithmetic says by how much. With `n` arrivals spread over a
//! span `S`, one outlier at the end of the run moves the slope by
//! `(y - y_hat) * (S/2) / (n * S^2 / 12)`. For A8's measured worst arrival of
//! 221 ms in a 120 s arm of 24000 datagrams that is 0.4 ppm, which is tolerable.
//! A hundred such arrivals in one burst is 80 ppm against the 9.29 being measured,
//! and the negative control below injects far more than that on purpose. An
//! estimator the negative control destroys cannot be used to judge the negative
//! control.
//!
//! The minimum of each time block is queue-free by construction: it is the
//! fastest path the link offered in that block, and queueing only ever adds. A
//! line through those minima measures the clock difference and nothing else, and
//! no burst can move it, because a burst raises delays and a minimum ignores
//! everything it did not lower. The all-points slope is reported beside it so
//! the difference between the two estimators is a number in the document rather
//! than an assertion in a comment.
//!
//! # Clusters, because the events are not independent
//!
//! A survival curve alone is not enough, and the failure is audible rather than
//! statistical: 100 isolated late frames and 20 bursts of five have identical
//! late ratios and sound nothing alike. The first is 100 concealments spread
//! through a run and the second is 20 gaps of 25 ms, which is where a listener
//! stops hearing audio and starts hearing the concealer.
//!
//! At each threshold a cluster is a maximal run of consecutive late frames in
//! sequence order, closed by one on-time frame. Sequence order and not arrival
//! order: what a listener loses is a span of the timeline, and the timeline is
//! what the RTP timestamps state.
//!
//! Which is also why the spread of a rate quoted here comes from time blocks and
//! never from a binomial over frames. The frames inside a cluster are one event,
//! so a binomial over 240000 frames would report a precision the measurement
//! does not have, by a factor of the mean cluster size.
//!
//! # The population is bimodal by construction, and a reader has to be told
//!
//! This sender packs two Opus frames into one captured packet, so both members
//! of a pair arrive at one instant while the second sits one frame later in
//! stream time. Excess subtracts stream time, so `excess(second) =
//! excess(first) - 5 ms` exactly, and the population has two modes one frame
//! apart with the first frames sitting on top.
//!
//! A6.1 measured that from the other side this session and the two derivations
//! close. It took the per-pair difference directly: `lateness(second) -
//! lateness(first)` came to -4.996 ms at p50, with 96 per cent of pairs inside
//! the [-5,-4) ms bucket, over 8998, 9000 and 120004 pairs across three arms. It
//! also found which member goes late in practice, and it is the first: 524
//! against 384, 476 against 354, and 8594 against 6391. Two instruments, one
//! conclusion, and the check belongs written down here rather than inferred,
//! because the sign convention above this comment was wrong once already.
//!
//! What that authorises is a floor and nothing else. A target below the pair
//! spacing cannot hold both members of a pair, so 5 ms is structurally
//! unreachable on this sender for a reason that has nothing to do with the air,
//! and the curve's 5 ms row will read near half the population with clusters of
//! one frame separated by gaps of one frame. That alternation is the signature
//! and it is what tells a reader the row is cadence rather than a broken link.
//!
//! What it does not authorise is a change to the sender. Spacing the second frame
//! a frame later would collapse the bimodality in excess and would also delay
//! that frame by 5 ms in real time, and whether the floor it removes is worth the
//! delay it adds is arithmetic nobody has done. An argument from this pair
//! structure for spacing was made earlier in this session and retracted in
//! TASKS.md for being wrong by a sign; nothing here revives it. The floor is
//! stated, its mechanism is named, and the decision is somebody else's.

use crate::pairs::Spread;

/// The thresholds the curve is evaluated at, in milliseconds.
///
/// Out to 100 ms because the shape above 20 ms is the diagnostic that says
/// whether this link is one heavy distribution or a normal regime with a second
/// class of stall behind it, and those two want different remedies. Reporting a
/// figure at 30, 50 or 80 ms authorises nothing: what latency budget the product
/// pays is decided elsewhere, and the job here is to say what the link does.
pub const THRESHOLDS_MS: [u32; 11] = [5, 10, 15, 20, 25, 30, 40, 50, 60, 80, 100];

/// Bin width of the survival histogram, in microseconds.
///
/// A quarter of a millisecond, so the 5 ms spacing between adjacent targets is
/// twenty bins and where a step in the curve sits is never a question about the
/// binning.
pub const BIN_US: i64 = 250;

/// Bins, the last of which holds everything at or past 100 ms.
///
/// Four hundred bins of 0.25 ms reach exactly 100 ms and the overflow is a bin
/// of its own rather than folded into the top one, because a tail with no
/// ceiling must not be read as a population sitting at 99.75 ms.
pub const BINS: usize = 401;

/// Seconds per block, for the drift fit and for every rate's spread.
///
/// The receiver's own counter window, so the two share a grid and a block of
/// this curve can be put beside a window row. At the wire's 200 datagrams a
/// second a block holds 2000 arrivals, which is far more than enough for its
/// minimum to be a queue-free arrival: the worst arm this project has measured
/// still delivered 92.6 per cent of its frames without losing them.
pub const BLOCK_SECONDS: f64 = 10.0;

/// Blocks a run needs before a line through their minima is a line.
///
/// Three, the fewest that can disagree with a straight line at all. A floor on
/// the arithmetic and not the length a run should be: the harness derives that
/// from the clusters a rate needs, and its answer is two orders of magnitude
/// above this.
const MINIMUM_BLOCKS: usize = 3;

/// Clusters a threshold needs before its rate is quoted rather than withheld.
///
/// A rate estimated from `k` independent events carries a fractional standard
/// error of `1/sqrt(k)`, and the independent event here is the cluster. Thirty
/// puts that at 18 per cent, so a factor of two between two thresholds is three
/// standard errors and a difference of a quarter is not claimed. Below thirty
/// the count is reported and the rate is not: a rate from four events has an
/// interval covering everything anybody would do with it.
pub const MINIMUM_CLUSTERS: u64 = 30;

/// One arrival, as the two numbers the whole curve is derived from.
///
/// Eight bytes, so a 1200 second run at one datagram per 5 ms frame is 1.9 MB,
/// fixed at construction and never grown. The frame index rather than a
/// timestamp because it is the sequence position, which is what a cluster is
/// consecutive in, and it doubles as the stream time the drift is fitted
/// against.
#[derive(Clone, Copy, Default)]
struct Arrival {
    /// Frames of stream time from the anchoring frame. Signed, because a
    /// datagram reordered across the anchor is behind it.
    frame: i32,
    /// How far past its own moment the frame turned up, exactly as the receiving
    /// thread already computed it. The playout anchor is still in it and comes
    /// off once, at the point of analysis, rather than per arrival.
    late_us: i32,
}

/// The receiving thread's end: a fixed array and an index.
///
/// Written by one thread and read by nobody until the run has ended, so it takes
/// no lock and shares nothing. A record is a bounds check and one eight-byte
/// store: no allocation, no clock, nothing to wait for.
///
/// The alternative was a histogram filled as the run went, which cannot be done,
/// and the reason is worth stating because the shape of this module follows from
/// it. Both the reference and the drift are properties of the whole run, so no
/// arrival's bin is known when it arrives. Anything binning online would be
/// binning against a reference it had not measured yet.
pub struct ExcessTrace {
    arrivals: Box<[Arrival]>,
    filled: usize,
    dropped: u64,
    frame_seconds: f64,
}

impl ExcessTrace {
    /// Room for `capacity` arrivals, sized before any thread starts.
    pub fn with_capacity(capacity: usize, frame_seconds: f64) -> ExcessTrace {
        ExcessTrace {
            arrivals: vec![Arrival::default(); capacity].into_boxed_slice(),
            filled: 0,
            dropped: 0,
            frame_seconds,
        }
    }

    /// Files one arrival, by its distance from the anchoring frame in frames and
    /// by the lateness the receiver measured for it.
    ///
    /// Anything that does not fit is counted rather than clamped. A clamped
    /// arrival is a fabricated member of the population the whole curve is
    /// computed over, and the count is what lets the analysis refuse instead of
    /// reporting a curve for the first part of a run.
    #[inline]
    pub fn record(&mut self, frame: i64, late_us: i64) {
        let (Ok(frame), Ok(late_us)) = (i32::try_from(frame), i32::try_from(late_us)) else {
            self.dropped += 1;
            return;
        };
        if self.filled == self.arrivals.len() {
            self.dropped += 1;
            return;
        }
        self.arrivals[self.filled] = Arrival { frame, late_us };
        self.filled += 1;
    }

    /// Everything the run establishes about excess delay, once it has ended.
    ///
    /// Off the deadline entirely: this runs after the device has stopped, so it
    /// sorts, allocates and divides as freely as the arithmetic wants.
    pub fn finish(mut self) -> ExcessReport {
        let arrivals = self.filled;
        let dropped = self.dropped;
        let frame_seconds = self.frame_seconds;
        let ordered = &mut self.arrivals[..arrivals];
        ordered.sort_unstable_by_key(|arrival| arrival.frame);

        // One frame is one position on the timeline, so a repeat is two
        // datagrams claiming the same one. It cannot happen on this wire - the
        // buffer refuses a duplicate sequence number and counts an off-grid
        // timestamp before either reaches here - and it is counted rather than
        // assumed impossible, because a second arrival silently sharing a
        // position would split one cluster into two.
        let mut repeated = 0u64;
        let mut kept = 0usize;
        for index in 0..arrivals {
            if kept > 0 && ordered[index].frame == ordered[kept - 1].frame {
                repeated += 1;
                continue;
            }
            ordered[kept] = ordered[index];
            kept += 1;
        }
        let series = &ordered[..kept];

        let short = |blocks| ExcessReport {
            arrivals,
            dropped,
            repeated,
            blocks,
            curve: None,
        };
        let (Some(first), Some(last)) = (series.first(), series.last()) else {
            return short(0);
        };
        // Stream time, from the timestamps alone. Nothing on either machine's
        // wall clock enters a rate quoted here: the span is a count of samples
        // the source produced, divided by the rate it produced them at.
        let stream_seconds = f64::from(last.frame - first.frame) * frame_seconds;
        let blocks = (stream_seconds / BLOCK_SECONDS) as usize;
        if blocks < MINIMUM_BLOCKS {
            return short(blocks);
        }

        // Stream time in seconds from the first present frame. The independent
        // variable of the fit and the axis every block index derives from.
        let at = |arrival: &Arrival| f64::from(arrival.frame - first.frame) * frame_seconds;
        let block_of = |arrival: &Arrival| ((at(arrival) / BLOCK_SECONDS) as usize).min(blocks - 1);

        let drift = fit_drift(series, blocks, stream_seconds, &at, &block_of);
        let raw_excess = excess_of(series, |arrival| f64::from(arrival.late_us));
        let corrected_excess = excess_of(series, |arrival| {
            f64::from(arrival.late_us) - drift.delay_ppm * at(arrival)
        });

        // A frame missing between two present ones is neither late nor on time,
        // so it can neither extend a cluster nor close one. Counted, because the
        // harness above refuses a run with any loss in it, and a criterion that
        // cannot be shown holding is worth as little as one that cannot fire.
        let span_frames = i64::from(last.frame - first.frame) + 1;
        let frames_missing = (span_frames - kept as i64).max(0) as u64;
        let sequence_breaks = series
            .windows(2)
            .filter(|pair| pair[1].frame != pair[0].frame + 1)
            .count() as u64;

        let thresholds = THRESHOLDS_MS
            .iter()
            .map(|&millis| {
                threshold(
                    millis,
                    series,
                    &corrected_excess,
                    &raw_excess,
                    blocks,
                    &block_of,
                )
            })
            .collect();

        ExcessReport {
            arrivals,
            dropped,
            repeated,
            blocks,
            curve: Some(ExcessCurve {
                population: kept,
                stream_seconds,
                frames_missing,
                sequence_breaks,
                drift,
                raw: distribution(&raw_excess),
                corrected: distribution(&corrected_excess),
                thresholds,
            }),
        }
    }
}

/// What one run establishes, and what it could not.
///
/// The three counts outside the curve are stated whether or not there is a
/// curve, because they are the reasons there might not be one. A report whose
/// curve is absent and whose reason is also absent sends its reader to guess.
#[derive(Debug)]
pub struct ExcessReport {
    /// Arrivals offered to the trace.
    pub arrivals: usize,
    /// Arrivals the trace had no room for, or whose distance from the anchor did
    /// not fit. Non-zero means anything computed here would describe part of a
    /// run, so nothing is computed.
    pub dropped: u64,
    /// Arrivals that claimed a timeline position another had already taken.
    pub repeated: u64,
    /// Blocks of [`BLOCK_SECONDS`] the run's stream time covered.
    pub blocks: usize,
    pub curve: Option<ExcessCurve>,
}

/// Everything the population says, once it is one population.
#[derive(Clone, Debug)]
pub struct ExcessCurve {
    /// Distinct timeline positions the curve was computed over.
    pub population: usize,
    /// Stream time from the first present frame to the last, in seconds, from
    /// the timestamps alone.
    pub stream_seconds: f64,
    /// Timeline positions inside the span that nothing arrived for.
    pub frames_missing: u64,
    /// Places in the span where the frames stop being consecutive.
    pub sequence_breaks: u64,
    pub drift: Drift,
    pub raw: Distribution,
    pub corrected: Distribution,
    pub thresholds: Vec<Threshold>,
}

impl ExcessCurve {
    /// A count as a rate per minute of stream time.
    pub fn per_minute(&self, count: u64) -> f64 {
        if self.stream_seconds <= 0.0 {
            return 0.0;
        }
        count as f64 * 60.0 / self.stream_seconds
    }

    /// The fraction of the population a count covers.
    pub fn fraction(&self, count: u64) -> f64 {
        if self.population == 0 {
            return 0.0;
        }
        count as f64 / self.population as f64
    }

    pub fn at(&self, millis: u32) -> Option<&Threshold> {
        self.thresholds
            .iter()
            .find(|threshold| threshold.millis == millis)
    }

    /// The lowest threshold whose row is the sender's pair cadence rather than
    /// the link, if any row is.
    ///
    /// The signature is alternation and not size: a large fraction of the
    /// population late, in clusters of one frame, separated by gaps of one frame.
    /// Every other frame late is what two frames per captured packet produces and
    /// is not something a radio can do - a burst is consecutive by definition, so
    /// a burst cannot leave every second frame on time.
    ///
    /// It is detected rather than assumed because the row that carries it depends
    /// on the sender's packing, which this end cannot see: which offset is a
    /// packet's first frame is a bit that lives in the sender's envelope. What
    /// this end can see is the alternation.
    ///
    /// A quarter of the population is the floor rather than a half, because the
    /// two modes are only equally populated when the threshold sits between them,
    /// and a threshold a little off centre still alternates.
    pub fn pair_cadence(&self) -> Option<&Threshold> {
        self.thresholds.iter().find(|threshold| {
            let alternating = threshold
                .cluster_frames
                .zip(threshold.cluster_gap_frames)
                .is_some_and(|(frames, gap)| frames.p50 == 1 && gap.p50 == 1);
            alternating && self.fraction(threshold.late) >= 0.25
        })
    }

    /// Whether the source clock rate this run measured agrees with an earlier
    /// measurement of the same pair to within a factor of two, in sign as well
    /// as in magnitude.
    ///
    /// Compared on [`Drift::source_ppm`] and never on the delay slope, because
    /// the two have opposite signs and this comparison has already been wrong
    /// once for exactly that reason: the first radio run fitted a delay slope of
    /// -13.22 ppm, which is a source clock fast by +13.22 ppm, and the gate
    /// reported it as disagreeing with A7's +9.29 ppm when the two agree to
    /// within a factor of 1.42.
    ///
    /// A finding and never a criterion. A7 compared a pair of crystals directly
    /// and this compares the source's audio clock against this Mac's monotonic
    /// clock through a radio and a jitter buffer, which are not the same pair on
    /// this end, so the two are not owed an exact match - but a factor of two
    /// means one of them is wrong, and proceeding quietly past that is how a
    /// number nobody checked becomes a number everybody cites.
    pub fn drift_agrees_with(&self, ppm: f64) -> bool {
        let fitted = self.drift.source_ppm();
        fitted.signum() == ppm.signum()
            && fitted.abs() >= ppm.abs() / 2.0
            && fitted.abs() <= ppm.abs() * 2.0
    }
}

/// The rate difference between the two clocks, as this run's own arrivals state
/// it, in the two forms it has to be read in.
///
/// # Which sign means what, derived once
///
/// The receiver measures `late_i = A_i - P0 - (R_i - R0)/f`, where `A` is arrival
/// on this Mac's monotonic clock, `R` is the source's own sample count and `f` is
/// the nominal 48000. If the source truly produces `f_s` samples per Mac second
/// then `R_i - R0 = f_s (T_i - T0)` for production instants `T`, and
///
/// ```text
/// late_i = (T_i - T0)(1 - f_s/f) + d_i + c
/// ```
///
/// so the slope of `late` against stream time is `1 - f_s/f`. A source clock
/// running FAST has `f_s > f` and therefore a NEGATIVE slope: the subtracted RTP
/// term outruns arrival time. Both quantities are stated below under names that
/// say which is which, because reading one as the other is not a subtle error and
/// it has already happened here once.
#[derive(Clone, Copy, Debug)]
pub struct Drift {
    /// Parts per million of delay accumulated per second of stream time, which is
    /// exactly what the correction subtracts. Negative when the source clock runs
    /// fast.
    pub delay_ppm: f64,
    /// The same fit over every arrival rather than over the block minima, so the
    /// difference between a robust estimator and the one a burst destroys is a
    /// number in the document rather than an argument in a comment. Same sign
    /// convention as [`Drift::delay_ppm`].
    pub delay_ppm_all_points: f64,
    /// Blocks that contributed a minimum.
    pub blocks: usize,
    /// What the correction took off the end of the run, in milliseconds. Same
    /// sign as the slope, so it is negative for a fast source.
    pub accumulated_ms: f64,
}

impl Drift {
    /// The source clock's rate referred to this Mac's timebase, positive when the
    /// source runs fast.
    ///
    /// The negation of the delay slope, for the reason derived above, and the one
    /// of the two figures that is comparable with A7's +9.29 ppm.
    pub fn source_ppm(&self) -> f64 {
        -self.delay_ppm
    }

    /// The same for the estimator this module rejected.
    pub fn source_ppm_all_points(&self) -> f64 {
        -self.delay_ppm_all_points
    }
}

/// One excess distribution: its order statistics and its shape.
#[derive(Clone, Debug)]
pub struct Distribution {
    /// Order statistics of the excess above the population's minimum, in
    /// microseconds. Non-negative by construction, so the minimum is zero and
    /// says so.
    pub spread: Spread,
    /// Counts per [`BIN_US`] bin, the last holding everything at or past 100 ms.
    pub bins: Box<[u64]>,
}

impl Distribution {
    /// Arrivals past the last named bin, which is where a second class of stall
    /// would show up.
    pub fn over_span(&self) -> u64 {
        self.bins[BINS - 1]
    }
}

/// What one threshold does to this population.
#[derive(Clone, Debug)]
pub struct Threshold {
    pub millis: u32,
    /// Frames whose drift-corrected excess exceeded the threshold.
    pub late: u64,
    /// The same count before the drift correction, so the size of the correction
    /// is visible at the thresholds and not only in the percentiles.
    pub late_raw: u64,
    pub clusters: u64,
    /// Frames per cluster. Absent when there were no clusters, which is a
    /// different thing from a cluster of no frames.
    pub cluster_frames: Option<Spread>,
    /// The worst excess inside a cluster, in microseconds, one value per
    /// cluster.
    pub cluster_worst_us: Option<Spread>,
    /// On-time frames between the end of one cluster and the start of the next,
    /// which needs two clusters to exist at all.
    ///
    /// The intact stretch and not the distance between two starts, because what
    /// a listener gets between two audible faults is the audio in between.
    pub cluster_gap_frames: Option<Spread>,
    /// Clusters per block, which is where a rate's spread comes from. Never a
    /// binomial over frames: the correlated unit is the cluster.
    pub block_clusters: Option<Spread>,
}

impl Threshold {
    /// Whether this threshold saw enough clusters for a rate to mean anything.
    pub fn rate_is_quotable(&self) -> bool {
        self.clusters >= MINIMUM_CLUSTERS
    }
}

/// Least squares through the minimum of each block, plus the same fit through
/// every point for comparison.
fn fit_drift(
    series: &[Arrival],
    blocks: usize,
    stream_seconds: f64,
    at: &impl Fn(&Arrival) -> f64,
    block_of: &impl Fn(&Arrival) -> usize,
) -> Drift {
    let mut minima = vec![f64::INFINITY; blocks];
    for arrival in series {
        let block = &mut minima[block_of(arrival)];
        *block = block.min(f64::from(arrival.late_us));
    }
    let block_points: Vec<(f64, f64)> = minima
        .iter()
        .enumerate()
        .filter(|(_, value)| value.is_finite())
        .map(|(index, &value)| ((index as f64 + 0.5) * BLOCK_SECONDS, value))
        .collect();
    // Microseconds of delay per second of stream time is parts per million
    // directly, with no conversion left to get wrong. Its sign is the delay's and
    // not the source clock's; `Drift::source_ppm` negates it and says why.
    let delay_ppm = slope(&block_points).unwrap_or(0.0);
    let all: Vec<(f64, f64)> = series
        .iter()
        .map(|arrival| (at(arrival), f64::from(arrival.late_us)))
        .collect();
    Drift {
        delay_ppm,
        delay_ppm_all_points: slope(&all).unwrap_or(0.0),
        blocks: block_points.len(),
        accumulated_ms: delay_ppm * stream_seconds / 1_000.0,
    }
}

/// The least-squares slope of `y` on `x`, or nothing when the abscissa does not
/// vary.
///
/// Centred on the mean rather than accumulated raw. The raw sums over a quarter
/// of a million arrivals of microseconds against seconds differ by twelve orders
/// of magnitude, and the difference of two such sums is where a double stops
/// carrying the digits the answer is in.
fn slope(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let count = points.len() as f64;
    let mean_x = points.iter().map(|&(x, _)| x).sum::<f64>() / count;
    let mean_y = points.iter().map(|&(_, y)| y).sum::<f64>() / count;
    let mut covariance = 0.0;
    let mut variance = 0.0;
    for &(x, y) in points {
        let dx = x - mean_x;
        covariance += dx * (y - mean_y);
        variance += dx * dx;
    }
    (variance > 0.0).then(|| covariance / variance)
}

/// Excess above the population's own minimum, in whole microseconds, in
/// sequence order.
fn excess_of(series: &[Arrival], value: impl Fn(&Arrival) -> f64) -> Vec<i64> {
    let least = series
        .iter()
        .map(&value)
        .fold(f64::INFINITY, |least, next| least.min(next));
    series
        .iter()
        .map(|arrival| (value(arrival) - least).round() as i64)
        .collect()
}

/// The order statistics and the shape of one excess series.
fn distribution(excess: &[i64]) -> Distribution {
    let mut bins = vec![0u64; BINS].into_boxed_slice();
    for &micros in excess {
        let bin = (micros.max(0) / BIN_US) as usize;
        bins[bin.min(BINS - 1)] += 1;
    }
    let mut sorted = excess.to_vec();
    sorted.sort_unstable();
    Distribution {
        spread: spread_of(&sorted).expect("a curve is only built over a non-empty population"),
        bins,
    }
}

/// Everything one threshold says about this population.
fn threshold(
    millis: u32,
    series: &[Arrival],
    corrected: &[i64],
    raw: &[i64],
    blocks: usize,
    block_of: &impl Fn(&Arrival) -> usize,
) -> Threshold {
    let bound = i64::from(millis) * 1_000;
    let late = corrected.iter().filter(|&&excess| excess > bound).count() as u64;
    let late_raw = raw.iter().filter(|&&excess| excess > bound).count() as u64;

    // One pass in sequence order. A cluster runs while consecutive frames are
    // late and ends at the first frame that is not - or at a break in the
    // sequence, because a frame that never arrived is not an on-time frame and
    // two late frames either side of a hole are not consecutive.
    let mut frames: Vec<i64> = Vec::new();
    let mut worst: Vec<i64> = Vec::new();
    let mut gaps: Vec<i64> = Vec::new();
    let mut per_block = vec![0i64; blocks];
    let mut open: Option<Cluster> = None;
    let mut previous_end: Option<i32> = None;
    let mut close = |open: &mut Option<Cluster>, previous_end: &mut Option<i32>| {
        if let Some(cluster) = open.take() {
            frames.push(i64::from(cluster.end - cluster.start) + 1);
            worst.push(cluster.peak);
            if let Some(before) = *previous_end {
                gaps.push(i64::from(cluster.start - before) - 1);
            }
            *previous_end = Some(cluster.end);
        }
    };

    for (index, arrival) in series.iter().enumerate() {
        if index > 0 && arrival.frame != series[index - 1].frame + 1 {
            close(&mut open, &mut previous_end);
        }
        let excess = corrected[index];
        if excess <= bound {
            close(&mut open, &mut previous_end);
            continue;
        }
        match &mut open {
            Some(cluster) => {
                cluster.end = arrival.frame;
                cluster.peak = cluster.peak.max(excess);
            }
            none => {
                *none = Some(Cluster {
                    start: arrival.frame,
                    end: arrival.frame,
                    peak: excess,
                });
                per_block[block_of(arrival)] += 1;
            }
        }
    }
    close(&mut open, &mut previous_end);

    frames.sort_unstable();
    worst.sort_unstable();
    gaps.sort_unstable();
    per_block.sort_unstable();

    Threshold {
        millis,
        late,
        late_raw,
        clusters: frames.len() as u64,
        cluster_frames: spread_of(&frames),
        cluster_worst_us: spread_of(&worst),
        cluster_gap_frames: spread_of(&gaps),
        block_clusters: spread_of(&per_block),
    }
}

/// A cluster while it is still open, in timeline positions.
struct Cluster {
    start: i32,
    end: i32,
    peak: i64,
}

/// Nearest-rank order statistics of an ascending slice, or nothing when it is
/// empty.
///
/// The same nearest rank the occupancy histogram and the run-wide stores use, so
/// every percentile in this crate is read on one convention. An empty slice is
/// absent and never a row of zeros: no clusters is not a cluster of no frames,
/// and this project has read that difference wrongly five times.
fn spread_of(sorted: &[i64]) -> Option<Spread> {
    let count = sorted.len();
    if count == 0 {
        return None;
    }
    let rank = |quantile: f64| {
        let index = (quantile * count as f64).ceil() as usize;
        sorted[index.clamp(1, count) - 1]
    };
    Some(Spread {
        count,
        min: sorted[0],
        p50: rank(0.50),
        p95: rank(0.95),
        p99: rank(0.99),
        max: sorted[count - 1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_SECONDS: f64 = 0.005;

    /// A run with no queueing at all and a source clock fast by a known amount,
    /// stated in the convention the report uses.
    ///
    /// A fast source makes the subtracted RTP term outrun arrival time, so its
    /// lateness ramps DOWNWARD - which is the identity `Drift` derives and the
    /// one this whole helper exists to keep honest.
    fn drifting(source_ppm: f64, seconds: f64) -> ExcessTrace {
        let frames = (seconds / FRAME_SECONDS) as i64;
        let mut trace = ExcessTrace::with_capacity(frames as usize + 1, FRAME_SECONDS);
        for frame in 0..frames {
            let at = frame as f64 * FRAME_SECONDS;
            trace.record(frame, (-source_ppm * at).round() as i64);
        }
        trace
    }

    fn two_minutes(late_us: impl Fn(i64) -> i64) -> ExcessCurve {
        let mut trace = ExcessTrace::with_capacity(24_001, FRAME_SECONDS);
        for frame in 0..24_000i64 {
            trace.record(frame, late_us(frame));
        }
        trace
            .finish()
            .curve
            .expect("two minutes is twelve blocks and a curve")
    }

    /// The whole point of referring to the minimum: a run whose first packet
    /// landed 25 ms badly is the same run, and its curve must not move. A8's
    /// arms read p50 -10.9 ms and -36.3 ms on one link for exactly this reason.
    #[test]
    fn the_curve_does_not_depend_on_where_the_anchor_landed() {
        let queueing = |frame: i64| if frame % 100 == 0 { 8_000 } else { 300 };
        let shifted = two_minutes(|frame| queueing(frame) - 25_000);
        let plain = two_minutes(queueing);
        assert_eq!(shifted.corrected.spread, plain.corrected.spread);
        assert_eq!(shifted.raw.spread, plain.raw.spread);
        assert_eq!(shifted.at(5).map(|t| t.late), plain.at(5).map(|t| t.late));
    }

    /// The drift is what A7 measured, in the sign A7 measured it in, and the
    /// correction is worth more than the step between two adjacent targets -
    /// which is the reason it is here at all.
    ///
    /// The two signs are asserted separately and on purpose. The first radio run
    /// fitted a delay slope of -13.22 ppm on a source clock fast by 13.22, and
    /// the gate reported it as disagreeing with A7's +9.29 ppm because one doc
    /// comment had the convention backwards. Nothing but a test on both
    /// quantities catches that.
    #[test]
    fn the_fit_recovers_a_known_rate_in_a7s_own_sign() {
        let curve = drifting(9.29, 600.0)
            .finish()
            .curve
            .expect("ten minutes is a curve");
        assert!(
            (curve.drift.delay_ppm + 9.29).abs() < 0.1,
            "a source clock fast by 9.29 ppm makes lateness ramp down at -9.29 ppm, and the fit \
             returned {:+.3}",
            curve.drift.delay_ppm
        );
        assert!(
            (curve.drift.source_ppm() - 9.29).abs() < 0.1,
            "referred to this Mac's timebase the source is fast by +9.29 ppm, and the report \
             says {:+.3}",
            curve.drift.source_ppm()
        );
        assert!(
            curve.drift.accumulated_ms.abs() > 5.0,
            "9.29 ppm over 600 s is 5.6 ms, larger than the 5 ms between targets, and the fit \
             reported {:+.2} ms",
            curve.drift.accumulated_ms
        );
        assert!(curve.drift_agrees_with(9.29));
        assert!(
            !curve.drift_agrees_with(-9.29),
            "a source clock of the opposite sign is a disagreement and not a match; comparing \
             on the delay slope instead of the source rate is exactly how this said the wrong \
             thing on its first radio run"
        );
        // Every arrival sat on the line, so nothing is left once the line comes
        // off, while the uncorrected curve still carries the whole ramp.
        assert!(curve.corrected.spread.max <= BIN_US);
        assert!(curve.raw.spread.max > 5_000);
    }

    /// One burst at the end of a run destroys the all-points slope and leaves the
    /// block minima alone. This is the measurement that chose the estimator, so it
    /// is the one a change to it has to survive - and the rejected estimator does
    /// not merely lose accuracy here, it reports a source clock running slow when
    /// it is running fast.
    #[test]
    fn a_burst_moves_the_all_points_fit_and_not_the_one_that_is_used() {
        let curve = two_minutes(|frame| {
            let at = frame as f64 * FRAME_SECONDS;
            let burst = if frame >= 23_800 { 200_000 } else { 0 };
            (-9.29 * at).round() as i64 + burst
        });
        assert!(
            (curve.drift.source_ppm() - 9.29).abs() < 0.5,
            "the block minima ignore a burst they did not lower: {:+.3} ppm",
            curve.drift.source_ppm()
        );
        assert!(
            curve.drift.source_ppm_all_points() < -20.0,
            "the rejected estimator has to be seen failing, and here it inverts the sign of the \
             answer: {:+.3} ppm against a source that is fast",
            curve.drift.source_ppm_all_points()
        );
    }

    /// A hundred isolated late frames and twenty bursts of five have the same
    /// late ratio and are not the same finding. Nothing but the cluster
    /// accounting separates them.
    #[test]
    fn one_late_ratio_comes_apart_into_two_cluster_structures() {
        let scattered = two_minutes(|frame| if frame % 240 == 0 { 30_000 } else { 0 });
        let bursty = two_minutes(|frame| if frame % 1_200 < 5 { 30_000 } else { 0 });
        let scattered = scattered.at(20).expect("20 ms is a threshold").clone();
        let bursty = bursty.at(20).expect("20 ms is a threshold").clone();

        assert_eq!(scattered.late, 100);
        assert_eq!(bursty.late, 100);
        assert_eq!(
            scattered.clusters, 100,
            "an isolated frame is its own cluster"
        );
        assert_eq!(bursty.clusters, 20);
        assert_eq!(scattered.cluster_frames.expect("sizes").max, 1);
        assert_eq!(bursty.cluster_frames.expect("sizes").p50, 5);
        // The gap is what a listener hears: twenty bursts of five frames are
        // twenty holes of 25 ms, and the scattered run has none.
        assert_eq!(bursty.cluster_gap_frames.expect("gaps").p50, 1_195);
        assert_eq!(scattered.cluster_gap_frames.expect("gaps").p50, 239);
    }

    /// The sender's pair cadence, and the fact that it is not the link.
    ///
    /// Two Opus frames in one captured packet arrive together, and the second is
    /// one frame later in stream time, so its excess is exactly one frame lower
    /// and half the population sits 5 ms above the other half. A6.1 measured the
    /// same thing from the other side this session: the per-pair difference came
    /// to -4.996 ms at p50 with 96 per cent of pairs inside the [-5,-4) ms bucket
    /// over 8998, 9000 and 120004 pairs.
    ///
    /// What makes it distinguishable from a link that is simply bad is
    /// alternation. A burst is consecutive by definition, so no radio can leave
    /// every second frame on time, and a burst of the same size is required to
    /// come out as cadence-free here.
    #[test]
    fn the_pair_cadence_is_told_apart_from_a_link_that_is_merely_bad() {
        // Both members queued 1 ms, and the first of each pair carrying the extra
        // frame of stream time the arithmetic gives it plus 0.2 ms so that it is
        // strictly past the 5 ms row rather than exactly on it - `late` is
        // `excess > T`, and a fixture sitting on the boundary would be testing the
        // comparison rather than the cadence.
        let paired = two_minutes(|frame| if frame % 2 == 0 { 6_200 } else { 1_000 });
        let signature = paired
            .pair_cadence()
            .expect("every second frame late is the cadence and nothing else");
        assert_eq!(signature.millis, 5, "the floor is one frame of stream time");
        assert_eq!(
            paired.at(5).expect("5 ms").late,
            12_000,
            "half the population"
        );
        assert_eq!(signature.cluster_frames.expect("sizes").p50, 1);
        assert_eq!(signature.cluster_gap_frames.expect("gaps").p50, 1);
        // And the floor it states: a threshold above the pair spacing clears it,
        // so the bimodality bounds a target below 5 ms and says nothing above it.
        assert_eq!(paired.at(10).expect("10 ms").late, 0);

        // The same fraction of the population late, in bursts instead. It must not
        // be read as cadence, because the remedy for the two is not the same one.
        let bursty = two_minutes(|frame| if frame % 200 < 100 { 6_200 } else { 1_000 });
        assert_eq!(bursty.at(5).expect("5 ms").late, 12_000);
        assert!(
            bursty.pair_cadence().is_none(),
            "a burst is consecutive, so it cannot be the alternation two frames per packet makes"
        );
    }

    /// Raising the threshold cannot make more frames late, at any step of the
    /// curve: one that rises was computed against a reference that moved.
    #[test]
    fn the_survival_curve_never_rises() {
        let curve = two_minutes(|frame| (frame % 130) * 1_000);
        for pair in curve.thresholds.windows(2) {
            assert!(
                pair[1].late <= pair[0].late,
                "{} ms left {} late and {} ms left {}",
                pair[0].millis,
                pair[0].late,
                pair[1].millis,
                pair[1].late
            );
        }
        assert!(
            curve.corrected.over_span() > 0,
            "an excess past 100 ms belongs in the named overflow bin and nowhere else"
        );
        assert_eq!(
            curve.corrected.bins.iter().sum::<u64>(),
            curve.population as u64,
            "every arrival is in exactly one bin"
        );
    }

    /// A rate from four clusters is not a rate, and the threshold says so rather
    /// than printing one.
    #[test]
    fn a_rate_is_withheld_below_thirty_clusters() {
        let curve = two_minutes(|frame| if frame % 6_000 == 0 { 90_000 } else { 0 });
        let at80 = curve.at(80).expect("80 ms is a threshold");
        assert_eq!(at80.clusters, 4);
        assert!(!at80.rate_is_quotable());
        let at5 = curve.at(5).expect("5 ms is a threshold");
        assert!(
            !at5.rate_is_quotable(),
            "four events are four events at every threshold; a quotable rate here would mean \
             the bound reads the frames rather than the clusters"
        );
    }

    /// A trace that filled up has measured part of a run, and part of a run is
    /// not a curve. The count is what the harness refuses on, so it is stated
    /// where the curve is not.
    #[test]
    fn a_trace_that_overflowed_states_the_overflow_and_no_curve() {
        let mut trace = ExcessTrace::with_capacity(100, FRAME_SECONDS);
        for frame in 0..24_000i64 {
            trace.record(frame, 0);
        }
        let report = trace.finish();
        assert_eq!(report.arrivals, 100);
        assert_eq!(report.dropped, 23_900);
        assert!(report.curve.is_none());
    }

    /// Thirty seconds is the fewest three blocks, and anything shorter cannot
    /// have a line fitted through it at all.
    #[test]
    fn a_run_too_short_to_fit_a_line_has_no_curve() {
        let report = drifting(9.29, 25.0).finish();
        assert_eq!(report.blocks, 2);
        assert!(report.curve.is_none());
        assert!(report.arrivals > 0, "the arrivals are still counted");
    }

    /// A gap in the sequence is not an on-time frame, so it cannot close a
    /// cluster, and it is counted so a run with loss in it can be refused rather
    /// than analysed.
    #[test]
    fn a_hole_in_the_sequence_breaks_a_cluster_and_is_counted() {
        let mut trace = ExcessTrace::with_capacity(24_001, FRAME_SECONDS);
        for frame in 0..24_000i64 {
            if (12_000..12_010).contains(&frame) {
                continue;
            }
            let late = (11_990..12_020).contains(&frame);
            trace.record(frame, if late { 30_000 } else { 0 });
        }
        let curve = trace.finish().curve.expect("two minutes is a curve");
        assert_eq!(curve.frames_missing, 10);
        assert_eq!(curve.sequence_breaks, 1);
        assert_eq!(
            curve.at(20).expect("20 ms is a threshold").clusters,
            2,
            "twenty late frames either side of a hole are two clusters and not one"
        );
    }

    /// Two datagrams claiming one timeline position would split a cluster in
    /// two, so a repeat leaves the population and is counted.
    #[test]
    fn a_repeated_timeline_position_is_counted_and_not_curved_over() {
        let mut trace = ExcessTrace::with_capacity(24_002, FRAME_SECONDS);
        for frame in 0..24_000i64 {
            trace.record(frame, 0);
        }
        trace.record(12_000, 40_000);
        let report = trace.finish();
        assert_eq!(report.repeated, 1);
        assert_eq!(report.curve.expect("a curve").population, 24_000);
    }
}
