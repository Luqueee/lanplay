//! Sampling the run in ten-second slices.
//!
//! An average over ten minutes cannot show a stall. A run that held 120 Hz
//! for the first two hundred seconds, collapsed to 78 for ten, and recovered
//! averages out at a comfortable 119 and looks perfect. The slices are the
//! only place that collapse is visible, so the gate reads them rather than
//! the mean.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_decoder_videotoolbox::DecoderCounters;
use lanplay_renderer_metal::{LatestFrameSlot, LiveCounters};
use lanplay_telemetry::{Nanos, Telemetry, Timestamp, wait_until};

use crate::report::Window;

/// Samples both halves of the picture on one timer: the renderer's counters
/// and the telemetry window have to describe the same slice of time, and
/// reading them from different threads would smear the boundary.
pub fn sample(
    telemetry: &Telemetry,
    counters: &Arc<LiveCounters>,
    slot: &Arc<LatestFrameSlot>,
    decoder: &DecoderCounters,
    // Access units handed to the decoder, bumped by whatever feeds it.
    arrived: &Arc<std::sync::atomic::AtomicU64>,
    every: Duration,
    stop: &Arc<AtomicBool>,
) -> Vec<Window> {
    let mut windows = Vec::new();
    let start = Timestamp::now();
    let period = Nanos(every.as_nanos() as u64);
    let mut last_callbacks = counters.callbacks.load(Ordering::Relaxed);
    let mut last_rendered = counters.rendered.load(Ordering::Relaxed);
    let mut last_superseded = slot.superseded();
    let mut last_decoded = decoder.decoded();
    let mut last_arrived = arrived.load(Ordering::Relaxed);
    let mut index = 1u64;

    while !stop.load(Ordering::Acquire) {
        wait_until(start.add(Nanos(period.get() * index)));
        if stop.load(Ordering::Acquire) {
            break;
        }

        let taken = telemetry.take_window();
        let callbacks = counters.callbacks.load(Ordering::Relaxed);
        let rendered = counters.rendered.load(Ordering::Relaxed);
        let decoded = decoder.decoded();
        let reassembled = arrived.load(Ordering::Relaxed);
        let seconds = taken.span.as_secs_f64().max(f64::EPSILON);

        let drawn = rendered - last_rendered;
        // Read from the slot, not inferred from presented frames: a presented
        // frame is by definition one that was not superseded, so subtracting
        // the two can only ever produce zero.
        let superseded_now = slot.superseded();
        let superseded = superseded_now - last_superseded;
        let offered = drawn + superseded;
        let ticks = callbacks - last_callbacks;
        windows.push(Window {
            from_s: (index - 1) as f64 * every.as_secs_f64(),
            to_s: index as f64 * every.as_secs_f64(),
            callback_hz: ticks as f64 / seconds,
            source_hz: (reassembled - last_arrived) as f64 / seconds,
            decode_hz: (decoded - last_decoded) as f64 / seconds,
            render_hz: drawn as f64 / seconds,
            superseded_pct: if offered == 0 {
                0.0
            } else {
                superseded as f64 * 100.0 / offered as f64
            },
            // Ticks that carried something new. Stated this way round because
            // it is the number a link configuration is ranked by: 100% means
            // every refresh opportunity was used, and bunching shows up here
            // before it shows up anywhere else.
            fresh_pct: if ticks == 0 {
                0.0
            } else {
                drawn as f64 * 100.0 / ticks as f64
            },
            source_interval_p99_ms: taken.source_interval.p99.as_millis_f64(),
            frame_age_p99_ms: taken.local_age.p99.as_millis_f64(),
        });

        last_callbacks = callbacks;
        last_rendered = rendered;
        last_superseded = superseded_now;
        last_decoded = decoded;
        last_arrived = reassembled;
        index += 1;
    }
    windows
}

/// The largest fall in callback rate between one slice and the next.
///
/// A stall shows up here and nowhere else in the summary: this is the number
/// the gate uses to refuse a run whose average looks healthy.
pub fn worst_callback_drop(windows: &[Window]) -> f64 {
    windows
        .windows(2)
        .filter(|pair| pair[0].callback_hz > 0.0)
        .map(|pair| 1.0 - pair[1].callback_hz / pair[0].callback_hz)
        .fold(0.0f64, f64::max)
}

#[allow(clippy::too_many_arguments)]
pub fn spawn(
    telemetry: Arc<Telemetry>,
    counters: Arc<LiveCounters>,
    slot: Arc<LatestFrameSlot>,
    decoder: DecoderCounters,
    arrived: Arc<std::sync::atomic::AtomicU64>,
    every: Duration,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<Vec<Window>> {
    thread::Builder::new()
        .name("windows".into())
        .spawn(move || {
            sample(
                &telemetry, &counters, &slot, &decoder, &arrived, every, &stop,
            )
        })
        .expect("spawn window sampler")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(callback_hz: f64) -> Window {
        Window {
            from_s: 0.0,
            to_s: 10.0,
            callback_hz,
            source_hz: callback_hz,
            decode_hz: callback_hz,
            render_hz: callback_hz,
            superseded_pct: 0.0,
            fresh_pct: 100.0,
            source_interval_p99_ms: 8.3,
            frame_age_p99_ms: 9.0,
        }
    }

    #[test]
    fn a_steady_run_has_no_drop() {
        let steady: Vec<Window> = (0..10).map(|_| window(119.9)).collect();
        assert!(worst_callback_drop(&steady) < 0.01);
    }

    #[test]
    fn one_bad_slice_is_found_even_though_the_average_is_fine() {
        // 119.9 Hz for most of the run with a single slice at 78.2: the mean
        // is 116 and looks healthy.
        let mut run: Vec<Window> = (0..60).map(|_| window(119.9)).collect();
        run[20] = window(78.2);
        let mean: f64 = run.iter().map(|w| w.callback_hz).sum::<f64>() / run.len() as f64;
        assert!(mean > 118.0, "the average hides it: {mean}");
        assert!(worst_callback_drop(&run) > 0.3);
    }

    #[test]
    fn a_run_that_never_started_reports_no_drop_rather_than_dividing_by_zero() {
        let dead: Vec<Window> = (0..5).map(|_| window(0.0)).collect();
        assert_eq!(worst_callback_drop(&dead), 0.0);
    }
}
