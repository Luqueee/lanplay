use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_decoder_videotoolbox::{DecodedFrame, DecoderConfig, VideoToolboxDecoder};
use lanplay_renderer_metal::{LatestFrameSlot, RenderStats, RendererConfig, SurfaceFrame};
use lanplay_telemetry::{
    Nanos, Recorder, Segment, Snapshot, Stage, Telemetry, TelemetryConfig, Timestamp, Trend,
    resident_bytes, wait_until,
};
use lanplay_video_core::{
    AccessUnitSource, FixtureSource, PixelFormat, VideoDecoder, ensure_fixture,
};

use crate::Cli;
use crate::gate::{GateInputs, evaluate};

/// How often the backlog and resident memory are sampled. Fast enough to fit a
/// slope through a one-minute run, slow enough to cost nothing.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
/// Time given to the pipeline to drain after the last access unit is
/// submitted, so frames already in flight are not counted as losses.
const DRAIN_GRACE: Duration = Duration::from_millis(300);

/// What the feeding thread learned, collected after it finishes so nothing has
/// to be shared with the render thread while the run is going.
struct FeedOutcome {
    submitted: u64,
    decoded: u64,
    errors: u64,
    dropped: u64,
    max_backlog: usize,
    trailing_backlog: usize,
    backlog: Trend,
}

pub fn run(cli: &Cli) -> Result<bool, Box<dyn Error>> {
    let spec = cli.fixture_spec();
    let path = ensure_fixture(&spec, &cli.fixture_dir)?;
    let mut source = FixtureSource::load(&path, cli.fps)?;
    // A run longer than the fixture wraps at an IDR rather than stopping.
    source.set_looping(true);

    let feed_fps = cli.feed_fps();
    let expected_frames = (feed_fps * cli.seconds).round() as u64;

    println!(
        "fixture {} ({} access units, {} IDR)",
        path.display(),
        source.access_unit_count(),
        source.idr_count()
    );
    println!(
        "feeding {feed_fps:.0} fps for {:.0} s ({expected_frames} access units), mode {}",
        cli.seconds,
        match cli.mode {
            crate::Mode::Immediate => "immediate",
            crate::Mode::DisplayLink => "display link",
        }
    );

    // One mark per frame from five stages, plus headroom for a burst; at
    // 120 fps this is over a minute of slack before anything could drop.
    let telemetry = Telemetry::start(TelemetryConfig {
        queue_capacity: 1 << 16,
        ..TelemetryConfig::default()
    });
    let recorder = telemetry.recorder();
    let slot = LatestFrameSlot::new();
    let stop = Arc::new(AtomicBool::new(false));

    let sink_slot = Arc::clone(&slot);
    let decoder = VideoToolboxDecoder::new(
        DecoderConfig {
            parameter_sets: source.parameter_sets().clone(),
            width: cli.width,
            height: cli.height,
            pixel_format: PixelFormat::Nv12VideoRange,
            require_hardware: !cli.allow_software_decoder,
            realtime: true,
            callback_delay: cli.decoder_callback_delay_ms.map(Duration::from_millis),
        },
        recorder.clone(),
        // Runs on a VideoToolbox thread: publish and return, nothing else.
        Box::new(move |frame: DecodedFrame| {
            sink_slot.publish(SurfaceFrame {
                id: frame.id,
                pixel_buffer: frame.pixel_buffer,
                decoded_at: frame.decoded_at,
            });
        }),
    )?;
    let hardware_decoder = decoder.uses_hardware_decoder();
    println!(
        "decoder: {} acceleration",
        if hardware_decoder {
            "hardware"
        } else {
            "SOFTWARE"
        }
    );
    let memory_stop = Arc::clone(&stop);
    let memory_sampler = thread::Builder::new()
        .name("memory".into())
        .spawn(move || sample_memory(memory_stop))?;

    let feed_stop = Arc::clone(&stop);
    let feed = thread::Builder::new().name("feed".into()).spawn(move || {
        feed_loop(
            decoder,
            source,
            recorder,
            feed_fps,
            expected_frames,
            feed_stop,
        )
    })?;

    // AppKit owns this thread from here until the run stops.
    let render_stats = lanplay_renderer_metal::run(
        RendererConfig {
            width: cli.width,
            height: cli.height,
            title: format!("lanplay phase 2 — {}x{}@{}", cli.width, cli.height, cli.fps),
            mode: cli.drive_mode(),
            recorder: telemetry.recorder(),
            stop: Arc::clone(&stop),
            render_delay: cli.render_delay_ms.map(Duration::from_millis),
            present_limit: None,
        },
        Arc::clone(&slot),
    )?;

    stop.store(true, Ordering::Release);
    let outcome = feed
        .join()
        .expect("feed thread")
        .map_err(|error| -> Box<dyn Error> {
            // The feed thread's error type is Send + Sync; the caller's is not.
            error.to_string().into()
        })?;
    let memory = memory_sampler.join().expect("memory sampler");

    if !telemetry.flush(Duration::from_secs(5)) {
        return Err("telemetry collector did not catch up".into());
    }
    let snapshot = telemetry.shutdown();

    let still_in_slot = slot
        .published()
        .saturating_sub(render_stats.rendered + render_stats.superseded);

    report(cli, &outcome, &memory, &render_stats, &snapshot);

    let verdict = evaluate(&GateInputs {
        target_fps: feed_fps,
        expected_frames,
        display_hz: render_stats.display_hz,
        hardware_decoder,
        submitted: outcome.submitted,
        decoded: outcome.decoded,
        decoder_errors: outcome.errors,
        decoder_dropped: outcome.dropped,
        run_seconds: cli.seconds,
        backlog: outcome.backlog,
        max_backlog: outcome.max_backlog,
        trailing_backlog: outcome.trailing_backlog,
        rendered: render_stats.rendered,
        superseded: render_stats.superseded,
        empty_ticks: render_stats.empty_ticks,
        still_in_slot,
        display_driven: matches!(cli.mode, crate::Mode::DisplayLink),
        memory: memory.clone(),
        snapshot,
        zero_copy_render_path: true,
        metal_texture_cache: true,
    });
    println!();
    println!("{verdict}");
    Ok(verdict.passed())
}

fn feed_loop(
    mut decoder: VideoToolboxDecoder,
    mut source: FixtureSource,
    recorder: Recorder,
    fps: f64,
    frames: u64,
    stop: Arc<AtomicBool>,
) -> Result<FeedOutcome, Box<dyn Error + Send + Sync>> {
    let period = Nanos((1_000_000_000.0 / fps) as u64);
    let start = Timestamp::now();
    let mut backlog = Trend::new();
    let mut max_backlog = 0usize;
    let mut next_sample = start;

    for index in 0..frames {
        if stop.load(Ordering::Acquire) {
            break;
        }
        // Absolute pacing: a slow frame must not push every later frame.
        wait_until(start.add(Nanos(period.get() * index)));

        let Some(unit) = source.next_access_unit() else {
            break;
        };
        // The content exists at the tick; the access unit is complete the
        // moment the source hands it over, which is what a depacketiser will
        // signal later.
        recorder.mark(unit.id, Stage::FrameCreated);
        recorder.mark(unit.id, Stage::FrameReassembled);
        decoder.submit(&unit)?;

        let in_flight = decoder.in_flight();
        max_backlog = max_backlog.max(in_flight);
        let now = Timestamp::now();
        if now >= next_sample {
            // Sampled just after a submit, so the backlog is read at its
            // highest point in the cycle: the pessimistic direction.
            backlog.record_at(now, in_flight as f64);
            next_sample = now.add(Nanos(SAMPLE_INTERVAL.as_nanos() as u64));
        }
    }

    decoder.flush()?;
    thread::sleep(DRAIN_GRACE);
    stop.store(true, Ordering::Release);

    Ok(FeedOutcome {
        submitted: decoder.submitted(),
        decoded: decoder.decoded(),
        errors: decoder.errors(),
        dropped: decoder.dropped(),
        max_backlog,
        trailing_backlog: decoder.in_flight(),
        backlog,
    })
}

/// Samples resident memory on its own thread.
///
/// It used to run inside the feed loop, where a `task_info` call every 250 ms
/// landed between a deadline and a submit and showed up as source jitter. The
/// measurement was perturbing the thing it measured.
fn sample_memory(stop: Arc<AtomicBool>) -> Trend {
    let mut memory = Trend::new();
    while !stop.load(Ordering::Acquire) {
        if let Some(bytes) = resident_bytes() {
            memory.record(bytes as f64);
        }
        thread::sleep(SAMPLE_INTERVAL);
    }
    memory
}

fn report(
    cli: &Cli,
    outcome: &FeedOutcome,
    memory: &Trend,
    render: &RenderStats,
    snapshot: &Snapshot,
) {
    let decode = snapshot.segment(Segment::Decode);
    let wait = snapshot.segment(Segment::PresentationWait);
    let render_segment = snapshot.segment(Segment::Render);

    println!();
    println!("VideoToolbox");
    println!("  submitted        {}", outcome.submitted);
    println!("  decoded          {}", outcome.decoded);
    println!(
        "  errors/dropped   {} / {}",
        outcome.errors, outcome.dropped
    );
    println!(
        "  backlog          peak {} frames, {} at exit, {}",
        outcome.max_backlog,
        outcome.trailing_backlog,
        match outcome.backlog.slope_per_minute() {
            Some(slope) => format!("{slope:+.2} frames/min"),
            None => "slope unmeasured".to_owned(),
        }
    );
    println!(
        "  decode           p50 {} p95 {} p99 {} max {}",
        decode.p50, decode.p95, decode.p99, decode.max
    );

    println!();
    println!("Metal");
    println!("  display          {:.2} Hz", render.display_hz);
    println!("  rendered         {}", render.rendered);
    println!("  superseded       {}", render.superseded);
    println!("  empty ticks      {}", render.empty_ticks);
    println!("  drawable wait    {}", render.drawable_wait);
    println!("  encode cpu       {}", render.encode_cpu);

    println!();
    println!("Pipeline");
    println!(
        "  presentation wait p50 {} p95 {} p99 {}",
        wait.p50, wait.p95, wait.p99
    );
    println!(
        "  render            p50 {} p95 {} p99 {}",
        render_segment.p50, render_segment.p95, render_segment.p99
    );
    println!(
        "  frame age         p50 {} p95 {} p99 {} max {}",
        snapshot.frame_age.p50,
        snapshot.frame_age.p95,
        snapshot.frame_age.p99,
        snapshot.frame_age.max
    );
    println!(
        "  unattributed gap  p50 {} p99 {}",
        snapshot.unattributed_gap.p50, snapshot.unattributed_gap.p99
    );
    println!("  cpu plane copies  0 (textures alias the decoder's IOSurfaces)");
    println!(
        "  {}",
        match memory.slope_per_minute() {
            Some(slope) => format!(
                "resident memory   {:.1} MB -> {:.1} MB, {:+.2} MB/min over {} samples",
                memory.first().unwrap_or(0.0) / 1_048_576.0,
                memory.last().unwrap_or(0.0) / 1_048_576.0,
                slope / 1_048_576.0,
                memory.count()
            ),
            None => format!("resident memory   unmeasured ({} samples)", memory.count()),
        }
    );

    if cli.render_delay_ms.is_some() || cli.decoder_callback_delay_ms.is_some() {
        println!();
        println!("  (a delay knob is set: this is a negative test, not a gate run)");
    }

    println!();
    println!("{snapshot}");
}
