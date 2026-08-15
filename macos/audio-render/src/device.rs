//! The default output device, asked what it is and told what buffer size to
//! use.
//!
//! Every question here goes through `AudioObjectGetPropertyData`, which is the
//! whole of the HAL's property interface: an object id, a selector, a scope and
//! an element, and a buffer to put the answer in. The generic helpers below
//! exist because getting the size argument wrong is how a property read
//! silently returns half a structure, and doing it once is easier to be sure of
//! than doing it eleven times.
//!
//! Nothing here converts anything. The format is read and reported as it is
//! found, and the buffer size is asked for and then read back rather than
//! assumed: Apple's own guidance for the analogous request on the other
//! platform is that a preferred value is a hint and the value in force after
//! the request is the one that counts, and `kAudioDevicePropertyBufferFrameSize`
//! carries the same caution in its header, where clients are told to listen for
//! it changing underneath them.

use core::ffi::c_void;
use core::fmt;
use core::mem::{MaybeUninit, size_of};
use core::ptr::{NonNull, null};

use objc2_core_audio::{
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectSetPropertyData, kAudioDevicePropertyBufferFrameSize,
    kAudioDevicePropertyBufferFrameSizeRange, kAudioDevicePropertyStreams,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeOutput,
    kAudioObjectSystemObject, kAudioObjectUnknown, kAudioStreamPropertyPhysicalFormat,
    kAudioStreamPropertyVirtualFormat,
};
use objc2_core_audio_types::{
    AudioStreamBasicDescription, AudioValueRange, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsNonInterleaved, kAudioFormatLinearPCM,
};
use objc2_core_foundation::{CFRetained, CFString};

use crate::format::{Layout, OutputFormat, SampleKind};

/// A CoreAudio call that failed, or a machine that cannot serve this run.
#[derive(Clone, Debug)]
pub enum Error {
    /// A HAL call returned a non-zero status.
    Api { call: &'static str, status: i32 },
    /// The machine or the device cannot do what this probe needs, with the
    /// finding spelled out rather than reduced to a code.
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api { call, status } => match four_cc(*status) {
                Some(code) => write!(f, "{call} failed: {status} '{code}'"),
                None => write!(f, "{call} failed: {status}"),
            },
            Error::Unsupported(why) => write!(f, "{why}"),
        }
    }
}

impl core::error::Error for Error {}

/// CoreAudio states most of its errors as four printable characters packed into
/// an integer, and the integer on its own is a number nobody can look up.
fn four_cc(status: i32) -> Option<String> {
    let bytes = (status as u32).to_be_bytes();
    bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
        .then(|| bytes.iter().map(|byte| *byte as char).collect())
}

fn status(call: &'static str, status: i32) -> Result<(), Error> {
    if status == 0 {
        Ok(())
    } else {
        Err(Error::Api { call, status })
    }
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads one fixed-size property.
///
/// The size is checked on the way out as well as passed in, because a property
/// that answered with fewer bytes than `T` needs would otherwise leave part of
/// the value uninitialised and the caller none the wiser.
fn property<T>(
    call: &'static str,
    object: AudioObjectID,
    selector: u32,
    scope: u32,
) -> Result<T, Error> {
    let mut address = address(selector, scope);
    let mut size = size_of::<T>() as u32;
    let mut value = MaybeUninit::<T>::uninit();
    // SAFETY: `address` and `size` are live locals, no qualifier is needed by
    // any property this crate reads, and `value` is exactly `size` bytes of
    // writable storage aligned for `T`.
    let result = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(value.as_mut_ptr().cast::<c_void>()).expect("a live local"),
        )
    };
    status(call, result)?;
    if size as usize != size_of::<T>() {
        return Err(Error::Unsupported(format!(
            "{call} answered with {size} bytes where {} were expected",
            size_of::<T>()
        )));
    }
    // SAFETY: the call succeeded and reported that it filled the whole of the
    // buffer, so the value is initialised.
    Ok(unsafe { value.assume_init() })
}

fn set_property<T>(
    call: &'static str,
    object: AudioObjectID,
    selector: u32,
    scope: u32,
    value: T,
) -> Result<(), Error> {
    let mut address = address(selector, scope);
    let mut value = value;
    // SAFETY: `address` and `value` are live locals and `value` really is
    // `size_of::<T>()` bytes long.
    let result = unsafe {
        AudioObjectSetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            null(),
            size_of::<T>() as u32,
            NonNull::from(&mut value).cast::<c_void>(),
        )
    };
    status(call, result)
}

/// Reads a property whose length the device decides, such as its list of
/// streams.
fn property_list<T: Copy + Default>(
    call: &'static str,
    object: AudioObjectID,
    selector: u32,
    scope: u32,
) -> Result<Vec<T>, Error> {
    let mut address = address(selector, scope);
    let mut size = 0u32;
    // SAFETY: `address` and `size` are live locals and no qualifier is needed.
    let result = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&mut address),
            0,
            null(),
            NonNull::from(&mut size),
        )
    };
    status(call, result)?;

    let count = size as usize / size_of::<T>();
    let mut values = vec![T::default(); count];
    if count == 0 {
        return Ok(values);
    }
    // SAFETY: `values` holds exactly `size` bytes, which is what the call above
    // said the property needs.
    let result = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut address),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new(values.as_mut_ptr().cast::<c_void>()).expect("a non-empty vector"),
        )
    };
    status(call, result)?;
    values.truncate(size as usize / size_of::<T>());
    Ok(values)
}

/// The device the system is currently sending audio to.
///
/// Deliberately the default rather than a device chosen by name: the phase is
/// about what this machine does, and a probe that had to be told which endpoint
/// to use would be reporting the operator's opinion.
pub fn default_output_device() -> Result<AudioObjectID, Error> {
    let device: AudioObjectID = property(
        "AudioObjectGetPropertyData(kAudioHardwarePropertyDefaultOutputDevice)",
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultOutputDevice,
        kAudioObjectPropertyScopeGlobal,
    )?;
    if device == kAudioObjectUnknown {
        return Err(Error::Unsupported(
            "this machine has no default output device, so there is nothing to render to".into(),
        ));
    }
    Ok(device)
}

pub fn device_name(device: AudioObjectID) -> Result<String, Error> {
    let name: *const CFString = property(
        "AudioObjectGetPropertyData(kAudioObjectPropertyName)",
        device,
        kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal,
    )?;
    let name = NonNull::new(name.cast_mut()).ok_or_else(|| {
        Error::Unsupported("the device answered its name property with nothing".into())
    })?;
    // SAFETY: the property follows the CoreFoundation get rule's exception for
    // `AudioObject` name properties, which are returned already retained for
    // the caller to release, so this handle becomes the owner.
    let name = unsafe { CFRetained::from_raw(name) };
    Ok(name.to_string())
}

/// The device's output streams. More than one means an aggregate whose buffers
/// this probe would have to fan out across, which is a different experiment.
pub fn output_streams(device: AudioObjectID) -> Result<Vec<AudioObjectID>, Error> {
    property_list(
        "AudioObjectGetPropertyDataSize(kAudioDevicePropertyStreams)",
        device,
        kAudioDevicePropertyStreams,
        kAudioObjectPropertyScopeOutput,
    )
}

/// The format the HAL mixes in, which is the format the callback's buffers are
/// in. Stream properties are addressed in the global scope: a stream is already
/// one direction, so there is nothing for an input or output scope to select.
pub fn virtual_format(stream: AudioObjectID) -> Result<OutputFormat, Error> {
    let description: AudioStreamBasicDescription = property(
        "AudioObjectGetPropertyData(kAudioStreamPropertyVirtualFormat)",
        stream,
        kAudioStreamPropertyVirtualFormat,
        kAudioObjectPropertyScopeGlobal,
    )?;
    decode(&description)
}

/// What the hardware is set to underneath the mixer. Read for the report only:
/// a device mixing in float and driving the converter at 24 bits is worth
/// knowing about and changes nothing about what the callback writes.
pub fn physical_format(stream: AudioObjectID) -> Result<OutputFormat, Error> {
    let description: AudioStreamBasicDescription = property(
        "AudioObjectGetPropertyData(kAudioStreamPropertyPhysicalFormat)",
        stream,
        kAudioStreamPropertyPhysicalFormat,
        kAudioObjectPropertyScopeGlobal,
    )?;
    decode(&description)
}

/// Turns a stream description into the report's format.
///
/// Only linear PCM is decoded. An encoded output — a device set to pass AC-3
/// through to a receiver, say — is not something a float ring can feed, and
/// saying so is more use than inventing a bit depth for it.
fn decode(description: &AudioStreamBasicDescription) -> Result<OutputFormat, Error> {
    if description.mFormatID != kAudioFormatLinearPCM {
        return Err(Error::Unsupported(format!(
            "this device's stream is '{}' rather than linear PCM, so there is no PCM buffer to \
             fill",
            four_cc(description.mFormatID as i32).unwrap_or_else(|| description.mFormatID.to_string())
        )));
    }
    // The float flag is what decides, and it is the only thing that decides: a
    // description carrying neither the float nor the signed-integer flag is
    // unsigned integer, which is still not something a float ring may write.
    let float = description.mFormatFlags & kAudioFormatFlagIsFloat != 0;
    let planar = description.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
    let channels = description.mChannelsPerFrame;
    // For a non-interleaved stream the description covers one channel, so the
    // container width has to come from the per-channel stride rather than from
    // dividing a frame across the channels.
    let bits = if planar {
        description.mBitsPerChannel
    } else if channels > 0 && description.mBytesPerFrame > 0 {
        description.mBytesPerFrame * 8 / channels
    } else {
        description.mBitsPerChannel
    };
    Ok(OutputFormat {
        sample_rate: description.mSampleRate.round() as u32,
        channels: channels as u16,
        bits: bits as u16,
        valid_bits: description.mBitsPerChannel as u16,
        kind: if float {
            SampleKind::Float
        } else {
            SampleKind::Int
        },
        layout: if planar {
            Layout::Planar
        } else {
            Layout::Interleaved
        },
    })
}

/// Frames the device will hand the callback each cycle.
pub fn buffer_frame_size(device: AudioObjectID) -> Result<u32, Error> {
    property(
        "AudioObjectGetPropertyData(kAudioDevicePropertyBufferFrameSize)",
        device,
        kAudioDevicePropertyBufferFrameSize,
        kAudioObjectPropertyScopeGlobal,
    )
}

/// Asks for a buffer size. Whether the device took it is not answered here:
/// the caller reads the property back, because a request the HAL clamped or
/// ignored would otherwise be reported as the size in force.
pub fn request_buffer_frame_size(device: AudioObjectID, frames: u32) -> Result<(), Error> {
    set_property(
        "AudioObjectSetPropertyData(kAudioDevicePropertyBufferFrameSize)",
        device,
        kAudioDevicePropertyBufferFrameSize,
        kAudioObjectPropertyScopeGlobal,
        frames,
    )
}

/// What the device says it will accept, when it says. Optional because a device
/// is allowed not to implement it, and a run that stopped for the want of an
/// advisory range would be measuring nothing over a formality.
pub fn buffer_frame_size_range(device: AudioObjectID) -> Option<(u32, u32)> {
    let range: AudioValueRange = property(
        "AudioObjectGetPropertyData(kAudioDevicePropertyBufferFrameSizeRange)",
        device,
        kAudioDevicePropertyBufferFrameSizeRange,
        kAudioObjectPropertyScopeGlobal,
    )
    .ok()?;
    Some((range.mMinimum as u32, range.mMaximum as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_audio_types::kAudioFormatFlagIsSignedInteger;

    #[test]
    fn a_printable_status_is_shown_as_the_four_characters_it_is() {
        // The HAL answers most failures with four characters packed into the
        // status, and an operator handed only the integer cannot look it up.
        let status = i32::from_be_bytes(*b"who?");
        let error = Error::Api {
            call: "AudioObjectGetPropertyData",
            status,
        };
        assert_eq!(
            error.to_string(),
            format!("AudioObjectGetPropertyData failed: {status} 'who?'")
        );
    }

    #[test]
    fn an_unprintable_status_is_shown_as_a_number() {
        let error = Error::Api {
            call: "AudioDeviceStart",
            status: -50,
        };
        assert_eq!(error.to_string(), "AudioDeviceStart failed: -50");
    }

    fn pcm(
        flags: u32,
        channels: u32,
        bytes_per_frame: u32,
        bits: u32,
    ) -> AudioStreamBasicDescription {
        AudioStreamBasicDescription {
            mSampleRate: 48_000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: flags,
            mBytesPerPacket: bytes_per_frame,
            mFramesPerPacket: 1,
            mBytesPerFrame: bytes_per_frame,
            mChannelsPerFrame: channels,
            mBitsPerChannel: bits,
            mReserved: 0,
        }
    }

    #[test]
    fn an_interleaved_stereo_float_stream_decodes_to_the_contract_format() {
        let format = decode(&pcm(kAudioFormatFlagIsFloat, 2, 8, 32)).expect("linear pcm");
        assert_eq!(format.to_string(), "48000 Hz 2 ch 32 bit float");
        assert_eq!(format.layout, Layout::Interleaved);
        assert!(format.is_writable());
    }

    /// A planar stream describes one channel per buffer, so its bytes per frame
    /// is one sample and the container width has to come from the bit depth.
    #[test]
    fn a_planar_stream_does_not_divide_its_stride_by_the_channel_count() {
        let format = decode(&pcm(
            kAudioFormatFlagIsFloat | kAudioFormatFlagIsNonInterleaved,
            2,
            4,
            32,
        ))
        .expect("linear pcm");
        assert_eq!(format.to_string(), "48000 Hz 2 ch 32 bit float");
        assert_eq!(format.layout, Layout::Planar);
        assert!(format.is_writable());
    }

    #[test]
    fn an_integer_stream_decodes_as_integer() {
        let format = decode(&pcm(kAudioFormatFlagIsSignedInteger, 2, 4, 16)).expect("linear pcm");
        assert_eq!(format.to_string(), "48000 Hz 2 ch 16 bit int");
        assert!(!format.is_writable());
    }

    #[test]
    fn an_encoded_stream_is_refused_with_its_four_character_code() {
        let mut description = pcm(0, 2, 0, 0);
        description.mFormatID = u32::from_be_bytes(*b"ac-3");
        let error = decode(&description).expect_err("ac-3 is not pcm");
        assert!(
            error.to_string().contains("'ac-3' rather than linear PCM"),
            "{error}"
        );
    }
}
