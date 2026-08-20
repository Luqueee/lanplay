//! The machine-readable result of a run.
//!
//! `xtask` reads this rather than parsing the human report, so the two cannot
//! drift. Every number here is measured on the client's own clock: the
//! sender's timestamps are not comparable until clock offset estimation
//! lands, and pretending otherwise would turn clock skew into latency.

use serde::Serialize;

#[derive(Serialize)]
pub struct Report {
    pub run: Run,
    pub stream: Stream,
    pub network: Network,
    /// The link's own cadence, independent of everything after it.
    pub delivery: Delivery,
    pub decode: Decode,
    pub display: Display,
    /// What the phase estimator did, which is not derivable from the wait it
    /// was trying to shrink.
    pub phase: Phase,
    pub environment: Environment,
    /// The observation contract's three tiers, beside the structs above
    /// rather than inside them.
    ///
    /// `NETWORK.md` separates what the radio was doing, what the stream did
    /// and what the user got, and only the middle one decides. That
    /// separation is not a new idea imposed on this report - `network`,
    /// `delivery`, `decode` and `display` above are already four tiers kept
    /// apart because delivery cadence used to be read off the presentation
    /// clock. This is the same separation stated in the vocabulary
    /// `crates/network-health` fixed, so an offline classifier can be handed
    /// exactly what a live one would see.
    ///
    /// Absent when nothing could be observed, in which case
    /// `observation_refused` below names the missing precondition.
    pub observation: Option<Observation>,
    /// Why there is no observation, when there is none.
    ///
    /// Exactly one of this and `observation` is present, and this one names the
    /// missing precondition: a rolling window that never closed, or a run that
    /// received no datagrams. `REFUSED` is a separate outcome from a finding,
    /// so a reader must be able to tell "the link was fine" from "nothing was
    /// measured", and the same shape is already used by `phase` above.
    pub observation_refused: Option<String>,
    /// What the monitor itself did, which the tiers above cannot say.
    ///
    /// Its cadence, its radio trace and its rolling windows. A run compared
    /// against another to prove the monitor costs nothing has to state which
    /// monitor it was running, and a comparison whose arms are told apart by
    /// the harness's own bookkeeping rather than by the artefact is a
    /// comparison whose labels nobody can check.
    pub monitor: Monitor,
    /// Resident memory across the run.
    ///
    /// Here rather than only in the printed report because the gate already
    /// decides on it and a harness that needed the same number was reduced to
    /// parsing a sentence. A ten-minute soak is the one run that can say
    /// whether a component added to this client leaks, and the figure it turns
    /// on has to be readable without a regex.
    pub memory: Memory,
    pub windows: Vec<Window>,
}

/// Resident memory, as the leak check sees it.
#[derive(Serialize)]
pub struct Memory {
    pub samples: usize,
    pub first_mb: f64,
    pub last_mb: f64,
    pub max_mb: f64,
    /// `None` when fewer than three samples were taken, which is a run too
    /// short to fit a line through rather than a run that did not grow. A slope
    /// of zero would say the opposite.
    pub slope_mb_per_min: Option<f64>,
    /// The same slope with the warm-up excluded, which is the one the gate
    /// decides on: filling a decoder pool, compiling a shader and reading a
    /// fixture each cost memory once, and a line fitted through them reads as a
    /// leak on any short run.
    pub steady_slope_mb_per_min: Option<f64>,
    pub steady_samples: usize,
    /// What the gate allows, so a reader comparing the slope against a
    /// threshold does not have to find the threshold in a different file.
    pub allowed_mb_per_min: f64,
    pub warmup_ms: f64,
}

#[derive(Serialize)]
pub struct Run {
    /// What the callback counters mean. Under `immediate` they count draw
    /// attempts rather than refreshes, so no rate derived from them is a
    /// display rate.
    pub drive_mode: &'static str,
    pub seconds: f64,
    pub target_fps: f64,
    /// True when something changed underneath the run that makes the
    /// presentation numbers untrustworthy, even if it recovered afterwards.
    pub invalidated: bool,
    pub invalidating_events: Vec<String>,
}

#[derive(Serialize)]
pub struct Stream {
    pub expected: u64,
    pub reconstructed: u64,
    pub packet_loss: u64,
    pub au_loss: u64,
    pub corruption: u64,
    pub reordered: u64,
    /// Furthest ahead of the missing packet that arrivals kept coming.
    pub max_reorder_depth: u32,
    /// How long a visible gap took to fill itself when nothing was lost.
    /// A NACK delay shorter than this asks for packets already in flight,
    /// and the mean hides exactly the tail that decides whether one is
    /// worth building.
    pub reorder_wait_mean_ms: f64,
    pub reorder_wait_p50_ms: f64,
    pub reorder_wait_p99_ms: f64,
    pub reorder_wait_max_ms: f64,
    pub reorder_gaps: u64,
    pub duplicates: u64,
}

#[derive(Serialize)]
pub struct Network {
    pub arrival_p50_ms: f64,
    pub arrival_p95_ms: f64,
    pub arrival_p99_ms: f64,
    pub arrival_max_ms: f64,
    pub rtp_jitter_us: f64,
    /// The DSCP most arriving datagrams carried, and its share. `None` when
    /// the kernel would not report the TOS byte.
    pub observed_dscp: Option<u8>,
    pub observed_dscp_share: f64,
}

/// What the network delivered, measured where delivery happens.
///
/// Kept apart from [`Display`] on purpose. Delivery cadence used to be read
/// off the presentation clock, and a suspended display link then made a
/// healthy link look like one stalling for 141 ms at p99. A stage is
/// measured at that stage or not at all.
#[derive(Serialize)]
pub struct Delivery {
    /// Complete access units the depacketiser handed over.
    pub delivered: u64,
    /// Interval between consecutive complete access units, on the receiver's
    /// own clock. The series a link experiment is ranked by.
    pub au_interval_p50_ms: f64,
    pub au_interval_p95_ms: f64,
    pub au_interval_p99_ms: f64,
    pub au_interval_max_ms: f64,
    /// Interval between the *first* datagram of consecutive access units.
    ///
    /// Compared against the complete-interval above, this says whether late
    /// units started late or merely finished late, which are faults of
    /// different parts of the link.
    pub first_interval_p50_ms: f64,
    pub first_interval_p95_ms: f64,
    pub first_interval_p99_ms: f64,
    pub first_interval_max_ms: f64,
    /// Wall time the delivery series covered, which turns counts into rates.
    pub span_s: f64,
    /// Access units per minute arriving at or beyond each multiple of the
    /// source period. Counted, never inferred from a percentile: a p99 of
    /// 15.92 ms against a 16.67 ms threshold says nothing about how many
    /// units crossed it.
    pub over_1_25t_per_min: f64,
    pub over_1_5t_per_min: f64,
    pub over_2t_per_min: f64,
    pub over_3t_per_min: f64,
    pub over_4t_per_min: f64,
    pub over_6t_per_min: f64,
    /// Stalls beyond two periods that were followed by units arriving inside
    /// one. This is bunching itself, rather than a percentile that bunching
    /// happens to move.
    pub stall_clusters_per_min: f64,
    pub mean_catch_up_units: f64,
    pub max_catch_up_units: u64,
    /// Interval between the starts of consecutive stalls. A tight
    /// distribution indicts a timer; a broad one indicts contention.
    pub stall_gap_p50_ms: f64,
    pub stall_gap_p95_ms: f64,
}

#[derive(Serialize)]
pub struct Decode {
    pub decoded: u64,
    pub errors: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub backlog_slope_per_min: f64,
}

#[derive(Serialize)]
pub struct Display {
    /// What the display is capable of, from the link's own reckoning. Not a
    /// measurement of this run.
    pub nominal_hz: f64,
    /// Callbacks divided by the span they were counted over. The span ends
    /// with the stream rather than with the clock, so dividing `callbacks`
    /// by the run's nominal seconds gives a different and wrong answer.
    pub observed_hz: f64,
    /// Whether the display link ran at a rate that makes anything below it a
    /// measurement.
    ///
    /// `false` means the instrument was not observing: the window was behind
    /// something, the screen slept, the Space changed. Not a failure of the
    /// pipeline - an absence of evidence about it, and the two must never be
    /// reported as the same thing.
    pub cadence_valid: bool,
    pub invalid_reason: Option<String>,
    pub callbacks: u64,
    pub rendered: u64,
    pub superseded: u64,
    pub empty_refreshes: u64,
    pub callback_interval_p50_ms: f64,
    pub callback_interval_p95_ms: f64,
    /// Share of display ticks that had a frame newer than the one shown at
    /// the previous tick.
    ///
    /// The experience metric. Rendered frames per second says how many
    /// pictures were drawn; this says how many of the viewer's refresh
    /// opportunities carried something new, which is what bunching actually
    /// costs: three ticks with nothing followed by one tick where three
    /// frames arrive and two are discarded.
    pub fresh_tick_ratio: f64,
    pub callback_interval_p99_ms: f64,
    pub callback_interval_max_ms: f64,
    /// The client's `local_age`: first local mark to present. Not the sender's
    /// frame age, which needs a synchronised clock.
    pub frame_age_p50_ms: f64,
    pub frame_age_p95_ms: f64,
    pub frame_age_p99_ms: f64,
}

/// The phase estimator's own account of the run.
///
/// A run that sent no shift and a run whose estimator never existed produce the
/// same presentation wait and mean completely different things, so the state is
/// recorded rather than inferred from the counts being zero.
#[derive(Serialize)]
pub struct Phase {
    /// `on`, `observe` or `off`. Three states rather than a flag, because a run
    /// that measured the phase and deliberately sent nothing is neither of the
    /// other two and is the control the comparison needs.
    pub mode: &'static str,
    /// False only for `off`. An observing run has the loop enabled: what it does
    /// not have is a wire.
    pub enabled: bool,
    /// True only when the loop actually observed this run.
    pub ran: bool,
    /// Why it did not, when it was asked for and could not run.
    pub unavailable_reason: Option<String>,
    /// How far in front of the display link's deadline frames ended up being
    /// aimed. Chosen from the jitter the run measured, so a run can only be read
    /// against this rather than against the constant it started from.
    pub margin_ms: f64,
    /// The least it would have aimed for whatever it measured.
    pub margin_floor_ms: f64,
    /// Scatter of the phases the margin was chosen from: the jitter the margin
    /// exists to absorb. Zero when no batch was ever believable, which
    /// `decisions` tells apart from a genuinely steady run.
    pub spread_ms: f64,
    /// Share of the period the measured phase visited, in sixteenths.
    ///
    /// The number that says whether a phase was held or left alone: two
    /// unsynchronised 120 Hz clocks beat through the whole period about every
    /// 33 s, so an untouched run of a few minutes approaches one while a held
    /// phase stays near a sixteenth.
    pub phase_coverage: f64,
    pub samples: u64,
    pub decisions: u64,
    /// Shifts that reached the wire. Zero while observing.
    pub shifts: u64,
    /// Shifts a decision asked for that were deliberately not sent. Non-zero
    /// only while observing, where it is the count of what an acting run of the
    /// same shape would have done.
    pub shifts_withheld: u64,
    /// Decisions that found the phase already where it was aimed.
    pub holds: u64,
    /// Decisions refused for want of evidence.
    pub declined: u64,
    /// The phase of the first believable batch: where the two clocks sat before
    /// anything was asked of them.
    pub first_phase_ms: Option<f64>,
    pub last_phase_ms: Option<f64>,
    pub last_delay_ms: Option<f64>,
    pub last_refusal: Option<String>,
    pub send_errors: u64,
    /// Decisions the series holds, reported separately so a series that stopped
    /// growing cannot be read as a phase that stopped moving.
    pub trace_entries: usize,
    /// Decisions past the series' capacity, and therefore missing from it.
    pub trace_dropped: u64,
    /// What the monotonic `at_ns` below was against the wall clock, sampled once
    /// when this report was written.
    ///
    /// An offset between two bases rather than one clock: the monotonic side
    /// counts through sleep, which is what makes a single pairing valid for the
    /// whole run. It is here so a decision can be lined up with an event in
    /// somebody else's log, which is the only reason to cross at all.
    pub clock_epoch_at_ns: u64,
    pub clock_epoch_unix_ms: f64,
    pub trace: Vec<PhaseSample>,
}

/// One decision from the phase loop.
///
/// The series exists because endpoints cannot settle anything on a link that
/// drifts: two clocks a quarter of a millisecond a second apart sweep two whole
/// periods across seventy seconds, which buries any single step applied in the
/// middle. Reading the phase either side of an event, close enough that the
/// drift between the readings is small against the step, needs every decision
/// rather than the first and the last.
#[derive(Serialize)]
pub struct PhaseSample {
    /// The newest frame the decision was computed from, on the same monotonic
    /// clock as every stage mark in this pipeline.
    pub at_ns: u64,
    /// Seconds from the first traced decision, which is what a reader wants when
    /// lining the series up against its own log.
    pub at_s: f64,
    pub phase_ms: f64,
    /// What the phase was being aimed at when this decision was taken.
    pub margin_ms: f64,
    /// The delay asked for, absent on a decision that asked for nothing.
    pub delay_ms: Option<f64>,
    /// Whether that delay reached the wire. Always false in `observe`.
    pub sent: bool,
}

#[derive(Serialize)]
pub struct Environment {
    pub occlusion_changes: u64,
    pub space_changes: u64,
    pub miniaturise_events: u64,
    pub display_changes: u64,
    pub link_pauses: u64,
    pub app_nap_protection: bool,
}

#[derive(Serialize)]
pub struct Window {
    pub from_s: f64,
    pub to_s: f64,
    /// Callbacks per second. A refresh rate only under `display-link`: in
    /// `immediate` mode a callback is a draw attempt from a loop that spins as
    /// fast as the slot can be read, and the figure runs to thousands. See
    /// `run.drive_mode` before reading this as a display rate.
    pub callback_hz: f64,
    /// Access units reassembled per second: what the link delivered.
    pub source_hz: f64,
    pub decode_hz: f64,
    pub render_hz: f64,
    pub superseded_pct: f64,
    /// Ticks that had something new to show, as a share of the window's
    /// ticks. The per-window form of `fresh_tick_ratio`, and the column to
    /// rank a link configuration by.
    pub fresh_pct: f64,
    /// Interval between complete access units inside the window, measured at
    /// the depacketiser. A cumulative percentile cannot be differenced, so
    /// this is the only place a link stall is visible - and it is taken from
    /// the delivery clock, not the presentation one.
    pub au_interval_p50_ms: f64,
    pub au_interval_p99_ms: f64,
    /// Counted crossings of two source periods inside this window. A run's
    /// median cannot show a bad twenty seconds; this can.
    pub over_2t_per_min: f64,
    /// Access units the host produced in this window that never arrived.
    pub au_loss: u64,
    /// The client's `local_age`, not the sender's frame age.
    pub frame_age_p99_ms: f64,
}

/// One observation of the link, in the three tiers `NETWORK.md` fixed.
///
/// The JSON projection of `lanplay_network_health::NetworkObservation`, which
/// is the type the monitor actually holds. Projected rather than serialised
/// directly because the contract crate carries no `serde` dependency and
/// should not grow one to satisfy this file: every other tier in this report
/// is a projection of something measured elsewhere for the same reason.
#[derive(Serialize)]
pub struct Observation {
    /// `None` when CoreWLAN did not answer, or when no monitor ran. Diagnostic
    /// only: it answers *why* and never *whether*. A link at -48 dBm
    /// negotiating 1200 Mbps produced concealment from 0.196 to 7.442 per cent
    /// across the ten arms of `results/audio/jitter-target-a8` while 3 dB of
    /// signal between those arms moved the negotiated rate by nothing at all.
    pub radio: Option<RadioHint>,
    /// The only tier that decides, and the newest closed short window of it:
    /// what something that has to react at all would have been looking at when
    /// the run ended.
    pub stream_short: StreamBehaviour,
    /// The same tier over the newest closed long window, which is here so that
    /// nothing reacts to one spike. Both lengths rather than one because that
    /// is the whole reason there are two, and a report carrying only the short
    /// one would leave the long one unfalsifiable.
    pub stream_long: StreamBehaviour,
    /// What the user got. Feeds the interface, indicts nothing.
    pub experience: Experience,
}

#[derive(Serialize)]
pub struct RadioHint {
    pub rssi_dbm: i64,
    pub noise_dbm: i64,
    /// The negotiated rate, which is a ceiling on throughput and not
    /// throughput.
    pub tx_rate_mbps: f64,
    pub channel: i64,
    pub width_mhz: u32,
}

/// What the stream did, over the window this observation is about.
#[derive(Serialize)]
pub struct StreamBehaviour {
    /// Which window this is: `short` or `long`, and how long each was. Stated
    /// in the artefact because both lengths are provisional - N3 fixes them
    /// from recorded sessions - and a reader must never have to guess which
    /// build wrote the row in front of them.
    pub window: &'static str,
    pub window_seconds: f64,
    /// A window that actually closed, always. An observation is refused rather
    /// than reported when no window of this length has closed yet, because a
    /// `Window` of every counter at zero reads as a flawless link and the
    /// refusal is a different outcome from a finding. `observation_refused`
    /// above carries the reason when this section is absent.
    pub span_s: f64,
    pub delivered: u64,
    pub au_interval_p50_ms: f64,
    pub au_interval_p99_ms: f64,
    /// Counted crossings of two source periods a minute, which is the multiple
    /// every comparison in this corpus is already written in.
    pub over_2t_per_min: f64,
    pub clusters_per_min: f64,
    /// Interval between the starts of consecutive stalls. A tight distribution
    /// indicts a timer - a scan, a beacon, a power-save cycle - and a broad one
    /// indicts contention, and those need different actions. Not poolable
    /// across windows, which is why each length keeps its own histogram.
    pub stall_gap_p50_ms: f64,
    pub stall_gap_p95_ms: f64,
    /// Datagrams that never arrived, over what the receiver accepted plus what
    /// never came. Datagrams over datagrams: the `stream` section above counts
    /// loss in datagrams and `expected` in access units, and the ratio of those
    /// two read 30.8 per cent reorder where the datagram fraction is nearer
    /// one, because a 40 Mbps access unit at 120 fps is some thirty-five
    /// datagrams.
    pub loss_events: u64,
    pub loss_population: u64,
    pub loss_ratio: f64,
    pub reorder_events: u64,
    pub reorder_population: u64,
    pub reorder_ratio: f64,
}

/// What the user got, and the tier that is structurally barred from deciding.
#[derive(Serialize)]
pub struct Experience {
    /// The fraction of display ticks that presented a frame newer than the one
    /// presented at the tick before. A fraction in 0..1, unlike
    /// `display.fresh_tick_ratio` above, which is the same quantity as a
    /// percentage and stays that way because sixty-three committed sessions
    /// carry it and one reader already divides it by a hundred.
    ///
    /// `None` when no tick was counted - a run with no display, which is every
    /// link-only arm - because zero would claim every refresh was stale.
    pub fresh_tick_ratio: Option<f64>,
    pub frame_age_p99_ms: Option<f64>,
}

/// What the monitor did, and what it cost to do it.
#[derive(Serialize)]
pub struct Monitor {
    /// `off`, `on` or `expensive`. Three states rather than a flag, because
    /// `expensive` is the positive control the neutrality comparison has to
    /// detect before its failure to detect `on` means anything.
    pub cadence: &'static str,
    /// Association reads attempted, and how many CoreWLAN answered. An empty
    /// read is recorded rather than skipped: a sampler that drops its failures
    /// reports a clean trace over a radio that was not there.
    pub radio_reads: u64,
    pub radio_answered: u64,
    pub radio_empty: u64,
    /// Reads a second the sampler actually achieved, so the expensive control
    /// states what it did rather than what it was asked for.
    pub radio_reads_per_s: f64,
    /// Worst single association read. One costs 3.2 ms at p50 and 15.5 ms at
    /// worst on this machine, measured by
    /// `tools/radio-sample/examples/read-cost.rs`; a scan costs hundreds of
    /// milliseconds, so a figure in that range would say the read was not the
    /// passive one this tier claims.
    pub radio_cost_max_ms: f64,
    /// Times the contention control took `crates/link-metrics`' own mutex, the
    /// one the receive thread takes on every access unit. Zero for every other
    /// cadence. The mechanism that control exercises, counted, so an arm's
    /// artefact states what it did rather than what it was named.
    pub radio_lock_takes: u64,
    /// What the monitor cost, measured at the source rather than looked for
    /// downstream.
    ///
    /// Looking for it downstream cannot work, and the reason is arithmetic
    /// rather than weather. The delivery p99 of arms with no monitor at all
    /// spreads 0.500 ms on a base of 8.442 ms, so a perturbation has to cost
    /// about 0.5 ms on the frames it touches - some 60 ms of accumulated delay a
    /// second at 120 fps - before a separation rule can see it. One association
    /// read a second costs 3.2 ms. The effect is about nineteen times under the
    /// instrument's floor, which is why two independent positive controls both
    /// failed to fire and why a third would too.
    ///
    /// Measured here there is no floor to clear, and the neutrality claim
    /// becomes a derivation: the sampler consumed this much CPU and held the
    /// shared lock this long, so it cannot account for more than that.
    pub cost: MonitorCost,
    pub short_windows: usize,
    pub long_windows: usize,
    /// Every closed window of each length, in cadence alone.
    ///
    /// Cadence and not the whole middle tier, because loss and reorder are
    /// counted at the socket over the run and there is no per-window figure for
    /// either. Putting a run total in a three-second row would let a reader
    /// difference two rows and get a loss that never happened.
    pub short: Vec<MonitorWindow>,
    pub long: Vec<MonitorWindow>,
    /// Every association read, so the radio trace can be read beside the
    /// windows rather than only as a summary.
    pub radio_trace: Vec<RadioSample>,
}

#[derive(Serialize)]
pub struct RadioSample {
    pub at_s: f64,
    pub rssi_dbm: Option<i64>,
    pub noise_dbm: Option<i64>,
    pub tx_rate_mbps: Option<f64>,
    pub channel: Option<i64>,
    pub width_mhz: Option<u32>,
    pub cost_ms: f64,
}

/// What the sampler consumed, and the bound that follows from it.
#[derive(Serialize)]
pub struct MonitorCost {
    /// Wall time the sampler thread existed for.
    pub span_s: f64,
    /// Loop iterations. Divided into the CPU figure rather than a nominal
    /// cadence, because a thread that fell behind did not hold its cadence.
    pub wakeups: u64,
    /// CPU the sampler thread actually consumed, from
    /// `CLOCK_THREAD_CPUTIME_ID`. A thread blocked in CoreWLAN consumes none,
    /// and it is consumption that competes with the receive thread.
    pub cpu_ms: f64,
    pub cpu_us_per_wakeup: f64,
    /// Share of one core. The denominator a reader wants beside it is this
    /// machine's core count: a figure of 0.3 per cent of a thread is 0.03 per
    /// cent of ten cores.
    pub cpu_share_of_one_core: f64,
    /// How long the sampler held `crates/link-metrics`' mutex in total, and how
    /// often. The only path it shares with the receive thread.
    pub lock_hold_ms: f64,
    pub lock_holds: u64,
    /// Share of the run during which the sampler held that shared lock.
    pub lock_share_of_span: f64,
    /// The bound the two figures above give: total lock hold spread over the
    /// access units delivered, which is the most the sampler could have delayed
    /// any one of them on average.
    ///
    /// An upper bound on the delay to the *thread*, and not to the recorded
    /// interval, which is a stronger statement: `transport.rs` timestamps
    /// arrival before taking the lock, so lock delay moves when an interval is
    /// recorded and never what it is. The delay to the measurement is zero by
    /// construction and this is the delay to the work.
    pub max_mean_delay_us_per_unit: Option<f64>,
    /// The source period this has to be read against, since a delay only means
    /// something as a fraction of the budget it eats.
    pub source_period_ms: f64,
    /// The bound as a share of that period.
    pub max_mean_delay_share_of_period: Option<f64>,
    /// Wall time per association read, as a distribution.
    ///
    /// The maximum is the figure that decides, and a mean would hide it: one
    /// read costs 3.2 ms at p50 and 15.5 ms at worst, and 15.5 ms is two frames
    /// at 120 Hz. A duty cycle cannot bound temporal interference - a sampler
    /// consuming 3.2 ms a second could make one blocking 3 ms call a second
    /// directly on a shared path and be invisible in the average while costing a
    /// frame every time.
    pub read_us: Span,
    /// Time this thread spent engaged with `crates/link-metrics`' locked
    /// section, per entry: wait and hold together, which is the whole time the
    /// receive thread could have been waiting behind it.
    ///
    /// This is the load-bearing measurement. Read it against `read_us`: the
    /// association read is a disjoint statement from every lock entry in
    /// `monitor::sample`, so an engagement far below the read's own cost is the
    /// observable consequence of the read being outside the section. If the two
    /// were comparable, the read would be inside it and costing frames.
    pub lock_path_us: Span,
}

/// One measured distribution, in microseconds.
#[derive(Serialize)]
pub struct Span {
    pub count: u64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    /// The figure that decides.
    pub max: f64,
    pub total: f64,
}

/// One closed rolling window, in the quantities `classify` reads.
///
/// Rates are derived here rather than left as counts because a rate is the
/// figure two windows of different length can be compared on, and both the
/// counts and the span are stated beside them so the derivation can be checked.
#[derive(Serialize)]
pub struct MonitorWindow {
    pub from_s: f64,
    pub to_s: f64,
    /// Wall time the window actually covered, which is not its nominal length:
    /// a window whose first access unit arrived late covers less.
    pub span_s: f64,
    pub delivered: u64,
    pub au_interval_p50_ms: f64,
    pub au_interval_p99_ms: f64,
    pub over_2t: u64,
    pub over_2t_per_min: f64,
    pub clusters: u64,
    pub clusters_per_min: f64,
    /// Interval between the starts of consecutive stalls, inside this window
    /// alone. Each length keeps its own histogram because a percentile cannot
    /// be pooled from percentiles, and this is the field that makes that
    /// matter: `classify` reads it.
    pub stall_gap_p50_ms: f64,
    pub stall_gap_p95_ms: f64,
}
