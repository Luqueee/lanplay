//! A WASAPI render source that plays the batch's contract tone, so that a
//! loopback capture has something to capture.
//!
//! This is the audio counterpart of `tools/present-source` and exists for the
//! same reason: WASAPI loopback, like Desktop Duplication, only produces data
//! while something is producing. Pointed at a silent machine, a loopback probe
//! measures its own wait loop and reports a clean run full of silent packets,
//! which the project has twice mistaken for success. So the source is a separate
//! program with its own report, and the two reports are compared.
//!
//! The tone is fixed rather than configurable: 48000 Hz stereo, 997 Hz on the
//! left, 1997 Hz on the right, at -20 dBFS. The capture side asserts those
//! numbers, so a flag that changed them here would turn a real disagreement
//! between the two halves into a silent pass. See [`tone::CONTRACT`].
//!
//! Nothing here resamples or converts. If the endpoint's mix format is not what
//! the tone needs, the run refuses and reports what it found, because a source
//! that quietly changed rate or bit depth would make everything the capture side
//! concluded meaningless — and because that format is itself the finding this
//! phase turns on. Refusing prints it just as loudly as succeeding does.
//!
//! Exclusive mode was rejected. It would let the tone reach the endpoint
//! bit-exact and at the device's own period, which sounds like the better
//! instrument, but loopback capture reads the shared mixer, and a stream that
//! bypasses the mixer is not captured at all. The measurement has to travel the
//! path the real audio will travel.

pub mod format;
pub mod report;
pub mod tone;

#[cfg(windows)]
pub mod render;

use core::fmt;

use crate::format::MixFormat;

#[derive(Debug)]
pub enum Error {
    /// A COM or WASAPI call failed.
    Api { call: &'static str, hresult: i32 },
    /// The request cannot be served on this machine or this platform.
    Unsupported(String),
    /// The endpoint does not mix at the tone's format, so the run refused to
    /// start. Carries the format so the refusal reports the finding.
    MixFormat { endpoint: String, found: MixFormat },
    /// The device stopped asking for buffers. Distinct from an underrun: an
    /// underrun is this program failing to keep a running device fed, while
    /// this is the device having gone away, and a report of frames rendered
    /// after it would be a report of frames nobody played.
    Stalled { buffers_filled: u64, waited_ms: u32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Api { call, hresult } => {
                write!(f, "{call} failed: 0x{:08X}", *hresult as u32)
            }
            Error::Unsupported(why) => write!(f, "{why}"),
            Error::MixFormat { endpoint, found } => write!(
                f,
                "endpoint {endpoint} mixes at {found}; the tone is {} Hz {} ch 32 bit float, \
                 and converting it silently would invalidate the capture it exists to feed. \
                 Nothing was rendered.",
                tone::CONTRACT.sample_rate,
                tone::CONTRACT.channels,
            ),
            Error::Stalled {
                buffers_filled,
                waited_ms,
            } => write!(
                f,
                "the device stopped asking for buffers: no event in {waited_ms} ms after \
                 {buffers_filled} buffers"
            ),
        }
    }
}

impl core::error::Error for Error {}

/// Turns a `windows` error into ours while naming the call that produced it,
/// because an HRESULT without its callsite is a number nobody can act on.
#[cfg(windows)]
pub(crate) fn api(call: &'static str) -> impl FnOnce(::windows::core::Error) -> Error {
    move |error| Error::Api {
        call,
        hresult: error.code().0,
    }
}
