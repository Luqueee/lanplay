//! Lists and sets display modes, because the benchmark needs the panel at its
//! real maximum and nothing over SSH can put it there.
//!
//! An SSH session on Windows is session 0, which has no display devices: user32
//! enumeration returns nothing and a mode change has nowhere to land. This must
//! therefore run in the interactive session, which in practice means being
//! launched by a scheduled task created with `/IT`. A process launched that way
//! has no usable stdout, so everything goes to a file given on the command
//! line, and the exit code carries the verdict for anything that can only see
//! that.
//!
//! Exit codes: 0 success, 1 the requested mode does not exist, 2 the change was
//! refused by the driver, 3 no display device.

use std::fmt::Write as _;
use std::path::PathBuf;

use windows::Win32::Foundation::TRUE;
use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DISPLAY_DEVICE_STATE_FLAGS, DISPLAY_DEVICEW, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    ENUM_CURRENT_SETTINGS, EnumDisplayDevicesW, EnumDisplaySettingsW,
};
use windows::core::PCWSTR;

/// The only flag that matters here: a device that is not attached to the
/// desktop has no modes worth listing.
const ATTACHED: DISPLAY_DEVICE_STATE_FLAGS = DISPLAY_DEVICE_STATE_FLAGS(0x0000_0001);

pub fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let mut out: Option<PathBuf> = None;
    let mut request: Option<(u32, u32, u32)> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--set" => {
                let spec = args.next().unwrap_or_default();
                match parse_mode(&spec) {
                    Some(mode) => request = Some(mode),
                    None => {
                        report(&out, &format!("bad mode {spec:?}, want WIDTHxHEIGHT@HZ\n"));
                        return std::process::ExitCode::from(1);
                    }
                }
            }
            other => {
                report(&out, &format!("unknown argument {other:?}\n"));
                return std::process::ExitCode::from(1);
            }
        }
    }

    let mut text = String::new();
    let Some(device) = primary_device() else {
        report(&out, "no display device attached to this desktop\n");
        return std::process::ExitCode::from(3);
    };
    let name = String::from_utf16_lossy(trim_nul(&device.DeviceName));
    let _ = writeln!(
        text,
        "device {name} | {}",
        String::from_utf16_lossy(trim_nul(&device.DeviceString))
    );

    if let Some(current) = settings(&device.DeviceName, ENUM_CURRENT_SETTINGS) {
        let _ = writeln!(
            text,
            "current {}x{}@{}",
            current.dmPelsWidth, current.dmPelsHeight, current.dmDisplayFrequency
        );
    }

    let modes = all_modes(&device.DeviceName);
    let mut listed: Vec<String> = modes
        .iter()
        .map(|m| {
            format!(
                "{}x{}@{}",
                m.dmPelsWidth, m.dmPelsHeight, m.dmDisplayFrequency
            )
        })
        .collect();
    listed.sort();
    listed.dedup();
    let _ = writeln!(text, "modes {}", listed.join(" "));

    let Some((width, height, hz)) = request else {
        report(&out, &text);
        return std::process::ExitCode::SUCCESS;
    };

    let Some(target) = modes
        .iter()
        .find(|m| m.dmPelsWidth == width && m.dmPelsHeight == height && m.dmDisplayFrequency == hz)
    else {
        let _ = writeln!(text, "FAIL {width}x{height}@{hz} is not an available mode");
        report(&out, &text);
        return std::process::ExitCode::from(1);
    };

    let mut mode = *target;
    mode.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    // SAFETY: `mode` came from the driver's own enumeration for this device and
    // the device name is a valid NUL-terminated wide string owned by `device`.
    let result = unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(device.DeviceName.as_ptr()),
            Some(&mode),
            None,
            CDS_UPDATEREGISTRY,
            None,
        )
    };

    if result == DISP_CHANGE_SUCCESSFUL {
        let _ = writeln!(text, "OK set {width}x{height}@{hz}");
        report(&out, &text);
        std::process::ExitCode::SUCCESS
    } else {
        let _ = writeln!(text, "FAIL ChangeDisplaySettingsEx returned {}", result.0);
        report(&out, &text);
        std::process::ExitCode::from(2)
    }
}

fn parse_mode(spec: &str) -> Option<(u32, u32, u32)> {
    let (size, hz) = spec.split_once('@')?;
    let (width, height) = size.split_once('x')?;
    Some((width.parse().ok()?, height.parse().ok()?, hz.parse().ok()?))
}

fn primary_device() -> Option<DISPLAY_DEVICEW> {
    for index in 0.. {
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `cb` is set as the API requires and the out-parameter lives
        // for the whole call.
        let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) };
        if ok != TRUE {
            return None;
        }
        if device.StateFlags.0 & ATTACHED.0 != 0 {
            return Some(device);
        }
    }
    None
}

fn settings(
    name: &[u16],
    which: windows::Win32::Graphics::Gdi::ENUM_DISPLAY_SETTINGS_MODE,
) -> Option<DEVMODEW> {
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: `dmSize` is set as the API requires; `name` is NUL-terminated.
    let ok = unsafe { EnumDisplaySettingsW(PCWSTR(name.as_ptr()), which, &mut mode) };
    (ok == TRUE).then_some(mode)
}

fn all_modes(name: &[u16]) -> Vec<DEVMODEW> {
    let mut modes = Vec::new();
    for index in 0.. {
        match settings(
            name,
            windows::Win32::Graphics::Gdi::ENUM_DISPLAY_SETTINGS_MODE(index),
        ) {
            // 32 bits per pixel only: the 8- and 16-bit legacy entries are
            // noise that would triple the listing.
            Some(mode) if mode.dmBitsPerPel == 32 => modes.push(mode),
            Some(_) => {}
            None => break,
        }
    }
    modes
}

fn trim_nul(text: &[u16]) -> &[u16] {
    let end = text.iter().position(|c| *c == 0).unwrap_or(text.len());
    &text[..end]
}

fn report(out: &Option<PathBuf>, text: &str) {
    print!("{text}");
    if let Some(path) = out {
        let _ = std::fs::write(path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::parse_mode;

    #[test]
    fn a_mode_spec_round_trips() {
        assert_eq!(parse_mode("3440x1440@100"), Some((3440, 1440, 100)));
        assert_eq!(parse_mode("1920x1080@120"), Some((1920, 1080, 120)));
    }

    #[test]
    fn a_spec_missing_its_rate_is_rejected_rather_than_defaulted() {
        // Defaulting the refresh rate is how a benchmark ends up measuring
        // 60 Hz while its report says 120.
        assert_eq!(parse_mode("1920x1080"), None);
        assert_eq!(parse_mode("1920@120"), None);
    }
}
