use core::ffi::c_void;

use lanplay_protocol::FrameId;

/// Packs a frame id into VideoToolbox's `sourceFrameRefCon`.
///
/// The result is an **opaque token, not a pointer**. VideoToolbox never
/// dereferences `sourceFrameRefCon`; it only copies the value from
/// `VTDecompressionSessionDecodeFrame` into the output callback. Encoding the
/// id directly avoids allocating a per-frame box and, more importantly,
/// removes any question of who frees it when a frame is dropped and the
/// callback fires with a null image buffer.
///
/// Anyone tempted to dereference this: it is a small integer. It will not
/// survive the attempt.
#[inline]
pub(crate) fn to_refcon(frame: FrameId) -> *mut c_void {
    // `usize` is 64 bits on every target this crate builds for, so no id is
    // ever truncated.
    frame.get() as usize as *mut c_void
}

/// Recovers the frame id packed by [`to_refcon`].
///
/// A null refcon decodes to [`FrameId::NONE`], which is exactly what the
/// reserved zero id means, so a callback for a frame we never tagged still
/// carries an honest value rather than a fabricated one.
#[inline]
pub(crate) fn from_refcon(refcon: *mut c_void) -> FrameId {
    FrameId::new(refcon as usize as u64)
}

#[cfg(test)]
mod tests {
    use super::{from_refcon, to_refcon};
    use core::ptr;
    use lanplay_protocol::FrameId;

    #[test]
    fn ids_survive_the_round_trip_through_an_opaque_pointer() {
        for raw in [1u64, 2, 599, 600, 120_000, u32::MAX as u64 + 1, u64::MAX] {
            let frame = FrameId::new(raw);
            assert_eq!(from_refcon(to_refcon(frame)), frame, "raw {raw}");
        }
    }

    #[test]
    fn null_refcon_is_the_reserved_none_id() {
        assert_eq!(from_refcon(ptr::null_mut()), FrameId::NONE);
        assert!(to_refcon(FrameId::NONE).is_null());
    }

    #[test]
    fn distinct_ids_never_collide() {
        assert_ne!(to_refcon(FrameId::new(1)), to_refcon(FrameId::new(2)));
    }
}
