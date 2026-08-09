use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lanplay_protocol::FrameId;
use lanplay_telemetry::Timestamp;
use objc2_core_foundation::CFRetained;
use objc2_core_video::CVPixelBuffer;
use parking_lot::Mutex;

/// A decoded picture on its way to the screen.
///
/// The pixel buffer is IOSurface-backed and still owned by the decoder's pool;
/// holding this struct is what keeps the surface out of that pool, so it must
/// be dropped as soon as the GPU is done with it.
pub struct SurfaceFrame {
    pub id: FrameId,
    pub pixel_buffer: CFRetained<CVPixelBuffer>,
    pub decoded_at: Timestamp,
}

// SAFETY: `CVPixelBuffer` is a CoreFoundation object whose retain and release
// are atomic, and CoreVideo documents pixel buffers as safe to hand from the
// decoder's callback thread to a renderer. Only `Send` is claimed: the slot
// below moves whole frames between threads and never lends out a shared
// reference, so no two threads can touch one buffer at the same time.
unsafe impl Send for SurfaceFrame {}

/// The project's presentation rule, in one object: a producer publishes and a
/// renderer takes, and only the newest frame survives in between.
///
/// A queue is the obvious alternative and the wrong one. Queued frames turn a
/// momentary producer burst into permanent added latency, because every later
/// frame waits behind the backlog. Dropping instead keeps the delay bounded by
/// one display interval no matter how far the producer runs ahead, and a frame
/// that would have been shown late is worth less than the one after it.
pub struct LatestFrameSlot {
    frame: Mutex<Option<SurfaceFrame>>,
    published: AtomicU64,
    superseded: AtomicU64,
}

impl LatestFrameSlot {
    pub fn new() -> Arc<Self> {
        Arc::new(LatestFrameSlot {
            frame: Mutex::new(None),
            published: AtomicU64::new(0),
            superseded: AtomicU64::new(0),
        })
    }

    /// Makes `frame` the one the renderer will pick up next, discarding any
    /// frame that has not been taken yet.
    pub fn publish(&self, frame: SurfaceFrame) {
        // The guard is a statement temporary, so the displaced frame's
        // `CFRelease` runs after the lock is dropped rather than under it.
        let displaced = self.frame.lock().replace(frame);
        self.published.fetch_add(1, Ordering::Relaxed);
        if displaced.is_some() {
            self.superseded.fetch_add(1, Ordering::Relaxed);
        }
        drop(displaced);
    }

    /// Removes and returns the pending frame, if any.
    pub fn take(&self) -> Option<SurfaceFrame> {
        self.frame.lock().take()
    }

    /// Frames handed to [`LatestFrameSlot::publish`] over the slot's life.
    pub fn published(&self) -> u64 {
        self.published.load(Ordering::Relaxed)
    }

    /// Frames replaced before anyone took them: the producer outran the
    /// display, and this is how much work was thrown away.
    pub fn superseded(&self) -> u64 {
        self.superseded.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{CVPixelBuffer, CVPixelBufferCreate, kCVReturnSuccess};

    use super::*;

    /// A minimal real pixel buffer. The slot must move genuine CoreVideo
    /// handles around, not a stand-in, or it proves nothing about ownership.
    fn frame(id: u64) -> SurfaceFrame {
        let mut raw: *mut CVPixelBuffer = core::ptr::null_mut();
        // SAFETY: `raw` is a live local, and no attributes dictionary is
        // passed, so there are no generics to get wrong.
        let status = unsafe {
            CVPixelBufferCreate(
                None,
                16,
                16,
                u32::from_be_bytes(*b"420v"),
                None,
                NonNull::from(&mut raw),
            )
        };
        assert_eq!(status, kCVReturnSuccess);
        let buffer = NonNull::new(raw).expect("CVPixelBufferCreate reported success");
        SurfaceFrame {
            id: FrameId::new(id),
            // SAFETY: `CVPixelBufferCreate` follows the create rule, so we own
            // the only reference and hand it straight to `CFRetained`.
            pixel_buffer: unsafe { CFRetained::from_raw(buffer) },
            decoded_at: Timestamp::now(),
        }
    }

    #[test]
    fn take_returns_the_published_frame_once() {
        let slot = LatestFrameSlot::new();
        assert!(slot.take().is_none());

        slot.publish(frame(7));
        assert_eq!(slot.take().map(|f| f.id.get()), Some(7));
        assert!(slot.take().is_none());

        assert_eq!(slot.published(), 1);
        assert_eq!(slot.superseded(), 0);
    }

    #[test]
    fn publishing_over_an_unconsumed_frame_keeps_only_the_newest() {
        let slot = LatestFrameSlot::new();
        slot.publish(frame(1));
        slot.publish(frame(2));
        slot.publish(frame(3));

        assert_eq!(slot.take().map(|f| f.id.get()), Some(3));
        assert!(slot.take().is_none());
        assert_eq!(slot.published(), 3);
        assert_eq!(slot.superseded(), 2);
    }

    #[test]
    fn a_consumed_frame_is_not_counted_as_superseded() {
        let slot = LatestFrameSlot::new();
        for id in 1..=4 {
            slot.publish(frame(id));
            assert_eq!(slot.take().map(|f| f.id.get()), Some(id));
        }
        assert_eq!(slot.published(), 4);
        assert_eq!(slot.superseded(), 0);
    }

    #[test]
    fn the_slot_releases_the_frame_it_drops() {
        let slot = LatestFrameSlot::new();
        let kept = frame(1);
        let watched = CFRetained::clone(&kept.pixel_buffer);
        let before = watched.retain_count();

        slot.publish(kept);
        slot.publish(frame(2));

        assert_eq!(
            watched.retain_count(),
            before - 1,
            "superseding a frame must release its pixel buffer back to the pool"
        );
    }

    #[test]
    fn publishing_from_another_thread_reaches_the_consumer() {
        let slot = LatestFrameSlot::new();
        let producer = Arc::clone(&slot);
        std::thread::spawn(move || producer.publish(frame(42)))
            .join()
            .unwrap();
        assert_eq!(slot.take().map(|f| f.id.get()), Some(42));
    }
}
