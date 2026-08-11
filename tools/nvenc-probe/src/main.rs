//! Isolated D3D11-to-NVENC latency, throughput, and cadence benchmark.
#[cfg(windows)]
mod nv12;

#[cfg(windows)]
const SLOT_COUNT: usize = 4;

fn main() {
    #[cfg(windows)]
    if let Err(error) = windows_main() {
        eprintln!("FAIL: {error}");
        std::process::exit(1);
    }
    #[cfg(not(windows))]
    {
        eprintln!("lanplay-nvenc-probe requires Windows");
        std::process::exit(2);
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum RunMode {
    Uncapped,
    Paced,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Preset {
    P1,
    P2,
    P3,
}

#[cfg(windows)]
impl From<Preset> for lanplay_encoder_nvenc::EncoderPreset {
    fn from(value: Preset) -> Self {
        match value {
            Preset::P1 => Self::P1,
            Preset::P2 => Self::P2,
            Preset::P3 => Self::P3,
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum Tuning {
    Ll,
    Ull,
}

#[cfg(windows)]
impl From<Tuning> for lanplay_encoder_nvenc::LatencyTuning {
    fn from(value: Tuning) -> Self {
        match value {
            Tuning::Ll => Self::LowLatency,
            Tuning::Ull => Self::UltraLowLatency,
        }
    }
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum InputFormat {
    Bgra,
    Nv12,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
enum ContentSource {
    /// One frame-wide colour, changed deterministically every frame.
    Flat,
    /// GPU-generated motion, high-frequency detail, and frame-wide scene changes.
    Pattern,
    /// Live Desktop Duplication copied into the encoder-owned slot.
    Dda,
    /// Live Windows.Graphics.Capture copied into the encoder-owned slot.
    Wgc,
}

#[cfg(windows)]
#[derive(clap::Parser)]
#[command(name = "lanplay-nvenc-probe")]
struct Args {
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 1080)]
    height: u32,
    #[arg(long, default_value_t = 120)]
    fps: u32,
    #[arg(long, default_value_t = 50)]
    bitrate_mbps: u32,
    #[arg(long, value_enum, default_value_t = Preset::P1)]
    preset: Preset,
    #[arg(long, value_enum, default_value_t = Tuning::Ull)]
    tuning: Tuning,
    #[arg(long, value_enum, default_value_t = InputFormat::Bgra)]
    input: InputFormat,
    #[arg(long, value_enum, default_value_t = RunMode::Uncapped)]
    mode: RunMode,
    #[arg(long, value_enum, default_value_t = ContentSource::Flat)]
    source: ContentSource,
    /// Which DXGI output to capture. Synthetic sources still create their
    /// D3D11/NVENC device on the adapter that owns this output.
    #[arg(long, default_value_t = 0)]
    output: u32,
    /// Send each completed H.264 access unit as RTP/UDP to this receiver.
    #[arg(long)]
    send_to: Option<std::net::SocketAddr>,
    /// RTP datagram size, including headers.
    #[arg(long, default_value_t = lanplay_transport::MAX_UDP_PAYLOAD)]
    mtu: usize,
    /// Overrides `--frames`; intended for soak runs.
    #[arg(long)]
    seconds: Option<u64>,
    /// Frames discarded before measurement.
    #[arg(long, default_value_t = 120)]
    warmup: u64,
    #[arg(long, default_value_t = 1200)]
    frames: u64,
    /// Width of each reporting slice. A ten-minute mean cannot show a stall.
    #[arg(long, default_value_t = 10.0)]
    window_seconds: f64,
    /// Force an IDR every N measured frames; zero means only the first.
    #[arg(long, default_value_t = 0)]
    idr_interval: u64,
}

#[cfg(windows)]
struct Surface {
    texture: windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
    target: windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default)]
struct SourceSample {
    wait_ns: u64,
    accumulated_frames: u32,
    update: lanplay_capture::FrameUpdate,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Default)]
struct NetworkSample {
    packets: u32,
    bytes: u64,
    errors: u64,
    send_ns: u64,
}

/// Access units onto the wire, one burst per frame.
///
/// There is deliberately no pacer here. Three measurements said the same
/// thing: pacing to the configured bitrate spent `p50 7.667 ms` of an
/// 8.333 ms period inside this call, and because it runs on the encoder's
/// completion thread that delay is charged to the next frame's completion as
/// well. CBR with a small VBV already bounds what we produce over the medium
/// term, and the AP aggregates regardless. If evidence ever reopens pacing -
/// an RTT gradient, a growing NIC queue, loss, late access units - it belongs
/// behind a bounded queue on a network thread, never here.
#[cfg(windows)]
struct MediaSender {
    socket: std::net::UdpSocket,
    packetizer: lanplay_transport::Packetizer,
    fps: u32,
}

#[cfg(windows)]
impl MediaSender {
    fn new(target: std::net::SocketAddr, fps: u32, mtu: usize) -> Result<Self, String> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0")
            .map_err(|error| format!("bind RTP sender: {error}"))?;
        socket
            .connect(target)
            .map_err(|error| format!("connect RTP sender to {target}: {error}"))?;
        let packetizer = lanplay_transport::Packetizer::new(
            lanplay_transport::Ssrc(lanplay_transport::random_u32()),
            lanplay_transport::RtpClock::new(
                lanplay_transport::H264_CLOCK_RATE,
                lanplay_transport::random_u32(),
            ),
            lanplay_transport::H264_PAYLOAD_TYPE,
            mtu,
        );
        Ok(Self {
            socket,
            packetizer,
            fps,
        })
    }

    fn send(&mut self, frame: u64, is_idr: bool, data: Vec<u8>) -> Result<NetworkSample, String> {
        let started = std::time::Instant::now();
        let unit = lanplay_video_core::EncodedAccessUnit {
            id: lanplay_protocol::FrameId::new(frame + 1),
            pts: lanplay_video_core::VideoTimestamp::from_frame_index(frame, self.fps, 1),
            is_idr,
            data,
        };
        let mut sent = 0u64;
        let mut errors = 0u64;
        let socket = &self.socket;
        let packetized = self
            .packetizer
            .packetize(&unit, |datagram| match socket.send(datagram) {
                Ok(bytes) => sent += bytes as u64,
                Err(_) => errors += 1,
            })
            .map_err(|error| format!("packetize frame {frame}: {error}"))?;
        Ok(NetworkSample {
            packets: packetized.packets,
            bytes: sent,
            errors,
            send_ns: started.elapsed().as_nanos() as u64,
        })
    }
}

#[cfg(windows)]
enum Content {
    Flat,
    Pattern(lanplay_present_source::gpu::Pipeline),
    Dda(lanplay_capture::DesktopDuplication),
    Wgc(lanplay_capture::GraphicsCapture),
}

#[cfg(windows)]
impl Content {
    fn new(source: ContentSource, device: &lanplay_capture::CaptureDevice) -> Result<Self, String> {
        match source {
            ContentSource::Flat => Ok(Self::Flat),
            ContentSource::Pattern => lanplay_present_source::gpu::Pipeline::new(device.device())
                .map(Self::Pattern)
                .map_err(|error| error.to_string()),
            ContentSource::Dda => {
                let capture =
                    lanplay_capture::DesktopDuplication::new(device).map_err(|e| e.to_string())?;
                start_capture(capture).map(Self::Dda)
            }
            ContentSource::Wgc => {
                let capture =
                    lanplay_capture::GraphicsCapture::new(device).map_err(|e| e.to_string())?;
                start_capture(capture).map(Self::Wgc)
            }
        }
    }

    fn draw(
        &mut self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        surface: &Surface,
        width: u32,
        height: u32,
        frame: u64,
    ) -> Result<SourceSample, String> {
        match self {
            Self::Flat => {
                clear_surface(context, &surface.target, frame);
                Ok(SourceSample::default())
            }
            Self::Pattern(pipeline) => {
                pipeline.draw_target(context, &surface.target, width, height, frame as u32);
                Ok(SourceSample::default())
            }
            Self::Dda(capture) => capture_into_surface(capture, context, surface, width, height),
            Self::Wgc(capture) => capture_into_surface(capture, context, surface, width, height),
        }
    }
}

#[cfg(windows)]
fn start_capture<B: lanplay_capture::CaptureBackend>(mut capture: B) -> Result<B, String> {
    capture
        .start(lanplay_capture::CaptureConfig {
            output: 0,
            buffers: 2,
            acquire_timeout_ms: 100,
            cursor: false,
        })
        .map_err(|error| error.to_string())?;
    Ok(capture)
}

#[cfg(windows)]
fn capture_into_surface<B: lanplay_capture::CaptureBackend>(
    capture: &mut B,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    surface: &Surface,
    width: u32,
    height: u32,
) -> Result<SourceSample, String> {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(1);
    loop {
        let restart = {
            match capture.acquire().map_err(|error| error.to_string())? {
                lanplay_capture::Acquired::Frame(captured) => {
                    if captured.width != width || captured.height != height {
                        return Err(format!(
                            "capture is {}x{}, encoder is {width}x{height}",
                            captured.width, captured.height
                        ));
                    }
                    let accumulated = captured.metadata.accumulated_frames.unwrap_or(1);
                    // SAFETY: both textures belong to the same device, have
                    // identical dimensions/format, and remain alive through
                    // command submission.
                    unsafe {
                        context.CopyResource(&surface.texture, captured.texture);
                    }
                    return Ok(SourceSample {
                        wait_ns: started.elapsed().as_nanos() as u64,
                        accumulated_frames: accumulated,
                        update: captured.metadata.update,
                    });
                }
                lanplay_capture::Acquired::Timeout => false,
                lanplay_capture::Acquired::Lost => true,
            }
        };
        if restart {
            capture.restart().map_err(|error| error.to_string())?;
        }
        if std::time::Instant::now() >= deadline {
            return Err("capture produced no frame for one second".into());
        }
    }
}

/// Where each thread last got to, so a wedge names itself.
///
/// A pipeline that stops has exactly two symptoms from the outside: no
/// output, and no CPU. Bisecting a hang by rerunning it with pieces removed
/// costs minutes per attempt and only ever narrows it to a component. One
/// atomic per thread, stored before every call that can block, narrows it to
/// a line.
#[cfg(windows)]
mod stage {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    pub static SUBMIT: AtomicUsize = AtomicUsize::new(0);
    pub static COMPLETE: AtomicUsize = AtomicUsize::new(0);
    /// Frames the submit loop has handed to the encoder. The watchdog's
    /// liveness signal: a wedge is this standing still.
    pub static SUBMITTED: AtomicU64 = AtomicU64::new(0);
    pub static COMPLETED: AtomicU64 = AtomicU64::new(0);

    pub const SUBMIT_NAMES: [&str; 8] = [
        "start",
        "pacing",
        "waiting for a free slot",
        "capturing",
        "converting to NV12",
        "mapping the input",
        "submitting to NVENC",
        "queueing for completion",
    ];
    pub const COMPLETE_NAMES: [&str; 6] = [
        "start",
        "waiting for work",
        "waiting for the completion event",
        "locking the bitstream",
        "unlocking",
        "sending",
    ];

    #[inline]
    pub fn submit(step: usize) {
        SUBMIT.store(step, Ordering::Relaxed);
    }

    #[inline]
    pub fn complete(step: usize) {
        COMPLETE.store(step, Ordering::Relaxed);
    }

    pub fn describe() -> String {
        format!(
            "submit thread {} (frame {}), completion thread {} (frame {})",
            SUBMIT_NAMES[SUBMIT.load(Ordering::Relaxed).min(SUBMIT_NAMES.len() - 1)],
            SUBMITTED.load(Ordering::Relaxed),
            COMPLETE_NAMES[COMPLETE
                .load(Ordering::Relaxed)
                .min(COMPLETE_NAMES.len() - 1)],
            COMPLETED.load(Ordering::Relaxed),
        )
    }
}

#[cfg(windows)]
struct Work<'a> {
    slot: usize,
    preprocess_ns: u64,
    source_wait_ns: u64,
    accumulated_frames: u32,
    update: lanplay_capture::FrameUpdate,
    submitted: lanplay_encoder_nvenc::SubmittedFrame<'a>,
    admitted: std::time::Instant,
    admission_wait_ns: u64,
    map_ns: u64,
    submit_ns: u64,
    submit_end: std::time::Instant,
}

#[cfg(windows)]
struct FrameMetrics {
    frame: u64,
    /// When the encoder said the frame was done. Windows are sliced on this
    /// rather than on the frame index: an index assumes the pacing held,
    /// which is exactly what a window is there to check.
    completed_at: std::time::Instant,
    is_idr: bool,
    preprocess_ns: u64,
    preprocess_gpu_ns: Option<u64>,
    source_wait_ns: u64,
    accumulated_frames: u32,
    update: lanplay_capture::FrameUpdate,
    bytes: usize,
    admission_wait_ns: u64,
    map_ns: u64,
    submit_ns: u64,
    completion_ns: u64,
    lock_ns: u64,
    copy_ns: u64,
    unlock_ns: u64,
    residency_ns: u64,
    slot_residency_ns: u64,
    network: NetworkSample,
}

#[cfg(windows)]
fn windows_main() -> Result<(), String> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use clap::Parser;

    let args = Args::parse();
    if args.width == 0
        || args.height == 0
        || args.fps == 0
        || args.frames == 0
        || args.bitrate_mbps == 0
    {
        return Err("dimensions, rate, bitrate, and frame count must be non-zero".into());
    }
    let frames = match args.seconds {
        Some(seconds) => seconds
            .checked_mul(u64::from(args.fps))
            .ok_or_else(|| "soak frame count overflows u64".to_owned())?,
        None => args.frames,
    };
    if frames == 0 {
        return Err("measured frame count must be non-zero".into());
    }
    let bitrate = args
        .bitrate_mbps
        .checked_mul(1_000_000)
        .ok_or_else(|| "bitrate overflows u32".to_owned())?;

    let device = lanplay_capture::CaptureDevice::open(args.output).map_err(|e| e.to_string())?;
    println!("device {}", device.identity());
    let mut surfaces = Vec::with_capacity(SLOT_COUNT);
    for _ in 0..SLOT_COUNT {
        surfaces.push(benchmark_surface(device.device(), args.width, args.height)?);
    }
    let mut content = Content::new(args.source, &device)?;

    let mut session =
        lanplay_encoder_nvenc::NvencSession::open(device.device()).map_err(|e| e.to_string())?;
    let codecs = session.encode_guids().map_err(|e| e.to_string())?;
    let h264 = lanplay_encoder_nvenc::NvencSession::h264_codec_guid();
    if !codecs.contains(&h264) {
        return Err("the selected NVENC session does not expose H.264".into());
    }
    let bgra_textures: Vec<_> = surfaces
        .iter()
        .map(|surface| surface.texture.clone())
        .collect();
    let mut converter = match args.input {
        InputFormat::Bgra => None,
        InputFormat::Nv12 => Some(nv12::Converter::new(
            device.device(),
            device.context(),
            args.width,
            args.height,
            args.fps,
            &bgra_textures,
        )?),
    };
    if !session.supports_async().map_err(|e| e.to_string())? {
        return Err("the selected NVENC session does not support async encoding".into());
    }
    let formats = session.input_formats(h264).map_err(|e| e.to_string())?;
    println!("H.264 input formats: {formats:?}");
    session
        .initialize_h264(lanplay_encoder_nvenc::H264Config {
            width: args.width,
            height: args.height,
            fps_num: args.fps,
            fps_den: 1,
            bitrate,
            async_mode: true,
            preset: args.preset.into(),
            tuning: args.tuning.into(),
        })
        .map_err(|e| e.to_string())?;

    let mut inputs = Vec::with_capacity(SLOT_COUNT);
    let mut outputs = Vec::with_capacity(SLOT_COUNT);
    for surface in &surfaces {
        let input = match &converter {
            Some(converter) => session.register_nv12(converter.texture(inputs.len())),
            None => session.register_bgra(&surface.texture),
        };
        inputs.push(input.map_err(|e| e.to_string())?);
        outputs.push(session.create_output_buffer().map_err(|e| e.to_string())?);
    }

    for frame in 0..args.warmup {
        let slot = frame as usize % SLOT_COUNT;
        let _ = content.draw(
            device.context(),
            &surfaces[slot],
            args.width,
            args.height,
            frame,
        )?;
        if let Some(converter) = converter.as_mut() {
            let _ = converter.convert(slot, frame)?;
        }
        let encoded = session
            .encode(&inputs[slot], &outputs[slot], frame, frame == 0)
            .map_err(|e| format!("warm-up frame {frame}: {e}"))?;
        validate_bitstream(&encoded.data, frame)?;
    }

    let memory_stop = Arc::new(AtomicBool::new(false));
    let memory_samples = std::thread::spawn({
        let stop = Arc::clone(&memory_stop);
        move || {
            let mut samples = Vec::new();
            while !stop.load(Ordering::Acquire) {
                if let Some(bytes) = lanplay_telemetry::resident_bytes() {
                    samples.push((Instant::now(), bytes));
                }
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
            if let Some(bytes) = lanplay_telemetry::resident_bytes() {
                samples.push((Instant::now(), bytes));
            }
            samples
        }
    });

    // A hang used to look identical to a slow run from the outside: no
    // output, no CPU, nothing to grep. This turns it into one line naming the
    // call that never returned, and then kills the run rather than letting a
    // harness sit on it until its own timeout.
    let watchdog_stop = Arc::clone(&memory_stop);
    std::thread::spawn(move || {
        const STALL: std::time::Duration = std::time::Duration::from_secs(5);
        let mut last = (0u64, 0u64);
        let mut still = std::time::Duration::ZERO;
        while !watchdog_stop.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let now = (
                stage::SUBMITTED.load(Ordering::Relaxed),
                stage::COMPLETED.load(Ordering::Relaxed),
            );
            if now == last {
                still += std::time::Duration::from_millis(250);
                if still >= STALL {
                    eprintln!(
                        "FAIL: stalled {} s in {}",
                        STALL.as_secs(),
                        stage::describe()
                    );
                    // The point of the watchdog is that the process is wedged
                    // in a call that will not return, so unwinding is not on
                    // the table.
                    std::process::exit(3);
                }
            } else {
                last = now;
                still = std::time::Duration::ZERO;
            }
        }
    });

    let sender = args
        .send_to
        .map(|target| MediaSender::new(target, args.fps, args.mtu))
        .transpose()?;
    let run = run_pipeline(
        &session,
        device.context(),
        &surfaces,
        &mut content,
        &inputs,
        &outputs,
        args.width,
        args.height,
        args.mode,
        converter.as_mut(),
        args.fps,
        args.warmup,
        frames,
        args.idr_interval,
        sender,
    );
    memory_stop.store(true, Ordering::Release);
    let memory = memory_samples
        .join()
        .map_err(|_| "memory sampler panicked".to_owned())?;
    let (metrics, elapsed, pool_exhaustions) = run?;
    report(&args, frames, &metrics, elapsed, pool_exhaustions, &memory)
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn run_pipeline<'a>(
    session: &'a lanplay_encoder_nvenc::NvencSession,
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    surfaces: &[Surface],
    content: &mut Content,
    inputs: &'a [lanplay_encoder_nvenc::RegisteredInput<'a>],
    outputs: &'a [lanplay_encoder_nvenc::OutputBuffer<'a>],
    width: u32,
    height: u32,
    mode: RunMode,
    mut converter: Option<&mut nv12::Converter>,
    fps: u32,
    first_frame: u64,
    frames: u64,
    idr_interval: u64,
    sender: Option<MediaSender>,
) -> Result<(Vec<FrameMetrics>, std::time::Duration, u64), String> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{TryRecvError, sync_channel};
    use std::time::{Duration, Instant};

    let (work_tx, work_rx) = sync_channel::<Work<'a>>(SLOT_COUNT);
    let (free_tx, free_rx) = sync_channel::<usize>(SLOT_COUNT);
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<FrameMetrics, String>>();
    for slot in 0..SLOT_COUNT {
        free_tx
            .send(slot)
            .map_err(|_| "failed to seed the free-slot queue".to_owned())?;
    }
    let failed = Arc::new(AtomicBool::new(false));
    let period = Duration::from_secs_f64(1.0 / f64::from(fps));
    let started = Instant::now();
    let mut pool_exhaustions = 0u64;
    let mut submitted_count = 0u64;
    let mut preprocess_gpu = Vec::with_capacity(frames as usize);

    let outcome = std::thread::scope(|scope| -> Result<Vec<FrameMetrics>, String> {
        let completion_failed = Arc::clone(&failed);
        let completion = scope.spawn(move || {
            let mut sender = sender;
            loop {
                stage::complete(1);
                let Ok(work) = work_rx.recv() else {
                    break;
                };
                let slot = work.slot;
                let result = if completion_failed.load(Ordering::Acquire) {
                    Err(format!(
                        "frame {} aborted after prior failure",
                        work.submitted.frame_index()
                    ))
                } else {
                    complete_work(session, work, sender.as_mut())
                };
                if result.is_err() {
                    completion_failed.store(true, Ordering::Release);
                }
                if let Ok(metrics) = &result {
                    stage::COMPLETED.store(metrics.frame, Ordering::Relaxed);
                }
                let _ = result_tx.send(result);
                let _ = free_tx.send(slot);
            }
        });

        for offset in 0..frames {
            if failed.load(Ordering::Acquire) {
                break;
            }
            if mode == RunMode::Paced {
                stage::submit(1);
                wait_until(started + period.mul_f64(offset as f64));
            }
            let wait_start = Instant::now();
            stage::submit(2);
            let slot = match free_rx.try_recv() {
                Ok(slot) => slot,
                Err(TryRecvError::Empty) => {
                    pool_exhaustions += 1;
                    loop {
                        if failed.load(Ordering::Acquire) {
                            break usize::MAX;
                        }
                        match free_rx.try_recv() {
                            Ok(slot) => break slot,
                            Err(TryRecvError::Empty) => std::thread::yield_now(),
                            Err(TryRecvError::Disconnected) => break usize::MAX,
                        }
                    }
                }
                Err(TryRecvError::Disconnected) => usize::MAX,
            };
            if slot == usize::MAX {
                break;
            }
            let admitted = Instant::now();
            let admission_wait_ns = (admitted - wait_start).as_nanos() as u64;
            let frame = first_frame + offset;
            stage::submit(3);
            let source = content.draw(context, &surfaces[slot], width, height, frame)?;
            stage::submit(4);
            let preprocess_start = Instant::now();
            if let Some(converter) = converter.as_deref_mut()
                && let Some(sample) = converter.convert(slot, frame)?
                && sample.0 >= first_frame
            {
                preprocess_gpu.push(sample);
            }
            let preprocess_ns = preprocess_start.elapsed().as_nanos() as u64;

            stage::submit(5);
            let map_start = Instant::now();
            let mapped = session
                .map_input(&inputs[slot])
                .map_err(|e| format!("map frame {frame}: {e}"))?;
            let map_end = Instant::now();
            let force_idr = offset == 0 || (idr_interval != 0 && offset % idr_interval == 0);
            stage::submit(6);
            let submit_start = Instant::now();
            let submitted = session
                .encode_submit(mapped, &outputs[slot], frame, force_idr)
                .map_err(|e| format!("submit frame {frame}: {e}"))?;
            stage::submit(7);
            let submit_end = Instant::now();
            work_tx
                .send(Work {
                    slot,
                    preprocess_ns,
                    source_wait_ns: source.wait_ns,
                    accumulated_frames: source.accumulated_frames,
                    update: source.update,
                    submitted,
                    admitted,
                    admission_wait_ns,
                    map_ns: (map_end - map_start).as_nanos() as u64,
                    submit_ns: (submit_end - submit_start).as_nanos() as u64,
                    submit_end,
                })
                .map_err(|_| "completion thread stopped accepting work".to_owned())?;
            stage::SUBMITTED.store(frame, Ordering::Relaxed);
            submitted_count += 1;
        }
        drop(work_tx);
        completion
            .join()
            .map_err(|_| "completion thread panicked".to_owned())?;

        let mut metrics = Vec::with_capacity(submitted_count as usize);
        let mut first_error = None;
        for _ in 0..submitted_count {
            match result_rx
                .recv()
                .map_err(|_| "completion result channel closed".to_owned())?
            {
                Ok(frame) => metrics.push(frame),
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else if submitted_count != frames {
            Err(format!(
                "submitted {submitted_count} of {frames} requested frames"
            ))
        } else {
            Ok(metrics)
        }
    });
    let elapsed = started.elapsed();
    let mut metrics = outcome?;
    if let Some(converter) = converter {
        preprocess_gpu.extend(
            converter
                .finish_timings()?
                .into_iter()
                .filter(|(frame, _)| *frame >= first_frame),
        );
    }
    let gpu_by_frame: std::collections::HashMap<_, _> = preprocess_gpu.into_iter().collect();
    for metric in &mut metrics {
        metric.preprocess_gpu_ns = gpu_by_frame.get(&metric.frame).copied();
    }
    Ok((metrics, elapsed, pool_exhaustions))
}

#[cfg(windows)]
fn complete_work(
    session: &lanplay_encoder_nvenc::NvencSession,
    work: Work<'_>,
    mut sender: Option<&mut MediaSender>,
) -> Result<FrameMetrics, String> {
    let frame = work.submitted.frame_index();
    let is_idr = work.submitted.is_idr();
    stage::complete(2);
    session
        .wait_completion(&work.submitted)
        .map_err(|e| format!("completion event frame {frame}: {e}"))?;
    stage::complete(3);
    let completed = std::time::Instant::now();
    let lock_start = std::time::Instant::now();
    let locked = session
        .lock_bitstream(work.submitted)
        .map_err(|e| format!("lock frame {frame}: {e}"))?;
    let lock_return = std::time::Instant::now();
    validate_bitstream(locked.bytes(), frame)?;
    let bytes = if sender.is_some() {
        lanplay_video_core::to_avcc(
            lanplay_video_core::split_annex_b(locked.bytes()),
            lanplay_transport::NAL_LENGTH_SIZE,
        )
    } else {
        locked.bytes().to_vec()
    };
    stage::complete(4);
    let encoded_bytes = bytes.len();
    let copy_end = std::time::Instant::now();
    let unlock_start = std::time::Instant::now();
    locked
        .unlock()
        .map_err(|e| format!("unlock frame {frame}: {e}"))?;
    let unlock_end = std::time::Instant::now();
    stage::complete(5);
    let network = match sender.as_deref_mut() {
        Some(sender) => sender.send(frame, is_idr, bytes)?,
        None => NetworkSample::default(),
    };
    Ok(FrameMetrics {
        frame,
        completed_at: completed,
        is_idr,
        preprocess_ns: work.preprocess_ns,
        source_wait_ns: work.source_wait_ns,
        accumulated_frames: work.accumulated_frames,
        update: work.update,
        preprocess_gpu_ns: None,
        bytes: encoded_bytes,
        admission_wait_ns: work.admission_wait_ns,
        map_ns: work.map_ns,
        submit_ns: work.submit_ns,
        completion_ns: (completed - work.submit_end).as_nanos() as u64,
        lock_ns: (lock_return - lock_start).as_nanos() as u64,
        copy_ns: (copy_end - lock_return).as_nanos() as u64,
        unlock_ns: (unlock_end - unlock_start).as_nanos() as u64,
        residency_ns: (copy_end - work.submit_end).as_nanos() as u64,
        slot_residency_ns: (unlock_end - work.admitted).as_nanos() as u64,
        network,
    })
}

#[cfg(windows)]
fn wait_until(deadline: std::time::Instant) {
    use std::time::Duration;

    const SPIN: Duration = Duration::from_micros(200);
    loop {
        let now = std::time::Instant::now();
        if now >= deadline {
            return;
        }
        let remaining = deadline - now;
        if remaining > SPIN {
            std::thread::sleep(remaining - SPIN);
        } else {
            std::hint::spin_loop();
        }
    }
}

#[cfg(windows)]
fn report(
    args: &Args,
    frames: u64,
    metrics: &[FrameMetrics],
    elapsed: std::time::Duration,
    pool_exhaustions: u64,
    memory: &[(std::time::Instant, u64)],
) -> Result<(), String> {
    let throughput = frames as f64 / elapsed.as_secs_f64();
    println!(
        "config {}x{} @ {} fps, {} Mbps, {:?} {:?} CBR, {:?} input, {:?} source, IDR interval {}, async {} slots, {:?}",
        args.width,
        args.height,
        args.fps,
        args.bitrate_mbps,
        args.preset,
        args.tuning,
        args.input,
        args.source,
        args.idr_interval,
        SLOT_COUNT,
        args.mode
    );
    println!(
        "frames {} in {:.3} s = {:.2} frames/s, pool exhaustions {}",
        frames,
        elapsed.as_secs_f64(),
        throughput,
        pool_exhaustions
    );
    if matches!(args.source, ContentSource::Dda | ContentSource::Wgc) {
        print_stage("capture acquire", metrics.iter().map(|m| m.source_wait_ns));
    }
    if args.source == ContentSource::Dda {
        let (desktop, pointer_only, other) =
            capture_update_counts(metrics.iter().map(|metric| metric.update));
        let accumulated =
            CountDistribution::new(metrics.iter().map(|metric| metric.accumulated_frames));
        println!(
            "DDA AcquireNextFrame S_OK {} = {:.2}/s",
            metrics.len(),
            metrics.len() as f64 / elapsed.as_secs_f64()
        );
        println!(
            "DDA desktop updates (LastPresentTime != 0) {desktop} = {:.2}/s",
            desktop as f64 / elapsed.as_secs_f64()
        );
        println!(
            "DDA pointer-only (LastPresentTime == 0, LastMouseUpdateTime != 0) {pointer_only} = {:.2}/s",
            pointer_only as f64 / elapsed.as_secs_f64()
        );
        println!(
            "DDA other updates (both timestamps zero) {other} = {:.2}/s",
            other as f64 / elapsed.as_secs_f64()
        );
        println!("DDA AccumulatedFrames {accumulated}");
        let accumulated_while_busy: u64 = metrics
            .iter()
            .map(|metric| u64::from(metric.accumulated_frames.saturating_sub(1)))
            .sum();
        println!("capture newer frames accumulated while busy: {accumulated_while_busy}");
    }
    print_stage(
        "admission wait",
        metrics.iter().map(|m| m.admission_wait_ns),
    );
    print_stage("map input", metrics.iter().map(|m| m.map_ns));
    print_stage("submit CPU", metrics.iter().map(|m| m.submit_ns));
    print_stage("HW completion", metrics.iter().map(|m| m.completion_ns));
    print_stage("lock after event", metrics.iter().map(|m| m.lock_ns));
    print_stage("bitstream copy", metrics.iter().map(|m| m.copy_ns));
    print_stage("unlock", metrics.iter().map(|m| m.unlock_ns));
    if args.send_to.is_some() {
        print_stage("network send", metrics.iter().map(|m| m.network.send_ns));
        let packets: u64 = metrics
            .iter()
            .map(|metric| u64::from(metric.network.packets))
            .sum();
        let bytes: u64 = metrics.iter().map(|metric| metric.network.bytes).sum();
        let errors: u64 = metrics.iter().map(|metric| metric.network.errors).sum();
        println!("RTP/UDP sent {packets} packets, {bytes} bytes, {errors} errors");
    }
    print_stage("RGB->NV12 CPU", metrics.iter().map(|m| m.preprocess_ns));
    let gpu_conversion: Vec<u64> = metrics.iter().filter_map(|m| m.preprocess_gpu_ns).collect();
    if !gpu_conversion.is_empty() {
        print_stage("RGB->NV12 GPU", gpu_conversion.into_iter());
    }
    print_stage("encoder residency", metrics.iter().map(|m| m.residency_ns));
    print_stage(
        "slot residency",
        metrics.iter().map(|m| m.slot_residency_ns),
    );
    print_stage(
        "P completion",
        metrics
            .iter()
            .filter(|m| !m.is_idr)
            .map(|m| m.completion_ns),
    );
    print_stage(
        "IDR completion",
        metrics.iter().filter(|m| m.is_idr).map(|m| m.completion_ns),
    );

    let p_sizes: Vec<u64> = metrics
        .iter()
        .filter(|m| !m.is_idr)
        .map(|m| m.bytes as u64)
        .collect();
    let idr_sizes: Vec<u64> = metrics
        .iter()
        .filter(|m| m.is_idr)
        .map(|m| m.bytes as u64)
        .collect();
    print_size("P frame", &p_sizes);
    print_size("IDR", &idr_sizes);
    let total_bytes: u64 = metrics.iter().map(|m| m.bytes as u64).sum();
    println!(
        "output {:.2} Mbit/s, {} bytes over {} frames",
        total_bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0,
        total_bytes,
        frames
    );
    if let (Some(first), Some(last)) = (memory.first(), memory.last()) {
        let peak = memory
            .iter()
            .map(|(_, bytes)| *bytes)
            .max()
            .unwrap_or(last.1);
        let delta = last.1 as i128 - first.1 as i128;
        println!(
            "resident memory start {:.1} MB end {:.1} MB peak {:.1} MB delta {:+.1} MB",
            first.1 as f64 / 1e6,
            last.1 as f64 / 1e6,
            peak as f64 / 1e6,
            delta as f64 / 1e6
        );
    }
    print_windows(metrics, args.window_seconds.max(1.0), args.fps);

    let period_ns = 1_000_000_000u64 / u64::from(args.fps);
    let completion = Distribution::new(metrics.iter().map(|m| m.completion_ns).collect());
    let paced_ok = args.mode != RunMode::Paced
        || (pool_exhaustions == 0 && throughput >= f64::from(args.fps) * 0.99);
    let ordered = metrics
        .windows(2)
        .all(|pair| pair[1].frame == pair[0].frame + 1);
    let network_errors: u64 = metrics.iter().map(|metric| metric.network.errors).sum();
    // Each criterion states its own verdict and its own numbers. A gate that
    // prints only FAIL sends the reader back to rerun the experiment to find
    // out which line moved.
    let checks: [(bool, &str, String); 6] = [
        (
            metrics.len() as u64 == frames,
            "frames completed",
            format!("{} of {frames}", metrics.len()),
        ),
        (
            completion.p99() <= period_ns,
            "completion within a period",
            format!(
                "p99 {:.3} ms against {:.3} ms",
                completion.p99() as f64 / 1_000_000.0,
                period_ns as f64 / 1_000_000.0
            ),
        ),
        (
            total_bytes > 0,
            "bitstream produced",
            format!("{total_bytes} bytes"),
        ),
        (ordered, "frames in order", format!("{ordered}")),
        (
            paced_ok,
            "paced rate held",
            format!(
                "{throughput:.2} of {} fps, {pool_exhaustions} pool exhaustions",
                args.fps
            ),
        ),
        (
            network_errors == 0,
            "socket accepted every datagram",
            format!("{network_errors} send errors"),
        ),
    ];
    let passed = checks.iter().all(|(ok, _, _)| *ok);
    println!();
    for (ok, name, detail) in &checks {
        println!(
            "  [{}] {name:32} {detail}",
            if *ok { "pass" } else { "FAIL" }
        );
    }
    println!("gate: {}", if passed { "PASS" } else { "FAIL" });
    if passed {
        Ok(())
    } else {
        Err("async encoder gate failed".into())
    }
}

/// The run in slices, so a stall cannot hide inside an average.
///
/// Sliced on the completion clock: the whole question a window answers is
/// whether the encoder kept up over that stretch of real time, and bucketing
/// by frame index would assume the answer.
#[cfg(windows)]
fn print_windows(metrics: &[FrameMetrics], seconds: f64, fps: u32) {
    let Some(first) = metrics.first() else {
        return;
    };
    let period_ms = 1_000.0 / f64::from(fps);
    let origin = first.completed_at;
    let width = std::time::Duration::from_secs_f64(seconds);
    println!();
    println!(
        "windows of {seconds:.0} s  (encode Hz, completion and capture p99 in ms, wire Mbit/s)"
    );
    let mut index = 0usize;
    loop {
        let from = width.mul_f64(index as f64);
        let to = from + width;
        let slice: Vec<&FrameMetrics> = metrics
            .iter()
            .filter(|metric| {
                let at = metric.completed_at.duration_since(origin);
                at >= from && at < to
            })
            .collect();
        // An empty slice mid-run is a stall, not an ending, so it is printed
        // and the table continues past it. Only running out of frames
        // altogether stops it.
        if slice.is_empty() {
            if metrics
                .iter()
                .all(|metric| metric.completed_at.duration_since(origin) < from)
            {
                break;
            }
            println!(
                "  {:>5.0}-{:<5.0} {:>7} fr {:>7.1} Hz  nothing completed in this window",
                from.as_secs_f64(),
                to.as_secs_f64(),
                0,
                0.0
            );
            index += 1;
            continue;
        }
        // The slice's own span, not the nominal width: the last one is short.
        let span = slice
            .last()
            .unwrap()
            .completed_at
            .duration_since(slice.first().unwrap().completed_at)
            .as_secs_f64()
            .max(f64::EPSILON);
        let completion = Distribution::new(slice.iter().map(|m| m.completion_ns).collect());
        let capture = Distribution::new(slice.iter().map(|m| m.source_wait_ns).collect());
        let bytes: u64 = slice.iter().map(|metric| metric.bytes as u64).sum();
        let late = slice
            .iter()
            .filter(|metric| metric.completion_ns as f64 / 1e6 > period_ms)
            .count();
        println!(
            "  {:>5.0}-{:<5.0} {:>7} fr {:>7.1} Hz  enc p99 {:>7.3}  cap p99 {:>7.3}  {:>6.2} Mbit/s  late {}",
            from.as_secs_f64(),
            to.as_secs_f64(),
            slice.len(),
            slice.len() as f64 / span,
            completion.p99() as f64 / 1e6,
            capture.p99() as f64 / 1e6,
            bytes as f64 * 8.0 / span / 1e6,
            late
        );
        index += 1;
    }
}

#[cfg(windows)]
fn print_stage(name: &str, values: impl Iterator<Item = u64>) {
    println!("{name:18} {}", Distribution::new(values.collect()));
}

#[cfg(windows)]
fn print_size(name: &str, values: &[u64]) {
    if values.is_empty() {
        println!("{name:18} none");
        return;
    }
    let distribution = Distribution::new(values.to_vec());
    println!(
        "{name:18} n={} p50 {} B p95 {} B p99 {} B max {} B",
        values.len(),
        distribution.percentile(50),
        distribution.percentile(95),
        distribution.percentile(99),
        distribution.max()
    );
}

#[cfg(windows)]
fn benchmark_surface(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    width: u32,
    height: u32,
) -> Result<Surface, String> {
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_SHARED,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
    };
    unsafe {
        let mut texture = None;
        device
            .CreateTexture2D(&desc, None, Some(&mut texture))
            .map_err(|e| format!("CreateTexture2D: {e}"))?;
        let texture = texture.ok_or_else(|| "CreateTexture2D returned null".to_owned())?;
        let mut target = None;
        device
            .CreateRenderTargetView(&texture, None, Some(&mut target))
            .map_err(|e| format!("CreateRenderTargetView: {e}"))?;
        let target = target.ok_or_else(|| "CreateRenderTargetView returned null".to_owned())?;
        Ok(Surface { texture, target })
    }
}

#[cfg(windows)]
fn clear_surface(
    context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
    target: &windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    frame: u64,
) {
    let phase = (frame % 360) as f32 / 360.0;
    let colour = [phase, 1.0 - phase, (phase * 3.0).fract(), 1.0];
    unsafe { context.ClearRenderTargetView(target, &colour) };
}

#[cfg(windows)]
fn validate_bitstream(bytes: &[u8], frame: u64) -> Result<(), String> {
    if bytes.is_empty() {
        return Err(format!("frame {frame} produced an empty bitstream"));
    }
    if !bytes.starts_with(&[0, 0, 1]) && !bytes.starts_with(&[0, 0, 0, 1]) {
        return Err(format!("frame {frame} is not Annex-B"));
    }
    Ok(())
}

#[cfg(windows)]
fn capture_update_counts(
    updates: impl IntoIterator<Item = lanplay_capture::FrameUpdate>,
) -> (u64, u64, u64) {
    updates.into_iter().fold(
        (0, 0, 0),
        |(desktop, pointer_only, other), update| match update {
            lanplay_capture::FrameUpdate::Desktop => (desktop + 1, pointer_only, other),
            lanplay_capture::FrameUpdate::PointerOnly => (desktop, pointer_only + 1, other),
            lanplay_capture::FrameUpdate::Other => (desktop, pointer_only, other + 1),
        },
    )
}

#[cfg(windows)]
struct CountDistribution {
    values: Vec<u64>,
}

#[cfg(windows)]
impl CountDistribution {
    fn new(values: impl IntoIterator<Item = u32>) -> Self {
        let mut values: Vec<_> = values.into_iter().map(u64::from).collect();
        values.sort_unstable();
        Self { values }
    }

    fn percentile(&self, percentile: usize) -> u64 {
        let index = (self.values.len() - 1) * percentile / 100;
        self.values[index]
    }
}

#[cfg(windows)]
impl std::fmt::Display for CountDistribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "p50 {} p95 {} p99 {} max {}",
            self.percentile(50),
            self.percentile(95),
            self.percentile(99),
            self.values[self.values.len() - 1],
        )
    }
}

#[cfg(windows)]
struct Distribution {
    values: Vec<u64>,
}

#[cfg(windows)]
impl Distribution {
    fn new(mut values: Vec<u64>) -> Distribution {
        values.sort_unstable();
        Distribution { values }
    }

    fn percentile(&self, percentile: usize) -> u64 {
        let index = (self.values.len() - 1) * percentile / 100;
        self.values[index]
    }

    fn p99(&self) -> u64 {
        self.percentile(99)
    }

    fn max(&self) -> u64 {
        self.values[self.values.len() - 1]
    }
}

#[cfg(windows)]
impl std::fmt::Display for Distribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "p50 {:.3} ms p95 {:.3} ms p99 {:.3} ms max {:.3} ms",
            self.percentile(50) as f64 / 1_000_000.0,
            self.percentile(95) as f64 / 1_000_000.0,
            self.percentile(99) as f64 / 1_000_000.0,
            self.max() as f64 / 1_000_000.0,
        )
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn capture_audit_separates_desktop_pointer_and_other_updates() {
        let updates = [
            lanplay_capture::FrameUpdate::Desktop,
            lanplay_capture::FrameUpdate::PointerOnly,
            lanplay_capture::FrameUpdate::Desktop,
            lanplay_capture::FrameUpdate::Other,
        ];

        assert_eq!(capture_update_counts(updates), (2, 1, 1));
    }

    #[test]
    fn accumulated_frame_distribution_reports_raw_frame_counts() {
        let distribution = CountDistribution::new([1, 1, 1, 2, 5]);

        assert_eq!(distribution.to_string(), "p50 1 p95 2 p99 2 max 5");
    }
}
