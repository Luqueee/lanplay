//! Aligning the host's capture phase to this display's refresh phase.
//!
//! Decode-complete to display-link pickup is the largest single term in the
//! latency of the whole pipeline, and none of it is work. On a cross-machine
//! 1080p120 run decode costs 1.0 ms at p50 while that wait costs 4.4 ms, and a
//! 600 s soak puts its percentiles at 4.4, 7.8 and 8.4 ms. A frame that becomes
//! ready at a uniformly distributed instant inside one 8.33 ms refresh period
//! waits T/2 at p50, 0.95T at p95 and 0.99T at p99, which is 4.17, 7.90 and
//! 8.25 ms. The measurement is the prediction, so what is being measured is the
//! phase relationship between two 120 Hz clocks that were never synchronised,
//! and nothing else.
//!
//! Two clocks running at almost the same rate cannot be brought together by
//! changing a rate: this panel measures 119.97 Hz against a 120.00 Hz source,
//! so the phase creeps by about two microseconds a frame whatever either side
//! does. What can be moved is the phase itself. Holding one capture tick back
//! by the right amount puts every frame after it a chosen distance in front of
//! the instant this display next wants one, and a loop that keeps making that
//! correction holds the arrangement against the creep.
//!
//! The phase is read back from the telemetry collector, which already measures
//! [`Segment::PresentationWait`] between the decoder's own `DecodeComplete`
//! mark and the renderer's `RenderSubmit`, the latter taken at the top of the
//! display link's callback. Timing the display link a second time in here was
//! rejected: two sources of truth for one interval disagree eventually, and the
//! disagreement gets blamed on whichever was read last.
//!
//! The deadline is therefore the instant the display link asked for a frame,
//! not the instant Core Animation will show it. `targetPresentationTimestamp`
//! is the truer deadline and was rejected because it is not on this project's
//! clock: it is Core Animation's base, `mach_absolute_time` in seconds, while
//! every mark here comes from `mach_continuous_time`, and the two drift apart
//! by however long the machine has spent asleep. Subtracting one from the other
//! is the same class of error as subtracting a timestamp taken on the host from
//! one taken here, which this project refuses everywhere - the renderer only
//! ever differences that timestamp against itself for the same reason. Nothing
//! is lost by staying on one base, because a callback leads the presentation it
//! feeds by the compositor's own pipeline depth, which is a constant number of
//! refreshes: a phase measured against the callback and a phase measured
//! against the presentation differ by a constant, and a loop that drives a
//! phase to a chosen offset inside one period never has to know it.
//!
//! Everything sent from here is advisory. A host free to ignore a shift still
//! streams correctly, this receiver still works when it never sends one, and a
//! run started with the estimate switched off is the negative control that says
//! whether an aligned run was aligned or merely lucky.
//! No lever currently moves this phase, and that is measured rather than
//! assumed.
//!
//! Two candidates were built and both are neutral. Delaying the capture tick is
//! neutral by derivation: it moves when a frame is ready but not when its content
//! was drawn, so the same display tick shows the same content and the age is
//! unchanged until the delay crosses a tick, at which point it costs a whole
//! period. Delaying the producer's draw is neutral by experiment: a 3.00 ms shift,
//! confirmed applied by the producer during the run, produced no step anywhere in
//! a 50-sample trace whose largest movement between samples was 0.374 ms.
//!
//! The reason is that Desktop Duplication follows the compositor, not the
//! producer. A draw moved inside a composition interval is composited at the same
//! virtual-display vblank, so the frame leaves the host at the same instant. The
//! phase therefore belongs to that vblank, which is the virtual display driver's
//! to time. That is a concrete and measured reason for the IddCx work - worth
//! about half a refresh period, the largest single term in this pipeline - rather
//! than the vaguer one that a display has to exist somewhere.
//!
//! So this module measures and, by default, does not act. `--phase-align on`
//! still asks, because the asking path is built and tested and is what an IddCx
//! vblank will be driven through; it just has nothing to move today.

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{Nanos, Segment, Stage, Telemetry, Timestamp};
use lanplay_transport::{ControlClient, ControlMessage};
use parking_lot::Mutex;

/// Smallest margin the loop will aim for, whatever it measures.
///
/// The two errors either side of a deadline cost wildly different amounts. A
/// frame ready a hair early waits that hair out and is shown on time; a frame
/// ready a hair late is not shown for another whole period, so missing by
/// 100 µs costs 8.33 ms. The aim therefore sits deliberately off centre.
///
/// This used to be the whole margin, and a fixed 2.00 ms failed a live arm for
/// exactly the reason the asymmetry predicts. It budgeted 1.22 ms for the
/// decode spread and 0.50 ms for the loop's own residual and nothing at all for
/// the link: on a Wi-Fi run whose arrival intervals reached 30.95 ms at p99
/// against an 8.33 ms period, frames landed later than the cushion could
/// absorb, waited the whole period out, and cost 1.5 points of fresh ticks and
/// 84 rendered frames against the unaligned control. The wait came down and the
/// experience got worse.
///
/// So the margin is now measured rather than declared - see [`margin_for`] -
/// and this is only its floor, which stops a suspiciously tight batch from
/// talking the loop into aiming at the deadline itself.
const MARGIN_FLOOR: Nanos = Nanos::from_micros(2_000);

/// How many measured standard deviations of phase the margin covers.
///
/// Three, because the distribution being covered is not symmetric in what it
/// costs. Two would leave a frame in fifty landing late, and each of those pays
/// a whole period and a stale refresh, while the same slack spent early costs
/// its own length and nothing else.
const MARGIN_SIGMAS: f64 = 3.0;

/// Largest share of the period the margin may take.
///
/// Aiming further ahead than this asks the host for more delay than the wait it
/// is buying out of, which is the point where the cure costs more than the
/// disease: an unaligned frame waits half a period on average, so a margin at
/// half a period would be paying full price for nothing. A third keeps it
/// clearly on the profitable side, and it is also what makes the margin give
/// way to the rate - two milliseconds is a quarter of a 120 Hz period and most
/// of a 240 Hz one, and a margin that swallowed the period would aim frames at
/// the deadline before the last one.
const MARGIN_CEILING_SHARE: u64 = 3;

/// Fraction of the measured error corrected in one step.
///
/// A loop that asks for the whole error every time chases its own measurement
/// noise, and whatever it overshoots by is paid back by the next correction:
/// the phase rings around the target instead of settling on it. Halving the
/// error leaves it decaying geometrically with no overshoot, reaching the dead
/// zone from the worst possible starting phase in five decisions, and it is
/// damped by a factor of two against a host that obeys a shift differently
/// from the way it was asked - ringing would need one that obeyed twice over.
///
/// A slacker loop was rejected because the correction is proportional only, so
/// its residual against a constant creep is the creep multiplied by
/// `(1 - gain) / gain`: at a quarter that is three times the creep and eats
/// most of the margin, where at a half it is the creep itself. The noise that
/// usually argues for a slack loop is not present here, because each decision
/// averages more than a hundred frames.
const GAIN: f64 = 0.5;

/// Error small enough to leave alone.
///
/// Without this the loop would ask for a correction on every decision forever,
/// and about half of those would be asking for the phase to move earlier.
/// Earlier is the direction that costs a tick: the only way to advance a phase
/// with a delay is to delay by nearly a whole period, which shows one frame
/// twice. That slip is the one the two clock rates force anyway - a display
/// 0.03 Hz slower than the source has one refresh with nothing new every 33
/// seconds - so a correction the long way round only chooses when it happens,
/// while a correction below this threshold would be buying nothing with it.
const DEADZONE: Nanos = Nanos::from_micros(250);

/// Fewest samples a decision may be taken from.
///
/// At 120 fps this is 400 ms of stream. A phase computed from a handful of
/// frames is a phase computed from decode jitter, and acting on it moves a
/// working stream for no reason.
const MIN_SAMPLES: usize = 48;

/// How far a pickup interval may sit from one period and still count as
/// cadenced.
const CADENCE_TOLERANCE: f64 = 0.25;

/// Share of pickup intervals that must be cadenced before a batch is taken to
/// describe a display link at all.
///
/// A link that was suspended, a window that spent the run behind something, or
/// a panel running at a fraction of the source rate all produce intervals
/// nowhere near one period. None of those runs has a phase worth sending, and
/// every one of them would produce a confident number if the intervals were
/// never looked at.
const MIN_CADENCED: f64 = 0.9;

/// How tightly a batch's phases must cluster.
///
/// Within half a second the phase is nearly constant even when the stream is
/// completely unaligned: the creep between two 120 Hz clocks moves it by about
/// 0.25 ms over that span, which is why a run whose phase is uniform across ten
/// minutes still has a sharp phase inside any one batch. Samples that do not
/// cluster are therefore not an unaligned stream, they are a batch with no
/// single phase in it, and there is nothing for a correction to aim at.
const MIN_CONCENTRATION: f64 = 0.8;

/// Slices of the period that phase coverage is reported in.
///
/// Sixteen because it has to answer one question - did the phase stay somewhere
/// or sweep everywhere - and a sixteenth of an 8.33 ms period is 0.52 ms, which
/// is finer than the margin and coarser than the jitter. Sixteen also fits the
/// bits of a `u16`, so a run's whole coverage is one word.
const COVERAGE_BINS: u32 = 16;

/// How often finished timelines are collected from the collector.
const HARVEST: Duration = Duration::from_millis(250);

/// How much stream each decision is taken from.
const DECIDE_EVERY: Nanos = Nanos::from_millis(500);

/// Samples ignored after a shift is sent.
///
/// The shift crosses a TCP connection and lands on the host's next tick, so
/// frames from either side of that instant carry two different phases. Averaged
/// together they describe neither.
const SETTLE: Nanos = Nanos::from_millis(100);

/// Bound on how long an advisory message may occupy the estimator's thread.
///
/// Nothing reads this socket once the handshake is done, and a host that has
/// stopped reading it must cost a bounded wait rather than a wedged thread.
const CONTROL_TIMEOUT: Nanos = Nanos::from_millis(1_000);

/// One frame's contribution to the estimate.
#[derive(Clone, Copy)]
struct Sample {
    /// How far in front of the display link's deadline the frame was ready,
    /// folded into one period.
    phase: Nanos,
    /// When the display link took the frame. Kept so a batch can be checked for
    /// being cadenced before its phase is believed.
    taken_at: Timestamp,
}

/// Decisions the trace keeps before it stops growing.
///
/// One decision per half second is 120 an hour of stream, so this is about
/// half an hour of run in a fixed 24 bytes an entry. There is a bound because
/// an unbounded series in a long soak is a leak, and the count is reported so
/// that a series which stopped growing can never be read as a phase that
/// stopped moving.
const TRACE_CAPACITY: usize = 4_096;

/// One decision, kept so the series can be read after the run.
///
/// A first and a last phase cannot settle anything on a link that drifts: two
/// clocks 250 µs a second apart sweep two whole periods in 70 s, which buries a
/// 3 ms step applied halfway. What settles it is the phase either side of the
/// step, close enough to it that the drift between the two readings is small
/// against the step, and that needs the series rather than its endpoints.
#[derive(Clone, Copy)]
pub struct Traced {
    /// The newest sample this decision was computed from.
    ///
    /// Stamped from the batch rather than from the moment the thread got round
    /// to judging it, because up to a harvest interval separates the two and the
    /// whole point of the series is lining it up with something else in time.
    pub at: Timestamp,
    pub phase: Nanos,
    pub margin: Nanos,
    /// The delay this decision asked for, if it asked for one.
    pub delay: Option<Nanos>,
    /// Whether that delay reached the wire. Always false while observing.
    pub sent: bool,
}

/// What the estimator concluded from one batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Ask the host to hold its next capture tick back by this much. Always
    /// more than nothing and less than one period.
    Shift(Nanos),
    /// The phase is where it was aimed. Asking for anything would cost more
    /// than it bought.
    Hold { phase: Nanos },
    /// There was not enough evidence to move a working stream.
    Declined(Reason),
}

/// Why a batch produced no estimate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reason {
    /// Fewer frames than a phase can honestly be computed from.
    TooFewSamples { have: usize, need: usize },
    /// The frames were not being picked up one refresh apart, so whatever
    /// produced them was not a display link running at the source rate.
    NotCadenced { cadenced: u32, of: u32 },
    /// The batch held no single phase. Carried in thousandths, so a reason
    /// stays a plain comparable value.
    NoSinglePhase { concentration: u32 },
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reason::TooFewSamples { have, need } => {
                write!(f, "only {have} of the {need} samples a decision needs")
            }
            Reason::NotCadenced { cadenced, of } => write!(
                f,
                "{cadenced} of {of} pickups were one refresh apart, so this \
                 run's display link was not cadenced"
            ),
            Reason::NoSinglePhase { concentration } => write!(
                f,
                "the batch held no single phase (clustering {:.2})",
                f64::from(*concentration) / 1000.0
            ),
        }
    }
}

/// Measures the phase between the host's capture and this display's refresh,
/// and turns it into the one thing the host can obey: a delay.
///
/// Holds no clock and no socket. Every input arrives through
/// [`PhaseEstimator::observe`] and every output leaves through
/// [`PhaseEstimator::decide`], which is what makes a loop that only exists
/// across two machines something a test can drive.
pub struct PhaseEstimator {
    /// The source period. The delay is applied to the host's tick, so the
    /// host's period is the modulus, and the two microseconds a frame by which
    /// this display's period differs from it are exactly the creep the loop
    /// exists to keep correcting.
    period: Nanos,
    /// Where frames are currently aimed: this far in front of the deadline.
    /// Revised on every believable batch from the jitter that batch showed, so
    /// a link that gets worse is aimed further ahead of rather than into it.
    margin: Nanos,
    /// The scatter the last margin was chosen from, kept so a run can be read
    /// afterwards and the margin traced back to the data.
    spread: Option<Nanos>,
    batch: Vec<Sample>,
    /// Highest frame already accounted for. The collector keeps its finished
    /// timelines for about two seconds, so every harvest sees frames the last
    /// one already had.
    highest: FrameId,
    /// Samples before this instant belong to the phase that was in force before
    /// the last shift. Set by [`PhaseEstimator::shifted`] rather than by
    /// deciding, because a decision nobody sent leaves the phase exactly where
    /// it was and has nothing to wait out.
    settled_at: Option<Timestamp>,
    /// Which sixteenths of the period the measured phase has been seen in. One
    /// bit each, because the question it answers is not how often but whether:
    /// an untouched pair of clocks beats through the whole period every 33 s and
    /// should light every bit, while a phase being held somewhere on purpose
    /// should light one or two. That difference is the control the aligned arm
    /// has to be read against.
    visited: u16,
    /// Every believable decision, in order. A refused batch leaves no entry
    /// because it produced no phase, which `decisions` against the length of
    /// this says without having to invent one.
    trace: Vec<Traced>,
    /// Decisions that would have been traced had the series not been full.
    untraced: u64,
    samples: u64,
    decisions: u64,
    asked: u64,
    holds: u64,
    declined: u64,
    first_phase: Option<Nanos>,
    last_phase: Option<Nanos>,
    last_delay: Option<Nanos>,
    last_reason: Option<Reason>,
}

/// The least a run will aim for, before it has measured anything.
///
/// Public because a run says what it is about to do before it does it, and the
/// number it prints has to be the one the loop starts from.
pub fn margin_floor(period: Nanos) -> Nanos {
    margin_for(Nanos::ZERO, period)
}

impl PhaseEstimator {
    pub fn new(period: Nanos) -> PhaseEstimator {
        PhaseEstimator {
            period,
            margin: margin_floor(period),
            spread: None,
            // Sized for one decision's worth of 120 fps stream, so a batch
            // never grows on the thread that is measuring.
            batch: Vec::with_capacity(128),
            highest: FrameId::NONE,
            settled_at: None,
            visited: 0,
            trace: Vec::new(),
            untraced: 0,
            samples: 0,
            decisions: 0,
            asked: 0,
            holds: 0,
            declined: 0,
            first_phase: None,
            last_phase: None,
            last_delay: None,
            last_reason: None,
        }
    }

    /// Takes one frame's presentation wait and the instant the display link
    /// picked it up. Frames already accounted for, and frames from before the
    /// last shift landed, are ignored rather than averaged in.
    pub fn observe(&mut self, frame: FrameId, wait: Nanos, taken_at: Timestamp) {
        if frame.get() <= self.highest.get() {
            return;
        }
        self.highest = frame;
        if self.settled_at.is_some_and(|settled| taken_at < settled) {
            return;
        }
        self.samples += 1;
        self.batch.push(Sample {
            phase: Nanos(wait.get() % self.period.get()),
            taken_at,
        });
    }

    /// How much stream the current batch covers, which is what says whether it
    /// is time to decide.
    pub fn span(&self) -> Nanos {
        match (self.batch.first(), self.batch.last()) {
            (Some(first), Some(last)) => last.taken_at.saturating_since(first.taken_at),
            _ => Nanos::ZERO,
        }
    }

    /// Judges the batch and starts a fresh one.
    ///
    /// Asking is all this does. Whether the request reaches a host is the
    /// caller's business, which is what makes an observing run the same
    /// measurement as an acting one rather than a different code path.
    pub fn decide(&mut self) -> Decision {
        // Read before the batch is judged, because judging clears it and the
        // entry has to be stamped with the evidence rather than with the clock.
        let newest = self.batch.last().map(|sample| sample.taken_at);
        let decision = self.judge();
        self.decisions += 1;
        match decision {
            Decision::Shift(delay) => {
                self.asked += 1;
                self.last_delay = Some(delay);
                self.record(newest, Some(delay));
            }
            Decision::Hold { .. } => {
                self.holds += 1;
                self.record(newest, None);
            }
            Decision::Declined(reason) => {
                self.declined += 1;
                self.last_reason = Some(reason);
            }
        }
        self.batch.clear();
        decision
    }

    /// Appends one decision to the series, or counts it as untraced.
    fn record(&mut self, at: Option<Timestamp>, delay: Option<Nanos>) {
        let (Some(at), Some(phase)) = (at, self.last_phase) else {
            return;
        };
        if self.trace.len() >= TRACE_CAPACITY {
            self.untraced += 1;
            return;
        }
        self.trace.push(Traced {
            at,
            phase,
            margin: self.margin,
            delay,
            sent: false,
        });
    }

    /// Told after a shift has actually gone out, at the instant it did.
    ///
    /// Samples from either side of that instant carry two different phases, so
    /// the next batch starts once the new one is in force. A decision that was
    /// only observed never calls this, because nothing moved and there is
    /// nothing to wait out.
    ///
    /// It is also what marks the series: an entry says whether its delay reached
    /// the wire, so a reader can see a request that was refused as distinct from
    /// one that was withheld.
    pub fn shifted(&mut self, at: Timestamp) {
        self.settled_at = Some(at.add(SETTLE));
        if let Some(last) = self.trace.last_mut() {
            last.sent = true;
        }
    }

    fn judge(&mut self) -> Decision {
        if self.batch.len() < MIN_SAMPLES {
            return Decision::Declined(Reason::TooFewSamples {
                have: self.batch.len(),
                need: MIN_SAMPLES,
            });
        }
        let (cadenced, intervals) = self.cadence();
        if f64::from(cadenced) < f64::from(intervals) * MIN_CADENCED {
            return Decision::Declined(Reason::NotCadenced {
                cadenced,
                of: intervals,
            });
        }
        let centre = centre(&self.batch, self.period);
        if centre.concentration < MIN_CONCENTRATION {
            return Decision::Declined(Reason::NoSinglePhase {
                concentration: (centre.concentration * 1000.0) as u32,
            });
        }

        self.first_phase.get_or_insert(centre.phase);
        self.last_phase = Some(centre.phase);
        // One bit per sixteenth of the period, so this run can afterwards be
        // told apart from one whose phase was left to beat through all of it.
        let bin = centre.phase.get() * u64::from(COVERAGE_BINS) / self.period.get();
        self.visited |= 1 << bin.min(u64::from(COVERAGE_BINS - 1));
        // The aim is revised before the error is taken, so a batch is judged
        // against the margin its own jitter justifies rather than against the
        // one the last batch needed. The estimate of the scatter is itself
        // accurate to about a fifteenth of its own size over a hundred samples,
        // which puts the resulting movement of the aim below the dead zone: an
        // aim that breathes with the link does not by itself ask for anything.
        self.margin = margin_for(centre.spread, self.period);
        self.spread = Some(centre.spread);

        let error = error_ns(centre.phase, self.margin, self.period);
        if error.unsigned_abs() <= DEADZONE.get() {
            return Decision::Hold {
                phase: centre.phase,
            };
        }
        let delay = delay_for((error as f64 * GAIN) as i64, self.period);
        if delay == Nanos::ZERO {
            // A correction that rounds away is not worth a message.
            return Decision::Hold {
                phase: centre.phase,
            };
        }
        Decision::Shift(delay)
    }

    /// How many of the batch's pickup intervals sit within tolerance of one
    /// period, and how many intervals there were.
    fn cadence(&self) -> (u32, u32) {
        let tolerance = (self.period.get() as f64 * CADENCE_TOLERANCE) as u64;
        let low = self.period.get().saturating_sub(tolerance);
        let high = self.period.get() + tolerance;
        let mut cadenced = 0;
        let mut intervals = 0;
        for pair in self.batch.windows(2) {
            let gap = pair[1].taken_at.saturating_since(pair[0].taken_at).get();
            intervals += 1;
            if gap >= low && gap <= high {
                cadenced += 1;
            }
        }
        (cadenced, intervals)
    }

    /// What the estimator measured and asked for over the whole run.
    ///
    /// It reports nothing as sent, because it has sent nothing: the estimator
    /// asks and the caller owns the wire. That caller fills in what actually
    /// went out and which state the run was in, which is what makes an observing
    /// run the same measurement as an acting one rather than a second code path.
    pub fn summary(&self) -> Summary {
        Summary {
            state: State::Ran,
            trace: self.trace.clone(),
            untraced: self.untraced,
            margin: self.margin,
            margin_floor: margin_floor(self.period),
            spread: self.spread,
            coverage: f64::from(self.visited.count_ones()) / f64::from(COVERAGE_BINS),
            samples: self.samples,
            decisions: self.decisions,
            asked: self.asked,
            sent: 0,
            holds: self.holds,
            declined: self.declined,
            first_phase: self.first_phase,
            last_phase: self.last_phase,
            last_delay: self.last_delay,
            last_reason: self.last_reason,
            send_errors: 0,
        }
    }
}

/// The circular mean of a batch's phases, and how far they scatter around it.
struct Centre {
    phase: Nanos,
    /// Length of the mean unit vector: one when every sample shares a phase,
    /// near zero when they are spread around the period.
    concentration: f64,
    /// Standard deviation of the phases, which is the jitter the margin has to
    /// absorb. Taken from the same vector sum as everything else here: for a
    /// mean resultant length R the circular deviation is `sqrt(-2 ln R)`
    /// radians, so a batch that clusters tightly reports a small number and one
    /// smeared by a bursty link reports a large one, with nothing new measured
    /// to get it.
    spread: Nanos,
}

/// Averages phases the only way a phase can be averaged.
///
/// An arithmetic mean of durations folded into a period is wrong exactly where
/// it matters most. A batch sitting on the wrap, half its samples at 8.30 ms
/// and half at 0.03 ms, has a phase within 0.06 ms of zero and a mean of
/// 4.17 ms: the far side of the period, and the one place a correction must
/// never aim. Treating each sample as a unit vector at its own angle and
/// averaging those gives the phase, and gives the length of the result as a
/// free measure of how much of a phase it is.
fn centre(batch: &[Sample], period: Nanos) -> Centre {
    let turn = core::f64::consts::TAU / period.get() as f64;
    let mut cosines = 0.0;
    let mut sines = 0.0;
    for sample in batch {
        let angle = sample.phase.get() as f64 * turn;
        cosines += angle.cos();
        sines += angle.sin();
    }
    let mut angle = sines.atan2(cosines);
    if angle < 0.0 {
        angle += core::f64::consts::TAU;
    }
    // Clamped because a batch of identical phases sums to a resultant of
    // exactly one, and floating point can land a hair above it, where the
    // logarithm below would go negative and the root would be NaN.
    let concentration = (cosines.hypot(sines) / batch.len() as f64).clamp(f64::MIN_POSITIVE, 1.0);
    Centre {
        phase: Nanos(((angle / turn) as u64) % period.get()),
        concentration,
        spread: Nanos(((-2.0 * concentration.ln()).sqrt() / turn) as u64),
    }
}

/// The margin this batch's own jitter says it needs.
///
/// A margin is a bet on how late a frame can turn up relative to where the
/// estimate puts it, and the batch that produced the estimate already says how
/// late that is. Multiplying its spread covers the tail; the floor stops a
/// quiet stretch of link from aiming at the deadline itself; the ceiling stops a
/// bursty one from asking for more delay than the wait it is buying out of.
///
/// Clamped in that order rather than with a single clamp, because at a high
/// enough frame rate the ceiling falls below the floor and the rate has to win.
///
/// There is a second bound on this that does not appear in the arithmetic: a
/// batch has to cluster to [`MIN_CONCENTRATION`] before it is believed at all,
/// and a resultant of 0.80 is a deviation of 0.89 ms, so a 120 Hz run can never
/// be aimed more than 2.66 ms ahead however bad the link looks. A link that
/// would need more than that is refused rather than aimed at, which is the
/// right way round: an aim that large is most of the wait it was trying to
/// remove.
fn margin_for(spread: Nanos, period: Nanos) -> Nanos {
    let measured = Nanos((spread.get() as f64 * MARGIN_SIGMAS) as u64);
    Nanos(
        measured
            .get()
            .max(MARGIN_FLOOR.get())
            .min(period.get() / MARGIN_CEILING_SHARE),
    )
}

/// Shortest signed distance from `phase` to `target`, positive when the frame
/// was ready earlier than it is wanted.
///
/// Signed and shortest because a phase is a point on a circle: a phase 0.1 ms
/// short of the target is 0.1 ms of error, not a period minus 0.1 ms of it, and
/// a loop that could not say so would take the long way round on every
/// correction. Choosing the short way costs nothing even when it points
/// backwards, because a backwards correction is served by a delay of nearly a
/// whole period, and the repeated refresh that buys is the cycle slip the two
/// clock rates were going to force regardless.
fn error_ns(phase: Nanos, target: Nanos, period: Nanos) -> i64 {
    let period = period.get() as i64;
    let mut error = phase.get() as i64 - target.get() as i64;
    if error > period / 2 {
        error -= period;
    } else if error <= -period / 2 {
        error += period;
    }
    error
}

/// Expresses a correction the only way the host can obey it.
///
/// A negative correction asks for a frame earlier than one already produced,
/// which is a request about the past. Delaying by one period less the amount
/// reaches the same phase and is always in the future, which is why the wire
/// carries an unsigned delay and why this is the only place the sign
/// disappears.
fn delay_for(correction_ns: i64, period: Nanos) -> Nanos {
    Nanos(correction_ns.rem_euclid(period.get() as i64) as u64)
}

/// What the loop was doing, which no count of shifts can say on its own.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    /// `--phase-align off`: nothing measured, nothing sent. The negative
    /// control for the mechanism.
    Off,
    /// `--phase-align observe`: measured exactly as an acting run measures, and
    /// deliberately silent.
    ///
    /// Two jobs. It settles a question that has now been derived wrongly twice -
    /// which way a held capture tick moves the phase measured here - by letting
    /// one shift be applied by hand and watched. And it is the control the
    /// comparison actually needs: a run's starting phase is an independent draw,
    /// so an arm that happens to begin where alignment aims proves a favourable
    /// draw rather than a working mechanism, while an untouched arm shows the
    /// distribution the acting arm has to be read against.
    Observing,
    /// The run had no host to ask or no display link to align to.
    Unavailable(&'static str),
    Ran,
}

/// What the estimator did, for the report.
#[derive(Clone)]
pub struct Summary {
    pub state: State,
    /// Every believable decision in order: when it was measured, the phase it
    /// measured, the aim it was judged against and what it asked for.
    ///
    /// This is what makes a drifting link readable. A first and a last phase say
    /// nothing about a step applied in the middle when the pair of clocks sweeps
    /// two periods across the run, and the two pictures the comparison actually
    /// needs - a phase sweeping the period when untouched, a phase held when
    /// acted on - are both properties of the series rather than of its ends.
    pub trace: Vec<Traced>,
    /// Decisions past the series' capacity. Non-zero means the series stopped
    /// growing, which is not the same as a phase that stopped moving.
    pub untraced: u64,
    /// The margin the run ended up aiming for. Chosen from measured jitter, so
    /// it is the number a run has to be read against; the one it started from is
    /// `margin_floor`.
    pub margin: Nanos,
    pub margin_floor: Nanos,
    /// Scatter of the phases the last margin was chosen from.
    pub spread: Option<Nanos>,
    pub samples: u64,
    pub decisions: u64,
    /// Share of the period the measured phase was seen in, in sixteenths. An
    /// untouched pair of clocks beats through all of it about every 33 s, so an
    /// observing run of a few minutes should approach one and a held phase
    /// should stay near a sixteenth.
    pub coverage: f64,
    /// Decisions that produced a shift, whether or not one went out.
    pub asked: u64,
    /// Shifts that actually reached the wire. Zero while observing, by
    /// construction rather than by accident.
    pub sent: u64,
    pub holds: u64,
    pub declined: u64,
    /// The phase the first believable batch found: where this pair of clocks
    /// happened to sit before anything was asked of them.
    pub first_phase: Option<Nanos>,
    pub last_phase: Option<Nanos>,
    pub last_delay: Option<Nanos>,
    pub last_reason: Option<Reason>,
    /// Shifts the control connection would not carry. Advisory messages, so a
    /// failure is counted rather than raised.
    pub send_errors: u64,
}

impl Summary {
    pub fn off() -> Summary {
        Summary::idle(State::Off)
    }

    pub fn unavailable(why: &'static str) -> Summary {
        Summary::idle(State::Unavailable(why))
    }

    fn idle(state: State) -> Summary {
        Summary {
            state,
            trace: Vec::new(),
            untraced: 0,
            margin: Nanos::ZERO,
            margin_floor: Nanos::ZERO,
            spread: None,
            samples: 0,
            decisions: 0,
            coverage: 0.0,
            asked: 0,
            sent: 0,
            holds: 0,
            declined: 0,
            first_phase: None,
            last_phase: None,
            last_delay: None,
            last_reason: None,
            send_errors: 0,
        }
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state {
            State::Off => return write!(f, "off: nothing measured, nothing sent"),
            State::Unavailable(why) => return write!(f, "did not run: {why}"),
            State::Observing | State::Ran if self.decisions == 0 => {
                return write!(f, "ran, no decision reached ({} samples)", self.samples);
            }
            State::Observing => write!(f, "observing, sent nothing: ")?,
            State::Ran => {}
        }
        match (self.first_phase, self.last_phase) {
            (Some(first), Some(last)) => write!(f, "phase {first} -> {last}")?,
            _ => write!(f, "phase never measured")?,
        }
        // Coverage before the counts, because in an observing run it is the
        // measurement: a phase left alone sweeps the period, and how much of the
        // period it swept is what an acting run has to be compared against.
        write!(f, " over {:.0}% of the period", self.coverage * 100.0)?;
        match self.spread {
            Some(spread) => write!(
                f,
                ", margin {} from a {spread} spread (floor {})",
                self.margin, self.margin_floor
            )?,
            None => write!(f, ", margin {} unmeasured", self.margin_floor)?,
        }
        match self.state {
            State::Observing => write!(f, ", {} shifts withheld", self.asked)?,
            _ => write!(f, ", {} shifts", self.sent)?,
        }
        write!(f, ", {} holds, {} declined", self.holds, self.declined)?;
        // The count, because a series that stopped growing must not read as a
        // phase that stopped moving.
        write!(f, ", {} traced", self.trace.len())?;
        if self.untraced > 0 {
            write!(f, " ({} past the trace's end)", self.untraced)?;
        }
        if let Some(delay) = self.last_delay {
            write!(f, ", last asked {delay}")?;
        }
        if self.send_errors > 0 {
            write!(f, ", {} not sent", self.send_errors)?;
        }
        if let Some(reason) = self.last_reason {
            write!(f, " (last refusal: {reason})")?;
        }
        Ok(())
    }
}

impl From<&Summary> for crate::report::Phase {
    fn from(summary: &Summary) -> crate::report::Phase {
        crate::report::Phase {
            mode: match summary.state {
                State::Off => "off",
                State::Observing => "observe",
                State::Unavailable(_) | State::Ran => "on",
            },
            // True whenever the loop was asked for and measured, which includes
            // observing: an observing arm is not a control for the mechanism,
            // because the mechanism is what it is running.
            enabled: summary.state != State::Off,
            ran: matches!(summary.state, State::Ran | State::Observing),
            unavailable_reason: match summary.state {
                State::Unavailable(why) => Some(why.to_owned()),
                _ => None,
            },
            margin_ms: summary.margin.as_millis_f64(),
            margin_floor_ms: summary.margin_floor.as_millis_f64(),
            // Zero rather than absent when nothing was measured, so a reader
            // that formats this as a number cannot be handed a null. `decisions`
            // is what tells an unmeasured run from a very steady one.
            spread_ms: summary.spread.unwrap_or(Nanos::ZERO).as_millis_f64(),
            phase_coverage: summary.coverage,
            samples: summary.samples,
            decisions: summary.decisions,
            shifts: summary.sent,
            shifts_withheld: summary.asked - summary.sent,
            holds: summary.holds,
            declined: summary.declined,
            first_phase_ms: summary.first_phase.map(Nanos::as_millis_f64),
            last_phase_ms: summary.last_phase.map(Nanos::as_millis_f64),
            last_delay_ms: summary.last_delay.map(Nanos::as_millis_f64),
            last_refusal: summary.last_reason.map(|reason| reason.to_string()),
            send_errors: summary.send_errors,
            trace_entries: summary.trace.len(),
            trace_dropped: summary.untraced,
            // Sampled here rather than during the run: the monotonic clock counts
            // through sleep, so one pairing with the wall clock stays valid for
            // the whole of it. Two bases, an offset between them, and nothing
            // subtracted across the two anywhere else.
            clock_epoch_at_ns: Timestamp::now().as_nanos(),
            clock_epoch_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |since| since.as_secs_f64() * 1_000.0),
            trace: {
                let first = summary.trace.first().map(|entry| entry.at);
                summary
                    .trace
                    .iter()
                    .map(|entry| crate::report::PhaseSample {
                        at_ns: entry.at.as_nanos(),
                        at_s: first
                            .map_or(0.0, |first| entry.at.saturating_since(first).as_secs_f64()),
                        phase_ms: entry.phase.as_millis_f64(),
                        margin_ms: entry.margin.as_millis_f64(),
                        delay_ms: entry.delay.map(Nanos::as_millis_f64),
                        sent: entry.sent,
                    })
                    .collect()
            },
        }
    }
}

/// Runs the loop for the length of a run, on its own thread.
///
/// Off the media path by construction: it reads finished timelines from the
/// collector and writes to the control connection, and touches neither the
/// decoder nor the renderer.
///
/// `control` is what separates an acting run from an observing one, and it is
/// the only difference between them: without a connection the same measurements
/// are taken, the same decisions are reached, and nothing is sent. An observing
/// run therefore needs no host at all, which is what lets it be the control for
/// a run that has one.
pub fn spawn(
    telemetry: Arc<Telemetry>,
    control: Option<Arc<Mutex<ControlClient>>>,
    period: Nanos,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<Summary> {
    thread::Builder::new()
        .name("phase".into())
        .spawn(move || {
            if let Some(control) = &control {
                // A stalled control peer must cost this thread a bounded wait.
                // The handshake left a minute on the socket, which is right for
                // a configuration exchange and wrong for an advisory message.
                let _ = control.lock().set_timeout(CONTROL_TIMEOUT);
            }

            let mut estimator = PhaseEstimator::new(period);
            let mut sent = 0u64;
            let mut send_errors = 0u64;
            let mut complained = false;
            while !stop.load(Ordering::Acquire) {
                thread::sleep(HARVEST);
                harvest(&telemetry, &mut estimator);
                if estimator.span() < DECIDE_EVERY {
                    continue;
                }
                let Decision::Shift(delay) = estimator.decide() else {
                    continue;
                };
                let Some(control) = &control else {
                    // Observing. The decision is counted and the phase is left
                    // exactly where it was, which is the whole point.
                    continue;
                };
                let message = ControlMessage::PhaseShift {
                    // Under one period by construction, so the cast cannot lose
                    // anything a period could hold.
                    delay_nanos: delay.get().min(u64::from(u32::MAX)) as u32,
                };
                match control.lock().send(&message) {
                    Ok(()) => {
                        sent += 1;
                        // Only now has the phase been asked to move, so only now
                        // does the settle window start.
                        estimator.shifted(Timestamp::now());
                    }
                    Err(error) => {
                        send_errors += 1;
                        if !complained {
                            complained = true;
                            println!("phase: control connection refused a shift: {error}");
                        }
                    }
                }
            }
            Summary {
                state: if control.is_some() {
                    State::Ran
                } else {
                    State::Observing
                },
                sent,
                send_errors,
                ..estimator.summary()
            }
        })
        .expect("spawn phase estimator")
}

/// Feeds the estimator every frame the collector has finished with.
///
/// Both halves of a sample come from the collector's own marks. A frame missing
/// either has nothing to say about the phase: a superseded frame never reached
/// the display link at all.
///
/// Four times a second, so copying the collector's recent ring rather than
/// asking it for a narrower view costs a few tens of kilobytes a second on a
/// thread that is doing nothing else, against a public surface the collector
/// would have to grow to avoid it.
fn harvest(telemetry: &Telemetry, estimator: &mut PhaseEstimator) {
    for timeline in telemetry.recent_frames() {
        let Some(wait) = timeline.segment(Segment::PresentationWait) else {
            continue;
        };
        let Some(taken_at) = timeline.at(Stage::RenderSubmit) else {
            continue;
        };
        estimator.observe(timeline.frame(), wait, taken_at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 120 Hz period.
    const PERIOD: Nanos = Nanos(8_333_333);
    /// Where a synthetic run's display link starts ticking.
    const BASE: u64 = 1_000_000_000;

    /// Feeds frames that were all ready `phase` in front of a display link
    /// ticking exactly one period apart, and returns the next free tick.
    fn feed(estimator: &mut PhaseEstimator, phase: Nanos, frames: u64, from: u64) -> u64 {
        for tick in from..from + frames {
            estimator.observe(
                FrameId::new(tick + 1),
                phase,
                Timestamp::from_nanos(BASE + tick * PERIOD.get()),
            );
        }
        from + frames
    }

    /// The host obeying a shift: every later frame's phase moves back by the
    /// delay, wrapping at the period.
    fn obeyed(phase: Nanos, delay: Nanos) -> Nanos {
        Nanos((phase.get() + PERIOD.get() - delay.get() % PERIOD.get()) % PERIOD.get())
    }

    /// One decision's worth of stream at `phase`, and the phase it produces.
    ///
    /// Stands in for the driver as well as for the host: a shift it takes is a
    /// shift that went out, so the estimator is told, exactly as the loop tells
    /// it when the wire accepts one.
    fn step(estimator: &mut PhaseEstimator, phase: Nanos, tick: &mut u64) -> (Decision, Nanos) {
        *tick = feed(estimator, phase, 120, *tick);
        let now = Timestamp::from_nanos(BASE + *tick * PERIOD.get());
        let decision = estimator.decide();
        let next = match decision {
            Decision::Shift(delay) => {
                estimator.shifted(now);
                obeyed(phase, delay)
            }
            _ => phase,
        };
        (decision, next)
    }

    /// What the sign experiment depends on: a step in the phase has to be
    /// visible in the series against the drift around it, and each entry has to
    /// be stamped with the evidence it came from rather than with anything else.
    #[test]
    fn the_trace_locates_a_step_against_the_drift_around_it() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        // A quarter of a millisecond a second of drift, which is what the live
        // link showed, and a 3.00 ms step applied a third of the way in. Nothing
        // is sent: this is what an observing arm sees while somebody moves the
        // producer by hand.
        let creep = Nanos(250_000 / 2); // per half-second decision
        let step = Nanos::from_micros(3_000);
        let mut phase = Nanos::from_micros(1_000);
        let mut tick = 0u64;
        for decision in 0..12u64 {
            phase = Nanos((phase.get() + creep.get()) % PERIOD.get());
            if decision == 4 {
                phase = obeyed(phase, step);
            }
            tick = feed(&mut estimator, phase, 120, tick);
            assert!(matches!(estimator.decide(), Decision::Shift(_)));
        }
        let summary = estimator.summary();
        assert_eq!(summary.trace.len(), 12);
        assert_eq!(summary.untraced, 0);

        // Every entry carries the newest sample it was computed from, so the
        // series can be lined up with an event by time rather than by counting.
        for pair in summary.trace.windows(2) {
            assert!(pair[1].at > pair[0].at, "the series is not in time order");
        }
        // Nothing was sent, so nothing claims to have been.
        assert!(summary.trace.iter().all(|entry| !entry.sent));

        // The step is visible where it happened and only there: the drift moves
        // the phase by 0.125 ms between neighbouring entries, while the step
        // moves it by 3 ms, and a delay moves the phase down.
        let moves: Vec<i64> = summary
            .trace
            .windows(2)
            .map(|pair| error_ns(pair[1].phase, pair[0].phase, PERIOD))
            .collect();
        let (stepped, quiet): (Vec<i64>, Vec<i64>) = moves
            .iter()
            .partition(|move_| move_.unsigned_abs() > 1_000_000);
        assert_eq!(
            stepped.len(),
            1,
            "found {} steps in {moves:?}",
            stepped.len()
        );
        assert_eq!(stepped[0], -(step.get() as i64) + creep.get() as i64);
        // Against the creep plus a microsecond, because a phase recovered from a
        // circular mean lands a nanosecond or two either side of the truth and
        // the claim being made is about milliseconds.
        assert!(
            quiet
                .iter()
                .all(|move_| move_.unsigned_abs() <= creep.get() + 1_000),
            "drift between entries exceeded the creep: {quiet:?}"
        );
    }

    #[test]
    fn a_series_that_stopped_growing_says_so() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        let mut tick = 0u64;
        // Two decisions past the cap, so the count and the overflow both have to
        // be reported rather than the series quietly ending.
        for _ in 0..TRACE_CAPACITY + 2 {
            tick = feed(&mut estimator, Nanos::from_micros(5_000), 120, tick);
            estimator.decide();
        }
        let summary = estimator.summary();
        assert_eq!(summary.trace.len(), TRACE_CAPACITY);
        assert_eq!(summary.untraced, 2);
        assert_eq!(summary.decisions, TRACE_CAPACITY as u64 + 2);
        assert!(format!("{summary}").contains("past the trace's end"));
    }

    #[test]
    fn a_correction_is_always_a_delay_inside_one_period() {
        // Half a period of error either way comes back as a delay inside one
        // period: the wire has no way to say "earlier".
        for error in [-4_000_000i64, -1, 1, 4_000_000] {
            let delay = delay_for(error, PERIOD);
            assert!(
                delay < PERIOD,
                "a delay of {delay} is not inside one period"
            );
        }
        assert_eq!(delay_for(1_000_000, PERIOD), Nanos(1_000_000));
        assert_eq!(
            delay_for(-1_000_000, PERIOD),
            Nanos(PERIOD.get() - 1_000_000)
        );
    }

    #[test]
    fn phase_error_takes_the_short_way_round_the_period() {
        let target = Nanos::from_micros(2_000);
        // Just past the target is a small positive error, not almost a period
        // of negative one.
        assert_eq!(error_ns(Nanos::from_micros(2_100), target, PERIOD), 100_000);
        assert_eq!(
            error_ns(Nanos::from_micros(1_900), target, PERIOD),
            -100_000
        );
        // On the far side of the wrap from the target, which is where an
        // unsigned subtraction would send the loop the long way round.
        let error = error_ns(Nanos(PERIOD.get() - 100_000), target, PERIOD);
        assert_eq!(error, -2_100_000);
        // And the correction for it is still a delay inside one period.
        assert!(delay_for(error, PERIOD) < PERIOD);
    }

    #[test]
    fn a_phase_on_the_wrap_averages_to_the_wrap_and_not_to_its_far_side() {
        // Half the frames land 30 µs after the boundary and half 30 µs before
        // it. The phase is within 30 µs of zero; an arithmetic mean would call
        // it half a period, which is the one answer that must never be sent.
        let batch: Vec<Sample> = (0..64)
            .map(|index| Sample {
                phase: if index % 2 == 0 {
                    Nanos(30_000)
                } else {
                    Nanos(PERIOD.get() - 30_000)
                },
                taken_at: Timestamp::from_nanos(BASE + index * PERIOD.get()),
            })
            .collect();
        let centre = centre(&batch, PERIOD);
        let distance = error_ns(centre.phase, Nanos::ZERO, PERIOD).unsigned_abs();
        assert!(
            distance < 40_000,
            "phase {} is not on the wrap",
            centre.phase
        );
        assert!(centre.concentration > 0.9);
    }

    #[test]
    fn the_margin_sits_in_front_of_the_deadline_by_more_than_the_jitter() {
        // A frame aimed at the floor still has time in hand when the decoder is
        // as slow as its p99 rather than its p50: the reference run measured
        // 1.04 ms and 2.26 ms, so 1.22 ms of the cushion is spoken for, and the
        // loop's own residual is the dead zone plus the creep of one decision.
        // An aim at or past the deadline would spend a whole period on a frame
        // that missed by microseconds.
        assert!(margin_floor(PERIOD) > Nanos::from_micros(1_220) + DEADZONE);
        // Never more than doing nothing costs, which is half a period.
        assert!(margin_for(Nanos::from_micros(5_000), PERIOD) < Nanos(PERIOD.get() / 2));
        // And a rate the floor does not fit inside gives way to the rate.
        assert_eq!(
            margin_floor(Nanos(4_166_666)),
            Nanos(4_166_666 / MARGIN_CEILING_SHARE)
        );
    }

    #[test]
    fn a_wide_spread_widens_the_margin_and_a_narrow_one_keeps_the_floor() {
        // The failure this exists for: a fixed 2.00 ms aim on a link whose
        // arrivals reached 30.95 ms at p99 put frames close enough to the
        // deadline that some missed it, and each miss waited a whole period and
        // showed a stale refresh. A batch that scatters has to be aimed further
        // ahead, and the scatter is the only evidence of by how much.
        let tight = margin_for(Nanos::from_micros(100), PERIOD);
        let loose = margin_for(Nanos::from_micros(800), PERIOD);
        assert_eq!(tight, margin_floor(PERIOD), "a quiet link lost its floor");
        assert!(
            loose > tight,
            "a spread of 0.8 ms bought no more margin than one of 0.1 ms"
        );
        assert_eq!(loose, Nanos::from_micros(2_400));
        // Three deviations of it, so the tail the margin is covering is inside.
        assert!(loose >= Nanos((800.0 * MARGIN_SIGMAS) as u64 * 1_000));
        // A link bad enough to ask for more than doing nothing costs is capped
        // rather than obeyed.
        assert_eq!(
            margin_for(Nanos::from_micros(4_000), PERIOD),
            Nanos(PERIOD.get() / MARGIN_CEILING_SHARE)
        );
    }

    #[test]
    fn the_margin_a_batch_gets_is_the_one_its_own_scatter_justifies() {
        // End to end through the estimator rather than through the arithmetic: a
        // batch whose phases sweep 1.3 ms either side of 5 ms must be judged
        // against a wider aim than one that does not scatter at all. Swept
        // rather than split between two extremes, because a batch split that far
        // apart clusters too loosely to be believed at all and would be refused
        // instead - which is the right answer for it, and not what is under test
        // here.
        let mut jittery = PhaseEstimator::new(PERIOD);
        for tick in 0..120u64 {
            let wobble = (tick as i64 % 21 - 10) * 130_000;
            jittery.observe(
                FrameId::new(tick + 1),
                Nanos((5_000_000 + wobble) as u64),
                Timestamp::from_nanos(BASE + tick * PERIOD.get()),
            );
        }
        assert!(matches!(jittery.decide(), Decision::Shift(_)));
        let jittery = jittery.summary();

        let mut steady = PhaseEstimator::new(PERIOD);
        feed(&mut steady, Nanos::from_micros(5_000), 120, 0);
        assert!(matches!(steady.decide(), Decision::Shift(_)));
        let steady = steady.summary();

        assert!(
            jittery.spread.expect("a spread") > steady.spread.expect("a spread"),
            "the scatter was not seen"
        );
        assert!(
            jittery.margin > steady.margin,
            "a jittery batch was aimed no further ahead than a clean one: {} against {}",
            jittery.margin,
            steady.margin
        );
        assert_eq!(steady.margin, steady.margin_floor);
        // The floor is 2.00 ms and a 0.79 ms scatter asks for 2.36 ms of aim.
        assert!(
            jittery.margin > Nanos::from_micros(2_300),
            "0.8 ms of scatter bought only {}",
            jittery.margin
        );
    }

    #[test]
    fn a_handful_of_frames_is_refused_rather_than_acted_on() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        feed(&mut estimator, Nanos::from_micros(6_000), 12, 0);
        assert_eq!(
            estimator.decide(),
            Decision::Declined(Reason::TooFewSamples {
                have: 12,
                need: MIN_SAMPLES
            })
        );
        let summary = estimator.summary();
        assert_eq!(summary.asked, 0);
        assert_eq!(summary.declined, 1);
        assert!(summary.last_phase.is_none());
    }

    #[test]
    fn a_run_whose_display_link_is_not_cadenced_is_refused() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        // Plenty of samples, all with one sharp phase, but picked up eight
        // periods apart: a suspended link, an occluded window, or a panel
        // running at a fraction of the source rate. The phase is sharp and
        // means nothing.
        for tick in 0..120u64 {
            estimator.observe(
                FrameId::new(tick + 1),
                Nanos::from_micros(6_000),
                Timestamp::from_nanos(BASE + tick * PERIOD.get() * 8),
            );
        }
        match estimator.decide() {
            Decision::Declined(Reason::NotCadenced { cadenced, of }) => {
                assert_eq!(cadenced, 0);
                assert_eq!(of, 119);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(estimator.summary().asked, 0);
    }

    #[test]
    fn a_batch_with_no_single_phase_is_refused() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        // Cadenced pickups, phases spread right around the period: nothing for
        // a correction to aim at.
        for tick in 0..120u64 {
            estimator.observe(
                FrameId::new(tick + 1),
                Nanos(tick * PERIOD.get() / 120),
                Timestamp::from_nanos(BASE + tick * PERIOD.get()),
            );
        }
        match estimator.decide() {
            Decision::Declined(Reason::NoSinglePhase { .. }) => {}
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_fixed_error_converges_without_ringing() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        // Every batch below is fed one phase with no scatter in it, so the
        // measured margin stays at its floor and the aim is a fixed
        // setpoint: what is under test here is the loop, not the margin.
        let target = margin_floor(PERIOD);
        // The worst starting phase there is: a frame ready as early as it can
        // be, waiting almost the whole period out.
        let mut phase = Nanos(PERIOD.get() - 100_000);
        let mut tick = 0u64;
        let mut previous = error_ns(phase, target, PERIOD).unsigned_abs();
        let sign = error_ns(phase, target, PERIOD).signum();
        let mut shifts = 0;
        let mut settled = false;
        for _ in 0..16 {
            let (decision, next) = step(&mut estimator, phase, &mut tick);
            phase = next;
            let error = error_ns(phase, target, PERIOD);
            match decision {
                Decision::Shift(_) => {
                    shifts += 1;
                    assert!(
                        error.unsigned_abs() < previous,
                        "error grew from {previous} to {}",
                        error.unsigned_abs()
                    );
                    // A loop that overshoots rings, and ringing would show up
                    // here as the error changing side.
                    if error != 0 {
                        assert_eq!(error.signum(), sign, "the correction overshot the target");
                    }
                    previous = error.unsigned_abs();
                }
                Decision::Hold { .. } => {
                    settled = true;
                    break;
                }
                Decision::Declined(reason) => panic!("refused a clean batch: {reason}"),
            }
        }
        assert!(shifts > 0, "the loop never asked for anything");
        assert!(settled, "the loop never settled");
        assert!(
            error_ns(phase, target, PERIOD).unsigned_abs() <= DEADZONE.get(),
            "settled at {phase}, wanted {target}"
        );
    }

    #[test]
    fn a_drifting_phase_is_tracked_rather_than_left_behind() {
        let mut estimator = PhaseEstimator::new(PERIOD);
        // Every batch below is fed one phase with no scatter in it, so the
        // measured margin stays at its floor and the aim is a fixed
        // setpoint: what is under test here is the loop, not the margin.
        let target = margin_floor(PERIOD);
        // 119.97 Hz against a 120.00 Hz source: the phase creeps by about two
        // microseconds a frame, which is a quarter of a millisecond across the
        // 120 frames a decision is taken from.
        let creep = Nanos(2_083 * 120);
        let mut phase = target;
        let mut tick = 0u64;
        let mut worst = 0u64;
        for _ in 0..200 {
            phase = Nanos((phase.get() + creep.get()) % PERIOD.get());
            let (decision, next) = step(&mut estimator, phase, &mut tick);
            assert!(
                !matches!(decision, Decision::Declined(_)),
                "refused a clean batch: {decision:?}"
            );
            phase = next;
            worst = worst.max(error_ns(phase, target, PERIOD).unsigned_abs());
        }
        // A hundred seconds of run, over which the creep alone would have swept
        // the phase through three whole periods.
        assert!(
            worst < DEADZONE.get() + creep.get() * 2,
            "drifted {worst} ns from the target"
        );
        let summary = estimator.summary();
        assert!(summary.asked > 0, "tracked without asking for anything");
        assert_eq!(summary.declined, 0);
    }

    #[test]
    fn a_run_that_never_ran_the_estimator_is_not_a_run_that_sent_nothing() {
        let off = Summary::off();
        let ran = PhaseEstimator::new(PERIOD).summary();
        assert_eq!(off.asked, ran.asked);
        assert_ne!(off.state, ran.state);
        assert!(format!("{off}").contains("off"));
        assert!(!format!("{ran}").contains("off"));
        let unavailable = Summary::unavailable("no control plane");
        assert!(format!("{unavailable}").contains("no control plane"));
    }

    /// The plumbing the arithmetic cannot cover: marks left by a decoder and a
    /// renderer, assembled by the collector, harvested, judged, and arriving at
    /// a host as a delay it can obey.
    #[test]
    fn a_measured_phase_reaches_a_host_as_a_delay() {
        use std::sync::mpsc;

        use lanplay_telemetry::TelemetryConfig;
        use lanplay_transport::ControlServer;

        const PATIENCE: Nanos = Nanos::from_millis(5_000);

        let server = ControlServer::bind("127.0.0.1:0", "phase-test").expect("bind a host");
        let address = server.local_addr().expect("the host's address");
        let (shifts, received) = mpsc::channel();
        let host = thread::Builder::new()
            .name("phase-test-host".into())
            .spawn(move || {
                let mut session = server.accept_session(PATIENCE).expect("accept");
                while let Ok(Some(message)) = session.next_message(PATIENCE) {
                    if let ControlMessage::PhaseShift { delay_nanos } = message {
                        shifts.send(delay_nanos).expect("report the shift");
                        return;
                    }
                }
            })
            .expect("spawn a host");

        let mut client = ControlClient::connect(address, PATIENCE).expect("connect");
        client.hello("phase-test").expect("hello");

        let telemetry = Arc::new(Telemetry::start(TelemetryConfig::default()));
        let recorder = telemetry.recorder();
        // A second of stream that was ready 6 ms in front of every pickup: a
        // sharp phase, cadenced, and nowhere near where it is wanted. Stamped
        // into the recent past, because the loop is about to read it as though
        // the run had just produced it.
        let phase = Nanos::from_micros(6_000);
        let base = Timestamp::from_nanos(Timestamp::now().as_nanos() - 1_100_000_000);
        for tick in 0..120u64 {
            let frame = FrameId::new(tick + 1);
            let taken_at = base.add(Nanos(tick * PERIOD.get()));
            recorder.mark_at(
                frame,
                Stage::DecodeComplete,
                Timestamp::from_nanos(taken_at.as_nanos() - phase.get()),
            );
            recorder.mark_at(frame, Stage::RenderSubmit, taken_at);
            recorder.mark_at(frame, Stage::PresentSubmit, taken_at.add(Nanos(50_000)));
        }
        assert!(
            telemetry.flush(Duration::from_secs(2)),
            "the collector never caught up"
        );

        let stop = Arc::new(AtomicBool::new(false));
        let loop_thread = spawn(
            Arc::clone(&telemetry),
            Some(Arc::new(Mutex::new(client))),
            PERIOD,
            Arc::clone(&stop),
        );
        let delay = received
            .recv_timeout(Duration::from_secs(5))
            .expect("a shift on the wire");
        stop.store(true, Ordering::Release);
        let summary = loop_thread.join().expect("the phase loop");
        host.join().expect("the host");

        // Four milliseconds too early against a 2 ms aim, halved by the gain,
        // and it arrives as a plain delay.
        assert!(
            (1_900_000..2_100_000).contains(&delay),
            "asked the host for {delay} ns"
        );
        assert_eq!(summary.state, State::Ran);
        assert_eq!(summary.sent, 1);
        assert_eq!(summary.send_errors, 0);
        assert_eq!(summary.first_phase, Some(phase));
    }

    /// The same measurement with the wire left out. This is the arm the
    /// comparison needs: it has to see the phase as clearly as an acting run
    /// does and move it not at all.
    #[test]
    fn an_observing_run_measures_everything_and_sends_nothing() {
        use lanplay_telemetry::TelemetryConfig;

        let telemetry = Arc::new(Telemetry::start(TelemetryConfig::default()));
        let recorder = telemetry.recorder();
        let phase = Nanos::from_micros(6_000);
        let base = Timestamp::from_nanos(Timestamp::now().as_nanos() - 1_100_000_000);
        for tick in 0..120u64 {
            let frame = FrameId::new(tick + 1);
            let taken_at = base.add(Nanos(tick * PERIOD.get()));
            recorder.mark_at(
                frame,
                Stage::DecodeComplete,
                Timestamp::from_nanos(taken_at.as_nanos() - phase.get()),
            );
            recorder.mark_at(frame, Stage::RenderSubmit, taken_at);
            recorder.mark_at(frame, Stage::PresentSubmit, taken_at.add(Nanos(50_000)));
        }
        assert!(
            telemetry.flush(Duration::from_secs(2)),
            "the collector never caught up"
        );

        let stop = Arc::new(AtomicBool::new(false));
        // No control connection at all, which is the only difference from the
        // acting loop above - and the reason an observing arm needs no host.
        let loop_thread = spawn(Arc::clone(&telemetry), None, PERIOD, Arc::clone(&stop));
        // Long enough for a harvest, a decision, and the shift it withholds.
        thread::sleep(Duration::from_millis(600));
        stop.store(true, Ordering::Release);
        let summary = loop_thread.join().expect("the phase loop");

        assert_eq!(summary.state, State::Observing);
        assert_eq!(summary.sent, 0, "an observing run put something on a wire");
        assert_eq!(summary.send_errors, 0);
        assert!(summary.asked > 0, "an observing run reached no decision");
        assert_eq!(summary.first_phase, Some(phase));
        assert!(summary.spread.is_some(), "the scatter was not reported");
        // One sixteenth of the period seen, because the phase was never moved
        // and one batch is one phase. A run left alone for minutes sweeps the
        // rest, and that sweep is what an acting arm is judged against.
        assert!(
            summary.coverage > 0.0 && summary.coverage <= 2.0 / 16.0,
            "coverage of {} from a single held phase",
            summary.coverage
        );
        // And the report says which of the three it was, distinctly.
        let reported = crate::report::Phase::from(&summary);
        assert_eq!(reported.mode, "observe");
        assert!(reported.enabled, "an observing run is not the off control");
        assert!(reported.ran);
        assert_eq!(reported.shifts, 0);
        assert_eq!(reported.shifts_withheld, summary.asked);
        assert_eq!(crate::report::Phase::from(&Summary::off()).mode, "off");
    }
}
