use serde::{Deserialize, Serialize};

use crate::video::{VideoCodec, VideoMode};

/// A display attached to either machine.
///
/// `current` is always in physical pixels. `scale_factor` is only reported
/// where the OS actually exposes a backing-scale (macOS); it is `None`
/// elsewhere rather than a fabricated 1.0.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct DisplayInfo {
    /// Platform identifier: a `CGDirectDisplayID` on macOS, a device name such
    /// as `\\.\DISPLAY1` on Windows.
    pub id: String,
    /// Best-effort human label.
    pub name: String,
    pub primary: bool,
    /// Current mode, in physical pixels.
    pub current: VideoMode,
    /// Physical pixels per logical point, when the OS reports it.
    pub scale_factor: Option<f32>,
    /// Every refresh rate the display advertises at its current pixel size.
    pub available_refresh_mhz: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other(u32),
}

impl GpuVendor {
    /// Maps a PCI vendor id, as found in a Windows adapter `DeviceID`.
    pub const fn from_pci_id(id: u32) -> Self {
        match id {
            0x10DE => GpuVendor::Nvidia,
            0x1002 | 0x1022 => GpuVendor::Amd,
            0x8086 => GpuVendor::Intel,
            0x106B => GpuVendor::Apple,
            other => GpuVendor::Other(other),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GpuInfo {
    pub name: String,
    pub vendor: GpuVendor,
}

/// What the installed NVIDIA driver's `nvEncodeAPI64.dll` reports.
///
/// Presence of the library plus a supported API version is all that can be
/// learned without opening an encode session; concrete codec, preset and
/// rate-control support is queried in the encoder crate.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct NvencInfo {
    pub api_major: u32,
    pub api_minor: u32,
}

/// What the streaming host (Windows) can do.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct HostCapabilities {
    pub gpus: Vec<GpuInfo>,
    pub displays: Vec<DisplayInfo>,
    pub nvenc: Option<NvencInfo>,
}

/// What the client (macOS) can do.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    pub displays: Vec<DisplayInfo>,
    /// Codecs for which the OS confirms a hardware decoder exists.
    pub hardware_decode: Vec<VideoCodec>,
}
