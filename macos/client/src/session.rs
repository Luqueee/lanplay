use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_decoder_videotoolbox::{DecodedFrame, DecoderConfig, VideoToolboxDecoder};
use lanplay_renderer_metal::{
    LatestFrameSlot, LiveCounters, RenderStats, RendererConfig, RendererError, SurfaceFrame,
};
use lanplay_telemetry::{
    Nanos, Recorder, Segment, Snapshot, Stage, Telemetry, TelemetryConfig, Timestamp, Trend,
    resident_bytes, wait_until,
};
use lanplay_transport::TxStats;
use lanplay_video_core::{
    AccessUnitSource, FixtureSource, PixelFormat, VideoDecoder, ensure_fixture,
};

use crate::Cli;
use crate::gate::{GateInputs, TransportInputs, evaluate};
use crate::preflight;
use crate::transport;

/// How often the backlog and resident memory are sampled. Fast enough to fit a
/// slope through a one-minute run, slow enough to cost nothing.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
/// Time given to the pipeline to drain after the last access unit is
/// submitted, so frames already in flight are not counted as losses.
const DRAIN_GRACE: Duration = Duration::from_millis(300);
/// How long the host is given to produce its first frame after being told the
/// receiver is ready.
///
/// This used to be twenty-five seconds, because the sender was launched by
/// hand at some unknown moment after the receiver started listening and the
/// run had to be padded to cover it. The control plane removed the unknown:
/// the receiver acknowledges the configuration itself, and the host sends its
/// first frame immediately afterwards. Twenty-two of those seconds were pure
/// idle in every measurement, which made a ten-second run cost forty.
const HOST_STARTUP_GRACE: Duration = Duration::from_secs(3);
/// How often the LAN watchdog looks at the stream.
const POLL: Duration = Duration::from_millis(50);
/// Silence that means the remote sender has finished.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// What the run produced, whichever path the access units took to the decoder.
struct RunOutcome {
    submitted: u64,
    decoded: u64,
    errors: u64,
    dropped: u64,
    max_backlog: usize,
    trailing_backlog: usize,
    backlog: Trend,
    transport: Option<transport::TransportOutcome>,
}

type FeedResult = Result<RunOutcome, Box<dyn Error + Send + Sync>>;
type SendResult = Result<(TxStats, u64, u64), Box<dyn Error + Send + Sync>>;
type ReceiveResult =
    Result<(transport::ReceiverOutcome, VideoToolboxDecoder), Box<dyn Error + Send + Sync>>;

/// The threads that move access units, in whichever shape this run uses.
enum Pipeline {
    Direct(thread::JoinHandle<FeedResult>),
    Loopback {
        sender: thread::JoinHandle<SendResult>,
        receiver: thread::JoinHandle<ReceiveResult>,
    },
    /// The sender is on another machine.
    Receive(thread::JoinHandle<ReceiveResult>),
}
impl Pipeline {
    fn join(self) -> Result<RunOutcome, Box<dyn Error>> {
        // Thread errors are Send + Sync and the caller's are not, so they cross
        // the boundary as text.
        let flatten =
            |error: Box<dyn Error + Send + Sync>| -> Box<dyn Error> { error.to_string().into() };
        match self {
            Pipeline::Direct(feed) => feed.join().expect("feed thread").map_err(flatten),
            Pipeline::Loopback { sender, receiver } => {
                let (tx, wire_bytes, payload_bytes) =
                    sender.join().expect("sender thread").map_err(flatten)?;
                let (received, decoder) =
                    receiver.join().expect("receiver thread").map_err(flatten)?;
                Ok(RunOutcome {
                    submitted: received.submitted,
                    decoded: decoder.decoded(),
                    errors: decoder.errors(),
                    dropped: decoder.dropped(),
                    max_backlog: received.max_backlog,
                    trailing_backlog: received.trailing_backlog,
                    backlog: received.backlog,
                    transport: Some(transport::TransportOutcome {
                        tx,
                        rx: received.rx,
                        jitter: received.jitter,
                        verified: received.verified,
                        mismatched: received.mismatched,
                        wire_bytes,
                        payload_bytes,
                        dscp: received.dscp,
                    }),
                })
            }
            Pipeline::Receive(receiver) => {
                let (received, decoder) =
                    receiver.join().expect("receiver thread").map_err(flatten)?;
                Ok(RunOutcome {
                    submitted: received.submitted,
                    decoded: decoder.decoded(),
                    errors: decoder.errors(),
                    dropped: decoder.dropped(),
                    max_backlog: received.max_backlog,
                    trailing_backlog: received.trailing_backlog,
                    backlog: received.backlog,
                    transport: Some(transport::TransportOutcome {
                        // Nothing was sent from this machine.
                        tx: TxStats::default(),
                        rx: received.rx,
                        jitter: received.jitter,
                        verified: received.verified,
                        mismatched: received.mismatched,
                        wire_bytes: received.rx.bytes,
                        payload_bytes: 0,
                        dscp: received.dscp,
                    }),
                })
            }
        }
    }
}

pub fn run(cli: &Cli) -> Result<bool, Box<dyn Error>> {
    // Held for the length of the run. Without it macOS throttles this process
    // whenever its window is occluded, and the throttling reaches the receive
    // thread rather than just the drawing.
    let _awake = crate::nap::Awake::begin("lanplay is presenting a live video stream");

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

    // Where the decoder's format description comes from. Over a real link it
    // is the encoder's own sequence header, carried by the control plane
    // before any media: parameter sets from the fixture describe a stream
    // some other encoder produced, and VideoToolbox rejects real slices
    // decoded against them as bad data.
    let (parameter_sets, mut control) = match cli.transport {
        crate::Transport::Lan => {
            // Negotiated whatever the sets are eventually taken from: the
            // handshake is also what stops the host sending before this
            // process can receive, and a negative control that skipped it
            // would be changing two things at once.
            let (config, control) = crate::config::negotiate(cli)?;
            println!(
                "codec config generation {} from the host: {}x{}, SPS {} B, PPS {} B",
                config.generation,
                config.width,
                config.height,
                config.sets.sps[0].len(),
                config.sets.pps[0].len()
            );
            let sets = match cli.parameter_sets {
                crate::ParameterSetSource::Host => config.sets,
                crate::ParameterSetSource::Fixture => {
                    println!(
                        "negative control: decoding the host's stream against the \
                         fixture's parameter sets instead"
                    );
                    source.parameter_sets().clone()
                }
            };
            (sets, Some((control, config.generation)))
        }
        _ => (source.parameter_sets().clone(), None),
    };
    let sink_slot = Arc::clone(&slot);
    let decoder = VideoToolboxDecoder::new(
        DecoderConfig {
            parameter_sets,
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

    // The three the client can answer for itself. The display, occlusion,
    // Space and miniaturised checks belong to the renderer, which prints them
    // once its window exists; the terminator is printed from `on_ready` below
    // so an orchestrator has exactly one line to wait on.
    let mut checks = vec![
        if hardware_decoder {
            preflight::Item::ok("decoder", "VideoToolbox reports hardware acceleration")
        } else {
            preflight::Item::fail("decoder", "session is not hardware accelerated")
        },
        preflight::Item::ok("app-nap", "LatencyCritical activity held for the run"),
    ];
    let memory_stop = Arc::clone(&stop);
    let memory_sampler = thread::Builder::new()
        .name("memory".into())
        .spawn(move || sample_memory(memory_stop))?;

    let counters = LiveCounters::new();
    // The far end of the measured span. The renderer keeps presenting until
    // `stop`, which the watchdog only sets two seconds after the sender has
    // finished, so the run's own end is the wrong mark to count to. Only the
    // The counters the drain moves are marked: no frame arrives after the
    // last access unit, so `rendered` and `superseded` cannot advance in it.
    let spans_end = Arc::new(SpanEnd::default());
    let stream_ended = Arc::new(AtomicBool::new(false));
    // Bumped for every access unit handed to the decoder. Two readers: the
    // LAN watchdog, which needs to know the stream is still alive, and the
    // window sampler, which turns it into the delivered rate.
    let arrived = Arc::new(AtomicU64::new(0));
    // Cloned before the decoder is moved into whichever thread submits to it.
    let decoder_counters = decoder.counters();
    let decoder_status = decoder_counters.clone();

    let pipeline = match cli.transport {
        crate::Transport::Direct => {
            let feed_stop = Arc::clone(&stop);
            let feed_arrived = Arc::clone(&arrived);
            Pipeline::Direct(thread::Builder::new().name("feed".into()).spawn(move || {
                feed_loop(
                    decoder,
                    source,
                    recorder,
                    feed_fps,
                    expected_frames,
                    feed_arrived,
                    feed_stop,
                )
            })?)
        }
        crate::Transport::Loopback => {
            let (sender_socket, receiver_socket, target) = transport::loopback_sockets()?;
            println!(
                "transport: RTP over UDP loopback to {target}, mtu {}",
                cli.mtu
            );
            let ledger = transport::VerifyLedger::new(cli.verify);

            let receive_stop = Arc::clone(&stop);
            let receive_ledger = Arc::clone(&ledger);
            let receive_recorder = recorder.clone();
            let receive_arrived = Arc::clone(&arrived);
            let receiver = thread::Builder::new()
                .name("rtp-rx".into())
                .spawn(move || {
                    transport::receive_loop(
                        receiver_socket,
                        decoder,
                        receive_recorder,
                        receive_ledger,
                        SAMPLE_INTERVAL,
                        receive_arrived,
                        receive_stop,
                    )
                })?;

            let send_stop = Arc::clone(&stop);
            let mtu = cli.mtu;
            let sender = thread::Builder::new()
                .name("rtp-tx".into())
                .spawn(move || {
                    let result = transport::send_loop(
                        sender_socket,
                        source,
                        recorder,
                        ledger,
                        transport::SenderConfig {
                            target,
                            fps: feed_fps,
                            frames: expected_frames,
                            mtu,
                        },
                        Arc::clone(&send_stop),
                    );
                    // Give whatever is still in flight time to arrive, decode and
                    // present before anything is called a loss.
                    thread::sleep(DRAIN_GRACE);
                    send_stop.store(true, Ordering::Release);
                    result
                })?;

            Pipeline::Loopback { sender, receiver }
        }
        crate::Transport::Lan => {
            // The sender is on the other machine. The fixture is still loaded
            // locally, but only for its SPS and PPS: our packetiser keeps
            // parameter sets out of the access units, so until the control
            // plane carries them the decoder has to be told out of band.
            drop(source);
            let socket = std::net::UdpSocket::bind(cli.bind)?;
            println!(
                "transport: RTP over UDP, listening on {}",
                socket.local_addr()?
            );

            let progress = Arc::clone(&arrived);
            let receive_stop = Arc::clone(&stop);
            let receive_progress = Arc::clone(&progress);
            let receiver = thread::Builder::new()
                .name("rtp-rx".into())
                .spawn(move || {
                    transport::receive_loop(
                        socket,
                        decoder,
                        recorder,
                        transport::VerifyLedger::new(false),
                        SAMPLE_INTERVAL,
                        receive_progress,
                        receive_stop,
                    )
                })?;

            // The run ends when the stream does; the clock is only a
            // backstop. Two deadlines rather than one sum, because they mean
            // different things: the host has `HOST_STARTUP_GRACE` to produce
            // a first frame after being acknowledged, and once it has, the
            // run lasts exactly as long as it was asked to. Adding the grace
            // to every run instead would pad the measurement with idle time
            // in the healthy case, which is precisely what the twenty-five
            // second version did.
            let deadline_stop = Arc::clone(&stop);
            let watchdog_counters = Arc::clone(&counters);
            let watchdog_mark = Arc::clone(&spans_end);
            let watchdog_ended = Arc::clone(&stream_ended);
            let run_for = Duration::from_secs_f64(cli.seconds) + DRAIN_GRACE;
            thread::Builder::new()
                .name("deadline".into())
                .spawn(move || {
                    let mut until =
                        Timestamp::now().add(Nanos(HOST_STARTUP_GRACE.as_nanos() as u64));
                    let mut last_seen = 0u64;
                    let mut idle = Duration::ZERO;
                    while !deadline_stop.load(Ordering::Acquire) && Timestamp::now() < until {
                        thread::sleep(POLL);
                        let seen = progress.load(Ordering::Relaxed);
                        if seen != last_seen {
                            if last_seen == 0 {
                                // First frame. From here the run is on its
                                // own clock, and the startup watchdog is done.
                                until = Timestamp::now().add(Nanos(run_for.as_nanos() as u64));
                            }
                            last_seen = seen;
                            idle = Duration::ZERO;
                            watchdog_mark.mark(&watchdog_counters);
                        } else if last_seen > 0 {
                            idle += POLL;
                            if idle >= STREAM_IDLE_TIMEOUT {
                                break;
                            }
                        }
                    }
                    watchdog_ended.store(true, Ordering::Release);
                    deadline_stop.store(true, Ordering::Release);
                })?;

            Pipeline::Receive(receiver)
        }
    };

    // Only now is the receiver actually ready: a decoder exists and the UDP
    // socket is bound. Acknowledging any earlier would invite the host to
    // start sending at a port nothing is listening on yet.
    if let Some((control, generation)) = control.as_mut() {
        crate::config::acknowledge(control, *generation)?;
        println!("control: acknowledged generation {generation}");
    }

    // The closure below consumes its copy; the original stays for the
    // failure path, where the renderer never calls it.
    let ready_checks = checks.clone();
    let telemetry = Arc::new(telemetry);
    let sampler_stop = Arc::clone(&stop);
    let sampler = crate::windows::spawn(
        Arc::clone(&telemetry),
        Arc::clone(&counters),
        Arc::clone(&slot),
        decoder_counters,
        Arc::clone(&arrived),
        Duration::from_secs_f64(cli.window_seconds.max(1.0)),
        sampler_stop,
    );

    // AppKit owns this thread from here until the run stops. The renderer
    // prints its own preflight items and then calls `on_ready`, which is
    // where the block is terminated.
    let render = lanplay_renderer_metal::run(
        RendererConfig {
            width: cli.width,
            height: cli.height,
            title: format!("lanplay — {}x{}@{}", cli.width, cli.height, cli.fps),
            mode: cli.drive_mode(),
            recorder: telemetry.recorder(),
            stop: Arc::clone(&stop),
            render_delay: cli.render_delay_ms.map(Duration::from_millis),
            present_limit: None,
            counters: Arc::clone(&counters),
            require_clean_environment: cli.require_clean_display.then_some(feed_fps),
            on_ready: Some(Box::new(move || {
                preflight::report(&ready_checks);
            })),
        },
        Arc::clone(&slot),
    );
    let render_stats = match render {
        Ok(stats) => stats,
        Err(RendererError::DirtyEnvironment(problems)) => {
            for problem in &problems {
                checks.push(preflight::Item::fail("display", problem));
            }
            preflight::report(&checks);
            stop.store(true, Ordering::Release);
            return Err(preflight::Refused.into());
        }
        Err(other) => return Err(other.into()),
    };

    stop.store(true, Ordering::Release);
    let outcome = pipeline.join()?;
    let memory = memory_sampler.join().expect("memory sampler");
    let mut slices = sampler.join().expect("window sampler");
    // The renderer draws on into silence for as long as the watchdog takes to
    // notice the sender has stopped. That drain lands somewhere inside the
    // last window the sampler emitted, and there is no way to tell from the
    // outside how much of that window it ate. An unverifiable window has no
    // place in a report whose whole purpose is that its numbers can be
    // trusted, so it is dropped rather than explained away.
    if stream_ended.load(Ordering::Acquire) {
        slices.pop();
    }

    if !telemetry.flush(Duration::from_secs(5)) {
        return Err("telemetry collector did not catch up".into());
    }
    let telemetry = Arc::try_unwrap(telemetry)
        .map_err(|_| "a telemetry handle outlived the run")
        .expect("every sampler has been joined");
    let snapshot = telemetry.shutdown();

    let still_in_slot = slot
        .published()
        .saturating_sub(render_stats.rendered + render_stats.superseded);

    // One rejected access unit costs every frame up to the next IDR, so an
    // error count without its status is a second of missing video with no
    // lead. VideoToolbox's OSStatus is the only evidence there is.
    if outcome.errors > 0
        && let Some(status) = decoder_status.first_error_status()
    {
        println!(
            "  decoder rejected {} frames, first OSStatus {status}",
            outcome.errors
        );
    }
    report(cli, &outcome, &memory, &render_stats, &snapshot);
    if !slices.is_empty() {
        print_windows(&slices);
        println!(
            "  worst callback drop between windows: {:.1}% over {} windows",
            crate::windows::worst_callback_drop(&slices) * 100.0,
            slices.len()
        );
    }
    if let Some(path) = &cli.report {
        let json = build_report(
            cli,
            feed_fps,
            expected_frames,
            &outcome,
            &render_stats,
            &spans_end,
            &snapshot,
            slices,
        );
        std::fs::write(path, serde_json::to_string_pretty(&json)?)?;
        println!("report written to {}", path.display());
    }

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
        span_callbacks: spans_end.callbacks(render_stats.callbacks),
        span_empty_ticks: spans_end.empty_ticks(render_stats.empty_ticks),
        span_missed_drawables: spans_end.missed_drawables(render_stats.missed_drawables),
        still_in_slot,
        display_driven: matches!(cli.mode, crate::Mode::DisplayLink),
        memory: memory.clone(),
        snapshot,
        zero_copy_render_path: true,
        transport: outcome.transport.as_ref().map(|transport| TransportInputs {
            tx: transport.tx,
            rx: transport.rx,
            jitter: transport.jitter,
            verified: transport.verified,
            mismatched: transport.mismatched,
            overhead_ratio: transport.overhead_ratio(),
        }),
        metal_texture_cache: true,
    });
    println!();
    println!("{verdict}");
    Ok(verdict.passed())
}

/// The run sliced into windows, printed as a table.
///
/// A ten-minute mean cannot show a stall: a run that held 120 Hz for four
/// minutes, collapsed for twenty seconds and recovered still averages 116.
/// Every column here is per-window for exactly that reason, and the p99s are
/// taken from histograms that get reset rather than differenced.
fn print_windows(windows: &[crate::report::Window]) {
    println!();
    println!("Windows");
    println!(
        "  {:>12}  {:>7} {:>7} {:>7} {:>7}  {:>8} {:>8}  {:>7} {:>7}",
        "window", "src/s", "dec/s", "rnd/s", "tick/s", "srcp99", "agep99", "super%", "fresh%"
    );
    for window in windows {
        println!(
            "  {:>5.0}-{:<6.0}  {:>7.1} {:>7.1} {:>7.1} {:>7.1}  {:>8.2} {:>8.2}  {:>7.1} {:>7.1}",
            window.from_s,
            window.to_s,
            window.source_hz,
            window.decode_hz,
            window.render_hz,
            window.callback_hz,
            window.source_interval_p99_ms,
            window.frame_age_p99_ms,
            window.superseded_pct,
            window.fresh_pct
        );
    }
}

fn feed_loop(
    mut decoder: VideoToolboxDecoder,
    mut source: FixtureSource,
    recorder: Recorder,
    fps: f64,
    frames: u64,
    arrived: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
) -> FeedResult {
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
        arrived.fetch_add(1, Ordering::Relaxed);

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

    Ok(RunOutcome {
        submitted: decoder.submitted(),
        decoded: decoder.decoded(),
        errors: decoder.errors(),
        dropped: decoder.dropped(),
        max_backlog,
        trailing_backlog: decoder.in_flight(),
        backlog,
        transport: None,
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
    outcome: &RunOutcome,
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
    // The experience number: how many of the viewer's refresh opportunities
    // carried something new. Rendered frames per second counts pictures
    // drawn; this counts opportunities used, which is what bunching costs.
    let ticks = render.rendered + render.empty_ticks;
    println!(
        "  fresh ticks      {:.1}% ({} of {} refreshes had a newer frame)",
        if ticks == 0 {
            0.0
        } else {
            render.rendered as f64 * 100.0 / ticks as f64
        },
        render.rendered,
        ticks
    );
    println!("  drawable wait    {}", render.drawable_wait);
    println!("  encode cpu       {}", render.encode_cpu);

    if let Some(transport) = &outcome.transport {
        let packetization = snapshot.segment(Segment::Packetization);
        let transit = snapshot.segment(Segment::Transit);
        let serialisation = snapshot.segment(Segment::Serialisation);
        let arrival = snapshot.segment(Segment::Arrival);
        let reassembly = snapshot.segment(Segment::Reassembly);
        println!();
        println!("Transport (RTP over UDP loopback)");
        println!(
            "  access units     {} sent, {} reconstructed, {} verified, {} mismatched",
            transport.tx.access_units,
            transport.rx.access_units_completed,
            transport.verified,
            transport.mismatched
        );
        println!(
            "  packets          {} ({} single NAL, {} FU-A), {:.1} per access unit",
            transport.tx.packets,
            transport.tx.single_nal,
            transport.tx.fu_a,
            transport.tx.packets as f64 / transport.tx.access_units.max(1) as f64
        );
        println!(
            "  wire             {:.1} MB for {:.1} MB of access unit ({:.1}% overhead)",
            transport.wire_bytes as f64 / 1e6,
            transport.payload_bytes as f64 / 1e6,
            (transport.overhead_ratio() - 1.0) * 100.0
        );
        println!(
            "  losses           {} lost, {} malformed, {} dropped AUs, {} duplicates, {} reordered",
            transport.rx.lost,
            transport.rx.malformed,
            transport.rx.access_units_dropped,
            transport.rx.duplicates,
            transport.rx.reordered
        );
        // The pair a retransmission scheme is sized from: how far ahead of
        // the hole packets keep arriving, and how long the hole stays open
        // when nothing was actually lost. A NACK sent inside that window asks
        // for a packet already on its way.
        println!(
            "  reordering       depth max {}, gap filled in {:.3} ms mean / {:.3} ms max over {} gaps",
            transport.rx.max_reorder_depth,
            transport.rx.mean_reorder_wait_ns() as f64 / 1e6,
            transport.rx.reorder_wait_max_ns as f64 / 1e6,
            transport.rx.reorder_waits
        );
        println!("  rfc3550 jitter   {}", transport.jitter);
        // What the sender asked for is on the host's side of the run; this is
        // what survived the path, and it is the only half that can decide a
        // QoS experiment.
        println!("  service class    observed {}", transport.dscp);
        println!(
            "  packetization    p50 {} p95 {} p99 {}",
            packetization.p50, packetization.p95, packetization.p99
        );
        println!(
            "  transit          p50 {} p95 {} p99 {}  (first byte out to last byte in)",
            transit.p50, transit.p95, transit.p99
        );
        println!(
            "  serialisation    p50 {} p95 {} p99 {}  (overlaps transit)",
            serialisation.p50, serialisation.p95, serialisation.p99
        );
        println!(
            "  arrival spread   p50 {} p95 {} p99 {}  (overlaps transit)",
            arrival.p50, arrival.p95, arrival.p99
        );
        println!(
            "  reassembly       p50 {} p95 {} p99 {}",
            reassembly.p50, reassembly.p95, reassembly.p99
        );
    }
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

/// Assembles the machine-readable result.
///
/// Every number comes from this machine's clock. The sender's timestamps are
/// not comparable until phase 9 estimates the offset, so `frame_age` here is
/// the client's `local_age`: its first sight of a frame to putting it on
/// screen.
#[allow(clippy::too_many_arguments)]
fn build_report(
    cli: &Cli,
    feed_fps: f64,
    expected_frames: u64,
    outcome: &RunOutcome,
    render: &RenderStats,
    span_end: &SpanEnd,
    snapshot: &Snapshot,
    slices: Vec<crate::report::Window>,
) -> crate::report::Report {
    let arrival = snapshot.segment(Segment::Arrival);
    let decode = snapshot.segment(Segment::Decode);
    let (rx, jitter, corruption) = match &outcome.transport {
        Some(transport) => (transport.rx, transport.jitter, transport.mismatched),
        None => (Default::default(), Nanos::ZERO, 0),
    };
    let reconstructed = if outcome.transport.is_some() {
        rx.access_units_completed
    } else {
        outcome.submitted
    };

    // Anything that moved the window during the run makes the presentation
    // numbers untrustworthy even if it recovered, so it is named rather than
    // averaged away.
    let mut invalidating = Vec::new();
    {
        let mut blame = |count: u64, what: &str| {
            if count > 0 {
                invalidating.push(format!("{count} {what}"));
            }
        };
        blame(render.occlusion_changes, "occlusion changes");
        blame(render.space_changes, "Space changes");
        blame(render.miniaturise_events, "miniaturise events");
        blame(render.display_changes, "display changes");
    }

    // A display link that was suspended did not measure presentation, it
    // measured a screensaver. Occlusion *changes* are zero when the window
    // was behind something for the whole run, so the count alone calls that
    // run clean. The comparison has to be against what the display is
    // capable of, not against the rate that was achieved: a suspended link
    // drags the measured rate down with it and would excuse itself.
    let expected_ticks = render.nominal_hz * cli.seconds;
    if expected_ticks > 0.0 && (render.callbacks as f64) < expected_ticks * 0.5 {
        invalidating.push(format!(
            "display link delivered {} of about {:.0} refreshes; presentation \
             figures from this run are not measurements",
            render.callbacks, expected_ticks
        ));
    }

    // The one number that says whether a QoS marking survived the path,
    // rather than whether the sender asked for it.
    let dominant_dscp = outcome
        .transport
        .as_ref()
        .and_then(|transport| transport.dscp.dominant());

    crate::report::Report {
        run: crate::report::Run {
            drive_mode: match cli.mode {
                crate::Mode::DisplayLink => "display-link",
                crate::Mode::Immediate => "immediate",
            },
            seconds: cli.seconds,
            target_fps: feed_fps,
            invalidated: !invalidating.is_empty(),
            invalidating_events: invalidating,
        },
        stream: crate::report::Stream {
            expected: expected_frames,
            reconstructed,
            packet_loss: rx.lost,
            au_loss: expected_frames.saturating_sub(reconstructed),
            corruption,
            reordered: rx.reordered,
            max_reorder_depth: rx.max_reorder_depth,
            reorder_wait_mean_ms: rx.mean_reorder_wait_ns() as f64 / 1e6,
            reorder_wait_max_ms: rx.reorder_wait_max_ns as f64 / 1e6,
            reorder_gaps: rx.reorder_waits,
            duplicates: rx.duplicates,
        },
        network: crate::report::Network {
            arrival_p50_ms: arrival.p50.as_millis_f64(),
            arrival_p95_ms: arrival.p95.as_millis_f64(),
            arrival_p99_ms: arrival.p99.as_millis_f64(),
            arrival_max_ms: arrival.max.as_millis_f64(),
            rtp_jitter_us: jitter.get() as f64 / 1000.0,
            observed_dscp: dominant_dscp.map(|(dscp, _)| dscp),
            observed_dscp_share: dominant_dscp.map_or(0.0, |(_, share)| share),
        },
        decode: crate::report::Decode {
            decoded: outcome.decoded,
            errors: outcome.errors,
            p50_ms: decode.p50.as_millis_f64(),
            p95_ms: decode.p95.as_millis_f64(),
            p99_ms: decode.p99.as_millis_f64(),
            backlog_slope_per_min: outcome.backlog.slope_per_minute().unwrap_or(0.0),
        },
        display: crate::report::Display {
            nominal_hz: render.display_hz,
            callbacks: span_end.callbacks(render.callbacks),
            rendered: render.rendered,
            superseded: render.superseded,
            empty_refreshes: span_end.empty_ticks(render.empty_ticks),
            fresh_tick_ratio: {
                let ticks = span_end.callbacks(render.callbacks);
                if ticks == 0 {
                    0.0
                } else {
                    render.rendered as f64 * 100.0 / ticks as f64
                }
            },
            callback_interval_p50_ms: render.callback_interval.p50.as_millis_f64(),
            callback_interval_p95_ms: render.callback_interval.p95.as_millis_f64(),
            callback_interval_p99_ms: render.callback_interval.p99.as_millis_f64(),
            callback_interval_max_ms: render.callback_interval.max.as_millis_f64(),
            frame_age_p50_ms: snapshot.local_age.p50.as_millis_f64(),
            frame_age_p95_ms: snapshot.local_age.p95.as_millis_f64(),
            frame_age_p99_ms: snapshot.local_age.p99.as_millis_f64(),
        },
        environment: crate::report::Environment {
            occlusion_changes: render.occlusion_changes,
            space_changes: render.space_changes,
            miniaturise_events: render.miniaturise_events,
            display_changes: render.display_changes,
            link_pauses: render.link_pauses,
            app_nap_protection: true,
        },
        windows: slices,
    }
}

/// Where the measured span ends.
///
/// The renderer stops when the run stops, which is a couple of seconds after
/// the sender does; every refresh in between finds nothing new, which is true
/// of the drain and a lie about the link. These are the counters as of one
/// poll after the final access unit.
#[derive(Default)]
struct SpanEnd {
    marked: AtomicBool,
    callbacks: AtomicU64,
    empty_ticks: AtomicU64,
    missed_drawables: AtomicU64,
}

impl SpanEnd {
    fn mark(&self, counters: &LiveCounters) {
        self.callbacks.store(
            counters.callbacks.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.empty_ticks.store(
            counters.empty_ticks.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.missed_drawables.store(
            counters.missed_drawables.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        self.marked.store(true, Ordering::Release);
    }

    /// The direct and loopback paths stop on the clock rather than on the
    /// stream, so they have no drain and their raw totals are already the span.
    fn callbacks(&self, whole_run: u64) -> u64 {
        if self.marked.load(Ordering::Acquire) {
            self.callbacks.load(Ordering::Relaxed)
        } else {
            whole_run
        }
    }

    fn empty_ticks(&self, whole_run: u64) -> u64 {
        if self.marked.load(Ordering::Acquire) {
            self.empty_ticks.load(Ordering::Relaxed)
        } else {
            whole_run
        }
    }

    fn missed_drawables(&self, whole_run: u64) -> u64 {
        if self.marked.load(Ordering::Acquire) {
            self.missed_drawables.load(Ordering::Relaxed)
        } else {
            whole_run
        }
    }
}
