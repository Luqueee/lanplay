use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use lanplay_telemetry::{Nanos, Recorder, Stage, Timestamp};
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
use objc2_foundation::{
    MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSObject, NSObjectProtocol, NSRunLoop,
};
use objc2_metal::MTLCreateSystemDefaultDevice;
use objc2_quartz_core::{
    CAFrameRateRange, CAMetalDisplayLink, CAMetalDisplayLinkDelegate, CAMetalDisplayLinkUpdate,
    CAMetalDrawable, CAMetalLayer,
};

use crate::environment::{Environment, LiveCounters, Watcher, bump, read};
use crate::error::RendererError;
use crate::gpu::Gpu;
use crate::slot::{LatestFrameSlot, SurfaceFrame};
use crate::stats::{Percentiles, Track};
use crate::window::Surface;

/// What decides when a frame is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveMode {
    /// Draw as soon as a frame appears, then let `nextDrawable` apply the
    /// compositor's back-pressure. Lowest possible delay from decode to
    /// submit, at the cost of submitting work the display may not be ready
    /// for.
    Immediate,
    /// Draw once per display refresh, on a `CAMetalDisplayLink` callback. The
    /// candidate architecture: the display dictates the rhythm and the slot
    /// absorbs everything the producer does in between.
    DisplayLink,
}

pub struct RendererConfig {
    pub width: u32,
    pub height: u32,
    pub title: String,
    pub mode: DriveMode,
    pub recorder: Recorder,
    pub stop: Arc<AtomicBool>,
    /// Test-only: burn this long before presenting, to model a slow renderer.
    pub render_delay: Option<Duration>,
    /// Stop after this many presents (None = until `stop` or the window closes).
    pub present_limit: Option<u64>,
    /// Read by a supervising thread while the run is in progress, so a long
    /// measurement can be watched rather than only autopsied.
    pub counters: Arc<LiveCounters>,
    /// The refresh rate this run needs. When set, `run` inspects the window
    /// before entering the loop and refuses to start in an environment that
    /// would make the measurement meaningless. `None` skips the check.
    pub require_clean_environment: Option<f64>,
    /// Called once, after the environment check passes and before the run
    /// loop starts. The client prints the preflight terminator here, which is
    /// the signal the orchestrator uses to start the remote sender.
    pub on_ready: Option<Box<dyn FnOnce() + Send>>,
}

/// `Default` describes a run that never rendered: `--link-only`, where there
/// is no window and no display link, and every field below is an honest zero
/// rather than a measurement.
#[derive(Clone, Debug, Default)]
pub struct RenderStats {
    pub rendered: u64,
    /// What the display is capable of, as distinct from `display_hz`, which
    /// is what the link actually delivered. A suspended link drags the
    /// achieved rate down with it, so only the nominal rate can tell a run
    /// that was throttled from one that had nothing to show.
    pub nominal_hz: f64,
    /// Frames the producer threw away because the renderer had not taken the
    /// previous one yet. Read from the slot, so it counts drops that happened
    /// while the renderer was busy or asleep.
    pub superseded: u64,
    /// Ticks that found the slot empty: the renderer was ready and no frame
    /// had arrived.
    pub empty_ticks: u64,
    /// Time spent acquiring a drawable, measured on its own because that is
    /// where the compositor pushes back when it has no free surface.
    ///
    /// Only [`DriveMode::Immediate`] measures a real wait: it calls
    /// `nextDrawable` itself and blocks there. In [`DriveMode::DisplayLink`]
    /// the link has already reserved a drawable before it calls back, so this
    /// reads as a handful of nanoseconds by construction — the back-pressure
    /// shows up as the link skipping a refresh instead.
    pub drawable_wait: Percentiles,
    /// CPU time from binding the pixel buffer's planes to `commit` returning.
    /// Excludes the drawable wait and all GPU execution.
    pub encode_cpu: Percentiles,
    /// Presentation rate. In [`DriveMode::DisplayLink`] this is the display's
    /// own figure, derived from the median gap between the link's successive
    /// `targetPresentationTimestamp`s. In [`DriveMode::Immediate`] no such
    /// signal exists and this is the measured cadence: presents divided by the
    /// span between the first and the last.
    pub display_hz: f64,
    /// Gap between consecutive display-link callbacks. Empty in
    /// [`DriveMode::Immediate`], which has no callbacks to space out: its loop
    /// spins as fast as the slot can be checked, so the interval between
    /// passes says nothing about the display.
    pub callback_interval: Percentiles,
    /// The window and display as the run found them, with `display_hz` filled
    /// in from what was actually measured.
    pub environment: Environment,
    /// Copies of [`LiveCounters`], read once the loop has ended, so a caller
    /// that did not keep the `Arc` still gets them.
    pub callbacks: u64,
    /// Zero by construction in [`DriveMode::DisplayLink`]: the link reserves
    /// the drawable before it calls back, so only [`DriveMode::Immediate`]
    /// can be refused one.
    pub missed_drawables: u64,
    pub occlusion_changes: u64,
    pub space_changes: u64,
    pub miniaturise_events: u64,
    pub display_changes: u64,
    pub link_pauses: u64,
}

impl core::fmt::Display for RenderStats {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        writeln!(
            f,
            "rendered {}  superseded {}  empty ticks {}  display {:.2} Hz",
            self.rendered, self.superseded, self.empty_ticks, self.display_hz
        )?;
        writeln!(f, "  drawable wait  {}", self.drawable_wait)?;
        writeln!(f, "  encode cpu     {}", self.encode_cpu)?;
        writeln!(f, "  callback gap   {}", self.callback_interval)?;
        write!(
            f,
            "  callbacks {}  missed drawables {}  link pauses {}  occlusion {}  space {}  miniaturise {}  display {}",
            self.callbacks,
            self.missed_drawables,
            self.link_pauses,
            self.occlusion_changes,
            self.space_changes,
            self.miniaturise_events,
            self.display_changes
        )
    }
}

/// Blocking. MUST be called on the main thread (AppKit).
pub fn run(
    mut config: RendererConfig,
    slot: Arc<LatestFrameSlot>,
) -> Result<RenderStats, RendererError> {
    let mtm = MainThreadMarker::new().ok_or(RendererError::NotMainThread)?;
    let device = MTLCreateSystemDefaultDevice().ok_or(RendererError::NoMetalDevice)?;
    let gpu = Gpu::new(&device)?;
    let surface = Surface::open(mtm, &device, config.width, config.height, &config.title)?;

    // Which display the window landed on decides the ceiling for everything
    // measured below, so it is stated once rather than left to be guessed.
    eprintln!(
        "renderer: {} mode on \"{}\" ({:.0} Hz nominal), drawable {}x{}",
        match config.mode {
            DriveMode::Immediate => "immediate",
            DriveMode::DisplayLink => "display-link",
        },
        surface.display_name,
        surface.nominal_hz,
        surface.drawable_width,
        surface.drawable_height,
    );

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    // Launched from a terminal there is no bundle and no -run, so AppKit has
    // not finished launching; without this the window never draws.
    app.finishLaunching();
    app.activate();
    surface.window.makeKeyAndOrderFront(None);
    settle(&app);

    // `FnOnce` has to be owned to be called, and the mode functions only
    // borrow the config.
    let on_ready = config.on_ready.take();
    // The fallback pause threshold, before the link has spoken for itself.
    let expected_period = Nanos(if surface.nominal_hz > 0.0 {
        (1e9 / surface.nominal_hz) as u64
    } else {
        0
    });
    let core = RenderLoop::new(gpu, slot, &config, expected_period);
    let result = match config.mode {
        DriveMode::Immediate => immediate(&app, &surface, core, &config, on_ready),
        DriveMode::DisplayLink => display_link(mtm, &app, &surface, core, &config, on_ready),
    };

    // Whatever ended the loop — the limit, the stop flag, the user closing the
    // window — the producer has no reason to keep decoding.
    config.stop.store(true, Ordering::Relaxed);
    surface.window.close();
    result
}

/// Everything the two drive modes share: the GPU, the slot, and the counters.
struct RenderLoop {
    gpu: Gpu,
    slot: Arc<LatestFrameSlot>,
    recorder: Recorder,
    render_delay: Option<Nanos>,
    present_limit: Option<u64>,
    rendered: u64,
    empty_ticks: u64,
    /// Ticks where a frame was taken but the compositor refused a drawable.
    missed_drawables: u64,
    drawable_wait: Track,
    encode_cpu: Track,
    /// Gaps between the display link's successive target presentation times.
    link_cadence: Track,
    last_target: Option<f64>,
    /// Gaps between consecutive display-link callbacks, measured on the local
    /// clock rather than from the link's target times: a suspended link keeps
    /// its target times consistent and simply stops calling, so only the
    /// arrival of the callback shows the pause.
    callback_interval: Track,
    last_callback: Option<Timestamp>,
    /// Twice this marks a callback gap as a pause. It starts from what the
    /// display advertises and is replaced by the link's own median cadence as
    /// soon as there are enough samples to trust it.
    expected_period: Nanos,
    cadence_samples: u64,
    counters: Arc<LiveCounters>,
    first_present: Option<Timestamp>,
    last_present: Option<Timestamp>,
}

/// How many cadence samples to gather before believing the link over the
/// display's advertised rate, and how often to revise that belief afterwards.
/// A second's worth at 120 Hz: long enough that a couple of slow first frames
/// do not set the threshold, short enough to follow a display that changes
/// mode mid-run.
const CADENCE_REVISION: u64 = 120;

impl RenderLoop {
    fn new(
        gpu: Gpu,
        slot: Arc<LatestFrameSlot>,
        config: &RendererConfig,
        expected_period: Nanos,
    ) -> RenderLoop {
        RenderLoop {
            gpu,
            slot,
            recorder: config.recorder.clone(),
            render_delay: config.render_delay.map(|d| Nanos(d.as_nanos() as u64)),
            present_limit: config.present_limit,
            rendered: 0,
            empty_ticks: 0,
            missed_drawables: 0,
            drawable_wait: Track::new(),
            encode_cpu: Track::new(),
            link_cadence: Track::new(),
            last_target: None,
            callback_interval: Track::new(),
            last_callback: None,
            expected_period,
            cadence_samples: 0,
            counters: Arc::clone(&config.counters),
            first_present: None,
            last_present: None,
        }
    }

    fn finished(&self) -> bool {
        self.present_limit
            .is_some_and(|limit| self.rendered >= limit)
    }

    /// Takes the newest frame and starts its render, marking the instant it
    /// left the slot. Returns `None` when there was nothing to draw.
    fn take_frame(&mut self) -> Option<SurfaceFrame> {
        let Some(frame) = self.slot.take() else {
            self.empty_ticks += 1;
            bump(&self.counters.empty_ticks);
            return None;
        };
        self.recorder.mark(frame.id, Stage::RenderSubmit);
        if let Some(delay) = self.render_delay {
            burn(delay);
        }
        Some(frame)
    }

    /// Immediate mode asks the layer for a drawable itself, which is where the
    /// compositor's back-pressure lands. Reports whether it drew.
    fn tick_from_layer(&mut self, layer: &CAMetalLayer) -> Result<bool, RendererError> {
        // Immediate mode has no callbacks, so what it counts here is draw
        // attempts: one per pass of its loop, whether or not a frame was
        // waiting.
        bump(&self.counters.callbacks);
        let Some(frame) = self.take_frame() else {
            return Ok(false);
        };
        let wait_start = Timestamp::now();
        let Some(drawable) = layer.nextDrawable() else {
            self.missed_drawables += 1;
            bump(&self.counters.missed_drawables);
            return Ok(false);
        };
        let wait = Timestamp::now().saturating_since(wait_start);
        self.present(frame, &drawable, wait)?;
        Ok(true)
    }

    /// Display-link mode is handed a drawable that the link already reserved,
    /// so the acquisition itself is a pointer read; the waiting happened
    /// before the callback fired.
    fn tick_from_link(&mut self, update: &CAMetalDisplayLinkUpdate) -> Result<(), RendererError> {
        self.record_callback(Timestamp::now());
        self.record_cadence(update.targetPresentationTimestamp());
        let Some(frame) = self.take_frame() else {
            return Ok(());
        };
        let wait_start = Timestamp::now();
        let drawable = update.drawable();
        let wait = Timestamp::now().saturating_since(wait_start);
        self.present(frame, &drawable, wait)
    }

    fn present(
        &mut self,
        frame: SurfaceFrame,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
        wait: Nanos,
    ) -> Result<(), RendererError> {
        let id = frame.id;
        let encode_start = Timestamp::now();
        self.gpu.draw(frame, drawable)?;
        let committed = Timestamp::now();

        self.recorder.mark_at(id, Stage::PresentSubmit, committed);
        self.drawable_wait.record(wait);
        self.encode_cpu
            .record(committed.saturating_since(encode_start));
        self.rendered += 1;
        bump(&self.counters.rendered);
        self.first_present.get_or_insert(committed);
        self.last_present = Some(committed);
        Ok(())
    }

    fn record_cadence(&mut self, target: f64) {
        if let Some(previous) = self.last_target.replace(target) {
            let gap = target - previous;
            // A paused or restarted link leaves a gap of many frames; folding
            // that into the cadence would misreport the refresh rate.
            if gap > 0.0 && gap < 0.1 {
                self.link_cadence.record(Nanos((gap * 1e9) as u64));
                self.cadence_samples += 1;
                // Revising from the histogram costs a percentile query, so it
                // happens once a second rather than once a frame.
                if self.cadence_samples % CADENCE_REVISION == 0 {
                    let median = self.link_cadence.percentiles().p50;
                    if median.0 > 0 {
                        self.expected_period = median;
                    }
                }
            }
        }
    }

    /// Counts the callback and measures how long it has been since the last
    /// one. A gap of more than two periods is the signature of a link macOS
    /// suspended, which is the failure this whole gate exists to catch.
    fn record_callback(&mut self, now: Timestamp) {
        bump(&self.counters.callbacks);
        let Some(previous) = self.last_callback.replace(now) else {
            return;
        };
        let gap = now.saturating_since(previous);
        self.callback_interval.record(gap);
        if gap.0 > 2 * self.expected_period.0 {
            bump(&self.counters.link_pauses);
        }
    }

    fn stats(&self, mode: DriveMode, mut environment: Environment) -> RenderStats {
        // The environment was read before the first frame, when the link had
        // only been asked for a rate; what it actually delivered is known now.
        environment.display_hz = self.display_hz(mode);
        RenderStats {
            rendered: self.rendered,
            superseded: self.slot.superseded(),
            empty_ticks: self.empty_ticks,
            // The loop already carries the display's period; the rate is its
            // reciprocal, and threading a second copy of the same fact would
            // be one more thing that can disagree with itself.
            nominal_hz: if self.expected_period.get() > 0 {
                1e9 / self.expected_period.get() as f64
            } else {
                0.0
            },
            drawable_wait: self.drawable_wait.percentiles(),
            encode_cpu: self.encode_cpu.percentiles(),
            display_hz: environment.display_hz,
            callback_interval: self.callback_interval.percentiles(),
            environment,
            callbacks: read(&self.counters.callbacks),
            missed_drawables: read(&self.counters.missed_drawables),
            occlusion_changes: read(&self.counters.occlusion_changes),
            space_changes: read(&self.counters.space_changes),
            miniaturise_events: read(&self.counters.miniaturise_events),
            display_changes: read(&self.counters.display_changes),
            link_pauses: read(&self.counters.link_pauses),
        }
    }

    fn display_hz(&self, mode: DriveMode) -> f64 {
        let from_link = self.link_cadence.percentiles();
        if mode == DriveMode::DisplayLink && from_link.count > 0 && from_link.p50.0 > 0 {
            return 1e9 / from_link.p50.0 as f64;
        }
        match (self.first_present, self.last_present) {
            (Some(first), Some(last)) if self.rendered > 1 => {
                let span = last.saturating_since(first).as_secs_f64();
                if span > 0.0 {
                    (self.rendered - 1) as f64 / span
                } else {
                    0.0
                }
            }
            _ => 0.0,
        }
    }
}

/// Busy-waits, because a slow renderer occupies the main thread rather than
/// yielding it, and `sleep` would hand the time back to the producer.
fn burn(delay: Nanos) {
    let deadline = Timestamp::now().add(delay);
    while Timestamp::now() < deadline {
        core::hint::spin_loop();
    }
}

/// Whether the loop should stop.
///
/// Deliberately does not consult `isVisible`. That reads false for a window
/// that is merely hidden, minimised, or on another Space, and using it here
/// ended runs silently part-way through: a twenty-second run reported twelve
/// seconds of perfect numbers and nothing to say it had been cut short.
/// Truncating a measurement is worse than drawing into a window nobody is
/// looking at, so the run ends when it is told to and not before.
fn should_stop(stop: &AtomicBool, core: &RenderLoop) -> bool {
    stop.load(Ordering::Relaxed) || core.finished()
}

/// Drains the event queue without blocking. Without this the window would not
/// respond to the close button, and after a few seconds macOS would mark the
/// process unresponsive.
fn pump(app: &NSApplication) {
    // SAFETY: reading a framework constant.
    let mode = unsafe { NSDefaultRunLoopMode };
    let past = NSDate::distantPast();
    while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
        NSEventMask::Any,
        Some(&past),
        mode,
        true,
    ) {
        app.sendEvent(&event);
    }
}

/// Lets AppKit publish the window's occlusion state before anything is judged
/// by it.
///
/// `occlusionState` is maintained by the window server, not set by
/// `orderFront`, and a window that has just appeared reads as not visible for
/// the first few passes of the run loop. Preflighting before it settles would
/// fail a perfectly clean desktop.
fn settle(app: &NSApplication) {
    let run_loop = NSRunLoop::currentRunLoop();
    // SAFETY: reading a framework constant.
    let mode = unsafe { NSDefaultRunLoopMode };
    let deadline = Timestamp::now().add(Nanos(SETTLE_NS));
    while Timestamp::now() < deadline {
        autoreleasepool(|_| {
            pump(app);
            run_loop.runMode_beforeDate(mode, &NSDate::dateWithTimeIntervalSinceNow(0.01));
        });
    }
}

/// Long enough for the window server to publish an occlusion state, short
/// enough that a preflight failure is still reported inside the first second.
const SETTLE_NS: u64 = 250_000_000;

/// Reports what was inspected and refuses the run if the environment would
/// make its numbers meaningless.
///
/// One line per item, on stdout, so an orchestrator can read the reasons
/// rather than infer them from an exit code. The terminator is the caller's
/// to print: only the client knows what else it checked.
fn preflight(environment: &Environment, required: Option<f64>) -> Result<(), RendererError> {
    let Some(required_hz) = required else {
        return Ok(());
    };
    let mut problems = Vec::new();
    for check in environment.checks(required_hz) {
        let verdict = if check.passed { "ok" } else { "FAIL" };
        println!("preflight: {verdict} {} — {}", check.name, check.detail);
        if !check.passed {
            problems.push(check.detail);
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(RendererError::DirtyEnvironment(problems))
    }
}

fn immediate(
    app: &NSApplication,
    surface: &Surface,
    mut core: RenderLoop,
    config: &RendererConfig,
    on_ready: Option<Box<dyn FnOnce() + Send>>,
) -> Result<RenderStats, RendererError> {
    // Immediate mode has no link to ask, so the display's advertised rate is
    // the only figure available until frames have been presented.
    let environment = surface.environment(surface.nominal_hz);
    preflight(&environment, config.require_clean_environment)?;
    let mut watcher = Watcher::new(Arc::clone(&config.counters), &surface.window, &environment);
    if let Some(ready) = on_ready {
        ready();
    }

    while !should_stop(&config.stop, &core) {
        // One pool per iteration: a drawable, an event and a date per pass at
        // over a thousand passes a second is not something to let accumulate.
        let drew = autoreleasepool(|_| {
            pump(app);
            watcher.sample(&surface.window);
            core.tick_from_layer(&surface.layer)
        })?;

        // A frameless pass costs nothing but a slot check, so without this the
        // loop would spin a core flat while waiting. 100 us is a hundredth of
        // a frame at 120 Hz: invisible in the presentation wait, and the
        // difference between one busy core and none.
        if !drew {
            std::thread::sleep(Duration::from_micros(100));
        }
    }
    report_missed(&core);
    Ok(core.stats(DriveMode::Immediate, environment))
}

fn display_link(
    mtm: MainThreadMarker,
    app: &NSApplication,
    surface: &Surface,
    core: RenderLoop,
    config: &RendererConfig,
    on_ready: Option<Box<dyn FnOnce() + Send>>,
) -> Result<RenderStats, RendererError> {
    let core = Rc::new(RefCell::new(core));
    let target = LinkTarget::new(mtm, Rc::clone(&core));

    let link = CAMetalDisplayLink::initWithMetalLayer(CAMetalDisplayLink::alloc(), &surface.layer);
    // Ask for the panel's full rate. On a variable-refresh display the link
    // otherwise settles wherever the system thinks is polite, which is exactly
    // the number this renderer exists to measure.
    let hz = surface.nominal_hz as f32;
    link.setPreferredFrameRateRange(CAFrameRateRange::new(hz, hz, hz));

    // Preflight once the link exists but before it is wired to anything: a
    // dirty environment must cost a fraction of a second, not ten minutes, and
    // a link that never received a callback needs no orderly teardown.
    let environment = surface.environment(link.preferredFrameRateRange().preferred as f64);
    if let Err(error) = preflight(&environment, config.require_clean_environment) {
        link.invalidate();
        return Err(error);
    }
    let mut watcher = Watcher::new(Arc::clone(&config.counters), &surface.window, &environment);

    link.setDelegate(Some(ProtocolObject::from_ref(&*target)));
    let run_loop = NSRunLoop::currentRunLoop();
    // SAFETY: this is the main thread's run loop and the link is used, and
    // later removed, only from this thread.
    let mode = unsafe { NSDefaultRunLoopMode };
    unsafe { link.addToRunLoop_forMode(&run_loop, mode) };

    if let Some(ready) = on_ready {
        ready();
    }

    let outcome = loop {
        let failure = autoreleasepool(|_| {
            pump(app);
            watcher.sample(&surface.window);
            // Blocks until the link's source fires, so the callback runs here.
            // The ceiling only bounds how long a stop request can go unnoticed.
            let deadline = NSDate::dateWithTimeIntervalSinceNow(0.05);
            run_loop.runMode_beforeDate(mode, &deadline);
            target.ivars().failure.borrow_mut().take()
        });
        if let Some(error) = failure {
            break Err(error);
        }
        if should_stop(&config.stop, &core.borrow()) {
            break Ok(());
        }
    };

    // SAFETY: same run loop and thread the link was added on.
    unsafe { link.removeFromRunLoop_forMode(&run_loop, mode) };
    link.setDelegate(None);
    link.invalidate();

    outcome?;
    let core = core.borrow();
    report_missed(&core);
    Ok(core.stats(DriveMode::DisplayLink, environment))
}

fn report_missed(core: &RenderLoop) {
    if core.missed_drawables > 0 {
        eprintln!(
            "renderer: {} frames dropped because no drawable was available",
            core.missed_drawables
        );
    }
}

struct LinkIvars {
    core: Rc<RefCell<RenderLoop>>,
    /// A failure inside the callback cannot unwind into Objective-C, so it is
    /// parked here for the main loop to pick up.
    failure: RefCell<Option<RendererError>>,
}

define_class!(
    // SAFETY:
    // - NSObject imposes no subclassing requirements.
    // - `LinkTarget` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = LinkIvars]
    struct LinkTarget;

    // SAFETY: `NSObjectProtocol` has no safety requirements.
    unsafe impl NSObjectProtocol for LinkTarget {}

    // SAFETY: the selector and signature match the protocol declaration.
    unsafe impl CAMetalDisplayLinkDelegate for LinkTarget {
        #[unsafe(method(metalDisplayLink:needsUpdate:))]
        fn needs_update(&self, _link: &CAMetalDisplayLink, update: &CAMetalDisplayLinkUpdate) {
            let outcome = self.ivars().core.borrow_mut().tick_from_link(update);
            if let Err(error) = outcome {
                *self.ivars().failure.borrow_mut() = Some(error);
            }
        }
    }
);

impl LinkTarget {
    fn new(mtm: MainThreadMarker, core: Rc<RefCell<RenderLoop>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(LinkIvars {
            core,
            failure: RefCell::new(None),
        });
        // SAFETY: `NSObject`'s `init` takes no arguments and returns self.
        unsafe { msg_send![super(this), init] }
    }
}
