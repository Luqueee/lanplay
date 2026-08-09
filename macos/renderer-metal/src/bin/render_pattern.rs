//! Drives the Metal presenter from a synthetic NV12 source, so the render path
//! can be measured before a decoder exists.
//!
//! The pattern is deliberately readable at a glance: a bar sweeping left to
//! right shows smoothness, and a binary counter along the top shows which
//! frame is on screen. If frames are being dropped, the counter skips.

use core::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Parser, ValueEnum};
use lanplay_protocol::FrameIdSource;
use lanplay_renderer_metal::{
    DriveMode, LatestFrameSlot, LiveCounters, RendererConfig, SurfaceFrame, run,
};
use lanplay_telemetry::{Recorder, Stage, Telemetry, TelemetryConfig, Timestamp};
use lanplay_video_core::PixelFormat;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFType};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferHeightKey, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelBufferPoolAllocationThresholdKey, kCVPixelBufferPoolMinimumBufferCountKey,
    kCVPixelBufferWidthKey, kCVReturnSuccess,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum Mode {
    Immediate,
    DisplayLink,
}

#[derive(Parser, Debug)]
#[command(about = "Feed a synthetic NV12 pattern through the Metal presenter")]
struct Args {
    /// Rate the source publishes frames at.
    #[arg(long, default_value_t = 120.0)]
    fps: f64,
    /// How long to publish for.
    #[arg(long, default_value_t = 10.0)]
    seconds: f64,
    #[arg(long, value_enum, default_value_t = Mode::DisplayLink)]
    mode: Mode,
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 1080)]
    height: u32,
    /// Burn this long in the renderer before presenting, to model a renderer
    /// that cannot keep up.
    #[arg(long)]
    render_delay_ms: Option<f64>,
}

fn main() {
    let args = Args::parse();

    let telemetry = Telemetry::start(TelemetryConfig::default());
    let slot = LatestFrameSlot::new();
    let stop = Arc::new(AtomicBool::new(false));

    let producer = {
        let slot = Arc::clone(&slot);
        let stop = Arc::clone(&stop);
        let recorder = telemetry.recorder();
        let (width, height, fps, seconds) = (args.width, args.height, args.fps, args.seconds);
        thread::Builder::new()
            .name("pattern-source".into())
            .spawn(move || produce(width, height, fps, seconds, slot, stop, recorder))
            .expect("spawn pattern source")
    };

    let config = RendererConfig {
        width: args.width,
        height: args.height,
        title: format!("lanplay pattern {}x{}", args.width, args.height),
        mode: match args.mode {
            Mode::Immediate => DriveMode::Immediate,
            Mode::DisplayLink => DriveMode::DisplayLink,
        },
        recorder: telemetry.recorder(),
        stop: Arc::clone(&stop),
        render_delay: args
            .render_delay_ms
            .map(|ms| Duration::from_secs_f64(ms / 1_000.0)),
        present_limit: None,
        counters: LiveCounters::new(),
        require_clean_environment: Some(120.0),
        on_ready: None,
    };

    let stats = run(config, slot);
    stop.store(true, Ordering::Relaxed);
    let produced = producer.join().expect("pattern source panicked");

    match stats {
        Ok(stats) => {
            println!("\n{produced}");
            println!("{stats}");
        }
        Err(error) => {
            eprintln!("renderer failed: {error}");
            std::process::exit(1);
        }
    }

    // Marks are queued, so the collector needs a moment to catch up before the
    // percentiles mean anything.
    telemetry.flush(Duration::from_secs(2));
    println!("\n{}", telemetry.shutdown());
}

struct Produced {
    published: u64,
    /// Frames skipped because the pool had hit its allocation ceiling. A
    /// non-zero count means surfaces are being held somewhere they should not
    /// be, which is exactly the leak the texture-cache flush prevents.
    starved: u64,
    elapsed: Duration,
}

impl core::fmt::Display for Produced {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "source: published {} frames in {:.2} s ({:.1} fps), {} pool starvations",
            self.published,
            self.elapsed.as_secs_f64(),
            self.published as f64 / self.elapsed.as_secs_f64(),
            self.starved
        )
    }
}

fn produce(
    width: u32,
    height: u32,
    fps: f64,
    seconds: f64,
    slot: Arc<LatestFrameSlot>,
    stop: Arc<AtomicBool>,
    recorder: Recorder,
) -> Produced {
    let pool = create_pool(width, height);
    let ids = FrameIdSource::new();
    let start = Instant::now();
    let mut index = 0u64;
    let mut published = 0u64;
    let mut starved = 0u64;

    while !stop.load(Ordering::Relaxed) {
        // Pace against the start, not the previous frame, so a slow tick does
        // not shift every later one.
        let due = Duration::from_secs_f64(index as f64 / fps);
        if due.as_secs_f64() >= seconds {
            break;
        }
        if let Some(wait) = due.checked_sub(start.elapsed()) {
            thread::sleep(wait);
        }
        index += 1;

        let Some(buffer) = pool_frame(&pool) else {
            starved += 1;
            continue;
        };
        let id = ids.next();
        recorder.mark(id, Stage::FrameCreated);
        draw_pattern(&buffer, index);
        recorder.mark(id, Stage::DecodeComplete);
        slot.publish(SurfaceFrame {
            id,
            pixel_buffer: buffer,
            decoded_at: Timestamp::now(),
        });
        published += 1;
    }

    // The renderer stops when the source does, not a moment later.
    stop.store(true, Ordering::Relaxed);
    Produced {
        published,
        starved,
        elapsed: start.elapsed(),
    }
}

/// A pool rather than a fresh `CVPixelBufferCreate` per frame: allocating an
/// IOSurface takes tens of microseconds and would show up in every number this
/// binary prints.
fn create_pool(width: u32, height: u32) -> CFRetained<CVPixelBufferPool> {
    let format = CFNumber::new_i32(PixelFormat::Nv12VideoRange.four_cc() as i32);
    let width = CFNumber::new_i32(width as i32);
    let height = CFNumber::new_i32(height as i32);
    let surface_properties = CFDictionary::<CFType, CFType>::empty();
    // SAFETY: reading framework constants.
    let (key_format, key_width, key_height, key_surface, key_metal) = unsafe {
        (
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferWidthKey,
            kCVPixelBufferHeightKey,
            kCVPixelBufferIOSurfacePropertiesKey,
            kCVPixelBufferMetalCompatibilityKey,
        )
    };
    // An IOSurface is what makes the buffer shareable with Metal without a
    // copy; without both of these keys the texture cache refuses the buffer.
    let buffer_attributes = CFDictionary::from_slices(
        &[
            &**key_format,
            &**key_width,
            &**key_height,
            &**key_surface,
            &**key_metal,
        ],
        &[
            &**format,
            &**width,
            &**height,
            &**surface_properties,
            &**CFBoolean::new(true),
        ],
    );

    let minimum = CFNumber::new_i32(4);
    // SAFETY: reading a framework constant.
    let key_minimum = unsafe { kCVPixelBufferPoolMinimumBufferCountKey };
    let pool_attributes = CFDictionary::from_slices(&[&**key_minimum], &[&**minimum]);

    let mut raw: *mut CVPixelBufferPool = core::ptr::null_mut();
    // SAFETY: `raw` is a live local and both dictionaries hold the CoreVideo
    // key and value types the pool expects.
    let status = unsafe {
        CVPixelBufferPool::create(
            None,
            Some(untyped(&pool_attributes)),
            Some(untyped(&buffer_attributes)),
            NonNull::from(&mut raw),
        )
    };
    let pool = NonNull::new(raw)
        .filter(|_| status == kCVReturnSuccess)
        .unwrap_or_else(|| panic!("CVPixelBufferPoolCreate failed with {status}"));
    // SAFETY: the create rule; we own the only reference.
    unsafe { CFRetained::from_raw(pool) }
}

/// Ceiling on live buffers. Chosen above the three the renderer keeps in
/// flight plus the one in the slot, so hitting it means something is leaking
/// surfaces rather than that the demo is merely busy.
const POOL_CEILING: i32 = 8;

fn pool_frame(pool: &CVPixelBufferPool) -> Option<CFRetained<CVPixelBuffer>> {
    let ceiling = CFNumber::new_i32(POOL_CEILING);
    // SAFETY: reading a framework constant.
    let key_ceiling = unsafe { kCVPixelBufferPoolAllocationThresholdKey };
    let aux = CFDictionary::from_slices(&[&**key_ceiling], &[&**ceiling]);

    let mut raw: *mut CVPixelBuffer = core::ptr::null_mut();
    // SAFETY: `raw` is a live local and `aux` holds the documented key type.
    let status = unsafe {
        CVPixelBufferPool::create_pixel_buffer_with_aux_attributes(
            None,
            pool,
            Some(untyped(&aux)),
            NonNull::from(&mut raw),
        )
    };
    let buffer = NonNull::new(raw).filter(|_| status == kCVReturnSuccess)?;
    // SAFETY: the create rule; we own the only reference.
    Some(unsafe { CFRetained::from_raw(buffer) })
}

/// CoreVideo's signatures take the unparameterised `CFDictionary`, while the
/// safe constructors hand back a typed one.
fn untyped(dictionary: &CFDictionary<CFType, CFType>) -> &CFDictionary {
    dictionary.as_ref()
}

const LUMA_BACKGROUND: u8 = 32;
const LUMA_BAR: u8 = 200;
const LUMA_BIT_SET: u8 = 235;
const LUMA_BIT_CLEAR: u8 = 16;
/// Bar travel per frame, in pixels. At 120 fps this crosses 1920 px in about
/// two seconds, fast enough that a dropped frame is visible as a jump.
const BAR_SPEED: usize = 8;
const BAR_WIDTH: usize = 96;
/// Bits of the frame index drawn along the top edge.
const COUNTER_BITS: usize = 16;

/// Fills `buffer` with the frame's picture.
///
/// Locking the base address is fine here and nowhere else: this function is
/// the source, standing in for a decoder writing into its own pool. The render
/// path never locks a buffer, never reads one on the CPU and never copies a
/// plane; that is the property the whole crate exists to demonstrate.
fn draw_pattern(buffer: &CVPixelBuffer, index: u64) {
    let write = CVPixelBufferLockFlags(0);
    // SAFETY: the buffer is owned solely by this thread until it is published.
    let status = unsafe { CVPixelBufferLockBaseAddress(buffer, write) };
    assert_eq!(status, kCVReturnSuccess, "locking a pool buffer for write");

    let luma_width = CVPixelBufferGetWidthOfPlane(buffer, 0);
    let luma_height = CVPixelBufferGetHeightOfPlane(buffer, 0);
    let bar_start = (index as usize * BAR_SPEED) % luma_width;

    // SAFETY: the buffer is locked, so the plane pointers are valid for
    // `bytes_per_row * height` and nothing else touches them meanwhile.
    unsafe {
        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
        let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0).cast::<u8>();
        let plane = core::slice::from_raw_parts_mut(base, stride * luma_height);
        for y in 0..luma_height {
            let row = &mut plane[y * stride..y * stride + luma_width];
            row.fill(LUMA_BACKGROUND);
            for offset in 0..BAR_WIDTH {
                row[(bar_start + offset) % luma_width] = LUMA_BAR;
            }
        }
        draw_counter(plane, stride, luma_width, luma_height, index);

        let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1);
        let width = CVPixelBufferGetWidthOfPlane(buffer, 1);
        let height = CVPixelBufferGetHeightOfPlane(buffer, 1);
        let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 1).cast::<u8>();
        let plane = core::slice::from_raw_parts_mut(base, stride * height);
        let bar_start = bar_start / 2;
        let bar_width = BAR_WIDTH / 2;
        for y in 0..height {
            let row = &mut plane[y * stride..y * stride + width * 2];
            // Neutral chroma everywhere, so the background is grey and any
            // colour fringing on the bar edge is a conversion bug, not the
            // source.
            row.fill(128);
            for offset in 0..bar_width {
                let x = (bar_start + offset) % width;
                // Cb high, Cr low: a saturated blue bar, which puts both
                // chroma channels far from neutral.
                row[x * 2] = 200;
                row[x * 2 + 1] = 70;
            }
        }
    }

    // SAFETY: paired with the lock above.
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, write) };
}

/// Draws the low [`COUNTER_BITS`] of `index` as blocks along the top edge, most
/// significant bit on the left.
fn draw_counter(plane: &mut [u8], stride: usize, width: usize, height: usize, index: u64) {
    let block_width = width / (COUNTER_BITS * 2);
    let block_height = (height / 12).max(1);
    for bit in 0..COUNTER_BITS {
        let set = index >> (COUNTER_BITS - 1 - bit) & 1 == 1;
        let level = if set { LUMA_BIT_SET } else { LUMA_BIT_CLEAR };
        let x0 = bit * block_width * 2;
        for y in 0..block_height {
            let row = &mut plane[y * stride + x0..y * stride + x0 + block_width];
            row.fill(level);
        }
    }
}
