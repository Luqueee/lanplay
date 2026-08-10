//! Hardware H.264 decode on macOS, with the honesty checks that make a
//! latency measurement worth taking.
//!
//! Two things distinguish this from the usual VideoToolbox wrapper. First,
//! there is no software fallback: if a hardware decoder was required and the
//! live session says it is not using one, construction fails, because every
//! millisecond measured on a software decoder would be a number about a
//! machine nobody is shipping. Second, the frames it emits are IOSurface
//! backed NV12 buffers handed on by reference; nothing on this path copies a
//! pixel.
//!
//! ```no_run
//! use std::time::Duration;
//! use lanplay_decoder_videotoolbox::{DecoderConfig, VideoToolboxDecoder};
//! use lanplay_telemetry::{Telemetry, TelemetryConfig};
//! use lanplay_video_core::{PixelFormat, VideoDecoder, parse_stream};
//!
//! let bytes = std::fs::read("fixture.h264")?;
//! let (parameter_sets, units) = parse_stream(&bytes)?;
//! let telemetry = Telemetry::start(TelemetryConfig::default());
//!
//! let mut decoder = VideoToolboxDecoder::new(
//!     DecoderConfig {
//!         parameter_sets,
//!         width: 1920,
//!         height: 1080,
//!         pixel_format: PixelFormat::Nv12VideoRange,
//!         require_hardware: true,
//!         realtime: true,
//!         callback_delay: None,
//!     },
//!     telemetry.recorder(),
//!     Box::new(|frame| drop(frame)),
//! )?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod description;
mod error;
mod sample;
mod session;
mod shared;
mod token;

use core::ffi::c_void;
use core::ptr::{self, NonNull};
use core::time::Duration;
use std::sync::Arc;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{Recorder, Timestamp};
use lanplay_video_core::{
    EncodedAccessUnit, ParameterSets, PixelFormat, VideoDecoder, VideoTimestamp,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMFormatDescription, CMSampleTimingInfo};
use objc2_core_video::CVPixelBuffer;
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
};

pub use description::PixelBufferDescription;
pub use error::DecoderError;

use sample::{INVALID_TIME, sample_buffer, valid_time};
use shared::{Shared, output_callback};
use token::to_refcon;

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DecoderConfig {
    pub parameter_sets: ParameterSets,
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    /// Refuse to run on a software decoder. True in every real run.
    pub require_hardware: bool,
    /// Tell VideoToolbox to favour latency over throughput. True in every
    /// real run.
    pub realtime: bool,
    /// Test-only knob: sleep this long inside the output callback to model a
    /// slow consumer. Used by the phase 2 negative test; never set in a real
    /// run.
    pub callback_delay: Option<Duration>,
}

/// A finished frame, still on the GPU.
///
/// `pixel_buffer` is a reference to VideoToolbox's own IOSurface-backed
/// buffer. Holding one keeps that surface out of the decoder's pool, so drop
/// it as soon as the renderer has a texture for it.
pub struct DecodedFrame {
    pub id: FrameId,
    pub pts: VideoTimestamp,
    pub pixel_buffer: CFRetained<CVPixelBuffer>,
    /// When the output callback observed the frame. Taken with the same clock
    /// read that produced the `DecodeComplete` mark, so the two never
    /// disagree.
    pub decoded_at: Timestamp,
}

// SAFETY: `CVPixelBuffer` is a CoreFoundation object whose retain and release
// are atomic, and CoreVideo documents pixel buffers as safe to hand from the
// decoder's callback thread to a renderer. Only `Send` is claimed: ownership
// of the frame is transferred, never shared, so no two threads can touch one
// buffer at the same time.
unsafe impl Send for DecodedFrame {}

/// Called on a VideoToolbox decode thread, once per finished frame.
///
/// It runs on the critical path: whatever it does is charged to the frame it
/// was handed. Hand the buffer to a latest-frame-wins slot and return.
pub type FrameSink = Box<dyn FnMut(DecodedFrame) + Send>;

pub struct VideoToolboxDecoder {
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    shared: Arc<Shared>,
    uses_hardware: bool,
    /// Previous presentation timestamp, used only to give each sample a
    /// truthful duration; a live stream does not know the next frame's time.
    previous_pts: Option<VideoTimestamp>,
}

/// The decoder's counters, readable from another thread.
///
/// The decoder itself is owned by whichever thread submits to it, so a
/// sampler cannot ask it anything. This shares the same atomics rather than
/// copying them, which is why a window can report decode rate without the
/// receive loop publishing it.
#[derive(Clone)]
pub struct DecoderCounters(Arc<Shared>);

impl DecoderCounters {
    pub fn decoded(&self) -> u64 {
        self.0.decoded()
    }

    pub fn in_flight(&self) -> usize {
        self.0.in_flight()
    }
}

// SAFETY: every field is owned by this struct and safe to use from whichever
// thread owns it. VideoToolbox documents decompression sessions as callable
// from any thread, CoreMedia format descriptions are immutable, and `Shared`
// is `Sync`. The bound that matters in practice is that a client can create
// the decoder on its setup thread and submit from its receive thread.
unsafe impl Send for VideoToolboxDecoder {}

impl VideoToolboxDecoder {
    pub fn new(
        config: DecoderConfig,
        recorder: Recorder,
        sink: FrameSink,
    ) -> Result<Self, DecoderError> {
        let format = session::format_description(&config.parameter_sets)?;
        let actual = session::dimensions(&format);
        if actual != (config.width, config.height) {
            return Err(DecoderError::DimensionMismatch {
                expected: (config.width, config.height),
                actual,
            });
        }
        let specification = session::decoder_specification(config.require_hardware);
        let attributes = session::destination_attributes(config.pixel_format);

        let shared = Arc::new(Shared::new(recorder, sink, config.callback_delay));
        // One strong count is handed to VideoToolbox as the output refcon and
        // reclaimed in `Drop`, after the session is invalidated.
        let refcon = Arc::into_raw(Arc::clone(&shared))
            .cast_mut()
            .cast::<c_void>();
        let callback = VTDecompressionOutputCallbackRecord {
            decompressionOutputCallback: Some(output_callback),
            decompressionOutputRefCon: refcon,
        };

        let mut out: *mut VTDecompressionSession = ptr::null_mut();
        // SAFETY: the format description and both dictionaries are live for
        // the call, the callback record points at a leaked `Arc<Shared>` that
        // outlives the session, and `out` is a live local.
        let status = unsafe {
            VTDecompressionSession::create(
                None,
                &format,
                specification.as_deref().map(|dict| dict.as_opaque()),
                Some(attributes.as_opaque()),
                &callback,
                NonNull::from(&mut out),
            )
        };
        let session = match NonNull::new(out).filter(|_| status == 0) {
            // SAFETY: VideoToolbox returned an owned +1 reference.
            Some(session) => unsafe { CFRetained::from_raw(session) },
            None => {
                // SAFETY: no session exists, so the callback can never fire;
                // the leaked strong count is ours to take back.
                drop(unsafe { Arc::from_raw(refcon.cast::<Shared>()) });
                return Err(DecoderError::SessionCreation {
                    status,
                    require_hardware: config.require_hardware,
                });
            }
        };

        let decoder = VideoToolboxDecoder {
            uses_hardware: session::uses_hardware_decoder(&session),
            session,
            format,
            shared,
            previous_pts: None,
        };

        if config.require_hardware && !decoder.uses_hardware {
            return Err(DecoderError::SoftwareDecoder);
        }
        if config.realtime {
            session::set_real_time(&decoder.session, true)?;
        }
        decoder
            .shared
            .set_metal_compatible(session::pool_is_metal_compatible(&decoder.session));

        Ok(decoder)
    }

    /// Read back from the live session, not from what was requested.
    pub fn uses_hardware_decoder(&self) -> bool {
        self.uses_hardware
    }

    /// Frames submitted but not yet through the sink. The backlog metric: it
    /// counts a slow consumer as well as a slow decoder, because from the
    /// pipeline's point of view they are the same stall.
    pub fn in_flight(&self) -> usize {
        self.shared.in_flight()
    }

    /// A cheap, cloneable view of the counters, for a sampler that runs
    /// beside the thread owning the decoder rather than inside it.
    pub fn counters(&self) -> DecoderCounters {
        DecoderCounters(Arc::clone(&self.shared))
    }

    pub fn submitted(&self) -> u64 {
        self.shared.submitted()
    }

    pub fn decoded(&self) -> u64 {
        self.shared.decoded()
    }

    /// Callbacks that arrived with a null image buffer.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped()
    }

    /// Callbacks that reported a non-zero status, plus submissions
    /// VideoToolbox refused outright.
    pub fn errors(&self) -> u64 {
        self.shared.errors()
    }

    /// Captured from the first decoded frame; `None` until one arrives.
    pub fn pixel_buffer_description(&self) -> Option<PixelBufferDescription> {
        self.shared.description()
    }

    /// One frame's worth of ticks, inferred from the gap to the previous
    /// timestamp. The first frame has nothing to measure against and gets a
    /// single tick; nothing downstream schedules from sample duration, so the
    /// value only has to be honest, not predictive.
    fn duration_for(&self, pts: VideoTimestamp) -> i64 {
        match self.previous_pts {
            Some(previous) if previous.timescale == pts.timescale && pts.value > previous.value => {
                pts.value - previous.value
            }
            _ => 1,
        }
    }
}

impl VideoDecoder for VideoToolboxDecoder {
    type Error = DecoderError;

    fn submit(&mut self, access_unit: &EncodedAccessUnit) -> Result<(), Self::Error> {
        let pts = access_unit.pts;
        let timing = CMSampleTimingInfo {
            duration: valid_time(self.duration_for(pts), pts.timescale),
            presentationTimeStamp: valid_time(pts.value, pts.timescale),
            // The stream has no reordering, so a decode timestamp would add
            // nothing a decoder could act on.
            decodeTimeStamp: INVALID_TIME,
        };
        let sample = sample_buffer(&access_unit.data, &self.format, timing)?;
        self.previous_pts = Some(pts);

        self.shared.mark_submit(access_unit.id);
        // SAFETY: the sample buffer is live for the call and retained by
        // VideoToolbox for as long as it needs it; the refcon is an opaque
        // frame-id token, never dereferenced.
        let status = unsafe {
            self.session.decode_frame(
                &sample,
                VTDecodeFrameFlags::Frame_EnableAsynchronousDecompression,
                to_refcon(access_unit.id),
                ptr::null_mut(),
            )
        };
        if status != 0 {
            // No callback fires for a rejected frame, so the submit
            // bookkeeping has to be undone here or in_flight never settles.
            self.shared.rollback_submit();
            return Err(DecoderError::DecodeFrame(status));
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        // SAFETY: the session is live and owned by this decoder.
        let status = unsafe { self.session.wait_for_asynchronous_frames() };
        if status == 0 {
            Ok(())
        } else {
            Err(DecoderError::WaitForFrames(status))
        }
    }
}

impl Drop for VideoToolboxDecoder {
    fn drop(&mut self) {
        // SAFETY: invalidate first. It tears the session down synchronously
        // and guarantees no further output callback will run, which is the
        // only thing that makes the `Arc::from_raw` below sound: the refcon
        // pointer must stay valid for as long as a callback could still
        // dereference it.
        unsafe { self.session.invalidate() };
        // SAFETY: `Arc::into_raw` in `new` leaked exactly one strong count for
        // this pointer, no callback can be running, and this runs once.
        drop(unsafe { Arc::from_raw(Arc::as_ptr(&self.shared)) });
    }
}
