//! Isolated D3D11-to-NVENC capability and throughput benchmark.

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
    /// Frames discarded before measurement.
    #[arg(long, default_value_t = 120)]
    warmup: u64,
    #[arg(long, default_value_t = 1200)]
    frames: u64,
}

#[cfg(windows)]
fn windows_main() -> Result<(), String> {
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
    let bitrate = args
        .bitrate_mbps
        .checked_mul(1_000_000)
        .ok_or_else(|| "bitrate overflows u32".to_owned())?;

    let device = lanplay_capture::CaptureDevice::open(0).map_err(|e| e.to_string())?;
    println!("device {}", device.identity());
    let (texture, target) = benchmark_surface(device.device(), args.width, args.height)?;

    let mut session =
        lanplay_encoder_nvenc::NvencSession::open(device.device()).map_err(|e| e.to_string())?;
    let codecs = session.encode_guids().map_err(|e| e.to_string())?;
    let h264 = lanplay_encoder_nvenc::NvencSession::h264_codec_guid();
    if !codecs.contains(&h264) {
        return Err("the selected NVENC session does not expose H.264".into());
    }
    let formats = session.input_formats(h264).map_err(|e| e.to_string())?;
    println!("H.264 input formats: {formats:?}");
    session
        .initialize_h264(args.width, args.height, args.fps, 1, bitrate)
        .map_err(|e| e.to_string())?;
    let input = session.register_bgra(&texture).map_err(|e| e.to_string())?;
    let output = session
        .create_bitstream_buffer()
        .map_err(|e| e.to_string())?;

    for frame in 0..args.warmup {
        clear_surface(device.context(), &target, frame);
        let encoded = session
            .encode_bgra(&input, &output, frame, frame == 0)
            .map_err(|e| format!("warm-up frame {frame}: {e}"))?;
        validate_bitstream(&encoded.data, frame)?;
    }

    let mut submit_ns = Vec::with_capacity(args.frames as usize);
    let mut complete_ns = Vec::with_capacity(args.frames as usize);
    let mut total_bytes = 0u64;
    let mut max_bytes = 0usize;
    let started = Instant::now();
    for offset in 0..args.frames {
        let frame = args.warmup + offset;
        clear_surface(device.context(), &target, frame);
        let before_submit = Instant::now();
        let submitted = session
            .submit_bgra(&input, &output, frame, offset == 0)
            .map_err(|e| format!("submit frame {frame}: {e}"))?;
        let after_submit = Instant::now();
        let encoded = session
            .lock_bitstream(&output, submitted)
            .map_err(|e| format!("complete frame {frame}: {e}"))?;
        let completed = Instant::now();
        validate_bitstream(&encoded.data, frame)?;
        submit_ns.push((after_submit - before_submit).as_nanos() as u64);
        complete_ns.push((completed - after_submit).as_nanos() as u64);
        total_bytes = total_bytes.saturating_add(encoded.data.len() as u64);
        max_bytes = max_bytes.max(encoded.data.len());
    }
    let elapsed = started.elapsed();
    let throughput = args.frames as f64 / elapsed.as_secs_f64();
    let total_ns = submit_ns
        .iter()
        .zip(&complete_ns)
        .map(|(submit, complete)| submit.saturating_add(*complete))
        .collect();
    let submit = Distribution::new(submit_ns);
    let complete = Distribution::new(complete_ns);
    let total = Distribution::new(total_ns);

    println!(
        "config {}x{} @ {} fps, {} Mbps, P1 ULL CBR, BGRA direct",
        args.width, args.height, args.fps, args.bitrate_mbps
    );
    println!(
        "frames {} in {:.3} s = {:.2} frames/s, {:.2} Mbit/s output, max frame {} bytes",
        args.frames,
        elapsed.as_secs_f64(),
        throughput,
        total_bytes as f64 * 8.0 / elapsed.as_secs_f64() / 1_000_000.0,
        max_bytes
    );
    println!("submit       {}", submit);
    println!("complete+copy {}", complete);
    println!("total         {}", total);

    let period_ns = 1_000_000_000u64 / u64::from(args.fps);
    let passed = throughput >= f64::from(args.fps) && total.p99() <= period_ns && total_bytes > 0;
    println!(
        "gate: {} (throughput {:.2}/{}, total p99 {:.3}/{:.3} ms)",
        if passed { "PASS" } else { "FAIL" },
        throughput,
        args.fps,
        total.p99() as f64 / 1_000_000.0,
        period_ns as f64 / 1_000_000.0
    );
    if passed {
        Ok(())
    } else {
        Err("encoder cannot sustain the requested frame rate without serialized backlog".into())
    }
}

#[cfg(windows)]
fn benchmark_surface(
    device: &windows::Win32::Graphics::Direct3D11::ID3D11Device,
    width: u32,
    height: u32,
) -> Result<
    (
        windows::Win32::Graphics::Direct3D11::ID3D11Texture2D,
        windows::Win32::Graphics::Direct3D11::ID3D11RenderTargetView,
    ),
    String,
> {
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
        Ok((texture, target))
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
            self.values[self.values.len() - 1] as f64 / 1_000_000.0,
        )
    }
}
