//! What mode the output is actually in.
//!
//! Every cadence claim in the report is relative to a frame period, and a
//! period taken from a constant is a claim about a machine nobody ran on. The
//! physical output here is 1920x1080 at 100 Hz and the virtual display used
//! for the second pass is 1920x1080 at 120; assuming either would misjudge the
//! other by twenty percent.
//!
//! Two sources, in that order. GDI knows which mode is current but reports the
//! rate as a rounded integer, so a 59.94 Hz mode comes back as 59 and a
//! classifier built on it would call every single interval a stall. DXGI knows
//! the exact rational for every mode but not which one is in use. Asking GDI
//! what is current and DXGI what that mode's timing really is gets both.

#![cfg(windows)]

use lanplay_capture::CaptureDevice;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_MODE_DESC,
};
use windows::Win32::Graphics::Dxgi::DXGI_ENUM_MODES;
use windows::Win32::Graphics::Gdi::{DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW};
use windows::core::PCWSTR;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub numerator: u32,
    pub denominator: u32,
    /// Which of the two sources produced the rational above.
    pub source: &'static str,
}

impl DisplayMode {
    pub fn hz(&self) -> f64 {
        if self.denominator == 0 {
            return 0.0;
        }
        self.numerator as f64 / self.denominator as f64
    }
}

/// The output's current mode, or `None` when the OS would not say.
///
/// `None` is deliberately not a default. A run that cannot learn the rate is a
/// run whose cadence numbers have no denominator, and the caller is told to
/// supply `--source-hz` rather than being handed a guess.
pub fn detect(device: &CaptureDevice) -> Option<DisplayMode> {
    let (width, height, rounded_hz) = current_mode(&device.identity().output)?;
    if rounded_hz <= 1 {
        // 0 and 1 are documented placeholders for "the hardware default",
        // which is not a rate.
        return None;
    }

    match exact_rational(device, width, height, rounded_hz) {
        Some((numerator, denominator)) => Some(DisplayMode {
            width,
            height,
            numerator,
            denominator,
            source: "dxgi mode list",
        }),
        None => Some(DisplayMode {
            width,
            height,
            numerator: rounded_hz,
            denominator: 1,
            source: "gdi (integer only)",
        }),
    }
}

/// Width, height and rounded refresh of whatever mode the output is in.
fn current_mode(device_name: &str) -> Option<(u32, u32, u32)> {
    let mut name: Vec<u16> = device_name.encode_utf16().collect();
    name.push(0);

    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..DEVMODEW::default()
    };
    // SAFETY: `name` is nul-terminated and outlives the call; `mode` is a
    // fully initialised DEVMODEW with its dmSize set, as the API requires.
    let ok = unsafe {
        EnumDisplaySettingsW(PCWSTR(name.as_ptr()), ENUM_CURRENT_SETTINGS, &raw mut mode)
    };
    if !ok.as_bool() {
        return None;
    }
    Some((mode.dmPelsWidth, mode.dmPelsHeight, mode.dmDisplayFrequency))
}

/// The exact rational for the mode GDI just described.
///
/// Chosen by nearest rate among the modes of the same size, because that is
/// the only field GDI's rounding leaves ambiguous.
fn exact_rational(
    device: &CaptureDevice,
    width: u32,
    height: u32,
    rounded_hz: u32,
) -> Option<(u32, u32)> {
    let mut best: Option<(f64, u32, u32)> = None;
    // The desktop is one of these two; enumerating both costs nothing and
    // avoids depending on which one this driver reports.
    for format in [DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM] {
        for mode in modes(device, format) {
            if mode.Width != width || mode.Height != height || mode.RefreshRate.Denominator == 0 {
                continue;
            }
            let hz = mode.RefreshRate.Numerator as f64 / mode.RefreshRate.Denominator as f64;
            let error = (hz - rounded_hz as f64).abs();
            // More than half a hertz away is a different mode, not this one
            // seen through GDI's rounding.
            if error > 0.5 {
                continue;
            }
            if best.is_none_or(|(previous, _, _)| error < previous) {
                best = Some((
                    error,
                    mode.RefreshRate.Numerator,
                    mode.RefreshRate.Denominator,
                ));
            }
        }
    }
    best.map(|(_, numerator, denominator)| (numerator, denominator))
}

fn modes(device: &CaptureDevice, format: DXGI_FORMAT) -> Vec<DXGI_MODE_DESC> {
    let output = device.output();
    let mut count = 0u32;
    // SAFETY: passing a null description buffer is the documented way to ask
    // for the count first; the second call is given a buffer of exactly that
    // many entries.
    unsafe {
        if output
            .GetDisplayModeList(format, DXGI_ENUM_MODES(0), &raw mut count, None)
            .is_err()
            || count == 0
        {
            return Vec::new();
        }
        let mut descriptions = vec![DXGI_MODE_DESC::default(); count as usize];
        if output
            .GetDisplayModeList(
                format,
                DXGI_ENUM_MODES(0),
                &raw mut count,
                Some(descriptions.as_mut_ptr()),
            )
            .is_err()
        {
            return Vec::new();
        }
        descriptions.truncate(count as usize);
        descriptions
    }
}
