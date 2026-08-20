//! A Mac's real output device, and the two things this project sends through
//! it.
//!
//! The first is synthetic PCM, so that the numbers the rest of the audio path
//! has been designed against stop being assumptions: what sample rate and
//! channel count the output device is actually at, how many frames it asks for
//! per IO cycle after being told what to use, how regularly it asks, and
//! whether a bounded ring fed by an ordinary thread can stay ahead of it for
//! five minutes without a gap. That is [`run`], and it decodes and receives
//! nothing.
//!
//! The second is the far end of the stream. [`receive`] takes RTP off a socket,
//! orders it through `lanplay-audio-codec`'s jitter buffer, decodes it with
//! Opus and fills the same ring the tone probe fills, so the sink the buffer
//! was measured against in the phase before this one is finally a device
//! instead of a clock. Both run the same callback out of [`stream`], because a
//! second copy of it would be a second place for the buffer-layout check and
//! the zero-fill to disagree, and the disagreement would only ever be heard.
//!
//! # The route, and what was rejected
//!
//! The output is a HAL IOProc, registered with
//! [`AudioDeviceCreateIOProcID`][ioproc] and started with `AudioDeviceStart`,
//! with the frames per cycle set through
//! [`kAudioDevicePropertyBufferFrameSize`][fsiz] — "a UInt32 whose value
//! indicates the number of frames in the IO buffers", whose header also tells
//! clients to listen for it changing, which is why this probe reads it back
//! rather than believing its own request. An IOProc is the callback the HAL
//! itself calls: Apple's driver documentation describes the HAL waking its own
//! thread and calling the client's `AudioDeviceIOProc` with the buffers and the
//! timestamps for the cycle, and Apple's engineers describe that thread as the
//! HAL's real-time thread, joined to an audio workgroup. There is nothing
//! between it and the device, which is exactly what a phase about real buffer
//! sizes and real cadence needs.
//!
//! Three other routes were considered and rejected.
//!
//! `AVAudioEngine` and its player nodes schedule buffers on your behalf. It is
//! the API to reach for when the question is "play this", and it is useless
//! when the question is "how big is the buffer and when does it come", because
//! answering that is precisely the work it hides. Nothing in this phase would
//! have been measured; the report would have described AVFoundation's
//! scheduling policy.
//!
//! `AudioQueue` is the same objection in an older shape. Buffers are enqueued
//! and handed back when consumed, so a program can see how often it is asked
//! for one but not what the device's IO cycle is, and it has no equivalent of
//! setting the cycle's size.
//!
//! AUHAL — an `AudioUnit` of subtype `kAudioUnitSubType_HALOutput` with a
//! render callback — was the closest call, and it is what most low-latency Mac
//! audio is built on. It was rejected on one specific ground: [TN2091][tn2091]
//! documents that the AUHAL flattens a device's streams and carries a built-in
//! `AudioConverter`, chosen by comparing the device's format with the client's,
//! so a client format that differs from the device's is silently converted. In
//! this phase a silent conversion is the one outcome that would invalidate
//! everything, because whether a converter is needed at all is the finding. The
//! buffer size would also still have been set through the same HAL property on
//! the same device object, so the audio unit would have added a converter
//! without adding an answer. Its cost is that a HAL IOProc has to handle both
//! interleaved and per-channel buffer layouts itself, which [`run`] does.
//!
//! [ioproc]: https://developer.apple.com/documentation/coreaudio/audiodevicecreateioprocid(_:_:_:_:)
//! [fsiz]: https://developer.apple.com/documentation/coreaudio/kaudiodevicepropertybufferframesize
//! [tn2091]: https://developer.apple.com/library/archive/technotes/tn2091/_index.html
//!
//! # The shape
//!
//! ```text
//! producer thread  ->  bounded PCM ring  ->  render callback
//! ```
//!
//! [`PcmRing`] is the only thing the two threads share, and it is lock-free
//! because the consumer is the HAL's real-time thread: a mutex there would let
//! an ordinary thread's scheduling decide whether audio arrives on time. The
//! callback takes what the ring has, writes silence over whatever it was short
//! of, counts that shortfall as an underrun, and returns. It never waits and it
//! never asks the producer for anything.
//!
//! Nothing about the device's format is assumed. If it is not the 48000 Hz
//! stereo the Windows endpoint mixes at, the tone is generated at the device's
//! own rate — the generator takes a sample rate, so that is asking it for a
//! different tone rather than converting one — and the disagreement is printed
//! as the finding it is. What is refused, loudly, is an output that is not
//! 32-bit float, because filling that would mean a conversion, and a converter
//! inserted here would make every number in the report a statement about the
//! converter.

pub mod excess;
pub mod format;
pub mod occupancy;
pub mod pairs;
pub mod report;
pub mod ring;

#[cfg(target_os = "macos")]
pub mod device;
#[cfg(target_os = "macos")]
pub mod receive;
#[cfg(target_os = "macos")]
pub mod receive_envelope;
#[cfg(target_os = "macos")]
pub mod run;
#[cfg(target_os = "macos")]
pub mod stream;

pub use excess::{ExcessCurve, ExcessReport, ExcessTrace};
pub use format::{Layout, OutputFormat, SampleKind};
pub use occupancy::{OccupancyReader, WindowOccupancy};
pub use report::{Report, Verdict};
pub use ring::{Drained, Filled, PcmRing};

#[cfg(target_os = "macos")]
pub use device::Error;
#[cfg(target_os = "macos")]
pub use receive::{Receipt, ReceiveError, ReceiveOptions, receive};
#[cfg(target_os = "macos")]
pub use run::{LEVEL_DBFS, Options, run};
