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
//! The callback allocates nothing, locks nothing, logs nothing and reads no
//! clock. Its cadence comes from the host timestamp the HAL hands it, which is
//! both free and better: it is the time the IO cycle began, so the intervals
//! are the device's cadence rather than this program's opinion of it. Its
//! measurements go into fixed-capacity stores sized before the device was
//! started, and the counters it touches are relaxed atomics on lines nobody
//! else writes.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use lanplay_audio_capture::{Percentiles, Samples};
use lanplay_tone_source::tone::{CONTRACT, Tone, ToneSpec};
use objc2_core_audio::{
    AudioConvertHostTimeToNanos, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop, AudioObjectID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};

use crate::device::{self, Error};
use crate::report::Report;
use crate::ring::PcmRing;
use crate::schedule::ScheduledAs;

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

/// Everything the callback touches.
///
/// Reached from the IOProc through the client pointer the HAL keeps, so it is
/// boxed and kept alive by [`run`] for as long as the IOProc exists.
struct RenderState {
    ring: Arc<PcmRing>,
    channels: usize,
    /// Published with a release store at the end of every cycle, and the only
    /// thing the reader needs to acquire: everything the callback wrote into
    /// [`RenderState::trace`] before that store is visible after it.
    callbacks: AtomicU64,
    /// Cycles whose buffer list was not the shape the format promised. Counted
    /// rather than handled, because a device that changed its stream layout
    /// underneath a running IOProc is a finding and not something to paper over.
    odd_cycles: AtomicU64,
    trace: UnsafeCell<Trace>,
}

/// The measurements, in fixed-capacity stores. Touched only by the callback
/// while the device is running, and only by the caller once it has stopped.
struct Trace {
    frames: Samples,
    occupancy: Samples,
    /// Intervals in host clock ticks. Kept raw because converting them is a
    /// function call, and the callback is the one place in this project where a
    /// function call has to be justified; the conversion is linear, so the
    /// percentiles of the ticks are the percentiles of the times.
    interval_ticks: Samples,
    /// Frames asked for by every cycle but the one in flight. Kept as a running
    /// total rather than derived afterwards because the span the report divides
    /// by runs between the first and last cycles' timestamps, so the frames of
    /// the cycle that closed it were never inside it.
    frames_in_span: u64,
    /// Frames the most recent cycle asked for, held so the one before it can be
    /// added to [`Trace::frames_in_span`] once its interval is known.
    last_frames: u64,
    callbacks: u64,
    first_host_time: u64,
    last_host_time: u64,
}

/// The IOProc.
///
/// # Safety
///
/// Registered by [`run`] with a pointer to a `RenderState` that outlives the
/// IOProc, and unregistered before that state is dropped.
unsafe extern "C-unwind" fn render(
    _device: AudioObjectID,
    now: NonNull<AudioTimeStamp>,
    _input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    // SAFETY: `client` is the pointer given to `AudioDeviceCreateIOProcID`,
    // which is a `RenderState` owned by `run` and kept alive until after the
    // IOProc has been destroyed.
    let state = unsafe { &*client.cast::<RenderState>() };

    // SAFETY: the HAL passes a buffer list whose `mBuffers` really has
    // `mNumberBuffers` entries; the list is only read here, and the audio is
    // written through the `mData` pointers rather than through this slice.
    let buffers = unsafe {
        let list = output.as_ptr();
        core::slice::from_raw_parts((*list).mBuffers.as_ptr(), (*list).mNumberBuffers as usize)
    };

    let channels = state.channels;
    let interleaved = buffers.len() == 1 && buffers[0].mNumberChannels as usize == channels;
    let planar = buffers.len() == channels && buffers.iter().all(|b| b.mNumberChannels == 1);
    if (!interleaved && !planar) || buffers[0].mData.is_null() {
        state.odd_cycles.fetch_add(1, Ordering::Relaxed);
        return 0;
    }

    let sample_bytes = size_of::<f32>();
    let frames = if interleaved {
        buffers[0].mDataByteSize as usize / (sample_bytes * channels)
    } else {
        buffers[0].mDataByteSize as usize / sample_bytes
    };

    let occupancy = state.ring.occupancy_frames();
    let drained = if interleaved {
        let base = buffers[0].mData.cast::<f32>();
        state.ring.drain(frames, &mut |offset, run| {
            // SAFETY: `run` is `channels` samples per frame and starts at frame
            // `offset` of this cycle, and the device buffer holds `frames`
            // frames, of which the ring never hands over more than were asked
            // for. The ring's storage and the device's buffer are separate
            // allocations, so the copy cannot overlap.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    run.as_ptr(),
                    base.add(offset * channels),
                    run.len(),
                );
            }
        })
    } else {
        state.ring.drain(frames, &mut |offset, run| {
            for (index, frame) in run.chunks_exact(channels).enumerate() {
                for (channel, sample) in frame.iter().enumerate() {
                    // SAFETY: one buffer per channel was established above, each
                    // holding `frames` samples, and `offset + index` is below
                    // the frames this cycle asked for.
                    unsafe {
                        *buffers[channel].mData.cast::<f32>().add(offset + index) = *sample;
                    }
                }
            }
        })
    };

    // The HAL documents the output buffers as arriving zeroed, so this is
    // belt and braces — but it is the concealment this report counts, and code
    // that counted an underrun without writing the silence it claims to have
    // written would be describing something it did not do. It costs nothing in
    // a healthy run: there is no tail to clear unless the ring came up short.
    if drained.zero_filled > 0 {
        if interleaved {
            // SAFETY: the tail runs from the last frame delivered to the end of
            // the buffer the device sized, all inside the one allocation.
            unsafe {
                core::ptr::write_bytes(
                    buffers[0]
                        .mData
                        .cast::<f32>()
                        .add(drained.frames * channels),
                    0,
                    drained.zero_filled * channels,
                );
            }
        } else {
            for buffer in buffers {
                // SAFETY: the same tail in each of the per-channel buffers.
                unsafe {
                    core::ptr::write_bytes(
                        buffer.mData.cast::<f32>().add(drained.frames),
                        0,
                        drained.zero_filled,
                    );
                }
            }
        }
    }

    // SAFETY: the trace belongs to the IO thread for as long as the IOProc is
    // registered. `run` reads it only after `AudioDeviceDestroyIOProcID` has
    // returned, which is the point at which the HAL guarantees this function
    // will not be called again.
    let trace = unsafe { &mut *state.trace.get() };
    trace.frames.record(frames as u64);
    trace.occupancy.record(occupancy as u64);
    // SAFETY: `now` is the timestamp the HAL passed for this cycle.
    let host_time = unsafe { now.as_ref().mHostTime };
    if trace.callbacks == 0 {
        trace.first_host_time = host_time;
    } else if host_time > trace.last_host_time {
        trace
            .interval_ticks
            .record(host_time - trace.last_host_time);
        // The cycle that just ended is the one whose frames the span now
        // covers; this cycle's own frames belong to the next interval.
        trace.frames_in_span += trace.last_frames;
    }
    trace.last_frames = frames as u64;
    trace.last_host_time = host_time;
    trace.callbacks += 1;
    state.callbacks.store(trace.callbacks, Ordering::Release);
    0
}

/// Registers the IOProc, runs the device for the requested time, and stops it.
///
/// The guard exists so that a panic or an early return between start and stop
/// cannot leave a live IOProc pointing at state that is about to be dropped,
/// which is a use-after-free the HAL would discover on a real-time thread.
struct Stream {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    running: bool,
}

impl Stream {
    fn new(device: AudioObjectID, state: &RenderState) -> Result<Stream, Error> {
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `proc_id` is a live local, and the client pointer is the
        // caller's `RenderState`, which the caller keeps alive until after this
        // `Stream` has been dropped.
        let result = unsafe {
            AudioDeviceCreateIOProcID(
                device,
                Some(render),
                (state as *const RenderState as *mut RenderState).cast::<c_void>(),
                NonNull::from(&mut proc_id),
            )
        };
        if result != 0 {
            return Err(Error::Api {
                call: "AudioDeviceCreateIOProcID",
                status: result,
            });
        }
        Ok(Stream {
            device,
            proc_id,
            running: false,
        })
    }

    fn start(&mut self) -> Result<(), Error> {
        // SAFETY: the IOProc id came from `AudioDeviceCreateIOProcID` on this
        // same device and has not been destroyed.
        let result = unsafe { AudioDeviceStart(self.device, self.proc_id) };
        if result != 0 {
            return Err(Error::Api {
                call: "AudioDeviceStart",
                status: result,
            });
        }
        self.running = true;
        Ok(())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if self.running {
            // SAFETY: stopping the IOProc this stream started.
            unsafe { AudioDeviceStop(self.device, self.proc_id) };
        }
        // SAFETY: destroying the IOProc id this stream created. After this
        // returns the HAL will not call the callback again, which is what makes
        // reading the trace afterwards sound.
        unsafe { AudioDeviceDestroyIOProcID(self.device, self.proc_id) };
    }
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

/// Turns a distribution of host clock ticks into one of microseconds.
///
/// The mapping is linear and monotonic, so converting the order statistics is
/// the same as converting every sample and is one call per figure rather than
/// one per cycle.
fn ticks_to_micros(ticks: Percentiles) -> Percentiles {
    // SAFETY: a pure arithmetic conversion with no pointers in it.
    let micros = |value: u64| unsafe { AudioConvertHostTimeToNanos(value) } / 1_000;
    Percentiles {
        count: ticks.count,
        min: micros(ticks.min),
        p50: micros(ticks.p50),
        p95: micros(ticks.p95),
        p99: micros(ticks.p99),
        max: micros(ticks.max),
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

    let state = Box::new(RenderState {
        ring: Arc::clone(&ring),
        channels,
        callbacks: AtomicU64::new(0),
        odd_cycles: AtomicU64::new(0),
        trace: UnsafeCell::new(Trace {
            frames: Samples::with_capacity(store),
            occupancy: Samples::with_capacity(store),
            interval_ticks: Samples::with_capacity(store),
            frames_in_span: 0,
            last_frames: 0,
            callbacks: 0,
            first_host_time: 0,
            last_host_time: 0,
        }),
    });

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
    // so nothing is writing the trace any more; the acquire load pairs with the
    // release store the last cycle made.
    let callbacks = state.callbacks.load(Ordering::Acquire);
    // SAFETY: the callback is unregistered and this is the only live reference.
    let trace = unsafe { &mut *state.trace.get() };
    let span_ticks = trace.last_host_time.saturating_sub(trace.first_host_time);
    // SAFETY: a pure arithmetic conversion with no pointers in it.
    let span_seconds = unsafe { AudioConvertHostTimeToNanos(span_ticks) } as f64 / 1e9;

    let samples_dropped =
        trace.frames.dropped() + trace.occupancy.dropped() + trace.interval_ticks.dropped();

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
        callbacks,
        odd_cycles: state.odd_cycles.load(Ordering::Relaxed),
        frames_requested: trace.frames.percentiles(),
        interval_us: trace.interval_ticks.percentiles().map(ticks_to_micros),
        occupancy_frames: trace.occupancy.percentiles(),
        underruns: ring.underruns(),
        underrun_frames: ring.underrun_frames(),
        overruns: ring.overruns(),
        overrun_frames: ring.overrun_frames(),
        frames_produced: ring.produced(),
        frames_consumed: ring.consumed(),
        span_seconds,
        frames_in_span: trace.frames_in_span,
        requested_seconds: options.seconds,
        level_dbfs: LEVEL_DBFS,
        left_hz: spec.left_hz,
        right_hz: spec.right_hz,
        samples_dropped,
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
