use core::ffi::{c_int, c_void};
use core::ptr::{self, NonNull};

use lanplay_video_core::{ParameterSets, PixelFormat};
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionGetDimensions,
};
use objc2_core_video::{CVPixelBufferPool, kCVPixelBufferMetalCompatibilityKey};
use objc2_video_toolbox::{
    VTDecompressionSession, VTSessionCopyProperty, VTSessionSetProperty,
    kVTDecompressionPropertyKey_PixelBufferPool, kVTDecompressionPropertyKey_RealTime,
    kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
    kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder,
};

use crate::error::DecoderError;

/// Builds the format description VideoToolbox needs before it will look at a
/// single coded byte.
pub(crate) fn format_description(
    parameter_sets: &ParameterSets,
) -> Result<CFRetained<CMFormatDescription>, DecoderError> {
    if parameter_sets.sps.is_empty() || parameter_sets.pps.is_empty() {
        return Err(DecoderError::MissingParameterSets {
            sps: parameter_sets.sps.len(),
            pps: parameter_sets.pps.len(),
        });
    }
    if !matches!(parameter_sets.nal_length_size, 1 | 2 | 4) {
        return Err(DecoderError::UnsupportedNalLengthSize(
            parameter_sets.nal_length_size,
        ));
    }

    // SPS first: CoreMedia parses the dimensions out of the first parameter
    // set it is given.
    let sets: Vec<&[u8]> = parameter_sets
        .sps
        .iter()
        .chain(parameter_sets.pps.iter())
        .map(Vec::as_slice)
        .collect();
    let pointers: Vec<NonNull<u8>> = sets
        .iter()
        .map(|set| NonNull::from(*set).cast::<u8>())
        .collect();
    let sizes: Vec<usize> = sets.iter().map(|set| set.len()).collect();

    let mut out: *const CMFormatDescription = ptr::null();
    // SAFETY: `pointers` and `sizes` are parallel arrays of `sets.len()`
    // entries, each pointer borrowed from a slice that outlives this call,
    // and `out` is a live local. CoreMedia copies the parameter sets into the
    // description, so the borrows end when the call returns.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            None,
            pointers.len(),
            NonNull::from(pointers.as_slice()).cast(),
            NonNull::from(sizes.as_slice()).cast(),
            c_int::from(parameter_sets.nal_length_size),
            NonNull::from(&mut out),
        )
    };
    let created = NonNull::new(out.cast_mut()).filter(|_| status == 0);
    match created {
        // SAFETY: CoreMedia returned an owned +1 reference.
        Some(description) => Ok(unsafe { CFRetained::from_raw(description) }),
        None => Err(DecoderError::FormatDescription(status)),
    }
}

/// Coded picture size as CoreMedia parsed it out of the SPS.
pub(crate) fn dimensions(format: &CMFormatDescription) -> (u32, u32) {
    // SAFETY: `format` is a live video format description built above.
    let dimensions = unsafe { CMVideoFormatDescriptionGetDimensions(format) };
    (
        dimensions.width.max(0) as u32,
        dimensions.height.max(0) as u32,
    )
}

/// `NULL` when hardware is optional: an empty specification dictionary is not
/// the same as no dictionary, and VideoToolbox treats the absent case as
/// "choose freely".
pub(crate) fn decoder_specification(
    require_hardware: bool,
) -> Option<CFRetained<CFDictionary<CFString, CFType>>> {
    if !require_hardware {
        return None;
    }
    // SAFETY: VideoToolbox key statics are initialised by the framework
    // before any of its functions can be reached.
    let key = unsafe { kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder };
    Some(CFDictionary::from_slices(&[key], &[CFBoolean::new(true)]))
}

/// Attributes for the buffers the decoder must produce: NV12, Metal usable,
/// IOSurface backed. Anything less and the renderer would need a CPU pass.
pub(crate) fn destination_attributes(
    pixel_format: PixelFormat,
) -> CFRetained<CFDictionary<CFString, CFType>> {
    // SAFETY: CoreVideo key statics are initialised by the framework.
    let (format_key, metal_key, iosurface_key) = unsafe {
        (
            objc2_core_video::kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferMetalCompatibilityKey,
            objc2_core_video::kCVPixelBufferIOSurfacePropertiesKey,
        )
    };
    let format = CFNumber::new_i32(pixel_format.four_cc() as i32);
    // An empty dictionary means "IOSurface backed, defaults for everything
    // else"; omitting the key entirely leaves CoreVideo free to hand back
    // malloc'd memory.
    let iosurface_properties = CFDictionary::<CFString, CFType>::empty();

    CFDictionary::from_slices(
        &[format_key, metal_key, iosurface_key],
        &[&format, CFBoolean::new(true), &iosurface_properties],
    )
}

pub(crate) fn set_real_time(
    session: &VTDecompressionSession,
    enabled: bool,
) -> Result<(), DecoderError> {
    // SAFETY: the key static is framework-initialised, the session is live,
    // and `RealTime` is documented as a boolean property.
    let status = unsafe {
        VTSessionSetProperty(
            session,
            kVTDecompressionPropertyKey_RealTime,
            Some(CFBoolean::new(enabled)),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(DecoderError::Property {
            key: "RealTime",
            status,
        })
    }
}

/// Asks the live session whether it ended up on a hardware decoder.
///
/// Deliberately not derived from the creation request: VideoToolbox is
/// allowed to grant a session and still run it in software, and that is the
/// exact failure this whole phase is built to catch.
pub(crate) fn uses_hardware_decoder(session: &VTDecompressionSession) -> bool {
    copy_property(
        session,
        // SAFETY: framework-initialised key static.
        unsafe { kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder },
    )
    .as_deref()
    .and_then(CFType::downcast_ref::<CFBoolean>)
    .is_some_and(CFBoolean::as_bool)
}

/// Reads Metal compatibility back off the pixel buffer pool VideoToolbox
/// built for this session.
///
/// This is the resolved attribute dictionary of an object the framework
/// created, not an echo of what we asked for, so a request VideoToolbox chose
/// to ignore shows up here as `false`.
pub(crate) fn pool_is_metal_compatible(session: &VTDecompressionSession) -> bool {
    let Some(pool) = copy_property(
        session,
        // SAFETY: framework-initialised key static.
        unsafe { kVTDecompressionPropertyKey_PixelBufferPool },
    ) else {
        return false;
    };
    let Some(pool) = pool.downcast_ref::<CVPixelBufferPool>() else {
        return false;
    };
    let Some(attributes) = pool.pixel_buffer_attributes() else {
        return false;
    };
    // SAFETY: the pool's attribute dictionary is a CFString-keyed property
    // list of CF values, which is what the cast asserts.
    let attributes =
        unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(attributes) };
    // SAFETY: framework-initialised key static.
    attributes
        .get(unsafe { kCVPixelBufferMetalCompatibilityKey })
        .as_deref()
        .and_then(CFType::downcast_ref::<CFBoolean>)
        .is_some_and(CFBoolean::as_bool)
}

fn copy_property(session: &VTDecompressionSession, key: &CFString) -> Option<CFRetained<CFType>> {
    let mut value: *const CFType = ptr::null();
    // SAFETY: `session` is a live VTSession, `key` is a framework key, and
    // the out parameter points at a live local of the pointer type
    // VTSessionCopyProperty writes through its `void *`.
    let status =
        unsafe { VTSessionCopyProperty(session, key, None, (&raw mut value).cast::<c_void>()) };
    let value = NonNull::new(value.cast_mut()).filter(|_| status == 0)?;
    // SAFETY: VTSessionCopyProperty returns an owned +1 reference.
    Some(unsafe { CFRetained::from_raw(value) })
}
