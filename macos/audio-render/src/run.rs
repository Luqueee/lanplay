//! The run: a tone producer on an ordinary thread, a bounded ring, and a HAL
//! IOProc draining it.
//!
//! The producer is not paced by a clock, and that is deliberate. How fast audio
//! leaves this machine is decided by the output device's own crystal, not by
//! `mach_absolute_time`, and a source that generated 48000 frames per wall
//! second against a device consuming 48000.4 would drift into an underrun or an
//! overrun eventually whatever the ring's size, having measured the difference
//! between two oscillators rather than anything about the buffer. So the
//! producer is demand-driven: it tops the ring up to a target occupancy and
//! sleeps when it is there. What the run then measures is the margin it kept,
//! which is the question — can a ring feeding this callback stay ahead of it
//! for five minutes — asked in the only form that has an answer.
//!
//! The target is half the ring. That leaves as much room above it to absorb a
//! producer that woke late as it leaves below it to absorb a callback that
//! arrived early, and it means the occupancy figures in the report are a margin
//! rather than a ceiling: a ring held brim-full would report the same p50 for a
//! producer with microseconds to spare as for one with milliseconds, because
//! the number would be the capacity and not the margin.
//!
//! The ring is prefilled to that target before the device is started. Starting
//! an empty ring would put an underrun in every run's first callback, and a
//! probe whose clean shape is one underrun is a probe nobody can read.
//!
//! The callback is [`crate::stream`]'s, shared with the receiver that fills
//! the same ring from a jitter buffer instead of from an oscillator.

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lanplay_telemetry::ScheduledAs;
use lanplay_tone_source::tone::{CONTRACT, Tone, ToneSpec};

use crate::device::{self, Error};
use crate::report::Report;
use crate::ring::PcmRing;
use crate::stream::{RenderState, Stream};

/// The level everything in this crate plays at.
///
/// Chosen for whoever might be near the speakers rather than for the
/// measurement. This machine is unattended and its output goes wherever the
/// system happens to be routing it, so five minutes of two-tone test signal at
/// a comfortable listening level is not a thing to inflict on a room. Choosing
/// it this way costs the measurement nothing at all: what is being measured is
/// a ring's occupancy and a callback's cadence, and neither of those has any
/// dependence on amplitude — the same float samples are copied through the same
/// buffers at the same rate whatever number is in them.
pub const LEVEL_DBFS: f64 = -40.0;

/// How long to wait for the producer to reach its target before starting the
/// device. Generous, because failing to fill a ring in a second means something
/// is wrong that the run should say rather than push into the first callback.
const PREFILL_TIMEOUT: Duration = Duration::from_secs(1);

/// Shortest nap the producer takes when the ring is already at target, so a
/// device with a very small buffer does not turn the producer into a spin.
const MINIMUM_NAP: Duration = Duration::from_micros(250);

/// What the probe was asked to do.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Options {
    pub seconds: f64,
    /// Frames per IO cycle to ask the device for. A request, not a setting: the
    /// device answers with what it will do.
    pub buffer_frames: u32,
    /// Ring capacity as a multiple of the granted IO buffer.
    pub ring_multiple: u32,
}

/// Fills `run` with the contract tone.
///
/// Stereo goes through the generator's own interleaved fill. Anything else gets
/// the two tones on the first two channels and silence elsewhere: a device with
/// six outputs is not a reason to invent four more frequencies, and a device
/// with one is not a reason to sum two tones into a signal neither analysis
/// would recognise.
fn fill_tone(tone: &mut Tone, channels: usize, run: &mut [f32]) {
    if channels == 2 {
        tone.fill_stereo(run);
        return;
    }
    for frame in run.chunks_exact_mut(channels) {
        let [left, right] = tone.next_frame();
        frame.fill(0.0);
        frame[0] = left;
        if channels > 1 {
            frame[1] = right;
        }
    }
}

/// Renders the tone for the requested time and reports what the device did.
pub fn run(options: Options) -> Result<Report, Error> {
    let device = device::default_output_device()?;
    let name = device::device_name(device)?;

    let streams = device::output_streams(device)?;
    let stream = match streams.as_slice() {
        [] => {
            return Err(Error::Unsupported(format!(
                "{name} has no output stream, so there is nothing to render into"
            )));
        }
        [single] => *single,
        many => {
            // An aggregate device hands the IOProc one buffer per stream, and
            // deciding which of them the tone belongs in is a different
            // experiment from the one this phase is running.
            return Err(Error::Unsupported(format!(
                "{name} has {} output streams; this probe measures a single-stream device",
                many.len()
            )));
        }
    };

    let requested_buffer_frames = options.buffer_frames;
    let buffer_request_refused = device::request_buffer_frame_size(device, options.buffer_frames)
        .err()
        .map(|error| error.to_string());
    let buffer_frames = device::buffer_frame_size(device)?;
    let buffer_frame_range = device::buffer_frame_size_range(device);
    if buffer_frames == 0 {
        return Err(Error::Unsupported(format!(
            "{name} reports an IO buffer of no frames, so no cycle would ever ask for audio"
        )));
    }

    let format = device::virtual_format(stream)?;
    let physical = device::physical_format(stream).ok();
    if !format.is_writable() {
        return Err(Error::Unsupported(format!(
            "{name} mixes at {format}; this probe writes 32-bit float and will not convert, \
             because a converter inserted here would make every figure in the report a \
             statement about the converter"
        )));
    }

    let channels = usize::from(format.channels);
    let ring_frames = buffer_frames as usize * options.ring_multiple as usize;
    let ring = Arc::new(PcmRing::new(ring_frames, channels));
    // As full as the ring will go while still leaving room to write a whole chunk
    // without a partial fill. Half the ring was the first choice, on the reasoning that
    // the occupancy then reports the margin the producer kept - but it also halves that
    // margin, and measured on this machine it was not enough: five underruns in twenty
    // seconds, each one a whole callback of silence, with the ring pinned at exactly
    // half and dipping to zero. The callback drains a chunk every 5.3 ms and an ordinary
    // thread on this system misses a 10 ms deadline several times a minute whatever it
    // is doing, so the margin has to be the whole ring rather than a readable fraction
    // of it. Occupancy still reports the margin, as a dip from full rather than a dip
    // from half.
    let producer_target_frames = ring_frames.saturating_sub(buffer_frames as usize);

    // Enough room for every cycle the run should see, plus a quarter, plus a
    // floor for a very short run. A store that filled would leave the
    // distributions describing a prefix of the run, which the report would
    // admit to — but a run that has to admit it is a run that measured less
    // than it was asked to.
    let expected_cycles =
        (options.seconds * f64::from(format.sample_rate) / f64::from(buffer_frames)) as usize;
    let store = expected_cycles + expected_cycles / 4 + 1_024;

    let state = Box::new(RenderState::new(Arc::clone(&ring), channels, store));

    let spec = ToneSpec {
        sample_rate: format.sample_rate,
        channels: format.channels,
        level_dbfs: LEVEL_DBFS,
        ..CONTRACT
    };
    let stop = Arc::new(AtomicBool::new(false));
    let producer = {
        let ring = Arc::clone(&ring);
        let stop = Arc::clone(&stop);
        let chunk = buffer_frames as usize;
        // A quarter of an IO buffer: short enough that a ring drained by one
        // cycle is topped up before the next one needs it, long enough that the
        // producer is asleep rather than spinning between them.
        let period_ns = (f64::from(buffer_frames) / f64::from(format.sample_rate) * 1e9) as u64;
        let nap =
            Duration::from_secs_f64(f64::from(buffer_frames) / f64::from(format.sample_rate) / 4.0)
                .max(MINIMUM_NAP);
        thread::spawn(move || {
            // The producer feeds a callback the system has already promoted, and an
            // ordinary thread cannot reliably keep up with it. Measured here, in order:
            // filling half the ring underran five times in twenty seconds; filling the
            // whole ring for sixteen milliseconds of margin underran ten times in three
            // hundred; and asking for a user-interactive quality of service produced
            // zero, twelve, zero and zero across four runs. Every underrun is a whole
            // callback of silence, because this deadline is not soft.
            //
            // What that sequence says is that a priority band is not a deadline. The
            // intermittency was never explained - a controlled comparison with a build
            // running showed no underruns at all, so sustained load is not the trigger -
            // and the fix does not depend on explaining it: the system provides a
            // real-time policy for precisely this shape of work, and asking for a band
            // and hoping is the thing being replaced.
            //
            // Growing the ring instead would work and is the wrong answer: in the
            // finished pipeline this ring sits between the jitter buffer and the device,
            // so every frame of it is latency added to audio that has already crossed a
            // network. The buffer stays small and the producer becomes reliable.
            //
            // The period is the callback's, because that is the cycle the producer has to
            // keep up with. The computation is an eighth of it, which is generous for
            // generating at most one buffer of sine and honest about it - a computation
            // claimed larger than the work steals a share of the machine nothing here
            // needs. Preemptible, because a non-preemptible thread that misbehaves takes
            // the machine with it and no measurement is worth that.
            let policy = ScheduledAs::request(period_ns);
            eprintln!("audio-render: producer scheduled as {policy}");
            if !policy.is_real_time() {
                eprintln!(
                    "audio-render: the producer did not get a deadline, so underruns below \
                     are the scheduler's and not the ring's"
                );
            }
            let mut tone = Tone::new(spec);
            while !stop.load(Ordering::Acquire) {
                let occupancy = ring.occupancy_frames();
                if occupancy >= producer_target_frames {
                    thread::sleep(nap);
                    continue;
                }
                let want = (producer_target_frames - occupancy).min(chunk);
                ring.fill(want, &mut |_, run| fill_tone(&mut tone, channels, run));
            }
        })
    };

    let deadline = Instant::now() + PREFILL_TIMEOUT;
    while ring.occupancy_frames() < producer_target_frames {
        if Instant::now() >= deadline {
            stop.store(true, Ordering::Release);
            let _ = producer.join();
            return Err(Error::Unsupported(format!(
                "the producer reached only {} of the {producer_target_frames} frames the ring \
                 wants before the device starts",
                ring.occupancy_frames()
            )));
        }
        thread::sleep(MINIMUM_NAP);
    }

    let outcome: Result<(), Error> = (|| {
        let mut audio = Stream::new(device, &state)?;
        audio.start()?;
        thread::sleep(Duration::from_secs_f64(options.seconds));
        Ok(())
    })();
    stop.store(true, Ordering::Release);
    producer.join().expect("the producer thread panicked");
    outcome?;

    // The IOProc was destroyed when the `Stream` guard went out of scope above,
    // so nothing is writing the trace any more, and consuming the state is what
    // says so: the borrow the stream held has ended.
    let rendered = state.finish();

    Ok(Report {
        device: name,
        format,
        physical,
        buffer_frames,
        requested_buffer_frames,
        buffer_request_refused,
        buffer_frame_range,
        ring_frames,
        ring_multiple: options.ring_multiple,
        producer_target_frames,
        callbacks: rendered.callbacks,
        odd_cycles: rendered.odd_cycles,
        frames_requested: rendered.frames_requested,
        interval_us: rendered.interval_us,
        occupancy_frames: rendered.occupancy_frames,
        underruns: ring.underruns(),
        underrun_frames: ring.underrun_frames(),
        overruns: ring.overruns(),
        overrun_frames: ring.overrun_frames(),
        frames_produced: ring.produced(),
        frames_consumed: ring.consumed(),
        span_seconds: rendered.span_seconds,
        frames_in_span: rendered.frames_in_span,
        requested_seconds: options.seconds,
        level_dbfs: LEVEL_DBFS,
        left_hz: spec.left_hz,
        right_hz: spec.right_hz,
        samples_dropped: rendered.samples_dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stereo path is the generator's own, and the two channels have to
    /// stay apart: a fill that wrote the left tone to both would pass every
    /// count in the report and be the wrong audio.
    #[test]
    fn a_stereo_fill_puts_a_different_tone_in_each_channel() {
        let mut tone = Tone::new(ToneSpec {
            level_dbfs: LEVEL_DBFS,
            ..CONTRACT
        });
        let mut run = vec![0.0f32; 200];
        fill_tone(&mut tone, 2, &mut run);
        let left: Vec<f32> = run.iter().step_by(2).copied().collect();
        let right: Vec<f32> = run.iter().skip(1).step_by(2).copied().collect();
        assert_ne!(left, right, "one tone was written to both channels");
        assert!(left.iter().any(|sample| *sample != 0.0));
        assert!(right.iter().any(|sample| *sample != 0.0));
    }

    /// -40 dBFS is a hundredth of full scale, and the fill must actually be at
    /// the level the report claims it played at.
    #[test]
    fn the_fill_is_at_the_level_the_report_names() {
        let mut tone = Tone::new(ToneSpec {
            level_dbfs: LEVEL_DBFS,
            ..CONTRACT
        });
        let mut run = vec![0.0f32; 2 * 48_000];
        fill_tone(&mut tone, 2, &mut run);
        let peak = run.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
        assert!(
            (peak - 0.01).abs() < 0.0005,
            "peak {peak} is not -40 dBFS full scale"
        );
    }

    /// A device that is not stereo still gets the two tones, on the first two
    /// channels, with the rest silent rather than a copy of something.
    #[test]
    fn a_wider_device_gets_the_tones_on_the_first_two_channels_and_silence_elsewhere() {
        let mut tone = Tone::new(ToneSpec {
            channels: 6,
            level_dbfs: LEVEL_DBFS,
            ..CONTRACT
        });
        let mut run = vec![f32::NAN; 6 * 64];
        fill_tone(&mut tone, 6, &mut run);
        for frame in run.chunks_exact(6) {
            assert_eq!(&frame[2..], &[0.0, 0.0, 0.0, 0.0]);
        }
    }

    #[test]
    fn a_mono_device_gets_the_left_tone_only() {
        let mut tone = Tone::new(ToneSpec {
            channels: 1,
            level_dbfs: LEVEL_DBFS,
            ..CONTRACT
        });
        let mut run = vec![f32::NAN; 64];
        fill_tone(&mut tone, 1, &mut run);
        assert!(run.iter().any(|sample| *sample != 0.0));
        assert!(run.iter().all(|sample| sample.abs() <= 0.011));
    }
}
