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
use lanplay_transport::{ControlClient, TxStats};
use lanplay_video_core::{
    AccessUnitSource, FixtureSource, PixelFormat, VideoDecoder, ensure_fixture,
};
use parking_lot::Mutex;

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
                        reorder_wait: received.reorder_wait,
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
                        reorder_wait: received.reorder_wait,
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
        radio_check(),
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
    // The highest frame id the network has shown. Together with `arrived` it
    // says how many access units the host produced that never turned up,
    // which a count of arrivals alone cannot distinguish from a host that
    // simply produced fewer.
    let highest_frame = Arc::new(AtomicU64::new(0));
    // Delivery cadence lives here, beside the arrival counter, because both
    // describe what the network did and neither may be inferred from what
    // the display later managed to show.
    // The thresholds are multiples of the source period, so delivery has to be
    // told what the host was asked to produce. One expression for that period,
    // because the phase loop shifts inside it too and two derivations of the
    // same number would eventually disagree.
    let source_period = Nanos::from_millis_f64(1000.0 / feed_fps.max(1.0));
    let delivery = Arc::new(lanplay_link_metrics::Delivery::new(source_period));
    // The passive monitor, started before anything is receiving so its first
    // window covers the first access unit rather than beginning wherever the
    // thread happened to get scheduled. It shares the run's stop flag: a
    // monitor that could end its trace early would report a quiet window it
    // had stopped watching.
    //
    // Its absence stops nothing. `--monitor off` returns a monitor with no
    // thread and no windows, every accessor still answers, and the report says
    // so instead of inventing a tier.
    let monitor = crate::monitor::Monitor::start(cli.monitor, source_period, Arc::clone(&stop));
    println!(
        "monitor: {}",
        match cli.monitor {
            crate::monitor::Cost::Off =>
                "off, no radio sampler and no rolling windows".to_string(),
            crate::monitor::Cost::Cheap => format!(
                "association read every {:.0} s, rolling {:.0} s and {:.0} s windows",
                crate::monitor::RADIO_INTERVAL.as_secs_f64(),
                crate::monitor::SHORT_WINDOW.as_secs_f64(),
                crate::monitor::LONG_WINDOW.as_secs_f64()
            ),
            crate::monitor::Cost::Expensive => format!(
                "POSITIVE CONTROL by frequency: association reads with no \
                 interval, rolling {:.0} s and {:.0} s windows. If a comparison \
                 cannot see this arm, the finding is that the machine had \
                 headroom.",
                crate::monitor::SHORT_WINDOW.as_secs_f64(),
                crate::monitor::LONG_WINDOW.as_secs_f64()
            ),
            crate::monitor::Cost::Contend => format!(
                "POSITIVE CONTROL by mechanism: no radio read at all, taking \
                 lanplay-link-metrics' own mutex - the one the receive thread \
                 takes on every access unit - as fast as it will be granted, \
                 rolling {:.0} s and {:.0} s windows. If a comparison cannot \
                 see this arm, the finding is that the comparison is blind.",
                crate::monitor::SHORT_WINDOW.as_secs_f64(),
                crate::monitor::LONG_WINDOW.as_secs_f64()
            ),
        }
    );
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
            let receive_highest = Arc::clone(&highest_frame);
            let receive_delivery = Arc::clone(&delivery);
            let receive_monitor = monitor.windows();
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
                        receive_highest,
                        Arc::clone(&receive_delivery),
                        receive_monitor,
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
            let receive_delivery = Arc::clone(&delivery);
            let receive_monitor = monitor.windows();
            let receive_highest = Arc::clone(&highest_frame);
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
                        receive_highest,
                        Arc::clone(&receive_delivery),
                        receive_monitor,
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

    // The handshake was not the last use of the control connection: the phase
    // loop asks the host over it for the rest of the run. Shared rather than
    // moved, so the connection lives exactly as long as it did when the
    // handshake was all it was for.
    let control = control.map(|(client, _)| Arc::new(Mutex::new(client)));

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
        Arc::clone(&delivery),
        Arc::clone(&highest_frame),
        Duration::from_secs_f64(cli.window_seconds.max(1.0)),
        sampler_stop,
    );

    // The phase loop, or the reason this run has nothing for it to do. Started
    // after the acknowledgement because it can only ask a host that is already
    // sending, and refused rather than faked wherever one of the two halves it
    // measures between does not exist.
    let phase_loop = start_phase(cli, &telemetry, control.as_ref(), source_period, &stop);
    match &phase_loop {
        PhaseLoop::Running(_) if cli.phase_align == crate::PhaseAlign::Observe => println!(
            "phase: observing only, sending nothing; measuring against an aim of at \
             least {}",
            crate::phase::margin_floor(source_period)
        ),
        PhaseLoop::Running(_) => println!(
            "phase: aligning the host's capture to this display, aiming at least \
             {} in front of each refresh and widening that to whatever jitter it \
             measures",
            crate::phase::margin_floor(source_period)
        ),
        PhaseLoop::Idle(summary) => println!("phase: {summary}"),
    }

    // AppKit owns this thread from here until the run stops. The renderer
    // prints its own preflight items and then calls `on_ready`, which is
    // where the block is terminated.
    //
    // Unless there is no renderer. A link-only run measures delivery, loss,
    // reordering and decode, all of which happen before anything reaches a
    // screen, so it waits on the watchdog instead of on AppKit and reports
    // zeros for presentation rather than a plausible-looking screen.
    let render_stats = if cli.link_only {
        preflight::report(&ready_checks);
        println!("link-only: no renderer, no window, no display link");
        while !stop.load(Ordering::Acquire) {
            thread::sleep(POLL);
        }
        RenderStats::default()
    } else {
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
        match render {
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
        }
    };

    stop.store(true, Ordering::Release);
    let outcome = pipeline.join()?;
    let memory = memory_sampler.join().expect("memory sampler");
    let mut slices = sampler.join().expect("window sampler");
    // Stopped with the rest of the run, and before the report is assembled so
    // its last window is closed rather than half open.
    let monitor_cadence = monitor.cadence();
    // Read from the live slot before the thread is joined, which is the read a
    // consumer on a deadline would make: one lock, five scalars, no scan.
    let monitor_radio = monitor.radio();
    let monitor_trace = monitor.stop();
    // Joined before the snapshot below, because the loop holds a telemetry
    // handle and the snapshot can only be taken once the last one is gone.
    let phase = match phase_loop {
        PhaseLoop::Running(estimator) => estimator.join().expect("phase estimator"),
        PhaseLoop::Idle(summary) => summary,
    };
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
    report(
        cli,
        &outcome,
        &memory,
        &render_stats,
        &snapshot,
        &phase,
        delivery.cumulative(),
    );
    if !slices.is_empty() {
        print_windows(&slices);
        println!(
            "  worst callback drop between windows: {:.1}% over {} windows",
            crate::windows::worst_callback_drop(&slices) * 100.0,
            slices.len()
        );
    }
    print_monitor(monitor_cadence, &monitor_trace);
    if let Some(path) = &cli.report {
        let json = build_report(
            cli,
            feed_fps,
            expected_frames,
            &outcome,
            &render_stats,
            &spans_end,
            &snapshot,
            &phase,
            delivery.cumulative(),
            monitor_cadence,
            monitor_radio,
            &monitor_trace,
            &memory,
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
        span_callbacks: spans_end.callbacks(render_stats.callbacks),
        span_empty_ticks: spans_end.empty_ticks(render_stats.empty_ticks),
        span_missed_drawables: spans_end.missed_drawables(render_stats.missed_drawables),
        still_in_slot,
        presents: !cli.link_only,
        link: delivery.cumulative(),
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
        "  {:>12}  {:>7} {:>7} {:>7} {:>7}  {:>8} {:>7} {:>5} {:>8}  {:>7} {:>7}",
        "window",
        "src/s",
        "dec/s",
        "rnd/s",
        "tick/s",
        "aup99",
        ">2T/m",
        "loss",
        "agep99",
        "super%",
        "fresh%"
    );
    for window in windows {
        println!(
            "  {:>5.0}-{:<6.0}  {:>7.1} {:>7.1} {:>7.1} {:>7.1}  {:>8.2} {:>7.1} {:>5} {:>8.2}  \
             {:>7.1} {:>7.1}",
            window.from_s,
            window.to_s,
            window.source_hz,
            window.decode_hz,
            window.render_hz,
            window.callback_hz,
            window.au_interval_p99_ms,
            window.over_2t_per_min,
            window.au_loss,
            window.frame_age_p99_ms,
            window.superseded_pct,
            window.fresh_pct
        );
    }
}

/// The monitor's own account of the run: the radio trace and both windows.
///
/// Printed rather than left to the JSON because the claim this whole component
/// rests on - a 1 Hz association read costs nothing - is only checkable against
/// what the sampler actually achieved and what its worst read cost. A sampler
/// asked for 1 Hz that delivered 0.4 Hz was measuring its own cost, and a read
/// that took hundreds of milliseconds was not the passive one this tier claims.
fn print_monitor(cadence: crate::monitor::Cost, trace: &crate::monitor::Trace) {
    if cadence == crate::monitor::Cost::Off {
        println!();
        println!("Monitor  off: no radio sampler, no rolling windows");
        return;
    }
    println!();
    println!("Monitor  {}", cadence.label());
    println!(
        "  radio       {} reads, {} answered, {} empty, {:.2}/s achieved, worst read {:.2} ms",
        trace.radio.samples.len(),
        trace.radio.answered,
        trace.radio.empty,
        trace.radio.reads_per_s,
        trace.radio.cost_max_ms
    );
    // The newest answered sample rather than the first: what the link was when
    // the run ended is what a diagnosis printed now would be about.
    match trace
        .radio
        .samples
        .iter()
        .rev()
        .find_map(|sample| sample.hint)
    {
        Some(hint) => println!(
            "  link        {} dBm, noise {} dBm, {:.0} Mbps, channel {} at {} MHz",
            hint.rssi_dbm, hint.noise_dbm, hint.tx_rate_mbps, hint.channel, hint.width_mhz
        ),
        None => println!("  link        CoreWLAN answered nothing; the radio tier is absent"),
    }
    for event in &trace.radio.moved {
        println!("  moved       {event}");
    }
    print_slices("short", crate::monitor::SHORT_WINDOW.as_secs_f64(), &trace.short);
    print_slices("long", crate::monitor::LONG_WINDOW.as_secs_f64(), &trace.long);
}

/// One length of rolling window, as the tier that decides sees it.
///
/// Both lengths are provisional: N3 fixes them from recorded sessions, so the
/// length is printed beside the label rather than left implicit in a constant
/// somebody has to go and read.
fn print_slices(label: &str, seconds: f64, slices: &[crate::monitor::Slice]) {
    if slices.is_empty() {
        println!("  {label:<11} no window of {seconds:.0} s closed; cadence uncounted");
        return;
    }
    println!(
        "  {:<11} {} windows of {:.0} s  {:>6} {:>8} {:>8} {:>8} {:>8} {:>9}",
        label,
        slices.len(),
        seconds,
        "aus",
        "p50 ms",
        "p99 ms",
        ">2T/m",
        "clus/m",
        "gapp50"
    );
    for slice in slices {
        let window = slice.window;
        println!(
            "    {:>5.0}-{:<6.0}                       {:>6} {:>8.2} {:>8.2} {:>8.1} {:>8.1} \
             {:>9.1}",
            slice.from_s,
            slice.to_s,
            window.delivered,
            window.p50_ms,
            window.p99_ms,
            window.tail.per_minute(2, window.span_s),
            window.tail.clusters_per_minute(window.span_s),
            window.tail.stall_gap_p50_ms
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

/// Starts the phase loop, or says why this run has nothing for it to do.
///
/// Every refusal here is a run where one of the two things the estimate is
/// measured between does not exist. Reporting those as a loop that decided to
/// send nothing would make the two indistinguishable, and the whole point of the
/// switch is that they are not.
///
/// The display is checked before the host, because an observing run needs a
/// refresh to measure against and no host at all: the phase it reports is the
/// property of this machine that an acting run then tries to move.
fn start_phase(
    cli: &Cli,
    telemetry: &Arc<Telemetry>,
    control: Option<&Arc<Mutex<ControlClient>>>,
    source_period: Nanos,
    stop: &Arc<AtomicBool>,
) -> PhaseLoop {
    if cli.phase_align == crate::PhaseAlign::Off {
        return PhaseLoop::Idle(crate::phase::Summary::off());
    }
    if cli.link_only {
        return PhaseLoop::Idle(crate::phase::Summary::unavailable(
            "--link-only has no display link to measure a phase against",
        ));
    }
    if cli.mode != crate::Mode::DisplayLink {
        return PhaseLoop::Idle(crate::phase::Summary::unavailable(
            "a refresh phase exists only under --mode display-link",
        ));
    }
    let control = match (cli.phase_align, control) {
        (crate::PhaseAlign::Observe, _) => None,
        (_, Some(control)) => Some(Arc::clone(control)),
        (_, None) => {
            return PhaseLoop::Idle(crate::phase::Summary::unavailable(
                "the host can only be asked over the control plane, which needs \
                 --transport lan; --phase-align observe measures without one",
            ));
        }
    };
    PhaseLoop::Running(crate::phase::spawn(
        Arc::clone(telemetry),
        control,
        source_period,
        Arc::clone(stop),
    ))
}

/// Whether the phase loop is running, and its summary when it is not.
///
/// Two outcomes rather than a success and a failure. A run with the switch off,
/// or one whose mode has no refresh to align to, is not a broken run: it is a
/// run that has a summary already and no thread to wait for. `Result` would say
/// otherwise to every reader.
enum PhaseLoop {
    Running(thread::JoinHandle<crate::phase::Summary>),
    Idle(crate::phase::Summary),
}

fn report(
    cli: &Cli,
    outcome: &RunOutcome,
    memory: &Trend,
    render: &RenderStats,
    snapshot: &Snapshot,
    // What the phase loop did, which the wait it was shrinking cannot say.
    phase: &crate::phase::Summary,
    // Delivery cadence, measured at the depacketiser rather than inferred
    // from presentation.
    link: lanplay_link_metrics::Window,
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
            "  reordering       depth max {}, gap filled in {:.3} ms p50 / {:.3} ms p99 / \
             {:.3} ms max over {} gaps",
            transport.rx.max_reorder_depth,
            transport.reorder_wait.p50_ns as f64 / 1e6,
            transport.reorder_wait.p99_ns as f64 / 1e6,
            transport.rx.reorder_wait_max_ns as f64 / 1e6,
            transport.rx.reorder_waits
        );
        println!("  rfc3550 jitter   {}", transport.jitter);
        // The link's own cadence, taken at the depacketiser. This is the
        // series a radio experiment is ranked by, and the only one that stays
        // a measurement when the display link does not.
        println!("  au delivery      {}", link);
        println!(
            "  au start         n={} p50 {:.2} ms p95 {:.2} ms p99 {:.2} ms max {:.2} ms",
            link.delivered,
            link.first_p50_ms,
            link.first_p95_ms,
            link.first_p99_ms,
            link.first_max_ms
        );
        // Counted crossings, because a percentile below a threshold says
        // nothing about how many units crossed it.
        println!(
            "  au late/min      >1.25T {:.1}  >1.5T {:.1}  >2T {:.1}  >3T {:.1}  \
             >4T {:.1}  >6T {:.1}",
            link.tail.per_minute(0, link.span_s),
            link.tail.per_minute(1, link.span_s),
            link.tail.per_minute(2, link.span_s),
            link.tail.per_minute(3, link.span_s),
            link.tail.per_minute(4, link.span_s),
            link.tail.per_minute(5, link.span_s)
        );
        // Bunching itself: a stall the link then makes up for.
        println!(
            "  au bunching      {:.1} clusters/min, catch-up {:.1} mean / {} max units, \
             stall gap p50 {:.0} ms p95 {:.0} ms",
            link.tail.clusters_per_minute(link.span_s),
            link.tail.mean_catch_up(),
            link.tail.catch_up_max,
            link.tail.stall_gap_p50_ms,
            link.tail.stall_gap_p95_ms
        );
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
    // Beside the wait rather than anywhere else, because this is the loop that
    // exists to shrink that line and the two are only readable together.
    println!("  phase alignment   {phase}");
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
/// What the link is running over, and whether that is a configuration this
/// pipeline is known to stall on.
///
/// A warning rather than a refusal. Measuring a radar band on purpose is how
/// the problem was found, and a check that blocked it would make the finding
/// unrepeatable. Read at the start of every session rather than assumed:
/// this access point was on Auto and moved itself from channel 108 to 116
/// between two sittings.
fn radio_check() -> preflight::Item {
    let Some(link) = lanplay_capabilities::wifi::association() else {
        return preflight::Item::ok("radio", "not on Wi-Fi");
    };
    let (low, high) = link.span_mhz();
    let where_ = format!(
        "channel {} at {} MHz wide, {low}-{high} MHz, {} dBm",
        link.channel, link.width_mhz, link.rssi_dbm
    );
    if link.uses_radar_band() {
        // Measured here: moving off channel 116 took access units arriving
        // more than two source periods late from 69 a minute to 5.5. The
        // regulation requires radar detection in this band; it does not
        // prescribe the 34 ms pause every 220 ms that produced those
        // numbers, so this names the band and not a mechanism.
        return preflight::Item::warn(
            "radio",
            format!(
                "{where_} - this band requires radar detection, and delivery \
                 stalled badly here. Prefer a non-radar channel: 36-48."
            ),
        );
    }
    if link.outside_es_rlan() {
        return preflight::Item::warn(
            "radio",
            format!("{where_} - outside the 5150-5725 MHz allocated to these networks here"),
        );
    }
    preflight::Item::ok("radio", where_)
}

#[allow(clippy::too_many_arguments)]
fn build_report(
    cli: &Cli,
    feed_fps: f64,
    expected_frames: u64,
    outcome: &RunOutcome,
    render: &RenderStats,
    span_end: &SpanEnd,
    snapshot: &Snapshot,
    phase: &crate::phase::Summary,
    // Delivery cadence, already summarised: measured at the depacketiser, so
    // it stays valid whatever the display was doing.
    link: lanplay_link_metrics::Window,
    // What the monitor was asked to cost, and what it saw. Passed rather than
    // read from `cli` again so a report can never disagree with the monitor
    // that produced it.
    monitor_cadence: crate::monitor::Cost,
    // The hint the sampler was holding when the run ended, read from its live
    // slot rather than recovered from the trace. The slot is what a live
    // consumer reads and the trace is the record of it, so taking the value
    // from the slot is what makes the two checkable against each other.
    radio: Option<lanplay_network_health::RadioHint>,
    monitor_trace: &crate::monitor::Trace,
    // Resident memory, so the ten-minute leak question has a structured answer
    // rather than a sentence in the printed report.
    memory: &Trend,
    slices: Vec<crate::report::Window>,
) -> crate::report::Report {
    let arrival = snapshot.segment(Segment::Arrival);
    let decode = snapshot.segment(Segment::Decode);
    let (rx, reorder_wait, jitter, corruption) = match &outcome.transport {
        Some(transport) => (
            transport.rx,
            transport.reorder_wait,
            transport.jitter,
            transport.mismatched,
        ),
        None => (Default::default(), Default::default(), Nanos::ZERO, 0),
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

    // A run whose radio changed channel or width underneath it is not a
    // sample of the conditions it is labelled with. `tools/link-arm.sh` throws
    // such runs away and two datasets were lost for want of exactly that
    // check; the monitor sees it once a second from inside the run, so it goes
    // where this client already says its numbers cannot be trusted rather than
    // into a mechanism of its own.
    for event in &monitor_trace.radio.moved {
        invalidating.push(event.clone());
    }

    // Whether anything below the display link is a measurement at all.
    let cadence_valid = expected_ticks <= 0.0 || (render.callbacks as f64) >= expected_ticks * 0.5;

    // The one number that says whether a QoS marking survived the path,
    // rather than whether the sender asked for it.
    let dominant_dscp = outcome
        .transport
        .as_ref()
        .and_then(|transport| transport.dscp.dominant());

    // One definition, three consumers: the display tier's percentage, the
    // per-window column, and the experience tier's fraction. The reason it may
    // not decide anything is recorded where it is defined.
    let fresh =
        crate::monitor::fresh_tick_ratio(render.rendered, span_end.callbacks(render.callbacks));
    let experience = lanplay_network_health::Experience {
        fresh_tick_ratio: fresh,
        // Absent for a run that presented nothing, for the same reason: the
        // client's `local_age` is measured to presentation, so a run with no
        // display has no age rather than an age of zero.
        frame_age_p99_ms: fresh.map(|_| snapshot.local_age.p99.as_millis_f64()),
        // Audio, which this client does not carry. Stated absent rather than
        // zero: no concealer ran, so nothing concealed nothing.
        concealed_ratio: None,
        silence_events: None,
    };

    // The three tiers as the contract holds them, once per window length: the
    // newest closed window of each, because that is what a live consumer would
    // have been looking at when the run ended.
    //
    // A refusal rather than a tier full of zeroes when either precondition is
    // missing - no window of that length closed, or the run received no
    // datagrams at all - because a `Window` of every counter at zero reads as a
    // flawless link and a loss of none over a population of none reads as a
    // link that lost nothing. Both are absence of evidence used as evidence,
    // and `REFUSED` is a separate outcome from a finding.
    //
    // Loss is datagrams over datagrams: what the receiver accepted plus what
    // never came. The sender's own count is not comparable on this clock, and
    // dividing datagram loss by the access unit count - which the `stream`
    // section above states - read 30.8 per cent reorder where the datagram
    // fraction is nearer one per cent.
    let observe = |window: Option<lanplay_link_metrics::Window>| {
        crate::monitor::observe(
            window,
            radio,
            rx.lost,
            rx.packets,
            rx.reordered,
            experience,
        )
    };
    let observation = observe(monitor_trace.newest_short().map(|slice| slice.window))
        .and_then(|short| {
            observe(monitor_trace.newest_long().map(|slice| slice.window))
                .map(|long| (short, long))
        });

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
            reorder_wait_p50_ms: reorder_wait.p50_ns as f64 / 1e6,
            reorder_wait_p99_ms: reorder_wait.p99_ns as f64 / 1e6,
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
        delivery: crate::report::Delivery {
            delivered: link.delivered,
            au_interval_p50_ms: link.p50_ms,
            au_interval_p95_ms: link.p95_ms,
            au_interval_p99_ms: link.p99_ms,
            au_interval_max_ms: link.max_ms,
            first_interval_p50_ms: link.first_p50_ms,
            first_interval_p95_ms: link.first_p95_ms,
            first_interval_p99_ms: link.first_p99_ms,
            first_interval_max_ms: link.first_max_ms,
            span_s: link.span_s,
            over_1_25t_per_min: link.tail.per_minute(0, link.span_s),
            over_1_5t_per_min: link.tail.per_minute(1, link.span_s),
            over_2t_per_min: link.tail.per_minute(2, link.span_s),
            over_3t_per_min: link.tail.per_minute(3, link.span_s),
            over_4t_per_min: link.tail.per_minute(4, link.span_s),
            over_6t_per_min: link.tail.per_minute(5, link.span_s),
            stall_clusters_per_min: link.tail.clusters_per_minute(link.span_s),
            mean_catch_up_units: link.tail.mean_catch_up(),
            max_catch_up_units: link.tail.catch_up_max,
            stall_gap_p50_ms: link.tail.stall_gap_p50_ms,
            stall_gap_p95_ms: link.tail.stall_gap_p95_ms,
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
            nominal_hz: render.nominal_hz,
            observed_hz: render.display_hz,
            cadence_valid,
            invalid_reason: (!cadence_valid).then(|| {
                format!(
                    "display link delivered {} of about {:.0} refreshes",
                    render.callbacks, expected_ticks
                )
            }),
            callbacks: span_end.callbacks(render.callbacks),
            rendered: render.rendered,
            superseded: render.superseded,
            empty_refreshes: span_end.empty_ticks(render.empty_ticks),
            // The percentage form of the one definition in `monitor`, kept a
            // percentage under a name that says ratio because sixty-three
            // committed sessions carry it that way and
            // `crates/network-health`'s corpus reader divides it by a hundred.
            // Zero for a run with no tick, which is what this field has always
            // said; the honest absence is in the experience tier below.
            fresh_tick_ratio: fresh
                .map(|fraction| fraction * 100.0)
                .unwrap_or(0.0),
            callback_interval_p50_ms: render.callback_interval.p50.as_millis_f64(),
            callback_interval_p95_ms: render.callback_interval.p95.as_millis_f64(),
            callback_interval_p99_ms: render.callback_interval.p99.as_millis_f64(),
            callback_interval_max_ms: render.callback_interval.max.as_millis_f64(),
            frame_age_p50_ms: snapshot.local_age.p50.as_millis_f64(),
            frame_age_p95_ms: snapshot.local_age.p95.as_millis_f64(),
            frame_age_p99_ms: snapshot.local_age.p99.as_millis_f64(),
        },
        phase: phase.into(),
        environment: crate::report::Environment {
            occlusion_changes: render.occlusion_changes,
            space_changes: render.space_changes,
            miniaturise_events: render.miniaturise_events,
            display_changes: render.display_changes,
            link_pauses: render.link_pauses,
            app_nap_protection: true,
        },
        observation: observation.as_ref().ok().map(|(short, long)| {
            crate::report::Observation {
            radio: short.radio.map(|hint| crate::report::RadioHint {
                rssi_dbm: hint.rssi_dbm,
                noise_dbm: hint.noise_dbm,
                tx_rate_mbps: hint.tx_rate_mbps,
                channel: hint.channel,
                width_mhz: hint.width_mhz,
            }),
            stream_short: behaviour(
                "short",
                crate::monitor::SHORT_WINDOW.as_secs_f64(),
                &short.stream,
            ),
            stream_long: behaviour(
                "long",
                crate::monitor::LONG_WINDOW.as_secs_f64(),
                &long.stream,
            ),
            experience: crate::report::Experience {
                fresh_tick_ratio: short.experience.fresh_tick_ratio,
                frame_age_p99_ms: short.experience.frame_age_p99_ms,
            },
            }
        }),
        observation_refused: observation.as_ref().err().cloned(),
        monitor: crate::report::Monitor {
            cadence: monitor_cadence.label(),
            radio_reads: monitor_trace.radio.samples.len() as u64,
            radio_answered: monitor_trace.radio.answered,
            radio_empty: monitor_trace.radio.empty,
            radio_reads_per_s: monitor_trace.radio.reads_per_s,
            radio_cost_max_ms: monitor_trace.radio.cost_max_ms,
            radio_lock_takes: monitor_trace.radio.lock_takes,
            short_windows: monitor_trace.short.len(),
            long_windows: monitor_trace.long.len(),
            short: monitor_trace.short.iter().map(monitor_window).collect(),
            long: monitor_trace.long.iter().map(monitor_window).collect(),
            radio_trace: monitor_trace
                .radio
                .samples
                .iter()
                .map(|sample| crate::report::RadioSample {
                    at_s: sample.at_s,
                    rssi_dbm: sample.hint.map(|hint| hint.rssi_dbm),
                    noise_dbm: sample.hint.map(|hint| hint.noise_dbm),
                    tx_rate_mbps: sample.hint.map(|hint| hint.tx_rate_mbps),
                    channel: sample.hint.map(|hint| hint.channel),
                    width_mhz: sample.hint.map(|hint| hint.width_mhz),
                    cost_ms: sample.cost_ms,
                })
                .collect(),
        },
        memory: {
            let steady = memory.after_warmup(crate::gate::MEMORY_WARMUP);
            let mb = |bytes: f64| bytes / 1_048_576.0;
            crate::report::Memory {
                samples: memory.count(),
                first_mb: memory.first().map_or(0.0, mb),
                last_mb: memory.last().map_or(0.0, mb),
                max_mb: memory.max().map_or(0.0, mb),
                slope_mb_per_min: memory.slope_per_minute().map(mb),
                steady_slope_mb_per_min: steady.slope_per_minute().map(mb),
                steady_samples: steady.count(),
                allowed_mb_per_min: mb(crate::gate::MAX_MEMORY_GROWTH),
                warmup_ms: crate::gate::MEMORY_WARMUP.get() as f64 / 1e6,
            }
        },
        windows: slices,
    }
}

/// One rolling window as the report states it.
///
/// The rates are derived from the window's own span rather than from its
/// nominal length: a window whose first access unit arrived late covers less
/// time than it is labelled with, and dividing by the label would understate
/// every crossing rate in it.
fn monitor_window(slice: &crate::monitor::Slice) -> crate::report::MonitorWindow {
    let window = slice.window;
    crate::report::MonitorWindow {
        from_s: slice.from_s,
        to_s: slice.to_s,
        span_s: window.span_s,
        delivered: window.delivered,
        au_interval_p50_ms: window.p50_ms,
        au_interval_p99_ms: window.p99_ms,
        over_2t: window.tail.over[2],
        over_2t_per_min: window.tail.per_minute(2, window.span_s),
        clusters: window.tail.clusters,
        clusters_per_min: window.tail.clusters_per_minute(window.span_s),
        stall_gap_p50_ms: window.tail.stall_gap_p50_ms,
        stall_gap_p95_ms: window.tail.stall_gap_p95_ms,
    }
}

/// The middle tier as the report states it.
///
/// A `StreamBehaviour` only exists when a window closed and a datagram
/// population was non-empty, so nothing here is optional. The refusal that
/// takes its place lives in `observation_refused`, which is what keeps "the
/// link was fine" and "nothing was measured" from serialising the same way.
///
/// `Incidence::population` is `Option` because the corpus contains envelopes
/// that state a numerator and no denominator. A live run always has both, so
/// the absence is expected never to occur here and is stated as zero rather
/// than hidden - a zero population beside a non-zero event count is visibly
/// wrong, which is what a reader needs it to be.
fn behaviour(
    window: &'static str,
    seconds: f64,
    stream: &lanplay_network_health::StreamBehaviour,
) -> crate::report::StreamBehaviour {
    let delivery = stream.delivery;
    crate::report::StreamBehaviour {
        window,
        window_seconds: seconds,
        span_s: delivery.span_s,
        delivered: delivery.delivered,
        au_interval_p50_ms: delivery.p50_ms,
        au_interval_p99_ms: delivery.p99_ms,
        over_2t_per_min: delivery.tail.per_minute(2, delivery.span_s),
        clusters_per_min: delivery.tail.clusters_per_minute(delivery.span_s),
        stall_gap_p50_ms: delivery.tail.stall_gap_p50_ms,
        stall_gap_p95_ms: delivery.tail.stall_gap_p95_ms,
        // The corpus reader looks for exactly these two beside the ratio, so a
        // live session is readable by the same parser as a committed one.
        loss_events: stream.loss_ratio.map_or(0, |loss| loss.events()),
        loss_population: stream.loss_ratio.map_or(0, |loss| loss.population()),
        loss_ratio: stream.loss_ratio.map_or(0.0, |loss| loss.value()),
        reorder_events: stream.reorder.events(),
        reorder_population: stream.reorder.population().unwrap_or(0),
        reorder_ratio: stream.reorder.value().unwrap_or(0.0),
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
