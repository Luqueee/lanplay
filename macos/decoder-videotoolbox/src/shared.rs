use core::ffi::c_void;
use core::panic::AssertUnwindSafe;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;
use std::ptr::NonNull;
use std::thread;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{Recorder, Stage, Timestamp};
use lanplay_video_core::VideoTimestamp;
use objc2_core_foundation::CFRetained;
use objc2_core_media::CMTime;
use objc2_core_video::{CVImageBuffer, CVPixelBuffer};
use objc2_video_toolbox::VTDecodeInfoFlags;
use parking_lot::Mutex;

use crate::description::PixelBufferDescription;
use crate::token::from_refcon;
use crate::{DecodedFrame, FrameSink};

/// State shared between the owning [`crate::VideoToolboxDecoder`] and the
/// VideoToolbox output callback.
///
/// Lives in an `Arc`; one strong count is leaked into the session as the
/// output refcon and reclaimed only after the session has been invalidated.
pub(crate) struct Shared {
    recorder: Recorder,
    sink: Mutex<FrameSink>,
    callback_delay: Option<Duration>,
    submitted: AtomicU64,
    decoded: AtomicU64,
    dropped: AtomicU64,
    errors: AtomicU64,
    /// The first non-zero `OSStatus` the callback ever reported.
    ///
    /// A count of errors says a decode failed; a status says why. One bad
    /// access unit costs every frame up to the next IDR, so the difference
    /// between "rejected the parameter sets" and "corrupt slice data" decides
    /// whether the fix is on the wire or in the session.
    first_error_status: AtomicI32,
    in_flight: AtomicUsize,
    /// Written once, before the first submit, from the session's own pixel
    /// buffer pool.
    metal_compatible: AtomicBool,
    description: Mutex<Option<PixelBufferDescription>>,
    description_captured: AtomicBool,
}

impl Shared {
    pub(crate) fn new(
        recorder: Recorder,
        sink: FrameSink,
        callback_delay: Option<Duration>,
    ) -> Shared {
        Shared {
            recorder,
            sink: Mutex::new(sink),
            callback_delay,
            submitted: AtomicU64::new(0),
            decoded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            first_error_status: AtomicI32::new(0),
            in_flight: AtomicUsize::new(0),
            metal_compatible: AtomicBool::new(false),
            description: Mutex::new(None),
            description_captured: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_metal_compatible(&self, value: bool) {
        self.metal_compatible.store(value, Ordering::Relaxed);
    }

    pub(crate) fn mark_submit(&self, frame: FrameId) {
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        self.submitted.fetch_add(1, Ordering::Relaxed);
        self.recorder.mark(frame, Stage::DecodeSubmit);
    }

    /// Undoes [`Shared::mark_submit`] when VideoToolbox rejected the frame, in
    /// which case no callback will ever fire for it.
    pub(crate) fn rollback_submit(&self) {
        self.in_flight.fetch_sub(1, Ordering::AcqRel);
        self.submitted.fetch_sub(1, Ordering::Relaxed);
        self.errors.fetch_add(1, Ordering::Relaxed);
    }

    /// Keeps the first status only: after one failure the stream is broken
    /// until the next IDR, so the statuses that follow describe the wreckage
    /// rather than the cause.
    pub(crate) fn record_error_status(&self, status: i32) {
        let _ = self.first_error_status.compare_exchange(
            0,
            status,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    pub(crate) fn first_error_status(&self) -> Option<i32> {
        match self.first_error_status.load(Ordering::Relaxed) {
            0 => None,
            status => Some(status),
        }
    }

    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    pub(crate) fn submitted(&self) -> u64 {
        self.submitted.load(Ordering::Relaxed)
    }

    pub(crate) fn decoded(&self) -> u64 {
        self.decoded.load(Ordering::Relaxed)
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn errors(&self) -> u64 {
        self.errors.load(Ordering::Relaxed)
    }

    pub(crate) fn description(&self) -> Option<PixelBufferDescription> {
        self.description.lock().clone()
    }

    /// Everything that happens inside the callback, minus the bookkeeping that
    /// must run even if this unwinds.
    fn deliver(
        &self,
        frame: FrameId,
        status: i32,
        image_buffer: *mut CVImageBuffer,
        pts: CMTime,
    ) -> Outcome {
        // First action, before any sleep: the whole point of DecodeComplete is
        // that it is the instant VideoToolbox handed the frame back. Anything
        // in front of it would be charged to the decoder and would hide a
        // stalled consumer inside the Decode segment instead of exposing it as
        // PresentationWait.
        let decoded_at = Timestamp::now();
        self.recorder
            .mark_at(frame, Stage::DecodeComplete, decoded_at);

        if let Some(delay) = self.callback_delay {
            thread::sleep(delay);
        }

        if status != 0 {
            self.record_error_status(status);
            return Outcome::Failed;
        }
        let Some(image_buffer) = NonNull::new(image_buffer) else {
            return Outcome::Dropped;
        };

        // SAFETY: a non-null image buffer from the decompression output
        // callback is a live CVPixelBuffer owned by VideoToolbox; retaining it
        // is what keeps it alive past the callback, which is exactly the
        // contract for handing it to the renderer.
        let pixel_buffer: CFRetained<CVPixelBuffer> = unsafe { CFRetained::retain(image_buffer) };

        if !self.description_captured.swap(true, Ordering::AcqRel) {
            let metal_compatible = self.metal_compatible.load(Ordering::Relaxed);
            *self.description.lock() = Some(PixelBufferDescription::capture(
                &pixel_buffer,
                metal_compatible,
            ));
        }

        // A CMTime with a zero timescale carries no rate; keep the timescale
        // the sample was submitted with so the id and the pts stay comparable
        // with what the source produced.
        let pts = VideoTimestamp {
            value: pts.value,
            timescale: pts.timescale.max(0) as u32,
        };

        (self.sink.lock())(DecodedFrame {
            id: frame,
            pts,
            pixel_buffer,
            decoded_at,
        });
        Outcome::Delivered
    }
}

enum Outcome {
    Delivered,
    /// Callback with a null image buffer: VideoToolbox decided not to emit a
    /// picture for this access unit.
    Dropped,
    Failed,
}

/// The C entry point VideoToolbox calls for every submitted frame.
///
/// # Safety
///
/// `output_refcon` must be the `Arc<Shared>` raw pointer handed to
/// `VTDecompressionSessionCreate`, and the `Arc` must still hold that strong
/// count. [`crate::VideoToolboxDecoder`] guarantees this by invalidating the
/// session before releasing the pointer.
pub(crate) unsafe extern "C-unwind" fn output_callback(
    output_refcon: *mut c_void,
    source_frame_refcon: *mut c_void,
    status: i32,
    _info: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: the caller contract above; the pointer came from
    // `Arc::into_raw` and the session outlives no callback.
    let shared: &Shared = unsafe { &*output_refcon.cast::<Shared>() };
    let frame = from_refcon(source_frame_refcon);

    // The sink is caller code. Letting a panic unwind through VideoToolbox's
    // own frames would be undefined behaviour, so it is caught here and
    // counted like any other failed frame.
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        shared.deliver(frame, status, image_buffer, pts)
    }));

    // Decremented after the sink has run, not before: in_flight is the
    // consumer-visible backlog, so a slow sink must show up here.
    shared.in_flight.fetch_sub(1, Ordering::AcqRel);

    match outcome {
        Ok(Outcome::Delivered) => {
            shared.decoded.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Outcome::Dropped) => {
            shared.dropped.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Outcome::Failed) | Err(_) => {
            shared.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}
