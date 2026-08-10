//! The two concrete backends, behind one type.
//!
//! An enum rather than `Box<dyn CaptureBackend>` for one reason: each backend
//! reports things about itself that the trait cannot express, because they are
//! about that API and not about capture. WGC knows how many frames its pool
//! discarded; Desktop Duplication knows which of the two duplication entry
//! points it got. Losing those to a trait object would mean losing exactly the
//! evidence the comparison is for.
//!
//! Everything else about the loop is written against the trait, so this file
//! is the only place the two APIs are named.

#![cfg(windows)]

use lanplay_capture::dda::{DdaApi, DesktopDuplication};
use lanplay_capture::wgc::GraphicsCapture;
use lanplay_capture::{Acquired, CaptureBackend, CaptureConfig, CaptureDevice, CaptureError};

use crate::schedule::BackendKind;

pub enum Capture {
    Wgc(GraphicsCapture),
    Dda(DesktopDuplication),
}

pub fn open(
    kind: BackendKind,
    device: &CaptureDevice,
    dda_api: DdaApi,
) -> Result<Capture, CaptureError> {
    Ok(match kind {
        BackendKind::Wgc => Capture::Wgc(GraphicsCapture::new(device)?),
        BackendKind::Dda => Capture::Dda(DesktopDuplication::new_with_api(device, dda_api)?),
    })
}

impl CaptureBackend for Capture {
    fn name(&self) -> &'static str {
        match self {
            Capture::Wgc(backend) => backend.name(),
            Capture::Dda(backend) => backend.name(),
        }
    }

    fn start(&mut self, config: CaptureConfig) -> Result<(), CaptureError> {
        match self {
            Capture::Wgc(backend) => backend.start(config),
            Capture::Dda(backend) => backend.start(config),
        }
    }

    fn acquire(&mut self) -> Result<Acquired<'_>, CaptureError> {
        match self {
            Capture::Wgc(backend) => backend.acquire(),
            Capture::Dda(backend) => backend.acquire(),
        }
    }

    fn stop(&mut self) {
        match self {
            Capture::Wgc(backend) => backend.stop(),
            Capture::Dda(backend) => backend.stop(),
        }
    }

    fn restart(&mut self) -> Result<(), CaptureError> {
        match self {
            Capture::Wgc(backend) => backend.restart(),
            Capture::Dda(backend) => backend.restart(),
        }
    }
}

/// What each API says about itself that the trait has no room for.
///
/// One struct for both, with the fields the other backend does not report left
/// at zero, so the report code has no branch in it. Which fields are
/// meaningful is decided by which backend ran, and the report says which.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras {
    pub superseded: u64,
    pub delivered: u64,
    pub drained_total: u64,
    /// WGC only: every `FrameArrived` raised, taken or not. Compared with
    /// `delivered` it separates a consumer that missed frames from a pool that
    /// never offered them.
    pub signals: u64,
    pub pool_recreations: u64,
    pub access_lost: u64,
    pub accumulated_over_one: u64,
    /// WGC only. `false` means the OS forced the capture border into the
    /// frames, which is content and would pollute a pixel comparison.
    pub border_suppressed: Option<bool>,
    /// Desktop Duplication only: which duplication entry point was available.
    pub api: &'static str,
}

impl Extras {
    /// What accrued since `baseline`. Saturating, because a backend that was
    /// torn down and rebuilt restarts its counters from zero.
    pub fn since(self, baseline: Extras) -> Extras {
        Extras {
            superseded: self.superseded.saturating_sub(baseline.superseded),
            delivered: self.delivered.saturating_sub(baseline.delivered),
            drained_total: self.drained_total.saturating_sub(baseline.drained_total),
            signals: self.signals.saturating_sub(baseline.signals),
            pool_recreations: self
                .pool_recreations
                .saturating_sub(baseline.pool_recreations),
            access_lost: self.access_lost.saturating_sub(baseline.access_lost),
            accumulated_over_one: self
                .accumulated_over_one
                .saturating_sub(baseline.accumulated_over_one),
            border_suppressed: self.border_suppressed,
            api: self.api,
        }
    }

    /// Adds another backend instance's contribution. Used by `compare`, where
    /// each block builds and destroys its own backend.
    pub fn plus(self, other: Extras) -> Extras {
        Extras {
            superseded: self.superseded + other.superseded,
            delivered: self.delivered + other.delivered,
            drained_total: self.drained_total + other.drained_total,
            signals: self.signals + other.signals,
            pool_recreations: self.pool_recreations + other.pool_recreations,
            access_lost: self.access_lost + other.access_lost,
            accumulated_over_one: self.accumulated_over_one + other.accumulated_over_one,
            border_suppressed: other.border_suppressed.or(self.border_suppressed),
            api: if other.api.is_empty() {
                self.api
            } else {
                other.api
            },
        }
    }
}

impl Capture {
    pub fn extras(&self) -> Extras {
        match self {
            Capture::Wgc(backend) => Extras {
                superseded: backend.superseded(),
                delivered: backend.delivered(),
                drained_total: backend.drained_total(),
                signals: backend.signals(),
                pool_recreations: backend.pool_recreations(),
                border_suppressed: Some(backend.border_suppressed()),
                api: "Direct3D11CaptureFramePool::CreateFreeThreaded",
                ..Extras::default()
            },
            Capture::Dda(backend) => Extras {
                access_lost: backend.access_lost(),
                accumulated_over_one: backend.accumulated_over_one(),
                api: backend.api(),
                ..Extras::default()
            },
        }
    }
}
