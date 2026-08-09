use core::ffi::c_void;
use core::ptr::{self, NonNull};

use objc2_core_foundation::{CFRetained, kCFAllocatorDefault};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    kCMBlockBufferAssureMemoryNowFlag,
};

use crate::error::DecoderError;

pub(crate) fn valid_time(value: i64, timescale: u32) -> CMTime {
    CMTime {
        value,
        timescale: timescale as i32,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

pub(crate) const INVALID_TIME: CMTime = CMTime {
    value: 0,
    timescale: 0,
    flags: CMTimeFlags::empty(),
    epoch: 0,
};

/// Wraps AVCC bytes in a sample buffer VideoToolbox can own.
///
/// The bytes are copied into CoreMedia-allocated storage rather than lent
/// from the caller's `Vec`. Asynchronous decode means the sample outlives the
/// `submit` call, and a `Vec` that reallocates or drops underneath a live
/// sample buffer is a use-after-free with a decoder thread on the other end.
/// One copy of a ~40 KB compressed frame is the price; no pixel data is
/// touched.
pub(crate) fn sample_buffer(
    data: &[u8],
    format: &CMFormatDescription,
    timing: CMSampleTimingInfo,
) -> Result<CFRetained<CMSampleBuffer>, DecoderError> {
    let block = block_buffer(data)?;

    let mut out: *mut CMSampleBuffer = ptr::null_mut();
    let size = data.len();
    // SAFETY: `block` holds exactly `size` bytes of ready data, `timing` and
    // `size` are live locals covering the single sample described, and `out`
    // is a live local. CoreMedia retains the block buffer and the format
    // description itself.
    let status = unsafe {
        CMSampleBuffer::create(
            kCFAllocatorDefault,
            Some(&block),
            true,
            None,
            ptr::null_mut(),
            Some(format),
            1,
            1,
            &timing,
            1,
            &size,
            NonNull::from(&mut out),
        )
    };
    match NonNull::new(out).filter(|_| status == 0) {
        // SAFETY: CoreMedia returned an owned +1 reference.
        Some(sample) => Ok(unsafe { CFRetained::from_raw(sample) }),
        None => Err(DecoderError::SampleBuffer(status)),
    }
}

fn block_buffer(data: &[u8]) -> Result<CFRetained<CMBlockBuffer>, DecoderError> {
    let mut out: *mut CMBlockBuffer = ptr::null_mut();
    // SAFETY: a null memory block with a non-null allocator and the
    // assure-memory-now flag asks CoreMedia to allocate and own `data.len()`
    // bytes; `out` is a live local.
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            kCFAllocatorDefault,
            ptr::null_mut(),
            data.len(),
            kCFAllocatorDefault,
            ptr::null(),
            0,
            data.len(),
            kCMBlockBufferAssureMemoryNowFlag,
            NonNull::from(&mut out),
        )
    };
    let block = match NonNull::new(out).filter(|_| status == 0) {
        // SAFETY: CoreMedia returned an owned +1 reference.
        Some(block) => unsafe { CFRetained::from_raw(block) },
        None => return Err(DecoderError::BlockBuffer(status)),
    };

    // SAFETY: `data` is a live slice of `data.len()` bytes and the block was
    // just created with exactly that capacity, memory assured.
    let status = unsafe {
        CMBlockBuffer::replace_data_bytes(
            NonNull::from(data).cast::<c_void>(),
            &block,
            0,
            data.len(),
        )
    };
    if status == 0 {
        Ok(block)
    } else {
        Err(DecoderError::BlockBuffer(status))
    }
}
