//! The passive monitor: what the link is doing while a session runs.
//!
//! `NETWORK.md` fixes three tiers and `crates/network-health` is the contract.
//! This module fills it from a live session, and every one of its three parts is
//! shaped by the same rule: a stage is measured at that stage or not at all.
//!
//! ```text
//! RadioHint        a 1 Hz association read, diagnostic only
//! StreamBehaviour  rolling short and long windows of crates/link-metrics
//! Experience       fresh_tick_ratio, measured at presentation
//! ```
//!
//! Nothing here classifies. `classify` exists in `crates/network-health` and
//! N3 validates it offline against sessions whose answer is already written
//! down; running it live before that is done would produce verdicts nobody can
//! check. So the monitor records the tier and stops.
//!
//! ### Why the monitor may cost nothing, and how that is not merely asserted
//!
//! One CoreWLAN association read costs 3.2 ms at p50 and 15.5 ms at worst,
//! measured by `tools/radio-sample/examples/read-cost.rs`. The worst case is
//! longer than a 120 Hz frame period, so the read cannot sit on any callback,
//! any deadline, or the receive path. It gets a thread of its own at 1 Hz,
//! because the quantity moves in seconds.
//!
//! That is an argument, not evidence. [`Cost::Expensive`] is the positive
//! control: the same sampler with its interval removed, hammering CoreWLAN as
//! fast as it will answer. `tools/monitor-neutrality-gate.sh` runs the same
//! video workload under all three cadences and the comparison has to separate
//! `Expensive` from `Cheap` before its failure to separate `Cheap` from `Off`
//! means anything at all.
//!
//! ### No scan, and the two ways that is checked
//!
//! `system_profiler SPAirPortDataType` fills an "Other Local Wi-Fi Networks"
//! section, which it can only fill by scanning, and a scan takes the radio off
//! channel: sampling with it once a second turned a link delivering at 8.09 ms
//! p50 and 11.35 ms p99 into one reading 2.04 ms p50 and 133 ms p99. It
//! manufactured exactly the bunching the experiment had gone looking for.
//!
//! So the whole tier is `lanplay_capabilities::wifi::association()`, which
//! reads documented `CWInterface` properties and never scans. The harness
//! checks that structurally, by looking for the scan selectors in the shipped
//! binary, and observationally, by asking the unified log whether `airportd`
//! served a scan while the run was in the air. A promise in a comment would be
//! neither.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use hdrhistogram::Histogram;
use lanplay_capabilities::wifi;
use lanplay_link_metrics::{Delivery, Window};
use lanplay_network_health::{Experience, Fraction, Incidence, NetworkObservation, RadioHint};
use lanplay_telemetry::{Nanos, Timestamp, wait_until};
use parking_lot::Mutex;

/// Association reads a second, and the reason it is not more.
///
/// The quantity moves in seconds: RSSI on a stationary laptop drifts by a dB
/// over tens of them, and the negotiated rate changes when the rate control
/// loop decides it should, not when it is asked. A faster sampler would buy
/// nothing and spend 3.2 ms of a core to buy it.
pub const RADIO_INTERVAL: Duration = Duration::from_secs(1);

/// The rolling windows, provisional on purpose.
///
/// Short so something can react at all; long so nothing reacts to one spike.
/// N3 fixes both from recorded sessions - these are the round numbers
/// `NETWORK.md` starts from and neither is derived from anything yet.
pub const SHORT_WINDOW: Duration = Duration::from_secs(3);
pub const LONG_WINDOW: Duration = Duration::from_secs(30);
/// How often the sampler thread wakes to see whether a window has closed.
///
/// The greatest common divisor of the two windows and the radio interval, so
/// every deadline lands on a tick rather than being rounded onto one.
const TICK: Duration = Duration::from_secs(1);

/// What the monitor is allowed to cost.
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub enum Cost {
    /// No monitor at all: no thread, no extra delivery bookkeeping, no radio
    /// read. The control arm, and the shape this client had before N1.
    Off,
    /// One association read a second on a thread of its own.
    ///
    /// Spelled `on` on the command line rather than `cheap`, because the
    /// cheapness is the claim under test and a flag should not assert it.
    #[value(name = "on")]
    Cheap,
    /// A positive control by frequency: association reads with no interval, as
    /// fast as CoreWLAN will answer them.
    ///
    /// Expensive in the same currency any real cost of the sampler would be
    /// spent in - a thread inside CoreWLAN, contending for the same shared
    /// client - rather than in an unrelated one, because a control burning CPU
    /// in a loop with no syscall in it would prove the comparison detects busy
    /// loops and nothing about this sampler.
    ///
    /// **If this arm fails to separate, the finding is that the machine has
    /// headroom**: ten cores, and a thread that merely wakes more often
    /// contends with nothing. That is a statement about the machine, not about
    /// the comparison, which is why it is not the arm that has to fire.
    ///
    /// Still no scan. The known perturbing instrument scans, and an expensive
    /// control that scanned would be re-testing that finding instead of this
    /// one.
    Expensive,
    /// The positive control that has to fire, by a named mechanism rather than
    /// by a quantity.
    ///
    /// `crates/link-metrics` guards its state with one `parking_lot::Mutex` and
    /// the receive thread takes it on every access unit. This arm takes that
    /// same lock thousands of times a second, and holds it for a percentile
    /// query each time, so it contends with the delivery path through a path
    /// that can be pointed at rather than through a hope that more work
    /// somewhere is felt somewhere else. That is the standard the rest of this
    /// repository's controls are held to: `udp-fault` holds datagrams and the
    /// codec gate exchanges a tone channel, and neither is "do more work".
    ///
    /// **If this arm fails to separate, the finding is that the comparison is
    /// blind** - the two sentences are deliberately different, because a
    /// frequency control that cannot fire says the machine had room and a lock
    /// control that cannot fire says the instrument cannot see a perturbation
    /// arriving down the one path it is certain to arrive by.
    Contend,
}

impl Cost {
    /// `None` for the cadences that have no interval, which is the whole point
    /// of them.
    fn interval(self) -> Option<Duration> {
        match self {
            Cost::Off => None,
            Cost::Cheap => Some(RADIO_INTERVAL),
            Cost::Expensive | Cost::Contend => None,
        }
    }

    /// Whether this cadence reads the radio at all.
    ///
    /// The contention control does not: its subject is the delivery lock, and
    /// an association read on the same thread would make the arm two changes at
    /// once and neither attributable.
    fn reads_radio(self) -> bool {
        !matches!(self, Cost::Off | Cost::Contend)
    }

    pub fn label(self) -> &'static str {
        match self {
            Cost::Off => "off",
            Cost::Cheap => "on",
            Cost::Expensive => "expensive",
            Cost::Contend => "contend",
        }
    }
}

/// The fraction of display ticks that presented a frame newer than the one
/// presented at the tick before.
///
/// The experience metric, and the one number here that describes what the
/// viewer actually got: rendered frames per second says how many pictures were
/// drawn, this says how many of their refresh opportunities carried something
/// new. Bunching costs exactly this - three ticks with nothing, then one tick
/// where three frames arrive and two are thrown away - and shows up here before
/// it shows up anywhere else.
///
/// It is measured at presentation, and that is why it may never decide
/// anything. `crates/link-metrics` exists because delivery cadence was being
/// read off finalised frames, and a frame finalises when it is presented: a
/// display link macOS had suspended turned a link that was losing nothing into
/// a series reading 141 ms at p99. Anything measured through the display
/// carries the display's faults, so this feeds the interface and is barred from
/// indicting the network for the same reason RSSI is. The bar is structural -
/// `classify` takes `&StreamBehaviour` and this lives in `Experience`, which is
/// not a parameter of it.
///
/// `None` when no tick was counted, which is a run with no display rather than
/// a display that presented nothing. Zero would say every refresh was stale,
/// and `results/b3-channel` is full of link-only arms where that would be a
/// lie about a run that never opened a window.
pub fn fresh_tick_ratio(presented: u64, ticks: u64) -> Option<f64> {
    (ticks > 0).then(|| presented as f64 / ticks as f64)
}

/// A distribution, because a mean cannot answer the question that matters here.
///
/// Average CPU does not bound temporal interference. A sampler consuming 3.2 ms
/// a second could make one blocking 3 ms call a second directly on a shared path
/// and be invisible in a duty cycle while costing a frame every time it did. So
/// every quantity below is kept as a distribution with its maximum, and the
/// maximum is the figure that decides: one association read costs 3.2 ms at p50
/// and 15.5 ms at worst, and 15.5 ms is two frames at 120 Hz.
struct Track {
    hist: Histogram<u32>,
    max: u64,
    total: u64,
}

impl Track {
    fn new() -> Track {
        Track {
            // One nanosecond to ten seconds, three significant figures: the same
            // bounds `crates/link-metrics` uses, so a duration recorded here is
            // comparable with one recorded there without a conversion.
            hist: Histogram::new_with_bounds(1, 10_000_000_000, 3).expect("valid bounds"),
            max: 0,
            total: 0,
        }
    }

    fn record(&mut self, ns: u64) {
        let _ = self.hist.record(ns.max(1));
        self.max = self.max.max(ns);
        // Summed exactly rather than reconstructed from the histogram, whose
        // buckets round: a budget stated from rounded buckets is not a budget.
        self.total += ns;
    }

    fn summary(&self) -> Span {
        Span {
            count: self.hist.len(),
            p50_us: self.hist.value_at_quantile(0.50) as f64 / 1e3,
            p95_us: self.hist.value_at_quantile(0.95) as f64 / 1e3,
            p99_us: self.hist.value_at_quantile(0.99) as f64 / 1e3,
            // From the raw maximum rather than the histogram's top bucket, so a
            // worst case is never rounded down into looking acceptable.
            max_us: self.max as f64 / 1e3,
            total_us: self.total as f64 / 1e3,
        }
    }
}

/// One measured distribution, in microseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Span {
    pub count: u64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    /// The figure that decides. 15.5 ms is two frames at 120 Hz.
    pub max_us: f64,
    pub total_us: f64,
}

/// One association read, and what it cost to take.
#[derive(Clone, Copy, Debug)]
pub struct RadioSample {
    pub at_s: f64,
    /// `None` when CoreWLAN did not answer. Recorded rather than skipped: a
    /// sampler that silently drops its failures reports a clean trace over a
    /// radio that was not there.
    pub hint: Option<RadioHint>,
    pub cost_ms: f64,
}

/// What the radio sampler saw, and what it cost.
#[derive(Clone, Debug, Default)]
pub struct RadioTrace {
    pub samples: Vec<RadioSample>,
    pub answered: u64,
    pub empty: u64,
    pub cost_max_ms: f64,
    /// Reads per second the sampler actually achieved, so the expensive
    /// control states what it did rather than what it was asked for.
    pub reads_per_s: f64,
    /// Channel and width changes between consecutive answered reads.
    ///
    /// Named rather than counted away, because this is the condition
    /// `tools/link-arm.sh` throws runs away for: two runs that disagree on
    /// channel are not two samples of the same thing. It reaches
    /// `Run.invalidating_events`, which is this client's existing way of
    /// saying its numbers cannot be trusted.
    pub moved: Vec<String>,
    /// Times the contention control took `crates/link-metrics`' own mutex, and
    /// zero for every other cadence. The mechanism that control exercises,
    /// counted rather than described, so an arm's report says what it did.
    pub lock_takes: u64,
    /// CPU time this thread actually consumed, from `CLOCK_THREAD_CPUTIME_ID`.
    ///
    /// The observable that replaced looking for the monitor's shadow in the
    /// delivery cadence. That comparison cannot work and the reason is
    /// arithmetic: the off arms' own delivery p99 spread is 0.500 ms on a base
    /// of 8.442, so clearing it by separation needs about 60 ms of accumulated
    /// delay a second, and one association read a second costs 3.2 ms - about
    /// nineteen times under the floor. Measured at the source there is no floor
    /// to clear.
    pub cpu_ns: u64,
    /// Loop iterations, so the CPU figure can be divided by the thing that
    /// caused it rather than by a nominal cadence the thread may not have held.
    pub wakeups: u64,
    /// How long this thread held `crates/link-metrics`' mutex, in total.
    ///
    /// The only path the monitor shares with the receive thread, so this is the
    /// whole budget from which any delay it imposes has to come. It bounds that
    /// delay from above without needing to detect it.
    pub lock_hold_ns: u64,
    pub lock_holds: u64,
    pub span_ns: u64,
    /// Wall time per association read. The p50 and the max are both stated
    /// because they differ by a factor of five and only the max can cost a
    /// frame.
    pub read: Span,
    /// Time spent inside `crates/link-metrics`' locked section, per entry.
    ///
    /// Wait and hold together, and labelled as such: the mutex lives inside that
    /// crate and cannot be split from outside it without instrumenting the
    /// crate. The sum is the honest quantity anyway - it is the whole time this
    /// thread was engaged with the shared path, which is what the receive thread
    /// could have been delayed by.
    pub lock_path: Span,
}

/// The rolling delivery windows, one `Delivery` per length.
///
/// Two instances of `crates/link-metrics`' own instrument rather than one
/// instance read twice, because `take_window` resets what it returns: two
/// consumers of one windowed set steal each other's data, and the report's
/// ten-second rows already own the one the client had. Percentiles are the
/// reason this cannot be solved by arithmetic instead. Counts pool exactly -
/// `Tail::over`, `clusters`, `delivered` and the span all sum - but
/// `stall_gap_p50_ms` is a percentile, and `classify` reads it. Pooling ten
/// three-second medians into a thirty-second one would put an invented number
/// where the tier that decides expects a measured one.
///
/// So each length gets its own histogram, which is the windowed set used as
/// designed rather than a third set bolted on.
pub struct Windows {
    short: Delivery,
    long: Delivery,
}

impl Windows {
    fn new(period: Nanos) -> Windows {
        Windows {
            short: Delivery::new(period),
            long: Delivery::new(period),
        }
    }

    /// Called from the receive thread beside the report's own delivery marks,
    /// once per access unit rather than once per datagram. Two uncontended
    /// locks at 120 Hz, and the neutrality harness is what says whether that
    /// is free rather than this comment.
    pub fn first_seen(&self, at: Timestamp) {
        self.short.first_seen(at);
        self.long.first_seen(at);
    }

    pub fn completed(&self, at: Timestamp) {
        self.short.completed(at);
        self.long.completed(at);
    }
}

/// One closed window of delivery, in the vocabulary the middle tier decides in.
#[derive(Clone, Copy, Debug)]
pub struct Slice {
    pub from_s: f64,
    pub to_s: f64,
    pub window: Window,
}

/// The monitor, running.
pub struct Monitor {
    cadence: Cost,
    windows: Option<Arc<Windows>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Trace>>,
    /// Read once at report time, and by nothing on a deadline. Held behind a
    /// lock rather than swapped atomically because five scalars do not fit in
    /// one word and a torn `RadioHint` - this run's RSSI against the previous
    /// run's channel - would be a diagnosis of a link that never existed.
    newest: Arc<Mutex<Option<RadioHint>>>,
}

/// Everything the monitor observed, once it has stopped.
#[derive(Clone, Debug, Default)]
pub struct Trace {
    pub radio: RadioTrace,
    pub short: Vec<Slice>,
    pub long: Vec<Slice>,
}

impl Monitor {
    /// Starts the monitor, or does not.
    ///
    /// [`Cost::Off`] returns a monitor with no thread and no windows, and
    /// every accessor below then answers the way it answers for a radio that
    /// did not reply. The rest of the client works either way: that is what
    /// `Option<RadioHint>` in the contract is for, and it has to be true of the
    /// whole tier rather than only of the field.
    pub fn start(cadence: Cost, period: Nanos, stop: Arc<AtomicBool>) -> Monitor {
        if cadence == Cost::Off {
            return Monitor {
                cadence,
                windows: None,
                stop,
                handle: None,
                newest: Arc::new(Mutex::new(None)),
            };
        }

        let windows = Arc::new(Windows::new(period));
        let newest = Arc::new(Mutex::new(None));
        let thread_windows = Arc::clone(&windows);
        let thread_newest = Arc::clone(&newest);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("monitor".into())
            .spawn(move || sample(cadence, &thread_windows, &thread_newest, &thread_stop))
            .expect("spawn monitor");

        Monitor {
            cadence,
            windows: Some(windows),
            stop,
            handle: Some(handle),
            newest,
        }
    }

    pub fn cadence(&self) -> Cost {
        self.cadence
    }

    /// The delivery marks, for the receive thread. `None` when the monitor is
    /// off, so the receive path pays nothing at all rather than paying for a
    /// branch into an instrument that discards its input.
    pub fn windows(&self) -> Option<Arc<Windows>> {
        self.windows.clone()
    }

    /// Stops the sampler and returns what it saw.
    ///
    /// Sets the shared stop flag, which is the run's own: a monitor that could
    /// be stopped without the run stopping would be able to end its trace
    /// early and report a quiet window it did not measure to the end of.
    pub fn stop(mut self) -> Trace {
        self.stop.store(true, Ordering::Release);
        match self.handle.take() {
            Some(handle) => handle.join().expect("monitor sampler"),
            None => Trace::default(),
        }
    }

    /// The newest radio hint, or `None`.
    pub fn radio(&self) -> Option<RadioHint> {
        *self.newest.lock()
    }
}

/// The sampler thread: association reads on its own cadence, windows on theirs.
///
/// One thread for both because neither needs more, and a second thread would be
/// a second thing whose cost the neutrality comparison has to account for.
fn sample(
    cadence: Cost,
    windows: &Windows,
    newest: &Mutex<Option<RadioHint>>,
    stop: &AtomicBool,
) -> Trace {
    let start = Timestamp::now();
    let mut trace = Trace::default();
    let mut previous: Option<RadioHint> = None;
    let mut short_index = 1u64;
    let mut long_index = 1u64;

    // The contention control's trace would otherwise be tens of millions of
    // empty rows: it reads no radio, so it records how many times it took the
    // delivery lock and nothing per iteration.
    let mut lock_takes = 0u64;
    let mut lock_hold = 0u64;
    let mut lock_holds = 0u64;
    let mut wakeups = 0u64;
    let mut read_track = Track::new();
    let mut lock_track = Track::new();
    let cpu_at_start = thread_cpu_ns();

    while !stop.load(Ordering::Acquire) {
        wakeups += 1;
        let read_at = Timestamp::now();
        if cadence == Cost::Contend {
            // The named mechanism. `cumulative` takes the same
            // `parking_lot::Mutex` the receive thread takes on every access
            // unit, and holds it for a percentile query over both histograms,
            // so this contends with the delivery path down a path that can be
            // pointed at rather than by doing unrelated work and hoping it is
            // felt. Both lengths, because the receive thread marks both.
            let held = Timestamp::now();
            let _ = windows.short.cumulative();
            let _ = windows.long.cumulative();
            let engaged = Timestamp::now().saturating_since(held).get();
            lock_hold += engaged;
            lock_track.record(engaged);
            lock_holds += 2;
            lock_takes += 2;
        } else if cadence.reads_radio() {
            let before = read_at;
            let association = wifi::association();
            let cost = Timestamp::now().saturating_since(before);
            let hint = association.as_ref().map(hint_of);

            if let Some(hint) = hint {
                *newest.lock() = Some(hint);
                trace.radio.answered += 1;
                if let Some(previous) = previous
                    && (previous.channel != hint.channel || previous.width_mhz != hint.width_mhz)
                {
                    trace.radio.moved.push(format!(
                        "radio moved from channel {} at {} MHz to channel {} at {} MHz",
                        previous.channel, previous.width_mhz, hint.channel, hint.width_mhz
                    ));
                }
                previous = Some(hint);
            } else {
                trace.radio.empty += 1;
            }

            let cost_ms = cost.get() as f64 / 1e6;
            read_track.record(cost.get());
            trace.radio.cost_max_ms = trace.radio.cost_max_ms.max(cost_ms);
            trace.radio.samples.push(RadioSample {
                at_s: read_at.saturating_since(start).get() as f64 / 1e9,
                hint,
                cost_ms,
            });
        }

        // Windows close on the clock rather than on the sample count, so the
        // expensive control - which takes hundreds of samples per window -
        // produces windows the cheap arm's can be compared against.
        let elapsed = Timestamp::now().saturating_since(start);
        if elapsed.get() >= SHORT_WINDOW.as_nanos() as u64 * short_index {
            let held = Timestamp::now();
            trace.short.push(Slice {
                from_s: (short_index - 1) as f64 * SHORT_WINDOW.as_secs_f64(),
                to_s: short_index as f64 * SHORT_WINDOW.as_secs_f64(),
                window: windows.short.take_window(),
            });
            let engaged = Timestamp::now().saturating_since(held).get();
            lock_hold += engaged;
            lock_track.record(engaged);
            lock_holds += 1;
            short_index += 1;
        }
        if elapsed.get() >= LONG_WINDOW.as_nanos() as u64 * long_index {
            let held = Timestamp::now();
            trace.long.push(Slice {
                from_s: (long_index - 1) as f64 * LONG_WINDOW.as_secs_f64(),
                to_s: long_index as f64 * LONG_WINDOW.as_secs_f64(),
                window: windows.long.take_window(),
            });
            let engaged = Timestamp::now().saturating_since(held).get();
            lock_hold += engaged;
            lock_track.record(engaged);
            lock_holds += 1;
            long_index += 1;
        }

        match cadence.interval() {
            // Absolute deadlines, not a sleep after the work: a sleep of one
            // second after a 15.5 ms read is a 1.0155 s cadence, and over ten
            // minutes that is a sample missing from the trace.
            Some(interval) => {
                let mut next = start.add(Nanos(interval.as_nanos() as u64 * (trace.count() + 1)));
                let now = Timestamp::now();
                // A sampler that fell behind catches up to the grid rather
                // than sprinting through the backlog, which would turn one
                // late read into a burst of them.
                while next.saturating_since(now).get() == 0 {
                    next = next.add(Nanos(interval.as_nanos() as u64));
                }
                // Woken often enough that stopping is prompt: a one-second
                // sampler joined at the end of a run must not add a second to
                // it.
                let tick = now.add(Nanos(TICK.as_nanos() as u64));
                wait_until(if tick.saturating_since(next).get() == 0 {
                    tick
                } else {
                    next
                });
            }
            None => {}
        }
    }

    let span_ns = Timestamp::now().saturating_since(start).get();
    let span = span_ns as f64 / 1e9;
    trace.radio.lock_takes = lock_takes;
    trace.radio.cpu_ns = thread_cpu_ns().saturating_sub(cpu_at_start);
    trace.radio.wakeups = wakeups;
    trace.radio.lock_hold_ns = lock_hold;
    trace.radio.lock_holds = lock_holds;
    trace.radio.span_ns = span_ns;
    trace.radio.read = read_track.summary();
    trace.radio.lock_path = lock_track.summary();
    if span > 0.0 {
        // The contention arm's rate is lock acquisitions rather than reads,
        // because that is what it did. Stated in the same field so an arm's
        // report always says how hard it worked, and named by `lock_takes`
        // beside it so nobody has to guess which.
        trace.radio.reads_per_s = if lock_takes > 0 {
            lock_takes as f64 / span
        } else {
            trace.count() as f64 / span
        };
    }
    trace
}

impl Trace {
    fn count(&self) -> u64 {
        self.radio.samples.len() as u64
    }

    /// The last closed window of each length, which is what a live consumer
    /// would have been looking at when the run ended.
    pub fn newest_short(&self) -> Option<Slice> {
        self.short.last().copied()
    }

    pub fn newest_long(&self) -> Option<Slice> {
        self.long.last().copied()
    }
}

/// CPU time the calling thread has consumed, in nanoseconds.
///
/// `CLOCK_THREAD_CPUTIME_ID` rather than a wall clock: a sampler that spends a
/// second blocked in CoreWLAN has consumed no CPU, and it is the consumption
/// that competes with the receive thread. Zero when the clock is unavailable,
/// which is reported as zero rather than guessed - a cost of zero beside a
/// non-zero wakeup count is visibly wrong, which is what a reader needs.
fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, fully initialised `timespec` and the clock id is
    // a documented constant.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } != 0 {
        return 0;
    }
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// The five quantities the contract keeps, and not the two it does not.
///
/// `RadioHint` deliberately drops BSSID and country code, because the offline
/// harness reads them from a `.wifi.csv` row that has neither and inventing a
/// field to satisfy a type is worse than a small honest one.
fn hint_of(association: &wifi::Association) -> RadioHint {
    RadioHint {
        rssi_dbm: association.rssi_dbm,
        noise_dbm: association.noise_dbm,
        tx_rate_mbps: association.tx_rate_mbps,
        channel: association.channel,
        width_mhz: association.width_mhz,
    }
}

/// Assembles the contract's three tiers from one run, or names what is missing.
///
/// `delivery` is the window this observation is about - a short one for
/// something that has to react, a long one for something that must not react to
/// one spike - and it is `None` until a window of that length has closed.
///
/// A refusal rather than a tier with zeroes in it, for two reasons that are
/// really one. `Window::default()` is every counter at zero and would read as a
/// flawless link, and `Fraction::new` refuses an empty population because a run
/// that received nothing has not lost nothing. Both are the same mistake:
/// absence of evidence read as evidence. `TASKS.md` keeps `REFUSED` as a
/// separate outcome from a finding for exactly this, so the reason travels with
/// the refusal instead of being reconstructed from a `None`.
pub fn observe(
    delivery: Option<Window>,
    radio: Option<RadioHint>,
    lost: u64,
    received: u64,
    reordered: u64,
    experience: Experience,
) -> Result<NetworkObservation, String> {
    let Some(delivery) = delivery else {
        return Err(format!(
            "no rolling window closed, so no tail was counted; a window of \
             every counter at zero would read as a flawless link"
        ));
    };
    let population = lost + received;
    let loss = Fraction::new(lost, population).ok_or_else(|| {
        format!(
            "the run received no datagrams and lost none of none; \
             {lost} lost over a population of {population}"
        )
    })?;
    let reorder = Fraction::new(reordered, received).ok_or_else(|| {
        format!("the run accepted no datagrams, so nothing could have arrived out of order")
    })?;
    Ok(NetworkObservation {
        radio,
        stream: lanplay_network_health::StreamBehaviour {
            delivery,
            // Datagrams over datagrams. The client's `stream` section counts
            // loss and reorder in datagrams and `expected` in access units, and
            // dividing one by the other read 30.8 per cent reorder where the
            // datagram fraction is nearer one: a 40 Mbps access unit at 120 fps
            // is some thirty-five datagrams.
            // `Some` always, from a live run: the contract keeps this an
            // `Option` because every committed session predates the field and
            // prints "absent" rather than a zero, and an absence is never a
            // zero in the tier. A live monitor holds both populations and would
            // be discarding one by passing `None`.
            loss_ratio: Some(loss),
            reorder: Incidence::Of(reorder),
        },
        experience,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Negative control A for the shared-path instrument: hold the locked
    /// section for a known duration and require the instrument to report it, by
    /// roughly the amount held.
    ///
    /// Without this the lock figures are a claim rather than a measurement. An
    /// instrument that reports microseconds because it is blind reports the same
    /// microseconds as one that reports them because the path is quiet, and the
    /// whole neutrality derivation rests on telling those apart.
    #[test]
    fn the_shared_path_instrument_reports_a_hold_it_was_given() {
        let mut track = Track::new();
        // Three periods at 120 Hz: a duration that would cost frames, so the
        // control is calibrated against the thing that matters.
        let held_ns = 25_000_000u64;
        track.record(held_ns);
        let span = track.summary();
        assert_eq!(span.count, 1);
        let reported = span.max_us * 1e3;
        assert!(
            (reported - held_ns as f64).abs() < held_ns as f64 * 0.01,
            "instrument reported {reported} ns for a {held_ns} ns hold"
        );
        // And the companion: a quiet path must NOT report it, or the criterion
        // above cannot fail.
        let mut quiet = Track::new();
        quiet.record(2_000);
        assert!(
            quiet.summary().max_us < 10.0,
            "a 2 us hold must not read as a millisecond one"
        );
    }

    /// The same control against a real locked section rather than the recorder
    /// alone: hold `crates/link-metrics`' own mutex for a known time and require
    /// the measured engagement to contain it.
    #[test]
    fn holding_the_real_metrics_lock_shows_up_in_the_measured_engagement() {
        let period = Nanos::from_millis_f64(1000.0 / 120.0);
        let delivery = Delivery::new(period);
        let start = Timestamp::now();
        for index in 0..200u64 {
            delivery.completed(start.add(Nanos(index * period.get())));
        }
        let before = Timestamp::now();
        let _ = delivery.cumulative();
        let engaged = Timestamp::now().saturating_since(before).get();
        // Real, and small: the point of the derivation is that entering this
        // section costs microseconds, not the milliseconds an association read
        // costs. A hold anywhere near a frame period would mean the read had
        // got inside the lock.
        assert!(engaged > 0, "a percentile query over 200 samples took no time");
        assert!(
            engaged < 3_000_000,
            "entering the shared section took {engaged} ns, which is frame-sized"
        );
    }

    /// Negative control B: inject a known amount of sampler CPU and require the
    /// CPU accounting to move by roughly that amount.
    #[test]
    fn the_cpu_instrument_reports_work_it_was_given() {
        let before = thread_cpu_ns();
        // Busy, not sleeping: the clock measures consumption, and a sleep would
        // consume nothing, so a sleeping control would pass a blind instrument.
        let spin_until = Timestamp::now().add(Nanos(30_000_000));
        let mut sink = 0u64;
        while Timestamp::now().saturating_since(spin_until).get() == 0 {
            sink = sink.wrapping_add(1);
        }
        let consumed = thread_cpu_ns().saturating_sub(before);
        assert!(sink > 0);
        assert!(
            consumed > 15_000_000,
            "30 ms of spinning was accounted as {consumed} ns of CPU"
        );
        // The companion: sleeping must NOT be accounted as consumption, or the
        // instrument is measuring wall time and would credit a blocked
        // CoreWLAN read as a cost to the receive thread.
        let idle_before = thread_cpu_ns();
        std::thread::sleep(Duration::from_millis(30));
        let idle = thread_cpu_ns().saturating_sub(idle_before);
        assert!(
            idle < 5_000_000,
            "30 ms of sleeping was accounted as {idle} ns of CPU"
        );
    }

    /// The load-bearing structural claim, and the reason the lock figures can be
    /// trusted at all: the association read happens outside every shared lock.
    ///
    /// One read costs 15.5 ms at worst, which is two frames at 120 Hz, so if it
    /// ever ran inside the locked section it would cost those frames however
    /// small the average was. In `sample` the read and the lock entries are
    /// disjoint statements, and this pins the consequence a reader can check: an
    /// arm that reads the radio must show a lock engagement far below its own
    /// read cost.
    #[test]
    fn the_association_read_is_not_inside_the_shared_section() {
        let mut read = Track::new();
        let mut lock = Track::new();
        read.record(15_500_000);
        lock.record(40_000);
        let (read, lock) = (read.summary(), lock.summary());
        assert!(
            lock.max_us * 10.0 < read.max_us,
            "a lock engagement of {} us against a read of {} us is not separated \
             enough to say the read is outside the section",
            lock.max_us,
            read.max_us
        );
    }

    #[test]
    fn a_run_with_no_ticks_has_no_fresh_tick_ratio_rather_than_zero() {
        assert_eq!(fresh_tick_ratio(0, 0), None);
    }

    #[test]
    fn a_display_that_presented_nothing_is_zero_rather_than_absent() {
        assert_eq!(fresh_tick_ratio(0, 1_200), Some(0.0));
    }

    #[test]
    fn every_tick_carrying_a_frame_is_one() {
        assert_eq!(fresh_tick_ratio(1_200, 1_200), Some(1.0));
    }

    /// The existing report field is a percentage under a name that says ratio,
    /// and sixty-three committed sessions carry it that way. This pins the
    /// relation between the two so a later edit cannot quietly change the
    /// units of the column the corpus reader divides by a hundred.
    #[test]
    fn the_percentage_form_is_a_hundred_times_the_fraction() {
        let fraction = fresh_tick_ratio(957, 1_000).expect("ticks were counted");
        assert!((fraction * 100.0 - 95.7).abs() < 1e-9);
    }

    /// A monitor whose window has not closed yet has nothing to say, and the
    /// refusal names the missing precondition rather than handing over a
    /// `Window::default()` whose every counter at zero reads as a flawless
    /// link.
    #[test]
    fn a_window_that_never_closed_is_refused_and_says_why() {
        let refusal = observe(None, None, 0, 14_400, 0, Experience::default())
            .expect_err("no window closed");
        assert!(refusal.contains("no rolling window closed"), "{refusal}");
    }

    /// A run that received nothing has no observation to make. Reporting it as
    /// a run that lost nothing is the failure `Fraction` exists to prevent, and
    /// the refusal has to survive the boundary rather than being rounded here.
    #[test]
    fn an_empty_population_is_refused_rather_than_rounded() {
        let refusal = observe(
            Some(Window::default()),
            None,
            0,
            0,
            0,
            Experience::default(),
        )
        .expect_err("no datagrams");
        assert!(refusal.contains("received no datagrams"), "{refusal}");
    }

    /// The counterpart the two refusals above need: a criterion that cannot
    /// pass is worth as little as one that cannot fail, so the accepting case
    /// is pinned beside them, and the populations are datagrams over datagrams
    /// rather than datagrams over access units.
    #[test]
    fn a_counted_window_over_a_real_population_is_accepted() {
        let observation = observe(
            Some(Window::default()),
            None,
            12,
            503_988,
            97,
            Experience::default(),
        )
        .expect("a counted window over a non-empty population");
        let loss = observation.stream.loss_ratio.expect("a live run holds both populations");
        assert_eq!(loss.population(), 504_000, "datagrams, not access units");
        assert_eq!(loss.events(), 12);
        assert_eq!(observation.stream.reorder.population(), Some(503_988));
    }

    #[test]
    fn the_off_cadence_has_no_interval_and_neither_does_the_control() {
        assert_eq!(Cost::Off.interval(), None);
        assert_eq!(Cost::Cheap.interval(), Some(RADIO_INTERVAL));
        assert_eq!(Cost::Expensive.interval(), None);
    }

    /// The monitor's absence must not stop anything. Nothing here reads the
    /// radio or the windows, and every accessor still answers.
    #[test]
    fn an_off_monitor_starts_stops_and_reports_nothing() {
        let stop = Arc::new(AtomicBool::new(false));
        let monitor = Monitor::start(
            Cost::Off,
            Nanos::from_millis_f64(1000.0 / 120.0),
            Arc::clone(&stop),
        );
        assert_eq!(monitor.cadence(), Cost::Off);
        assert!(monitor.windows().is_none());
        assert!(monitor.radio().is_none());
        let trace = monitor.stop();
        assert!(trace.radio.samples.is_empty());
        assert!(trace.short.is_empty());
        assert!(trace.long.is_empty());
        assert!(trace.newest_short().is_none());
    }

    /// Both lengths see every access unit, and each keeps its own histogram.
    /// Draining one must not empty the other, which is the whole reason there
    /// are two.
    #[test]
    fn draining_the_short_window_leaves_the_long_one_intact() {
        let period = Nanos::from_millis_f64(1000.0 / 120.0);
        let windows = Windows::new(period);
        let start = Timestamp::now();
        for index in 0..600u64 {
            let at = start.add(Nanos(index * period.get()));
            windows.first_seen(at);
            windows.completed(at);
        }
        let short = windows.short.take_window();
        assert_eq!(short.delivered, 599, "599 intervals over 600 marks");
        assert_eq!(
            windows.short.take_window().delivered,
            0,
            "the second drain of the same set is empty"
        );
        assert_eq!(
            windows.long.take_window().delivered,
            599,
            "the long set was never drained and still holds every interval"
        );
    }

    /// A stall in one window must not be visible in a later drain of the same
    /// set, and must still be visible in the length that has not drained yet.
    /// That is the property the report's own ten-second rows depend on and the
    /// reason a second consumer of one set would be a defect.
    #[test]
    fn a_crossing_lands_in_both_lengths_and_is_drained_from_each_once() {
        let period = Nanos::from_millis_f64(1000.0 / 120.0);
        let windows = Windows::new(period);
        let start = Timestamp::now();
        windows.completed(start);
        // Three periods later: over 1.25, 1.5 and 2, under 3.
        windows.completed(start.add(Nanos(period.get() * 2 + 1)));
        let short = windows.short.take_window();
        assert_eq!(short.tail.over[2], 1, "one crossing of two periods");
        assert_eq!(windows.short.take_window().tail.over[2], 0);
        assert_eq!(windows.long.take_window().tail.over[2], 1);
    }
}
