//! What the Wi-Fi radio is doing, sampled without disturbing it.
//!
//! `system_profiler SPAirPortDataType` was the obvious instrument and it is
//! the wrong one: its report includes "Other Local Wi-Fi Networks", which it
//! can only fill by scanning. A scan takes the radio off channel. Sampling
//! once a second with it turned a link whose access units arrive every
//! 8.09 ms at p50 and 11.35 ms at p99 into one reading 2.04 ms at p50 and
//! 133 ms at p99 - the instrument produced exactly the bunching the
//! experiment was looking for.
//!
//! The reading itself lives in `lanplay-capabilities`, because the client's
//! preflight needs the same facts and two copies of an OS probe is two
//! chances to disagree about what the radio was doing.
//!
//! usage:
//!   radio-sample [seconds] [interval_ms]

use std::io::Write as _;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn main() {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(120);
    let interval_ms: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(1000);

    if lanplay_capabilities::wifi::association().is_none() {
        eprintln!("no Wi-Fi association");
        std::process::exit(1);
    }

    println!("t_s,unix_s,rssi_dbm,noise_dbm,tx_rate_mbps,channel,width_mhz,radar_band");
    let start = Instant::now();
    let deadline = Duration::from_secs(seconds);
    let interval = Duration::from_millis(interval_ms.max(50));
    while start.elapsed() < deadline {
        let at = start.elapsed();
        let Some(link) = lanplay_capabilities::wifi::association() else {
            // A momentary loss is a fact about the run, not a reason to stop
            // sampling: the gap in the series is the observation.
            continue;
        };
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        println!(
            "{:.3},{unix:.3},{},{},{:.0},{},{},{}",
            at.as_secs_f64(),
            link.rssi_dbm,
            link.noise_dbm,
            link.tx_rate_mbps,
            link.channel,
            link.width_mhz,
            u8::from(link.uses_radar_band())
        );
        // Flushed every row: a sampler killed at the end of a run must not
        // lose the tail of what it saw.
        let _ = std::io::stdout().flush();
        let elapsed = start.elapsed();
        let next = interval * ((elapsed.as_nanos() / interval.as_nanos()) as u32 + 1);
        if next > elapsed {
            std::thread::sleep(next - elapsed);
        }
    }
}
