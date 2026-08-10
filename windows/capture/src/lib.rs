//! Desktop capture on Windows, behind one interface, so two APIs can be
//! compared without the comparison depending on how each was wired up.
//!
//! Phase 3 answers exactly one question: which API hands the streamer the
//! newest desktop frame with the lowest latency, the lowest jitter and the
//! smallest cost to the game. No encoder, no network, no colour conversion.
//! Everything here exists to make that question answerable and nothing here
//! should outlive the answer without being reconsidered.

pub mod backend;

#[cfg(windows)]
pub mod dda;
#[cfg(windows)]
pub mod device;
#[cfg(windows)]
pub mod display_mode;
#[cfg(windows)]
pub mod texture_pool;
#[cfg(windows)]
mod trace;
#[cfg(windows)]
pub mod wgc;

pub use backend::{
    Acquired, CaptureBackend, CaptureConfig, CaptureError, CapturedFrame, FrameMetadata, SourceMark,
};

#[cfg(windows)]
pub use dda::DesktopDuplication;
#[cfg(windows)]
pub use device::{CaptureDevice, DeviceIdentity};
#[cfg(windows)]
pub use texture_pool::{PoolHandle, TexturePool};
#[cfg(windows)]
pub use wgc::GraphicsCapture;
