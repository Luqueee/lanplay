//! The present loop.
//!
//! Rate control is entirely in software. `SyncInterval` is 0, so DXGI never
//! blocks and never rounds the rate to a multiple of the refresh: asking for
//! 240 fps on a 120 Hz panel produces 240 presents per second, half of which
//! the display will never scan out, which is precisely the case a capture
//! backend has to be measured against. The schedule comes from
//! [`crate::pace::Pacer`], which derives each deadline from the start instant
//! rather than accumulating periods.

#![cfg(windows)]

use lanplay_protocol::FrameIdSource;
use lanplay_telemetry::{Stage, Telemetry, TelemetryConfig, Timestamp, wait_until};

use crate::Error;
use crate::gpu::Gpu;
use crate::pace::Pacer;
use crate::report::Report;
use crate::window::Window;

/// Everything the run needs from the command line.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// 0 means run until the window is closed.
    pub seconds: u64,
    pub fullscreen: bool,
    pub monitor: u32,
}

/// Presents until the window closes or `--seconds` elapses, and reports what
/// the run actually achieved.
pub fn run(options: Options) -> Result<Report, Error> {
    let gpu = Gpu::open(options.monitor)?;
    let window = Window::open(
        gpu.monitor(),
        options.width,
        options.height,
        options.fullscreen,
    )?;
    let chain = gpu.swap_chain(window.hwnd(), window.width(), window.height())?;
    let pipeline = gpu.pipeline()?;

    // Everything descriptive goes to stderr: stdout carries the report and
    // nothing else, so a capture run can consume it without parsing around a
    // banner.
    eprintln!(
        "present-source: {}x{} at {} fps on {} ({}){}",
        chain.width(),
        chain.height(),
        options.fps,
        gpu.monitor().name,
        gpu.adapter_name(),
        if options.fullscreen {
            ", borderless full screen"
        } else {
            ""
        },
    );

    let telemetry = Telemetry::start(TelemetryConfig::default());
    let recorder = telemetry.recorder();
    let frames = FrameIdSource::new();

    let pacer = Pacer::new(Timestamp::now(), options.fps);
    let final_index = pacer.last_index(options.seconds);
    let mut missed = 0u64;
    let mut index = 0u64;

    loop {
        if !window.pump() {
            break;
        }

        let deadline = pacer.deadline(index);
        // A deadline already in the past means the previous frame overran it.
        // Waiting would be a no-op, so the loop goes straight on and the
        // schedule pulls the rate back by itself; only the count is kept.
        if Timestamp::now() > deadline {
            missed += 1;
        } else {
            wait_until(deadline);
        }

        let frame = frames.next();
        // The frame's content instant is its scheduled time, not the moment
        // the loop woke: measuring against the wake would hide exactly the
        // lateness this producer has to report.
        recorder.mark_at(frame, Stage::FrameCreated, deadline);

        // The index wraps into the shader's 32-bit counter after 4.3 billion
        // frames, 207 days at 240 fps. The readout is a frame identifier, and
        // wrapping is the right behaviour for one.
        pipeline.draw(gpu.context(), &chain, index as u32);
        chain.present()?;
        recorder.mark(frame, Stage::PresentSubmit);

        if final_index.is_some_and(|last| index >= last) {
            break;
        }
        index += 1;
    }

    let snapshot = telemetry.shutdown();
    Ok(Report::from_snapshot(&snapshot, options.fps, missed))
}
