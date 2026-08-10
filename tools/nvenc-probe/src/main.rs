//! Isolated D3D11-to-NVENC latency, throughput, and cadence benchmark.

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
enum ContentSource {
    /// One frame-wide colour, changed deterministically every frame.
    Flat,
    /// GPU-generated motion, high-frequency detail, and frame-wide scene changes.
    Pattern,
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
    #[arg(long, value_enum, default_value_t = RunMode::Uncapped)]
    mode: RunMode,
    #[arg(long, value_enum, default_value_t = ContentSource::Flat)]
    source: ContentSource,
    /// Overrides `--frames`; intended for paced soak runs.
    #[arg(long)]
    seconds: Option<u64>,
    /// Frames discarded before measurement.
    #[arg(long, default_value_t = 120)]
    warmup: u64,
    #[arg(long, default_value_t = 1200)]
    frames: u64,
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
enum Content {
    Flat,
    Pattern(lanplay_present_source::gpu::Pipeline),
}

#[cfg(windows)]
impl Content {
    fn new(
        source: ContentSource,
        device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    ) -> Result<Self, String> {
        match source {
            ContentSource::Flat => Ok(Self::Flat),
            ContentSource::Pattern => lanplay_present_source::gpu::Pipeline::new(device)
                .map(Self::Pattern)
                .map_err(|error| error.to_string()),
        }
    }

    fn draw(
        &self,
        context: &windows::Win32::Graphics::Direct3D11::ID3D11DeviceContext,
        surface: &Surface,
        width: u32,
        height: u32,
        frame: u64,
    ) {
        match self {
            Self::Flat => clear_surface(context, &surface.target, frame),
            Self::Pattern(pipeline) => {
                pipeline.draw_target(context, &surface.target, width, height, frame as u32);
            }
        }
    }
}

#[cfg(windows)]
struct Work<'a> {
    slot: usize,
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
    is_idr: bool,
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

    let device = lanplay_capture::CaptureDevice::open(0).map_err(|e| e.to_string())?;
    println!("device {}", device.identity());
    let mut surfaces = Vec::with_capacity(SLOT_COUNT);
    for _ in 0..SLOT_COUNT {
        surfaces.push(benchmark_surface(device.device(), args.width, args.height)?);
    }
    let content = Content::new(args.source, device.device())?;

    let mut session =
        lanplay_encoder_nvenc::NvencSession::open(device.device()).map_err(|e| e.to_string())?;
    let codecs = session.encode_guids().map_err(|e| e.to_string())?;
    let h264 = lanplay_encoder_nvenc::NvencSession::h264_codec_guid();
    if !codecs.contains(&h264) {
        return Err("the selected NVENC session does not expose H.264".into());
    }
    if !session.supports_async().map_err(|e| e.to_string())? {
        return Err("the selected NVENC session does not support async encoding".into());
    }
    let formats = session.input_formats(h264).map_err(|e| e.to_string())?;
    println!("H.264 input formats: {formats:?}");
    session
        .initialize_h264(args.width, args.height, args.fps, 1, bitrate, true)
        .map_err(|e| e.to_string())?;

    let mut inputs = Vec::with_capacity(SLOT_COUNT);
    let mut outputs = Vec::with_capacity(SLOT_COUNT);
    for surface in &surfaces {
        inputs.push(
            session
                .register_bgra(&surface.texture)
                .map_err(|e| e.to_string())?,
        );
        outputs.push(session.create_output_buffer().map_err(|e| e.to_string())?);
    }

    for frame in 0..args.warmup {
        let slot = frame as usize % SLOT_COUNT;
        content.draw(
            device.context(),
            &surfaces[slot],
            args.width,
            args.height,
            frame,
        );
        let encoded = session
            .encode_bgra(&inputs[slot], &outputs[slot], frame, frame == 0)
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

    let run = run_pipeline(
        &session,
        device.context(),
        &surfaces,
        &content,
        &inputs,
        &outputs,
        args.width,
        args.height,
        args.mode,
        args.fps,
        args.warmup,
        frames,
        args.idr_interval,
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
    content: &Content,
    inputs: &'a [lanplay_encoder_nvenc::RegisteredInput<'a>],
    outputs: &'a [lanplay_encoder_nvenc::OutputBuffer<'a>],
    width: u32,
    height: u32,
    mode: RunMode,
    fps: u32,
    first_frame: u64,
    frames: u64,
    idr_interval: u64,
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

    let outcome = std::thread::scope(|scope| -> Result<Vec<FrameMetrics>, String> {
        let completion_failed = Arc::clone(&failed);
        let completion = scope.spawn(move || {
            while let Ok(work) = work_rx.recv() {
                let slot = work.slot;
                let result = if completion_failed.load(Ordering::Acquire) {
                    Err(format!(
                        "frame {} aborted after prior failure",
                        work.submitted.frame_index()
                    ))
                } else {
                    complete_work(session, work)
                };
                if result.is_err() {
                    completion_failed.store(true, Ordering::Release);
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
                wait_until(started + period.mul_f64(offset as f64));
            }
            let wait_start = Instant::now();
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
            content.draw(context, &surfaces[slot], width, height, frame);

            let map_start = Instant::now();
            let mapped = session
                .map_input(&inputs[slot])
                .map_err(|e| format!("map frame {frame}: {e}"))?;
            let map_end = Instant::now();
            let force_idr = offset == 0 || (idr_interval != 0 && offset % idr_interval == 0);
            let submit_start = Instant::now();
            let submitted = session
                .encode_submit(mapped, &outputs[slot], frame, force_idr)
                .map_err(|e| format!("submit frame {frame}: {e}"))?;
            let submit_end = Instant::now();
            work_tx
                .send(Work {
                    slot,
                    submitted,
                    admitted,
                    admission_wait_ns,
                    map_ns: (map_end - map_start).as_nanos() as u64,
                    submit_ns: (submit_end - submit_start).as_nanos() as u64,
                    submit_end,
                })
                .map_err(|_| "completion thread stopped accepting work".to_owned())?;
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
    outcome.map(|metrics| (metrics, elapsed, pool_exhaustions))
}

#[cfg(windows)]
fn complete_work(
    session: &lanplay_encoder_nvenc::NvencSession,
    work: Work<'_>,
) -> Result<FrameMetrics, String> {
    let frame = work.submitted.frame_index();
    let is_idr = work.submitted.is_idr();
    session
        .wait_completion(&work.submitted)
        .map_err(|e| format!("completion event frame {frame}: {e}"))?;
    let completed = std::time::Instant::now();
    let lock_start = std::time::Instant::now();
    let locked = session
        .lock_bitstream(work.submitted)
        .map_err(|e| format!("lock frame {frame}: {e}"))?;
    let lock_return = std::time::Instant::now();
    validate_bitstream(locked.bytes(), frame)?;
    let bytes = locked.bytes().to_vec();
    let copy_end = std::time::Instant::now();
    let unlock_start = std::time::Instant::now();
    locked
        .unlock()
        .map_err(|e| format!("unlock frame {frame}: {e}"))?;
    let unlock_end = std::time::Instant::now();
    Ok(FrameMetrics {
        frame,
        is_idr,
        bytes: bytes.len(),
        admission_wait_ns: work.admission_wait_ns,
        map_ns: work.map_ns,
        submit_ns: work.submit_ns,
        completion_ns: (completed - work.submit_end).as_nanos() as u64,
        lock_ns: (lock_return - lock_start).as_nanos() as u64,
        copy_ns: (copy_end - lock_return).as_nanos() as u64,
        unlock_ns: (unlock_end - unlock_start).as_nanos() as u64,
        residency_ns: (copy_end - work.submit_end).as_nanos() as u64,
        slot_residency_ns: (unlock_end - work.admitted).as_nanos() as u64,
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
        "config {}x{} @ {} fps, {} Mbps, P1 ULL CBR, BGRA direct, {:?} source, IDR interval {}, async {} slots, {:?}",
        args.width,
        args.height,
        args.fps,
        args.bitrate_mbps,
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

    let period_ns = 1_000_000_000u64 / u64::from(args.fps);
    let completion = Distribution::new(metrics.iter().map(|m| m.completion_ns).collect());
    let paced_ok = args.mode != RunMode::Paced
        || (pool_exhaustions == 0 && throughput >= f64::from(args.fps) * 0.99);
    let ordered = metrics
        .windows(2)
        .all(|pair| pair[1].frame == pair[0].frame + 1);
    let passed = metrics.len() as u64 == frames
        && completion.p99() <= period_ns
        && total_bytes > 0
        && ordered
        && paced_ok;
    println!(
        "gate: {} (completed {}/{}, ordered {}, completion p99 {:.3}/{:.3} ms, pool exhausted {})",
        if passed { "PASS" } else { "FAIL" },
        metrics.len(),
        frames,
        ordered,
        completion.p99() as f64 / 1_000_000.0,
        period_ns as f64 / 1_000_000.0,
        pool_exhaustions
    );
    if passed {
        Ok(())
    } else {
        Err("async encoder gate failed".into())
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
