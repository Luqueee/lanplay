//! The loop, and the two scenarios it runs.
//!
//! One loop serves both levels. 3A acquires, marks and releases and does
//! nothing else; 3B adds the copy into an owned texture, the early release of
//! the source and the simulated downstream hold. Sharing the loop is what
//! makes the difference between the two numbers the cost of ownership rather
//! than the cost of two different harnesses.
//!
//! The warm-up is a full run of the same loop, discarded. Device creation, the
//! first allocation, the driver's first path through the copy and the frame
//! pool filling all happen there, are timed, and are reported separately
//! instead of being smeared over the steady-state tail.

#![cfg(windows)]

use core::error::Error;

use lanplay_capture::backend::CapturedFrame;
use lanplay_capture::{
    Acquired, CaptureBackend, CaptureConfig, CaptureDevice, CaptureError, PoolHandle, TexturePool,
};
use lanplay_telemetry::{Nanos, Timestamp, Trend, resident_bytes};
use windows::Win32::Foundation::FILETIME;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

use crate::display;
use crate::gpu::CopyTimer;
use crate::report::{
    BlockReport, CompareReport, ConfigReport, DeviceReport, HandoffReport, InjectedStallReport,
    RunReport, StartupReport, SystemReport,
};
use crate::schedule::{self, BackendKind};
use crate::seam::{self, Capture, Extras};
use crate::stall::{StallClass, period_for};
use crate::stats::{FrameObservation, Stats};

/// How often resident memory, the backlog and pool starvation are sampled into
/// their growth trends. Four a second is enough to fit a line over a minute
/// and cheap enough not to show up in the loop it is measuring.
const SAMPLE_INTERVAL: Nanos = Nanos::from_millis(250);
/// Discarded at the head of every `compare` block. A capture that started a
/// moment ago is measuring its own start-up, and the alternation exists to
/// remove differences that are not about the API.
const BLOCK_LEAD_IN: Nanos = Nanos::from_millis(250);
/// How long after an injected stall the capture has to come back on cadence
/// before it counts as not having come back, in source frames.
const RECOVERY_LIMIT_SECONDS: f64 = 4.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Native,
    Handoff,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Native => "native",
            Mode::Handoff => "handoff",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Plan {
    pub mode: Mode,
    pub seconds: f64,
    pub warmup_seconds: f64,
    pub buffers: u32,
    pub output: u32,
    pub acquire_timeout_ms: u32,
    pub cursor: bool,
    /// Overrides the detected display rate when the producer is not the
    /// compositor's own cadence.
    pub source_hz: Option<f64>,
    pub stall_ms: u64,
    pub pool: u32,
    pub hold_ms: f64,
    pub block_seconds: f64,
}

impl Plan {
    fn capture_config(&self) -> CaptureConfig {
        CaptureConfig {
            output: self.output,
            buffers: self.buffers,
            acquire_timeout_ms: self.acquire_timeout_ms,
            cursor: self.cursor,
        }
    }
}

/// The output, the rate everything is judged against, and where that rate came
/// from.
struct Subject {
    device: CaptureDevice,
    device_open_ms: f64,
    report: DeviceReport,
    source_hz: f64,
    overridden: bool,
}

fn open_subject(plan: &Plan) -> Result<Subject, Box<dyn Error>> {
    let started = Timestamp::now();
    let device = CaptureDevice::open(plan.output)?;
    let device_open_ms = Timestamp::now().saturating_since(started).as_millis_f64();

    let detected = display::detect(&device);
    let source_hz = match (plan.source_hz, detected) {
        (Some(hz), _) if hz > 0.0 => hz,
        (_, Some(mode)) => mode.hz(),
        _ => {
            return Err(format!(
                "could not read the current mode of {}; pass --source-hz to say what rate to \
                 judge the capture against",
                device.identity().output
            )
            .into());
        }
    };

    let identity = device.identity();
    let report = DeviceReport {
        adapter: identity.adapter.clone(),
        luid: identity.luid,
        vendor_id: identity.vendor_id,
        device_id: identity.device_id,
        dedicated_vram_mb: identity.dedicated_vram_mb,
        feature_level: format!(
            "{}.{}",
            identity.feature_level >> 12,
            (identity.feature_level >> 8) & 0xf
        ),
        output: identity.output.clone(),
        output_width: identity.output_width,
        output_height: identity.output_height,
        refresh_numerator: detected.map(|mode| mode.numerator).unwrap_or(0),
        refresh_denominator: detected.map(|mode| mode.denominator).unwrap_or(0),
        refresh_hz: detected.map(|mode| mode.hz()).unwrap_or(0.0),
        refresh_source: detected
            .map(|mode| mode.source.to_owned())
            .unwrap_or_else(|| "undetected".to_owned()),
        description: identity.to_string(),
    };

    Ok(Subject {
        device,
        device_open_ms,
        report,
        source_hz,
        overridden: plan.source_hz.is_some_and(|hz| hz > 0.0),
    })
}

/// One backend, one scenario, warm-up then measurement.
pub fn single(plan: &Plan, kind: BackendKind) -> Result<RunReport, Box<dyn Error>> {
    let subject = open_subject(plan)?;
    let mut harness = Harness::new(&subject, plan);
    let mut capture = seam::open(kind, &subject.device)?;

    let started = Timestamp::now();
    capture.start(plan.capture_config())?;
    let backend_start_ms = Timestamp::now().saturating_since(started).as_millis_f64();

    harness.pump(
        &mut capture,
        Timestamp::now().add(Nanos::from_millis_f64(plan.warmup_seconds * 1_000.0)),
    )?;

    // Anything the backend counted during the warm-up belongs to the warm-up.
    harness.begin_window(&capture);
    let until = Timestamp::now().add(Nanos::from_millis_f64(plan.seconds * 1_000.0));
    harness.arm_stall(until);
    harness.pump(&mut capture, until)?;

    harness.finish_block(&mut capture, Timestamp::now());
    harness.finish_run(Timestamp::now());

    Ok(harness.report(&subject, plan, kind, backend_start_ms))
}

/// Both backends, alternating, so drift cannot be read as a difference.
pub fn compare(plan: &Plan, kind_seed: u64) -> Result<CompareReport, Box<dyn Error>> {
    let subject = open_subject(plan)?;
    let blocks = schedule::alternating(plan.seconds, plan.block_seconds, kind_seed);

    let mut harnesses = [
        (BackendKind::Wgc, Harness::new(&subject, plan)),
        (BackendKind::Dda, Harness::new(&subject, plan)),
    ];
    let mut starts = [0.0f64; 2];

    // Each backend gets half the warm-up, on its own, before any block runs.
    // Their first allocations are then behind them when the alternation starts.
    let warmup_each = Nanos::from_millis_f64(plan.warmup_seconds * 500.0);
    for (index, (kind, harness)) in harnesses.iter_mut().enumerate() {
        let mut capture = seam::open(*kind, &subject.device)?;
        let started = Timestamp::now();
        capture.start(plan.capture_config())?;
        starts[index] = Timestamp::now().saturating_since(started).as_millis_f64();
        harness.pump(&mut capture, Timestamp::now().add(warmup_each))?;
        harness.finish_block(&mut capture, Timestamp::now());
    }
    for (_, harness) in &mut harnesses {
        harness.begin_window_cold();
    }

    let mut block_reports = Vec::with_capacity(blocks.len());
    for block in &blocks {
        let index = usize::from(block.backend == BackendKind::Dda);
        let harness = &mut harnesses[index].1;

        let mut capture = seam::open(block.backend, &subject.device)?;
        capture.start(plan.capture_config())?;

        let lead_in_until = Timestamp::now().add(BLOCK_LEAD_IN);
        harness.pump(&mut capture, lead_in_until)?;

        let measured_from = Timestamp::now();
        let mark = harness.stats.mark();
        let until = measured_from.add(Nanos::from_millis_f64(
            (block.seconds * 1_000.0) - BLOCK_LEAD_IN.as_millis_f64(),
        ));
        harness.pump(&mut capture, until)?;
        let measured_to = Timestamp::now();
        harness.finish_block(&mut capture, measured_to);

        let seconds = measured_to.saturating_since(measured_from).as_secs_f64();
        let stats = harness.stats.block(mark, seconds);
        block_reports.push(BlockReport {
            index: block.index,
            backend: block.backend.as_str().to_owned(),
            seconds,
            frames: stats.frames,
            frames_per_second: stats.frames_per_second,
            delivery_delay: stats.delivery,
            acquire: stats.acquire,
            interval: stats.interval,
            intervals_over_2x: stats.intervals_over_2x,
            access_lost: stats.access_lost,
        });
    }

    let finished = Timestamp::now();
    for (_, harness) in &mut harnesses {
        harness.finish_run(finished);
    }

    let [(_, wgc), (_, dda)] = harnesses;
    let mut report = CompareReport::new();
    report.device = subject.report.clone();
    report.config = config_report(plan, &subject, Some(blocks[0].seconds), Some(kind_seed));
    report.blocks = block_reports;
    report.wgc = wgc.report(&subject, plan, BackendKind::Wgc, starts[0]);
    report.dda = dda.report(&subject, plan, BackendKind::Dda, starts[1]);
    Ok(report)
}

fn config_report(
    plan: &Plan,
    subject: &Subject,
    block_seconds: Option<f64>,
    seed: Option<u64>,
) -> ConfigReport {
    ConfigReport {
        seconds: plan.seconds,
        warmup_seconds: plan.warmup_seconds,
        buffers: plan.buffers,
        output: plan.output,
        acquire_timeout_ms: plan.acquire_timeout_ms,
        cursor: plan.cursor,
        source_hz: subject.source_hz,
        source_hz_overridden: subject.overridden,
        stall_ms: plan.stall_ms,
        pool: (plan.mode == Mode::Handoff).then_some(plan.pool),
        hold_ms: (plan.mode == Mode::Handoff).then_some(plan.hold_ms),
        seed,
        block_seconds,
    }
}

struct Harness<'device> {
    device: &'device CaptureDevice,
    mode: Mode,
    pool_size: u32,
    hold: Nanos,
    stats: Stats,

    pool: Option<TexturePool>,
    timer: Option<CopyTimer>,
    /// Owned textures whose simulated downstream time has not elapsed.
    holds: Vec<(PoolHandle, Timestamp)>,
    pool_create_ms: Option<f64>,
    owned_pool_rebuilds: u64,
    pool_cpu_accessible: bool,
    starvation_baseline: u64,
    starvation: Trend,

    first_frame_at: Option<Timestamp>,
    backend_started_at: Timestamp,
    warmup_frames: u64,
    /// When the frame currently on loan was handed over. The release happens
    /// at the head of the next acquire, so this is what the hold is measured
    /// against.
    pending_source_hold: Option<Timestamp>,
    skip_next_source_hold: bool,
    next_sample: Timestamp,

    stall: Nanos,
    stall_at: Option<Timestamp>,
    awaiting_stall_frame: bool,
    recovery: Option<u64>,
    recovery_limit: u64,
    injected: Option<InjectedStallReport>,

    extras: Extras,
    extras_baseline: Extras,
    cpu_baseline: Option<(Nanos, Timestamp)>,
    cpu_percent: Option<f64>,
    working_set_start: Option<u64>,
    working_set: Option<u64>,
}

impl<'device> Harness<'device> {
    fn new(subject: &'device Subject, plan: &Plan) -> Harness<'device> {
        let now = Timestamp::now();
        Harness {
            device: &subject.device,
            mode: plan.mode,
            pool_size: plan.pool.max(1),
            hold: Nanos::from_millis_f64(plan.hold_ms),
            stats: Stats::new(period_for(subject.source_hz)),
            pool: None,
            timer: None,
            holds: Vec::new(),
            pool_create_ms: None,
            owned_pool_rebuilds: 0,
            pool_cpu_accessible: false,
            starvation_baseline: 0,
            starvation: Trend::new(),
            first_frame_at: None,
            backend_started_at: now,
            warmup_frames: 0,
            pending_source_hold: None,
            skip_next_source_hold: false,
            next_sample: now,
            stall: Nanos::from_millis(plan.stall_ms),
            stall_at: None,
            awaiting_stall_frame: false,
            recovery: None,
            recovery_limit: (RECOVERY_LIMIT_SECONDS * subject.source_hz).ceil() as u64,
            injected: None,
            extras: Extras::default(),
            extras_baseline: Extras::default(),
            cpu_baseline: None,
            cpu_percent: None,
            working_set_start: None,
            working_set: None,
        }
    }

    fn pump(&mut self, capture: &mut Capture, until: Timestamp) -> Result<(), CaptureError> {
        let device = self.device;
        while Timestamp::now() < until {
            let now = Timestamp::now();
            self.retire_holds(now);
            if let Some(timer) = &mut self.timer {
                timer.poll(device.context(), now);
            }
            self.sample(now);
            self.maybe_stall(now);

            let before = Timestamp::now();
            if let Some(ready) = self.pending_source_hold.take() {
                if self.skip_next_source_hold {
                    self.skip_next_source_hold = false;
                } else if self.mode == Mode::Handoff {
                    self.stats.source_held(before.saturating_since(ready));
                }
            }

            match capture.acquire()? {
                Acquired::Frame(frame) => {
                    let acquired = frame.acquired;
                    if self.first_frame_at.is_none() {
                        self.first_frame_at = Some(acquired);
                    }
                    if self.awaiting_stall_frame {
                        self.awaiting_stall_frame = false;
                        if let Some(stall) = &mut self.injected {
                            stall.first_acquire_ms =
                                acquired.saturating_since(before).as_millis_f64();
                            stall.first_frame_delivery_delay_ms =
                                frame.delivery_delay().map(|delay| delay.as_millis_f64());
                            stall.first_frame_accumulated = frame.metadata.accumulated_frames;
                        }
                        self.recovery = Some(0);
                    }

                    let class = self.stats.frame(FrameObservation {
                        acquired,
                        source: frame.source.at(),
                        delivery: frame.delivery_delay(),
                        duration: acquired.saturating_since(before),
                        accumulated: frame.metadata.accumulated_frames,
                        pending: frame.metadata.pending,
                        duplicate: frame.metadata.duplicate,
                    });
                    self.track_recovery(class);

                    if self.mode == Mode::Handoff {
                        self.copy_out(&frame)?;
                    }
                    self.pending_source_hold = Some(acquired);
                }
                Acquired::Timeout => self.stats.timeout(),
                Acquired::Lost => {
                    // Expected, not exceptional: a mode change, a desktop
                    // switch or a fullscreen transition. Rebuild and carry on.
                    self.stats.lost();
                    match capture.restart() {
                        Ok(()) => self.stats.restarted(),
                        Err(_) => self.stats.restart_failed(),
                    }
                    // Nothing is on loan across a restart.
                    self.pending_source_hold = None;
                }
            }
        }
        Ok(())
    }

    fn retire_holds(&mut self, now: Timestamp) {
        let Some(pool) = &mut self.pool else {
            return;
        };
        let holds = &mut self.holds;
        let mut index = 0;
        while index < holds.len() {
            if holds[index].1 <= now {
                let (handle, _) = holds.swap_remove(index);
                pool.release(handle);
            } else {
                index += 1;
            }
        }
    }

    fn sample(&mut self, now: Timestamp) {
        if now < self.next_sample {
            return;
        }
        self.next_sample = now.add(SAMPLE_INTERVAL);
        if let Some(bytes) = resident_bytes() {
            self.stats.sample_memory(now, bytes);
            self.working_set = Some(bytes);
        }
        self.stats.sample_backlog(now);
        if let Some(pool) = &self.pool {
            self.starvation.record_at(
                now,
                pool.starved().saturating_sub(self.starvation_baseline) as f64,
            );
        }
    }

    fn arm_stall(&mut self, until: Timestamp) {
        if self.stall == Nanos::ZERO {
            return;
        }
        // Halfway through the measured window: far enough in that the capture
        // is settled, far enough from the end that the recovery is observable.
        let half = Nanos(until.saturating_since(Timestamp::now()).get() / 2);
        self.stall_at = Some(Timestamp::now().add(half));
    }

    fn maybe_stall(&mut self, now: Timestamp) {
        let Some(at) = self.stall_at else {
            return;
        };
        if now < at {
            return;
        }
        self.stall_at = None;

        let began = Timestamp::now();
        std::thread::sleep(self.stall.as_duration());
        let ended = Timestamp::now();

        // The gap and the hold that spans it are the harness deliberately not
        // consuming, so neither is charged to the API.
        self.stats.skip_next_interval();
        self.skip_next_source_hold = true;
        self.awaiting_stall_frame = true;
        self.next_sample = ended.add(SAMPLE_INTERVAL);
        self.injected = Some(InjectedStallReport {
            requested_ms: self.stall.get() / 1_000_000,
            actual_ms: ended.saturating_since(began).as_millis_f64(),
            ..InjectedStallReport::default()
        });
    }

    fn track_recovery(&mut self, class: Option<StallClass>) {
        let (Some(frames), Some(class)) = (self.recovery, class) else {
            return;
        };
        let frames = frames + 1;
        let Some(stall) = &mut self.injected else {
            self.recovery = None;
            return;
        };
        if class == StallClass::OnCadence {
            stall.recovered = true;
            stall.frames_to_recover = Some(frames);
            self.recovery = None;
        } else if frames >= self.recovery_limit {
            stall.recovered = false;
            stall.frames_to_recover = None;
            self.recovery = None;
        } else {
            self.recovery = Some(frames);
        }
    }

    fn ensure_pool(&mut self, width: u32, height: u32) -> Result<(), CaptureError> {
        if self
            .pool
            .as_ref()
            .is_some_and(|pool| pool.matches(width, height))
        {
            return Ok(());
        }
        // A resolution change makes every owned texture the wrong size, so the
        // holds die with the pool that backs them.
        let rebuilding = self.pool.is_some();
        self.holds.clear();

        let started = Timestamp::now();
        let mut pool = TexturePool::new(self.device.device(), self.pool_size, width, height)?;
        let elapsed = Timestamp::now().saturating_since(started);

        self.pool_cpu_accessible = pool_is_cpu_accessible(&mut pool, self.pool_size);
        self.starvation_baseline = pool.starved();
        if rebuilding {
            self.owned_pool_rebuilds += 1;
        } else {
            self.pool_create_ms = Some(elapsed.as_millis_f64());
        }
        self.pool = Some(pool);

        // Query sets do not depend on the texture size, so a rebuild keeps
        // the ring and everything it has already measured. One deeper than the
        // pool: a copy must never go unmeasured merely because a result has
        // not been collected yet.
        if self.timer.is_none() {
            self.timer = Some(CopyTimer::new(self.device.device(), self.pool_size + 2)?);
        }
        Ok(())
    }

    fn copy_out(&mut self, frame: &CapturedFrame<'_>) -> Result<(), CaptureError> {
        self.ensure_pool(frame.width, frame.height)?;
        let context = self.device.context();

        let pool = self.pool.as_mut().expect("ensure_pool built one");
        // No free slot is a result, not a problem to wait out: the pool counts
        // it and the report says how often the rate outran the pool.
        let Some(handle) = pool.take() else {
            return Ok(());
        };
        let timer = self.timer.as_mut().expect("ensure_pool built one");
        let slot = timer.open(context);

        let submitted = Timestamp::now();
        // SAFETY: both textures are B8G8R8A8_UNORM of identical size on this
        // device, and the source is valid for as long as `frame` is borrowed.
        unsafe { context.CopyResource(pool.texture(&handle), frame.texture) };
        let cpu = Timestamp::now().saturating_since(submitted);

        if let Some(slot) = slot {
            timer.close(context, slot, submitted);
        }
        timer.submitted(cpu);
        self.holds.push((handle, submitted.add(self.hold)));
        Ok(())
    }

    /// Ends the warm-up: everything measured from here counts.
    fn begin_window(&mut self, capture: &Capture) {
        self.extras_baseline = capture.extras();
        self.begin_window_cold();
    }

    /// The same, for `compare`, where the warm-up ran on a backend instance
    /// that has already been destroyed and whose counters went with it.
    fn begin_window_cold(&mut self) {
        let now = Timestamp::now();
        self.warmup_frames = self.stats.frames;
        self.stats.begin_window(now);
        if let Some(timer) = &mut self.timer {
            timer.begin_window();
        }
        if let Some(pool) = &self.pool {
            self.starvation_baseline = pool.starved();
        }
        self.starvation = Trend::new();
        self.cpu_baseline = process_cpu_time().map(|cpu| (cpu, now));
        self.working_set_start = resident_bytes();
        self.next_sample = now;
    }

    /// Stops one backend instance, keeping what it counted.
    fn finish_block(&mut self, capture: &mut Capture, at: Timestamp) {
        if let Some(ready) = self.pending_source_hold.take()
            && self.mode == Mode::Handoff
            && !self.skip_next_source_hold
        {
            self.stats.source_held(at.saturating_since(ready));
        }
        self.skip_next_source_hold = false;
        self.extras = self
            .extras
            .plus(capture.extras().since(self.extras_baseline));
        self.extras_baseline = Extras::default();
        capture.stop();
    }

    fn finish_run(&mut self, at: Timestamp) {
        self.stats.end_window(at);
        if let Some(timer) = &mut self.timer {
            timer.drain(self.device.context());
        }
        if let (Some((baseline, from)), Some(now)) = (self.cpu_baseline, process_cpu_time()) {
            let wall = at.saturating_since(from).get();
            if wall > 0 {
                let used = now.get().saturating_sub(baseline.get());
                self.cpu_percent = Some(used as f64 / wall as f64 * 100.0);
            }
        }
        if let Some(bytes) = resident_bytes() {
            self.working_set = Some(bytes);
        }
    }

    fn report(
        &self,
        subject: &Subject,
        plan: &Plan,
        kind: BackendKind,
        backend_start_ms: f64,
    ) -> RunReport {
        let source_mark = match kind {
            BackendKind::Wgc => "compositor rendered",
            BackendKind::Dda => "desktop presented",
        };

        let mut report = RunReport::new(plan.mode.as_str(), kind.as_str());
        report.backend_api = self.extras.api.to_owned();
        report.device = subject.report.clone();
        report.config = config_report(plan, subject, None, None);

        report.startup = StartupReport {
            device_open_ms: subject.device_open_ms,
            backend_start_ms,
            pool_create_ms: self.pool_create_ms,
            first_frame_ms: self
                .first_frame_at
                .map(|at| at.saturating_since(self.backend_started_at).as_millis_f64()),
            warmup_frames: self.warmup_frames,
            warmup_seconds: plan.warmup_seconds,
        };

        report.capture = self.stats.capture_report(subject.source_hz, source_mark);
        report.capture.superseded = self.extras.superseded;
        report.capture.drained = self.extras.drained_total;

        report.stability = self.stats.stability_report();
        report.stability.pool_recreations = self.extras.pool_recreations;
        report.stability.border_suppressed = self.extras.border_suppressed;
        report.stability.pool_cpu_accessible = self.pool_cpu_accessible;
        // Structurally zero: nothing in this crate maps a resource or copies
        // into a staging texture, and the check above proves the pool could
        // not be mapped even if something tried.
        report.stability.mapped_bytes = 0;

        report.system = SystemReport {
            process_cpu_percent: self.cpu_percent,
            working_set_bytes: self.working_set,
            working_set_start_bytes: self.working_set_start,
            memory_slope_bytes_per_min: self.stats.memory.slope_per_minute(),
            memory_samples: self.stats.memory.count(),
        };

        if self.mode == Mode::Handoff {
            let (copies, resolved) = self
                .timer
                .as_ref()
                .map(|timer| (timer.copies(), timer.resolved()))
                .unwrap_or((0, 0));
            report.handoff = Some(HandoffReport {
                pool_size: self.pool_size,
                hold_ms: plan.hold_ms,
                copies,
                copy_submit_cpu: self
                    .timer
                    .as_ref()
                    .map(CopyTimer::submit_cpu)
                    .unwrap_or_default(),
                copy_gpu: self.timer.as_ref().map(CopyTimer::gpu).unwrap_or_default(),
                copy_completion_observed: self
                    .timer
                    .as_ref()
                    .map(CopyTimer::completion)
                    .unwrap_or_default(),
                source_hold: self.stats.source_hold.summary(),
                pool_starvation: self
                    .pool
                    .as_ref()
                    .map(|pool| pool.starved().saturating_sub(self.starvation_baseline))
                    .unwrap_or(0),
                pool_starvation_slope_per_min: self.starvation.slope_per_minute(),
                owned_pool_rebuilds: self.owned_pool_rebuilds,
                queries_resolved: resolved,
                queries_slot_exhausted: self.timer.as_ref().map(CopyTimer::exhausted).unwrap_or(0),
                queries_disjoint_discarded: self
                    .timer
                    .as_ref()
                    .map(CopyTimer::disjoint_discarded)
                    .unwrap_or(0),
                queries_unresolved_at_exit: self
                    .timer
                    .as_ref()
                    .map(CopyTimer::unresolved)
                    .unwrap_or(0),
                gpu_result_fraction: if copies > 0 {
                    resolved as f64 / copies as f64
                } else {
                    0.0
                },
            });
        }

        report.injected_stall = self.injected.clone();
        report
    }
}

/// Whether any pool texture could be read by the CPU.
///
/// A real check rather than an assertion about the source: if `TexturePool`
/// ever gains a CPU-accessible flag, the gate says so instead of the report
/// quietly becoming a measurement of the PCIe bus.
fn pool_is_cpu_accessible(pool: &mut TexturePool, count: u32) -> bool {
    let mut taken = Vec::with_capacity(count as usize);
    let mut accessible = false;
    for _ in 0..count {
        let Some(handle) = pool.take() else {
            break;
        };
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the handle came from this pool, so the texture is live, and
        // GetDesc only writes the description.
        unsafe { pool.texture(&handle).GetDesc(&raw mut desc) };
        accessible |= desc.CPUAccessFlags != 0 || desc.Usage == D3D11_USAGE_STAGING;
        taken.push(handle);
    }
    for handle in taken {
        pool.release(handle);
    }
    accessible
}

/// Kernel plus user time this process has consumed.
fn process_cpu_time() -> Option<Nanos> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all four out-pointers address live locals for the whole call and
    // the pseudo-handle from GetCurrentProcess needs no release.
    unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .ok()?;
    Some(Nanos(filetime_nanos(kernel) + filetime_nanos(user)))
}

/// `FILETIME` counts hundreds of nanoseconds.
fn filetime_nanos(time: FILETIME) -> u64 {
    ((time.dwHighDateTime as u64) << 32 | time.dwLowDateTime as u64) * 100
}
