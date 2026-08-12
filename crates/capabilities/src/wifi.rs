//! What the Wi-Fi association is, and whether it sits in a band the radio
//! has to keep watching for radar.
//!
//! Measured, not assumed. Moving this machine's access point from channel
//! 116 to channel 36 took access units arriving more than two source periods
//! late from 69 a minute to 5.5, and more than four periods late from 42 a
//! minute to 1.5, with nothing else changed. Channel 116 is 5580 MHz, inside
//! 5470-5725; channel 36 is 5180 MHz, inside 5150-5250.
//!
//! What is claimed and what is not:
//!
//! * EN 301 893 requires dynamic frequency selection and continuous
//!   in-service monitoring for radar in 5250-5350 and 5470-5725 MHz. That is
//!   the regulation, and it is why those bands are flagged.
//! * It does *not* prescribe periodically pausing transmission, and the
//!   34 ms stall every 220 ms measured here is nowhere in it. That pattern
//!   is this access point's implementation while meeting the requirement,
//!   and other access points may not do it.
//!
//! So this warns that a configuration is known to go wrong here, and does
//! not claim to know that every DFS channel on every access point will.
//!
//! Bands rather than channel numbers, because channel numbering is shared
//! across regulatory domains while availability is not: 149 and above is
//! outside the range harmonised for radio local area networks in Spain,
//! whatever a router's channel list offers.

#![cfg(target_os = "macos")]

use core::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

/// The current association, as the driver reports it. Reading these
/// properties never triggers a scan.
#[derive(Clone, Debug, PartialEq)]
pub struct Association {
    pub rssi_dbm: i64,
    pub noise_dbm: i64,
    pub tx_rate_mbps: f64,
    pub channel: i64,
    pub width_mhz: u32,
    /// The regulatory domain the driver believes it is in, e.g. `ES`. Empty
    /// when the driver will not say.
    pub country: String,
}

impl Association {
    /// Centre frequency of the primary 20 MHz channel, in MHz.
    pub fn centre_mhz(&self) -> u32 {
        channel_to_mhz(self.channel)
    }

    /// The whole span the association occupies, primary channel plus the
    /// rest of its block.
    ///
    /// A 160 MHz block starting at channel 36 reaches 5330 MHz and so covers
    /// 5250-5350, which is a radar band even though channel 36 is not. The
    /// primary channel alone cannot answer that.
    pub fn span_mhz(&self) -> (u32, u32) {
        occupied_span(self.channel, self.width_mhz)
    }

    /// True when any part of the occupied span falls in a band where
    /// EN 301 893 requires radar detection.
    pub fn uses_radar_band(&self) -> bool {
        let (low, high) = self.span_mhz();
        RADAR_BANDS
            .iter()
            .any(|(band_low, band_high)| low < *band_high && high > *band_low)
    }

    /// True when the span falls outside what Spain's national frequency
    /// table harmonises for radio local area networks: 5150-5350 and
    /// 5470-5725 MHz. Channels 149 and above are in this category, and a
    /// recommendation must not send anyone there.
    pub fn outside_es_rlan(&self) -> bool {
        if !self.country.is_empty() && self.country != "ES" {
            return false;
        }
        let (low, high) = self.span_mhz();
        // 2.4 GHz is harmonised and not the subject here.
        if high <= 2500 {
            return false;
        }
        low < 5150 || high > 5725
    }
}

/// Bands where radar detection is required. Identical under EN 301 893 and
/// FCC part 15E, which is why no domain check guards them.
const RADAR_BANDS: [(u32, u32); 2] = [(5250, 5350), (5470, 5725)];

/// Lowest channel of each 80 MHz block, and of each 160 MHz block.
const BLOCKS_80: [i64; 6] = [36, 52, 100, 116, 132, 149];
const BLOCKS_160: [i64; 2] = [36, 100];

fn channel_to_mhz(channel: i64) -> u32 {
    match channel {
        1..=13 => (2407 + 5 * channel) as u32,
        14 => 2484,
        c if c >= 32 => (5000 + 5 * c) as u32,
        _ => 0,
    }
}

/// The frequency span a primary channel occupies at a given width.
fn occupied_span(channel: i64, width_mhz: u32) -> (u32, u32) {
    let centre = channel_to_mhz(channel);
    if centre == 0 {
        return (0, 0);
    }
    // A block is named by its lowest channel and spans width/20 channels of
    // 20 MHz each. Anything the tables do not cover - 2.4 GHz, an unknown
    // width - falls back to the primary channel alone, which is honest
    // rather than invented.
    let first = match width_mhz {
        80 => BLOCKS_80
            .iter()
            .rev()
            .find(|start| channel >= **start)
            .copied(),
        160 => BLOCKS_160
            .iter()
            .rev()
            .find(|start| channel >= **start)
            .copied(),
        40 if channel >= 36 => Some(channel - (channel - 36) % 8),
        _ => None,
    };
    match first {
        Some(first) => {
            let channels = (width_mhz / 20).max(1) as i64;
            let low = channel_to_mhz(first) - 10;
            let high = channel_to_mhz(first + (channels - 1) * 4) + 10;
            (low, high)
        }
        None => (centre - 10, centre + 10),
    }
}

/// The current association, or `None` when there is no Wi-Fi or no
/// connection.
pub fn association() -> Option<Association> {
    // SAFETY: `CWWiFiClient` is a documented class, `sharedWiFiClient` is a
    // singleton accessor, and every selector below is a documented
    // `CWInterface` property returning a scalar or an object that is null
    // checked before use. None of them scan.
    unsafe {
        let client: *mut AnyObject = msg_send![class!(CWWiFiClient), sharedWiFiClient];
        if client.is_null() {
            return None;
        }
        let interface: *mut AnyObject = msg_send![client, interface];
        let interface = Retained::retain(interface)?;

        let channel_object: *mut AnyObject = msg_send![&*interface, wlanChannel];
        if channel_object.is_null() {
            return None;
        }
        let channel: i64 = msg_send![channel_object, channelNumber];
        let width: i64 = msg_send![channel_object, channelWidth];

        let country: *mut AnyObject = msg_send![&*interface, countryCode];
        let country = if country.is_null() {
            String::new()
        } else {
            let country: Retained<NSString> = Retained::retain(country.cast())?;
            country.to_string()
        };

        Some(Association {
            rssi_dbm: msg_send![&*interface, rssiValue],
            noise_dbm: msg_send![&*interface, noiseMeasurement],
            tx_rate_mbps: msg_send![&*interface, transmitRate],
            channel,
            width_mhz: width_to_mhz(width),
            country,
        })
    }
}

/// `CWChannelWidth` is an enumeration, and its ordinal is not the width.
pub fn width_to_mhz(raw: i64) -> u32 {
    match raw {
        1 => 20,
        2 => 40,
        3 => 80,
        4 => 160,
        // 0 is unknown, and anything else is a width this build has never
        // heard of. Reporting 0 says so; guessing would put an invented
        // number where a measurement belongs.
        _ => 0,
    }
}

// Keeps the linker honest: `msg_send!` alone does not pull in CoreWLAN, and
// the class lookup would then fail at runtime with a much less obvious
// message than a link error.
#[link(name = "CoreWLAN", kind = "framework")]
unsafe extern "C" {
    fn CWKeychainDeleteWiFiEAPUsernameAndPassword(interface: *const c_void) -> i32;
}

#[allow(dead_code)]
fn keep_framework_linked() -> i32 {
    unsafe { CWKeychainDeleteWiFiEAPUsernameAndPassword(core::ptr::null()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(channel: i64, width_mhz: u32) -> Association {
        Association {
            rssi_dbm: -50,
            noise_dbm: -95,
            tx_rate_mbps: 1200.0,
            channel,
            width_mhz,
            country: "ES".into(),
        }
    }

    #[test]
    fn the_two_channels_this_was_built_for_are_classified_correctly() {
        // 116 at 80 MHz is what the access point chose on its own and what
        // stalled; 36 at 80 MHz is what did not.
        let dfs = at(116, 80);
        assert_eq!(dfs.centre_mhz(), 5580);
        assert_eq!(dfs.span_mhz(), (5570, 5650));
        assert!(dfs.uses_radar_band());

        let clean = at(36, 80);
        assert_eq!(clean.centre_mhz(), 5180);
        assert_eq!(clean.span_mhz(), (5170, 5250));
        assert!(!clean.uses_radar_band());
    }

    #[test]
    fn a_wide_block_is_judged_by_what_it_occupies_not_its_primary() {
        // 160 MHz from channel 36 reaches 5330 and so covers 5250-5350,
        // where radar detection is required, even though 36 alone does not.
        let wide = at(36, 160);
        assert_eq!(wide.span_mhz(), (5170, 5330));
        assert!(
            wide.uses_radar_band(),
            "a 160 MHz block from 36 overlaps 5250-5350"
        );
        // The same primary at 80 MHz does not.
        assert!(!at(36, 80).uses_radar_band());
    }

    #[test]
    fn every_channel_in_a_radar_band_is_flagged() {
        for channel in [52, 56, 60, 64, 100, 104, 116, 132, 140] {
            assert!(at(channel, 20).uses_radar_band(), "channel {channel}");
        }
        for channel in [36, 40, 44, 48] {
            assert!(!at(channel, 20).uses_radar_band(), "channel {channel}");
        }
    }

    #[test]
    fn channels_above_the_spanish_allocation_are_named_as_such() {
        // 149 and up sit beyond 5725 MHz, which Spain's national frequency
        // table does not harmonise for these networks. They are not a
        // recommendation just because they avoid radar detection.
        let high = at(149, 80);
        assert!(!high.uses_radar_band());
        assert!(high.outside_es_rlan());
        assert!(!at(36, 80).outside_es_rlan());
        assert!(!at(116, 80).outside_es_rlan());

        // Somewhere else, that judgement is not ours to make.
        let elsewhere = Association {
            country: "US".into(),
            ..at(149, 80)
        };
        assert!(!elsewhere.outside_es_rlan());
    }

    #[test]
    fn an_unknown_width_falls_back_to_the_primary_channel() {
        let unknown = at(116, 0);
        assert_eq!(unknown.span_mhz(), (5570, 5590));
        assert!(unknown.uses_radar_band());
    }
}
