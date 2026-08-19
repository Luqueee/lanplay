//! The HAL IOProc, and the state it reaches through the client pointer.
//!
//! It lives on its own rather than inside [`crate::run`] because two runs now
//! drive the same device the same way: the tone probe, which fills the ring
//! from an oscillator, and the receiver, which fills it from a jitter buffer.
//! What the callback does is identical in both — take what the ring has, write
//! silence over the shortfall, count it, and record the cycle — and the twenty
//! lines of it that are unsafe are exactly the lines that must not exist twice.
//! A second copy would be a second place for the buffer-layout check and the
//! zero-fill to disagree, and the disagreement would only ever be heard.
//!
//! The callback allocates nothing, locks nothing, logs nothing and reads no
//! clock. Its cadence comes from the host timestamp the HAL hands it, which is
//! both free and better: it is the time the IO cycle began, so the intervals
//! are the device's cadence rather than the program's opinion of it. Its
//! measurements go into fixed-capacity stores sized before the device was
//! started, and the counters it touches are relaxed atomics on lines nobody
//! else writes.

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use lanplay_audio_capture::{Drift, Percentiles, Rate, Samples};
use objc2_core_audio::{
    AudioConvertHostTimeToNanos, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop, AudioObjectID,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp, AudioTimeStampFlags};

use crate::device::Error;
use crate::ring::PcmRing;

/// Everything the callback touches.
///
/// Reached from the IOProc through the client pointer the HAL keeps, so it is
/// boxed and kept alive by its owner for as long as the IOProc exists.
pub struct RenderState {
    ring: Arc<PcmRing>,
    channels: usize,
    /// Nanoseconds in one tick of the host clock, read out of the HAL once
    /// before the device started.
    ///
    /// The ratio is one on this machine and 1/24 on others, and which it is has
    /// to come out of the API rather than out of an assumption. Held here so
    /// that the callback turns a cycle's host time into seconds with a multiply
    /// instead of a call into CoreAudio: the call is safe on a real-time thread,
    /// but a conversion that can be hoisted out of a deadline should be.
    nanos_per_tick: f64,
    /// Published with a release store at the end of every cycle, and the only
    /// thing a reader needs to acquire: everything the callback wrote into
    /// [`RenderState::trace`] before that store is visible after it.
    callbacks: AtomicU64,
    /// Cycles whose buffer list was not the shape the format promised. Counted
    /// rather than handled, because a device that changed its stream layout
    /// underneath a running IOProc is a finding and not something to paper over.
    odd_cycles: AtomicU64,
    trace: UnsafeCell<Trace>,
}

// SAFETY: the trace is written only by the IO thread, and only while an IOProc
// is registered. `Stream` borrows the state for exactly that interval, so the
// only way to read the trace is [`RenderState::finish`], which cannot be called
// while a `Stream` is alive. The two atomics are atomics.
unsafe impl Sync for RenderState {}
// SAFETY: the same argument. Nothing in here is bound to the thread that built
// it, and the ring is already `Sync`.
unsafe impl Send for RenderState {}

/// The measurements, in fixed-capacity stores. Touched only by the callback
/// while the device is running, and only by the owner once it has stopped.
struct Trace {
    frames: Samples,
    occupancy: Samples,
    /// Intervals in host clock ticks. Kept raw because converting them is a
    /// function call, and the callback is the one place in this project where a
    /// function call has to be justified; the conversion is linear, so the
    /// percentiles of the ticks are the percentiles of the times.
    interval_ticks: Samples,
    /// Frames asked for by every cycle but the one in flight. Kept as a running
    /// total rather than derived afterwards because the span a report divides
    /// by runs between the first and last cycles' timestamps, so the frames of
    /// the cycle that closed it were never inside it.
    frames_in_span: u64,
    /// Frames the most recent cycle asked for, held so the one before it can be
    /// added to [`Trace::frames_in_span`] once its interval is known.
    last_frames: u64,
    callbacks: u64,
    first_host_time: u64,
    last_host_time: u64,
    /// The device's rate, from the `mSampleTime` and `mHostTime` the HAL reports
    /// for the same cycle.
    ///
    /// A7.1's Mac half, and it measures what the device physically consumed
    /// rather than the rate it advertises. Anchored on the first cycle's own
    /// host time so that what reaches [`Drift`] is an elapsed interval and never
    /// an absolute reading of a clock whose epoch is this machine's last boot.
    drift: Drift,
    /// Cycles whose timestamp did not carry both a sample time and a host time.
    ///
    /// The HAL states which of a timestamp's representations it filled in, and a
    /// cycle missing either is a cycle whose rate cannot be read. Counted and
    /// left out, because reading an absent `mSampleTime` as zero would put a
    /// rewind of the whole run into the fit.
    invalid_timestamps: u64,
}

/// What the device did, read out once the IOProc is gone.
pub struct Rendered {
    pub callbacks: u64,
    pub odd_cycles: u64,
    pub frames_requested: Option<Percentiles>,
    pub occupancy_frames: Option<Percentiles>,
    pub interval_us: Option<Percentiles>,
    /// Between the first and last cycles' own timestamps, which is the device's
    /// clock rather than the caller's.
    pub span_seconds: f64,
    /// When the first cycle ran, in the HAL's own host ticks. Kept raw and not
    /// converted, so that a caller measuring how long `AudioDeviceStart` took
    /// subtracts two readings of one clock rather than comparing two clocks.
    pub first_host_time: u64,
    pub frames_in_span: u64,
    /// The device's rate against nominal, or nothing when too few cycles carried
    /// a timestamp to state one.
    pub sink_rate: Option<Rate>,
    /// Cycles left out of that rate because the HAL did not fill both the sample
    /// time and the host time in.
    pub invalid_timestamps: u64,
    /// Measurements a full store had nowhere to put. Non-zero means every
    /// distribution above describes a prefix of the run.
    pub samples_dropped: u64,
}

impl RenderState {
    /// `store` is how many cycles each distribution has room for, and
    /// `nominal_hz` the rate the device advertises, which the drift below is a
    /// deviation from. Both sized and read by the caller before the device
    /// starts, because the callback cannot allocate and should not call.
    pub fn new(ring: Arc<PcmRing>, channels: usize, store: usize, nominal_hz: f64) -> RenderState {
        // A billion ticks rather than one, because the conversion is integer and
        // a ratio of one twenty-fourth asked about a single tick quantises to
        // zero.
        // SAFETY: a pure arithmetic conversion with no pointers in it.
        let nanos_per_tick = unsafe { AudioConvertHostTimeToNanos(1_000_000_000) } as f64 / 1e9;
        RenderState {
            ring,
            channels,
            nanos_per_tick,
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
                drift: Drift::new(nominal_hz),
                invalid_timestamps: 0,
            }),
        }
    }

    /// Cycles the device has run so far. Safe to read while it is running,
    /// which is what a windowed report does between windows.
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Acquire)
    }

    /// Reads the trace out, once the IOProc has been destroyed.
    ///
    /// Consuming the state is what makes that sound rather than merely
    /// intended: [`Stream`] borrows it for its whole life, so this cannot be
    /// called while a live IOProc still points at it, and the borrow checker
    /// rather than a comment is what says so.
    pub fn finish(self) -> Rendered {
        let callbacks = self.callbacks.load(Ordering::Acquire);
        let odd_cycles = self.odd_cycles.load(Ordering::Relaxed);
        let mut trace = self.trace.into_inner();

        let span_ticks = trace.last_host_time.saturating_sub(trace.first_host_time);
        // SAFETY: a pure arithmetic conversion with no pointers in it.
        let span_seconds = unsafe { AudioConvertHostTimeToNanos(span_ticks) } as f64 / 1e9;
        let samples_dropped =
            trace.frames.dropped() + trace.occupancy.dropped() + trace.interval_ticks.dropped();

        Rendered {
            callbacks,
            odd_cycles,
            frames_requested: trace.frames.percentiles(),
            occupancy_frames: trace.occupancy.percentiles(),
            interval_us: trace.interval_ticks.percentiles().map(ticks_to_micros),
            span_seconds,
            first_host_time: trace.first_host_time,
            frames_in_span: trace.frames_in_span,
            sink_rate: trace.drift.rate(),
            invalid_timestamps: trace.invalid_timestamps,
            samples_dropped,
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

/// The IOProc.
///
/// # Safety
///
/// Registered by [`Stream::new`] with a pointer to a `RenderState` that
/// outlives the IOProc, and unregistered before that state is dropped.
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
    // which is a `RenderState` owned by the caller and kept alive until after
    // the IOProc has been destroyed.
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
    // registered. It is read only after `AudioDeviceDestroyIOProcID` has
    // returned, which is the point at which the HAL guarantees this function
    // will not be called again, and `RenderState::finish` is the only reader.
    let trace = unsafe { &mut *state.trace.get() };
    trace.frames.record(frames as u64);
    trace.occupancy.record(occupancy as u64);
    // SAFETY: `now` is the timestamp the HAL passed for this cycle.
    let stamp = unsafe { now.as_ref() };
    let host_time = stamp.mHostTime;
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
    // What the device physically consumed against when it consumed it, which is
    // the rate A7.1 asks for rather than the rate the device advertises. Both
    // halves of the pair come out of this one timestamp, so nothing here is
    // subtracted from a reading taken anywhere else, and the elapsed interval is
    // taken in the HAL's own ticks before being scaled, so the full resolution of
    // a counter whose absolute value is meaningless survives.
    //
    // Both fields or neither. A cycle the HAL did not fill both in for is a
    // cycle whose rate cannot be read, and reading an absent `mSampleTime` as
    // zero would put a rewind of the whole run into the fit.
    if stamp
        .mFlags
        .contains(AudioTimeStampFlags::SampleHostTimeValid)
    {
        let elapsed = host_time.saturating_sub(trace.first_host_time) as f64;
        trace
            .drift
            .record(stamp.mSampleTime, (elapsed * state.nanos_per_tick) as u64);
    } else {
        trace.invalid_timestamps += 1;
    }
    trace.last_frames = frames as u64;
    trace.last_host_time = host_time;
    trace.callbacks += 1;
    state.callbacks.store(trace.callbacks, Ordering::Release);
    0
}

/// Registers the IOProc, and unregisters it on the way out.
///
/// The guard exists so that a panic or an early return between start and stop
/// cannot leave a live IOProc pointing at state that is about to be dropped,
/// which is a use-after-free the HAL would discover on a real-time thread. The
/// lifetime is the other half of the same guarantee, in the type system: the
/// state cannot be consumed and read while a stream still points at it.
pub struct Stream<'state> {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    running: bool,
    state: PhantomData<&'state RenderState>,
}

impl<'state> Stream<'state> {
    pub fn new(device: AudioObjectID, state: &'state RenderState) -> Result<Stream<'state>, Error> {
        let mut proc_id: AudioDeviceIOProcID = None;
        // SAFETY: `proc_id` is a live local, and the client pointer is the
        // caller's `RenderState`, which outlives this `Stream` by the lifetime
        // above and is unreachable for reading until it has been dropped.
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
            state: PhantomData,
        })
    }

    pub fn start(&mut self) -> Result<(), Error> {
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

impl Drop for Stream<'_> {
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
