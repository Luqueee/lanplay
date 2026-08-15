//! Win32 probes for the host role.
//!
//! Deliberately COM-free: adapter, monitor and mode discovery come from
//! `user32`, and NVENC availability is answered the way NVIDIA's own samples
//! answer it, by loading `nvEncodeAPI64.dll` and asking the driver which API
//! version it supports. Nothing here needs a D3D device, so it stays cheap
//! enough to run during startup or a capability handshake.

use core::ffi::{c_char, c_void};
use core::mem;

use lanplay_protocol::{DisplayInfo, GpuInfo, GpuVendor, NvencInfo, VideoCodec, VideoMode};

const CCHDEVICENAME: usize = 32;
const CCHFORMNAME: usize = 32;

const ENUM_CURRENT_SETTINGS: u32 = u32::MAX;
const DISPLAY_DEVICE_ATTACHED_TO_DESKTOP: u32 = 0x0000_0001;
const DISPLAY_DEVICE_PRIMARY_DEVICE: u32 = 0x0000_0004;
const DISPLAY_DEVICE_MIRRORING_DRIVER: u32 = 0x0000_0008;

#[repr(C)]
#[derive(Clone, Copy)]
struct DisplayDeviceW {
    cb: u32,
    device_name: [u16; CCHDEVICENAME],
    device_string: [u16; 128],
    state_flags: u32,
    device_id: [u16; 128],
    device_key: [u16; 128],
}

impl DisplayDeviceW {
    fn new() -> Self {
        // SAFETY: every field is a plain integer or integer array, so all-zero
        // is a valid value; `cb` is then set as the API requires.
        let mut device: DisplayDeviceW = unsafe { mem::zeroed() };
        device.cb = mem::size_of::<DisplayDeviceW>() as u32;
        device
    }
}

/// `DEVMODEW`. The two anonymous unions are represented by byte arrays of the
/// right size and alignment; only the display fields are read.
#[repr(C)]
#[derive(Clone, Copy)]
struct DevModeW {
    dm_device_name: [u16; CCHDEVICENAME],
    dm_spec_version: u16,
    dm_driver_version: u16,
    dm_size: u16,
    dm_driver_extra: u16,
    dm_fields: u32,
    dm_position_union: [u32; 4],
    dm_color: i16,
    dm_duplex: i16,
    dm_y_resolution: i16,
    dm_tt_option: i16,
    dm_collate: i16,
    dm_form_name: [u16; CCHFORMNAME],
    dm_log_pixels: u16,
    dm_bits_per_pel: u32,
    dm_pels_width: u32,
    dm_pels_height: u32,
    dm_display_flags: u32,
    dm_display_frequency: u32,
    dm_icm_method: u32,
    dm_icm_intent: u32,
    dm_media_type: u32,
    dm_dither_type: u32,
    dm_reserved1: u32,
    dm_reserved2: u32,
    dm_panning_width: u32,
    dm_panning_height: u32,
}

impl DevModeW {
    fn new() -> Self {
        // SAFETY: all fields are integers or integer arrays.
        let mut mode: DevModeW = unsafe { mem::zeroed() };
        mode.dm_size = mem::size_of::<DevModeW>() as u16;
        mode
    }
}

// Checked at compile time, including from a cross-target `cargo check`: if
// either layout drifts, the Win32 calls below silently read garbage.
const _: () = assert!(mem::size_of::<DevModeW>() == 220);
const _: () = assert!(mem::size_of::<DisplayDeviceW>() == 840);

#[allow(non_snake_case)]
#[link(name = "user32")]
unsafe extern "system" {
    fn EnumDisplayDevicesW(
        device: *const u16,
        device_num: u32,
        display_device: *mut DisplayDeviceW,
        flags: u32,
    ) -> i32;
    fn EnumDisplaySettingsW(device_name: *const u16, mode_num: u32, dev_mode: *mut DevModeW)
    -> i32;
}

#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(file_name: *const u16) -> *mut c_void;
    fn FreeLibrary(module: *mut c_void) -> i32;
    fn GetProcAddress(module: *mut c_void, proc_name: *const c_char) -> *mut c_void;
}

/// `NVENCSTATUS NvEncodeAPIGetMaxSupportedVersion(uint32_t*)`
type NvEncodeApiGetMaxSupportedVersion = unsafe extern "system" fn(*mut u32) -> i32;
const NV_ENC_SUCCESS: i32 = 0;

pub fn displays() -> Vec<DisplayInfo> {
    let mut displays = Vec::new();
    for adapter in adapters() {
        if adapter.state_flags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP == 0
            || adapter.state_flags & DISPLAY_DEVICE_MIRRORING_DRIVER != 0
        {
            continue;
        }

        let mut mode = DevModeW::new();
        // SAFETY: `device_name` is a NUL-terminated array from the OS and
        // `mode` is a correctly sized out-parameter with `dm_size` set.
        let ok = unsafe {
            EnumDisplaySettingsW(
                adapter.device_name.as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut mode,
            )
        };
        if ok == 0 {
            continue;
        }

        displays.push(DisplayInfo {
            id: from_wide(&adapter.device_name),
            name: monitor_name(&adapter),
            primary: adapter.state_flags & DISPLAY_DEVICE_PRIMARY_DEVICE != 0,
            current: VideoMode::new(
                mode.dm_pels_width,
                mode.dm_pels_height,
                mode.dm_display_frequency * 1000,
            ),
            // Win32 reports DPI scaling per monitor through a different API
            // and the host renders nothing, so it stays unanswered here.
            scale_factor: None,
            available_refresh_mhz: available_refresh_mhz(
                &adapter.device_name,
                mode.dm_pels_width,
                mode.dm_pels_height,
            ),
        });
    }
    displays
}

pub fn gpus() -> Vec<GpuInfo> {
    let mut gpus: Vec<GpuInfo> = Vec::new();
    for adapter in adapters() {
        if adapter.state_flags & DISPLAY_DEVICE_MIRRORING_DRIVER != 0 {
            continue;
        }
        let name = from_wide(&adapter.device_string);
        if name.is_empty() || gpus.iter().any(|gpu| gpu.name == name) {
            continue;
        }
        gpus.push(GpuInfo {
            vendor: parse_vendor(&from_wide(&adapter.device_id)),
            name,
        });
    }
    gpus
}

pub fn nvenc() -> Option<NvencInfo> {
    let library = wide("nvEncodeAPI64.dll");
    // SAFETY: NUL-terminated wide string; a missing library returns null.
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    if module.is_null() {
        return None;
    }

    // SAFETY: `module` is a live handle; the symbol name is NUL-terminated.
    let symbol = unsafe { GetProcAddress(module, c"NvEncodeAPIGetMaxSupportedVersion".as_ptr()) };
    let info = if symbol.is_null() {
        None
    } else {
        // SAFETY: the symbol has this signature in every published nvEncodeAPI
        // version; it writes one u32 and returns an NVENCSTATUS.
        let get_version =
            unsafe { mem::transmute::<*mut c_void, NvEncodeApiGetMaxSupportedVersion>(symbol) };
        let mut version = 0u32;
        // SAFETY: valid out-parameter.
        let status = unsafe { get_version(&mut version) };
        (status == NV_ENC_SUCCESS).then_some(NvencInfo {
            // Packed as (major << 4) | minor.
            api_major: version >> 4,
            api_minor: version & 0xF,
        })
    };

    // SAFETY: we own the handle taken by LoadLibraryW.
    unsafe { FreeLibrary(module) };
    info
}

/// Not probed on Windows: the host encodes, it never decodes.
pub fn hardware_decode() -> Vec<VideoCodec> {
    Vec::new()
}

fn adapters() -> Vec<DisplayDeviceW> {
    let mut adapters = Vec::new();
    let mut index = 0u32;
    loop {
        let mut adapter = DisplayDeviceW::new();
        // SAFETY: a null device pointer asks for adapters; `adapter` is a
        // correctly sized out-parameter with `cb` set.
        let ok = unsafe { EnumDisplayDevicesW(core::ptr::null(), index, &mut adapter, 0) };
        if ok == 0 {
            return adapters;
        }
        adapters.push(adapter);
        index += 1;
    }
}

fn monitor_name(adapter: &DisplayDeviceW) -> String {
    let mut monitor = DisplayDeviceW::new();
    // SAFETY: passing an adapter name asks for its monitors; index 0 is the
    // first one attached to that adapter.
    let ok = unsafe { EnumDisplayDevicesW(adapter.device_name.as_ptr(), 0, &mut monitor, 0) };
    if ok != 0 {
        let name = from_wide(&monitor.device_string);
        if !name.is_empty() {
            return name;
        }
    }
    from_wide(&adapter.device_string)
}

fn available_refresh_mhz(device_name: &[u16], width: u32, height: u32) -> Vec<u32> {
    let mut rates = Vec::new();
    let mut index = 0u32;
    loop {
        let mut mode = DevModeW::new();
        // SAFETY: NUL-terminated device name, correctly sized out-parameter.
        let ok = unsafe { EnumDisplaySettingsW(device_name.as_ptr(), index, &mut mode) };
        if ok == 0 {
            break;
        }
        index += 1;
        if mode.dm_pels_width == width && mode.dm_pels_height == height {
            rates.push(mode.dm_display_frequency * 1000);
        }
    }
    rates.sort_unstable();
    rates.dedup();
    rates
}

/// Pulls the PCI vendor id out of a device id such as
/// `PCI\VEN_10DE&DEV_2684&SUBSYS_...`.
fn parse_vendor(device_id: &str) -> GpuVendor {
    let Some(rest) = device_id.split("VEN_").nth(1) else {
        return GpuVendor::Other(0);
    };
    let digits: String = rest.chars().take(4).collect();
    match u32::from_str_radix(&digits, 16) {
        Ok(id) => GpuVendor::from_pci_id(id),
        Err(_) => GpuVendor::Other(0),
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_ids_are_parsed_from_device_ids() {
        assert_eq!(
            parse_vendor(r"PCI\VEN_10DE&DEV_2684&SUBSYS_167E10DE&REV_A1"),
            GpuVendor::Nvidia
        );
        assert_eq!(parse_vendor(r"PCI\VEN_8086&DEV_A780"), GpuVendor::Intel);
        assert_eq!(parse_vendor("root\\basicdisplay"), GpuVendor::Other(0));
    }

    #[test]
    fn a_reported_display_is_described_plausibly() {
        // Guarded rather than asserted, and for the reason the macOS side is
        // already guarded. A hosted runner has no screen, and a Windows
        // session without an interactive desktop reports no attached adapter
        // either; neither is a defect in these Win32 calls, and the old
        // assertion that the list is non-empty failed a suite on a machine
        // that was never meant to satisfy it. That the host has a display is
        // an environment claim, and `xtask gates` is where this project keeps
        // those, reporting a requirement present, absent or unknown.
        //
        // What can be wrong here is the description of a display the probe
        // does report, so that is what is asserted and it can still fail: two
        // adapters both flagged primary, or none, is the primary-flag test
        // reading the wrong bit; a mode below VGA is a `DEVMODEW` field read
        // at the wrong offset, which is the failure the size assertions above
        // cannot catch on their own because a wrong offset inside a
        // right-sized struct still compiles.
        let displays = displays();
        if displays.is_empty() {
            return;
        }
        let primary: Vec<&DisplayInfo> =
            displays.iter().filter(|display| display.primary).collect();
        assert_eq!(
            primary.len(),
            1,
            "exactly one attached adapter is primary, not {}: {displays:?}",
            primary.len()
        );
        let primary = primary[0];
        assert!(!primary.id.is_empty(), "a display with no device name");
        assert!(
            primary.current.width >= 640 && primary.current.height >= 480,
            "implausible mode: {:?}",
            primary.current
        );
        // Zero is how `EnumDisplaySettingsW` says "the hardware's default
        // rate" rather than a rate, so it is not a finding. Anything else has
        // to be a rate a panel could run at, and has to be one the same
        // adapter advertises at the same pixel size - the two come from
        // separate calls, and a mode list that omits the mode currently in use
        // means the enumeration is filtering on the wrong fields.
        if primary.current.refresh_mhz != 0 {
            assert!(
                primary.current.refresh_mhz >= 24_000,
                "implausible refresh: {:?}",
                primary.current
            );
            assert!(
                primary
                    .available_refresh_mhz
                    .contains(&primary.current.refresh_mhz),
                "the current rate is missing from what the adapter advertises: {primary:?}"
            );
        }
    }
}
