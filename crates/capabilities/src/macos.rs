//! CoreGraphics and VideoToolbox probes for the client role.

use core::ffi::c_void;

use lanplay_protocol::{DisplayInfo, GpuInfo, NvencInfo, VideoCodec, VideoMode};

type CGDirectDisplayID = u32;
type CGDisplayModeRef = *mut c_void;
type CFArrayRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFIndex = isize;
type CGError = i32;
type Boolean = u8;
type OSType = u32;
type CVReturn = i32;
type CVDisplayLinkRef = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CVTime {
    time_value: i64,
    time_scale: i32,
    flags: i32,
}

/// `kCVTimeIsIndefinite`
const CV_TIME_IS_INDEFINITE: i32 = 1;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGGetActiveDisplayList(
        max_displays: u32,
        active_displays: *mut CGDirectDisplayID,
        display_count: *mut u32,
    ) -> CGError;
    fn CGMainDisplayID() -> CGDirectDisplayID;
    fn CGDisplayIsBuiltin(display: CGDirectDisplayID) -> Boolean;
    fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    fn CGDisplayCopyAllDisplayModes(
        display: CGDirectDisplayID,
        options: CFDictionaryRef,
    ) -> CFArrayRef;
    fn CGDisplayModeGetWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    fn CGDisplayModeGetRefreshRate(mode: CGDisplayModeRef) -> f64;
    fn CGDisplayModeRelease(mode: CGDisplayModeRef);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFArrayGetCount(array: CFArrayRef) -> CFIndex;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: CFIndex) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVDisplayLinkCreateWithCGDisplay(
        display: CGDirectDisplayID,
        link_out: *mut CVDisplayLinkRef,
    ) -> CVReturn;
    fn CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link: CVDisplayLinkRef) -> CVTime;
    fn CVDisplayLinkRelease(link: CVDisplayLinkRef);
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTIsHardwareDecodeSupported(codec_type: OSType) -> Boolean;
}

const MAX_DISPLAYS: usize = 16;

pub fn displays() -> Vec<DisplayInfo> {
    let mut ids = [0 as CGDirectDisplayID; MAX_DISPLAYS];
    let mut count = 0u32;
    // SAFETY: `ids` has room for MAX_DISPLAYS entries, `count` is a valid out-parameter.
    let error =
        unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
    if error != 0 {
        return Vec::new();
    }

    // SAFETY: no arguments, always returns a valid id (possibly 0 if headless).
    let main = unsafe { CGMainDisplayID() };
    ids[..count as usize]
        .iter()
        .filter_map(|&id| describe(id, main))
        .collect()
}

fn describe(id: CGDirectDisplayID, main: CGDirectDisplayID) -> Option<DisplayInfo> {
    // SAFETY: `id` came from CGGetActiveDisplayList.
    let mode = unsafe { CGDisplayCopyDisplayMode(id) };
    if mode.is_null() {
        return None;
    }

    // SAFETY: `mode` is a live CGDisplayMode we own until CGDisplayModeRelease.
    let (pixel_width, pixel_height, point_width, refresh_hz) = unsafe {
        (
            CGDisplayModeGetPixelWidth(mode) as u32,
            CGDisplayModeGetPixelHeight(mode) as u32,
            CGDisplayModeGetWidth(mode) as u32,
            CGDisplayModeGetRefreshRate(mode),
        )
    };
    // SAFETY: we own this copy.
    unsafe { CGDisplayModeRelease(mode) };

    let available = available_refresh_mhz(id, pixel_width, pixel_height);
    // Built-in panels historically report 0 Hz through CoreGraphics; fall back
    // to the mode list, then to the display link's nominal period.
    let refresh_mhz = match to_mhz(refresh_hz) {
        Some(mhz) => mhz,
        None => available
            .iter()
            .copied()
            .max()
            .or_else(|| nominal_refresh_mhz(id))
            .unwrap_or(0),
    };

    // SAFETY: `id` is a valid display id.
    let builtin = unsafe { CGDisplayIsBuiltin(id) } != 0;

    Some(DisplayInfo {
        id: id.to_string(),
        name: if builtin {
            "Built-in Display".to_owned()
        } else {
            format!("Display {id}")
        },
        primary: id == main,
        current: VideoMode::new(pixel_width, pixel_height, refresh_mhz),
        scale_factor: (point_width > 0).then(|| pixel_width as f32 / point_width as f32),
        available_refresh_mhz: available,
    })
}

/// Every refresh rate the display advertises at its current pixel size.
fn available_refresh_mhz(id: CGDirectDisplayID, width: u32, height: u32) -> Vec<u32> {
    // SAFETY: `id` is valid; passing no options asks for the default mode list.
    let modes = unsafe { CGDisplayCopyAllDisplayModes(id, core::ptr::null()) };
    if modes.is_null() {
        return Vec::new();
    }

    // SAFETY: `modes` is a live CFArray of CGDisplayModeRef we own.
    let count = unsafe { CFArrayGetCount(modes) };
    let mut rates = Vec::new();
    for index in 0..count {
        // SAFETY: index is in range; the array borrows its elements, so the
        // modes must not be released individually.
        let mode = unsafe { CFArrayGetValueAtIndex(modes, index) } as CGDisplayModeRef;
        if mode.is_null() {
            continue;
        }
        // SAFETY: `mode` is owned by the array and outlives this loop body.
        let (mode_width, mode_height, rate) = unsafe {
            (
                CGDisplayModeGetPixelWidth(mode) as u32,
                CGDisplayModeGetPixelHeight(mode) as u32,
                CGDisplayModeGetRefreshRate(mode),
            )
        };
        if mode_width != width || mode_height != height {
            continue;
        }
        if let Some(mhz) = to_mhz(rate) {
            rates.push(mhz);
        }
    }
    // SAFETY: we own the array returned by a Copy function.
    unsafe { CFRelease(modes) };

    rates.sort_unstable();
    rates.dedup();
    rates
}

/// Refresh rate derived from the display link's nominal output period, for
/// panels that report 0 Hz through CoreGraphics.
fn nominal_refresh_mhz(id: CGDirectDisplayID) -> Option<u32> {
    let mut link: CVDisplayLinkRef = core::ptr::null_mut();
    // SAFETY: valid out-parameter; the link is released below.
    let status = unsafe { CVDisplayLinkCreateWithCGDisplay(id, &mut link) };
    if status != 0 || link.is_null() {
        return None;
    }
    // SAFETY: `link` is live.
    let period = unsafe { CVDisplayLinkGetNominalOutputVideoRefreshPeriod(link) };
    // SAFETY: we own the link.
    unsafe { CVDisplayLinkRelease(link) };

    if period.flags & CV_TIME_IS_INDEFINITE != 0 || period.time_value <= 0 {
        return None;
    }
    let hz = f64::from(period.time_scale) / period.time_value as f64;
    to_mhz(hz)
}

fn to_mhz(hz: f64) -> Option<u32> {
    (hz > 0.0).then(|| (hz * 1000.0).round() as u32)
}

pub fn hardware_decode() -> Vec<VideoCodec> {
    [VideoCodec::H264, VideoCodec::Hevc, VideoCodec::Av1]
        .into_iter()
        // SAFETY: the four-character code is the only argument; the call is a
        // pure query.
        .filter(|codec| unsafe { VTIsHardwareDecodeSupported(codec.four_cc()) } != 0)
        .collect()
}

/// Not probed on macOS: the Mac is the client, and Metal device enumeration
/// would tell us nothing the decoder probe does not.
pub fn gpus() -> Vec<GpuInfo> {
    Vec::new()
}

/// NVENC does not exist on macOS.
pub fn nvenc() -> Option<NvencInfo> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_reports_at_least_one_usable_display() {
        let displays = displays();
        assert!(!displays.is_empty(), "no active displays reported");
        let primary = displays
            .iter()
            .find(|display| display.primary)
            .expect("a primary display");
        assert!(primary.current.width >= 640);
        assert!(
            primary.current.refresh_mhz >= 24_000,
            "implausible refresh: {:?}",
            primary.current
        );
        assert!(primary.scale_factor.is_some_and(|scale| scale >= 1.0));
    }

    #[test]
    fn h264_has_a_hardware_decoder() {
        // Every Mac the client targets can decode H.264 in hardware. If this
        // fails, phase 2 has no floor to stand on.
        assert!(hardware_decode().contains(&VideoCodec::H264));
    }

    #[test]
    fn the_display_link_fallback_answers_for_the_main_display() {
        // Exercised only when CoreGraphics reports 0 Hz, so it needs its own
        // test: a lazily bound CoreVideo symbol would otherwise blow up the
        // first time a panel takes that path.
        // SAFETY: no arguments.
        let main = unsafe { CGMainDisplayID() };
        let refresh = nominal_refresh_mhz(main).expect("nominal refresh for the main display");
        assert!(refresh >= 24_000, "implausible refresh: {refresh} mHz");
    }
}
