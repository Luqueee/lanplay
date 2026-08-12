//! What the Wi-Fi radio is doing, sampled without disturbing it.
//!
//! `system_profiler SPAirPortDataType` was the obvious instrument and it is
//! the wrong one: its report includes "Other Local Wi-Fi Networks", which it
//! can only fill by scanning. A scan takes the radio off channel. Sampling
//! once a second with it turned a link whose access units arrive every 8.09 ms
//! at p50 and 11.35 ms at p99 into one reading 2.04 ms at p50 and 133 ms at
//! p99 - the instrument produced exactly the bunching the experiment was
//! looking for.
//!
//! CoreWLAN's `CWInterface` properties are reads of the current association,
//! served from the driver's own state. Nothing here scans, associates or
//! changes power state.
//!
//! Emits CSV on stdout, one row a second, so a run can be correlated against
//! the radio that carried it.
//!
//! usage:
//!   radio-sample [seconds] [interval_ms]

use core::ffi::c_void;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

/// One reading of the association, as the driver reports it.
struct Sample {
    rssi: i64,
    noise: i64,
    /// Negotiated PHY rate in Mbps, which moves with MCS and channel width.
    tx_rate: f64,
    channel: i64,
    /// Channel width in MHz. 0 when the driver will not say.
    width_mhz: i64,
    /// CoreWLAN's PHY mode enumeration: 4 is 802.11n, 5 ac, 6 ax.
    phy_mode: i64,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(120);
    let interval_ms: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(1000);

    let Some(interface) = interface() else {
        eprintln!("no Wi-Fi interface");
        std::process::exit(1);
    };

    println!("t_s,unix_s,rssi_dbm,noise_dbm,tx_rate_mbps,channel,width_mhz,phy_mode");
    let start = Instant::now();
    let deadline = Duration::from_secs(seconds);
    let interval = Duration::from_millis(interval_ms.max(50));
    while start.elapsed() < deadline {
        let at = start.elapsed();
        let sample = read(&interface);
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        println!(
            "{:.3},{unix:.3},{},{},{:.0},{},{},{}",
            at.as_secs_f64(),
            sample.rssi,
            sample.noise,
            sample.tx_rate,
            sample.channel,
            sample.width_mhz,
            sample.phy_mode
        );
        // Flushed every row: a sampler killed at the end of a run must not
        // lose the tail of what it saw.
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        let elapsed = start.elapsed();
        let next = interval * ((elapsed.as_nanos() / interval.as_nanos()) as u32 + 1);
        if next > elapsed {
            std::thread::sleep(next - elapsed);
        }
    }
}

/// The default Wi-Fi interface, or `None` on a machine with no radio.
fn interface() -> Option<Retained<AnyObject>> {
    // SAFETY: `CWWiFiClient` is a documented class; `sharedWiFiClient` is a
    // singleton accessor and `interface` returns an autoreleased object or
    // nil, which `Retained::retain` handles.
    unsafe {
        let client: *mut AnyObject = msg_send![class!(CWWiFiClient), sharedWiFiClient];
        if client.is_null() {
            return None;
        }
        let interface: *mut AnyObject = msg_send![client, interface];
        Retained::retain(interface)
    }
}

fn read(interface: &Retained<AnyObject>) -> Sample {
    // SAFETY: every selector below is a documented `CWInterface` property
    // returning a scalar or an object; the channel object is only messaged
    // after a null check. None of them scan.
    unsafe {
        let rssi: i64 = msg_send![&**interface, rssiValue];
        let noise: i64 = msg_send![&**interface, noiseMeasurement];
        let tx_rate: f64 = msg_send![&**interface, transmitRate];
        let phy_mode: i64 = msg_send![&**interface, activePHYMode];
        let channel_object: *mut AnyObject = msg_send![&**interface, wlanChannel];
        let (channel, width_mhz) = if channel_object.is_null() {
            (0, 0)
        } else {
            let number: i64 = msg_send![channel_object, channelNumber];
            let width: i64 = msg_send![channel_object, channelWidth];
            (number, width_mhz(width))
        };
        Sample {
            rssi,
            noise,
            tx_rate,
            channel,
            width_mhz,
            phy_mode,
        }
    }
}

/// `CWChannelWidth` is an enumeration, and its ordinal is not the width.
fn width_mhz(raw: i64) -> i64 {
    match raw {
        1 => 20,
        2 => 40,
        3 => 80,
        4 => 160,
        // 0 is `kCWChannelWidthUnknown`, and anything else is a width this
        // build has never heard of. Reporting 0 says so; guessing would put
        // an invented number in a table meant to decide an experiment.
        _ => 0,
    }
}

// Keeps the linker honest: `msg_send!` alone does not pull in CoreWLAN, and
// the class lookup would fail at runtime with a much less obvious message.
#[link(name = "CoreWLAN", kind = "framework")]
unsafe extern "C" {
    fn CWKeychainDeleteWiFiEAPUsernameAndPassword(interface: *const c_void) -> i32;
}

#[allow(dead_code)]
fn _keep_framework_linked() -> i32 {
    // Never called. Referencing one symbol is what makes the framework load.
    unsafe { CWKeychainDeleteWiFiEAPUsernameAndPassword(core::ptr::null()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_widths_are_megahertz_not_ordinals() {
        assert_eq!(width_mhz(1), 20);
        assert_eq!(width_mhz(3), 80);
        assert_eq!(width_mhz(4), 160);
        // Unknown must not be reported as a plausible width.
        assert_eq!(width_mhz(0), 0);
        assert_eq!(width_mhz(99), 0);
    }

    #[test]
    fn a_mac_with_wifi_reports_an_association() {
        // Guarded rather than asserted: a machine with the radio off is a
        // legitimate place to run the suite, and this test exists to prove
        // the selectors are right, not that the laptop is online.
        let Some(interface) = interface() else {
            return;
        };
        let sample = read(&interface);
        if sample.channel == 0 {
            return;
        }
        assert!(
            (-100..0).contains(&sample.rssi),
            "implausible rssi {}",
            sample.rssi
        );
        assert!(sample.tx_rate > 0.0, "associated but no rate");
        assert!(
            [0, 20, 40, 80, 160].contains(&sample.width_mhz),
            "implausible width {}",
            sample.width_mhz
        );
    }
}
