//! What this machine can actually do, asked of the OS rather than assumed.
//!
//! Two roles, two sets of questions:
//!
//! * host (Windows): which GPUs exist, is there an NVENC-capable driver, what
//!   displays are attached;
//! * client (macOS): which displays exist and at what refresh, which codecs
//!   have a hardware decoder.
//!
//! Each probe is implemented only where it is meaningful. A platform that
//! cannot answer returns nothing rather than a plausible-looking default.

use lanplay_protocol::{ClientCapabilities, HostCapabilities};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use lanplay_protocol::{DisplayInfo, GpuInfo, NvencInfo, VideoCodec};

    pub fn displays() -> Vec<DisplayInfo> {
        Vec::new()
    }
    pub fn gpus() -> Vec<GpuInfo> {
        Vec::new()
    }
    pub fn nvenc() -> Option<NvencInfo> {
        None
    }
    pub fn hardware_decode() -> Vec<VideoCodec> {
        Vec::new()
    }
}

/// Probes the streaming-host role. GPU and NVENC discovery is Windows-only.
pub fn host() -> HostCapabilities {
    HostCapabilities {
        gpus: platform::gpus(),
        displays: platform::displays(),
        nvenc: platform::nvenc(),
    }
}

/// Probes the client role. Hardware-decode discovery is macOS-only.
pub fn client() -> ClientCapabilities {
    ClientCapabilities {
        displays: platform::displays(),
        hardware_decode: platform::hardware_decode(),
    }
}

/// True when this build can fill in host-side GPU and encoder capabilities.
pub const fn host_probes_supported() -> bool {
    cfg!(windows)
}

/// True when this build can fill in client-side decoder capabilities.
pub const fn client_probes_supported() -> bool {
    cfg!(target_os = "macos")
}
