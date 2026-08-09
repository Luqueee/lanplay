//! Puts a decoded NV12 picture on a macOS display without ever touching a
//! pixel on the CPU.
//!
//! The path is: an IOSurface-backed `CVPixelBuffer` arrives from the decoder,
//! its two planes are aliased as Metal textures through a
//! `CVMetalTextureCache`, and a fragment shader converts BT.709 video-range
//! YUV into the drawable of a `CAMetalLayer`. No plane is ever locked, copied,
//! or staged; the only bytes that move are the command buffer's.
//!
//! Between decoder and renderer sits a [`LatestFrameSlot`] rather than a
//! queue. That single decision is the project's latency policy: when the
//! producer runs ahead, frames are dropped, not buffered, because a frame
//! shown one interval late is worth less than the one behind it.
//!
//! ```no_run
//! use std::sync::Arc;
//! use std::sync::atomic::AtomicBool;
//!
//! use lanplay_renderer_metal::{DriveMode, LatestFrameSlot, RendererConfig, run};
//! use lanplay_telemetry::{Telemetry, TelemetryConfig};
//!
//! let telemetry = Telemetry::start(TelemetryConfig::default());
//! let slot = LatestFrameSlot::new();
//! // ... a decoder thread publishes `SurfaceFrame`s into `slot` ...
//! let stats = run(
//!     RendererConfig {
//!         width: 1920,
//!         height: 1080,
//!         title: "lanplay".into(),
//!         mode: DriveMode::DisplayLink,
//!         recorder: telemetry.recorder(),
//!         stop: Arc::new(AtomicBool::new(false)),
//!         render_delay: None,
//!         present_limit: None,
//!     },
//!     slot,
//! )
//! .unwrap();
//! println!("{stats}");
//! ```

mod error;
mod gpu;
mod run;
mod shader;
mod slot;
mod stats;
mod window;

pub use error::RendererError;
pub use run::{DriveMode, RenderStats, RendererConfig, run};
pub use slot::{LatestFrameSlot, SurfaceFrame};
pub use stats::Percentiles;
