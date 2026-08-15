//! The one module that calls libopus, and the only one in this crate that
//! contains `unsafe`.
//!
//! `opus-head-sys` publishes the bindings and the vendored reference C and
//! nothing above them, so the safe boundary has to be written rather than
//! depended on. Writing it here is the right shape rather than a concession:
//! this crate already exists to be the codec boundary the host and the client
//! each own half of, so the FFI belongs to the one place whose job that is, and
//! the layer that used to sit in between is one fewer dependency to go
//! unmaintained. The previous one did — `audiopus_sys`, last touched in 2020,
//! vendoring a libopus whose `cmake_minimum_required(VERSION 3.1)` CMake 4
//! refuses outright, so whether the build worked depended on whether the
//! machine happened to have a system libopus for pkg-config to find instead.
//!
//! Everything the C side trusts a caller to have got right is checked here and
//! nowhere else, and each of these is a way the same mistake goes quiet:
//!
//! A returned length and a returned error code are the same `opus_int32`, so
//! every one of them is tested for sign before it becomes a length. A negative
//! code cast to a length is how a refused encode turns into a read past the end
//! of the packet buffer.
//!
//! Every frame size handed over is derived from the length of the buffer it
//! describes rather than from the configuration that was supposed to have sized
//! it. libopus refuses a packet that would not fit in the room it was told
//! about, which makes the wrong duration an error return; it cannot refuse a
//! buffer that is shorter than the room it was told about, which makes the
//! same mistake undefined behaviour instead.
//!
//! Both CTL functions are variadic, so the compiler checks no argument against
//! its request. Every request is reached through one of two private helpers,
//! one for the `opus_int32` the OPUS_SET_* requests take and one for the
//! `opus_int32 *` the OPUS_GET_* requests write through.
//!
//! And the two states are owned rather than borrowed: created once, destroyed
//! once, and never copied, because a second owner is a second free.

use core::ffi::{CStr, c_int};
use core::ptr::{self, NonNull};

use opus_head_sys as sys;

/// libopus's error codes, which are the fixed set `opus_defines.h` enumerates
/// rather than an open space of integers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ErrorCode {
    /// One or more invalid or out of range arguments.
    BadArg,
    /// Not enough bytes allocated in the buffer. The decoder depends on this
    /// one: a buffer holding exactly one configured frame is what turns a
    /// packet of any other duration into a refusal.
    BufferTooSmall,
    /// An internal error was detected.
    InternalError,
    /// The compressed data passed is corrupted.
    InvalidPacket,
    /// Invalid or unsupported request number.
    Unimplemented,
    /// An encoder or decoder structure is invalid or already freed.
    InvalidState,
    /// Memory allocation has failed.
    AllocFail,
    /// A negative return the header has no name for, kept as the number libopus
    /// gave. Folding a code from a future version into one of the seven above
    /// would report a diagnosis nobody made.
    Unknown(c_int),
}

impl ErrorCode {
    fn from_code(code: c_int) -> ErrorCode {
        match code {
            sys::OPUS_BAD_ARG => ErrorCode::BadArg,
            sys::OPUS_BUFFER_TOO_SMALL => ErrorCode::BufferTooSmall,
            sys::OPUS_INTERNAL_ERROR => ErrorCode::InternalError,
            sys::OPUS_INVALID_PACKET => ErrorCode::InvalidPacket,
            sys::OPUS_UNIMPLEMENTED => ErrorCode::Unimplemented,
            sys::OPUS_INVALID_STATE => ErrorCode::InvalidState,
            sys::OPUS_ALLOC_FAIL => ErrorCode::AllocFail,
            other => ErrorCode::Unknown(other),
        }
    }

    fn code(self) -> c_int {
        match self {
            ErrorCode::BadArg => sys::OPUS_BAD_ARG,
            ErrorCode::BufferTooSmall => sys::OPUS_BUFFER_TOO_SMALL,
            ErrorCode::InternalError => sys::OPUS_INTERNAL_ERROR,
            ErrorCode::InvalidPacket => sys::OPUS_INVALID_PACKET,
            ErrorCode::Unimplemented => sys::OPUS_UNIMPLEMENTED,
            ErrorCode::InvalidState => sys::OPUS_INVALID_STATE,
            ErrorCode::AllocFail => sys::OPUS_ALLOC_FAIL,
            ErrorCode::Unknown(code) => code,
        }
    }

    /// libopus's own text for the code, asked of `opus_strerror` rather than
    /// restated in a table here, because a paraphrase of a C library's errors
    /// drifts from them without anybody noticing.
    pub fn description(self) -> &'static str {
        // SAFETY: `opus_strerror` answers a pointer to one of its own string
        // literals for every possible input, including the codes below -7 that
        // arrive here as `Unknown`, so the pointer is non-null, NUL-terminated
        // and really does live as long as the program does.
        let text = unsafe { CStr::from_ptr(sys::opus_strerror(self.code())) };
        text.to_str()
            .expect("opus_strerror answers one of its own ASCII literals")
    }
}

/// The version string of the libopus this binary was linked against.
///
/// Reported by the probe rather than assumed, because a measurement that did
/// not name the library it came out of is a number with no provenance.
pub fn version() -> &'static str {
    // SAFETY: `opus_get_version_string` takes no arguments and answers a
    // pointer to a NUL-terminated literal built into the library, so it is
    // non-null and outlives every caller.
    let text = unsafe { CStr::from_ptr(sys::opus_get_version_string()) };
    text.to_str()
        .expect("libopus's version string is an ASCII literal")
}

/// The channel counts the single-stream API has.
#[derive(Clone, Copy, Debug)]
pub enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    /// The count libopus is created with, which is also the divisor the length
    /// of an interleaved buffer has to be read through.
    fn count(self) -> usize {
        match self {
            Channels::Mono => 1,
            Channels::Stereo => 2,
        }
    }
}

/// The coding modes `opus_encoder_create` takes.
#[derive(Clone, Copy, Debug)]
pub enum Application {
    Voip,
    Audio,
    /// OPUS_APPLICATION_RESTRICTED_LOWDELAY.
    LowDelay,
    /// A mode this module has no name for. libopus's own head has added two
    /// restricted modes already, and an encoder that answered with one of them
    /// must not be reported as being in one of the three above.
    Unnamed(c_int),
}

impl Application {
    fn from_code(code: c_int) -> Application {
        match code {
            code if code == sys::OPUS_APPLICATION_VOIP as c_int => Application::Voip,
            code if code == sys::OPUS_APPLICATION_AUDIO as c_int => Application::Audio,
            code if code == sys::OPUS_APPLICATION_RESTRICTED_LOWDELAY as c_int => {
                Application::LowDelay
            }
            other => Application::Unnamed(other),
        }
    }

    fn code(self) -> c_int {
        match self {
            Application::Voip => sys::OPUS_APPLICATION_VOIP as c_int,
            Application::Audio => sys::OPUS_APPLICATION_AUDIO as c_int,
            Application::LowDelay => sys::OPUS_APPLICATION_RESTRICTED_LOWDELAY as c_int,
            Application::Unnamed(code) => code,
        }
    }
}

/// One of libopus's bandwidth constants.
///
/// A value rather than an enumeration because fullband is the only one this
/// crate ever names, and four more constants nothing constructs would be four
/// names nobody has checked against the header.
#[derive(Clone, Copy, Debug)]
pub struct Bandwidth(c_int);

impl Bandwidth {
    pub const FULLBAND: Bandwidth = Bandwidth(sys::OPUS_BANDWIDTH_FULLBAND as c_int);
}

/// How `OPUS_SET_EXPERT_FRAME_DURATION` is told to pick a frame duration, a
/// value rather than an enumeration for the same reason.
#[derive(Clone, Copy, Debug)]
pub struct FrameSize(c_int);

impl FrameSize {
    /// OPUS_FRAMESIZE_ARG: one call encodes one frame of whatever duration the
    /// buffer's length implies.
    pub const ARG: FrameSize = FrameSize(sys::OPUS_FRAMESIZE_ARG as c_int);
}

/// A libopus encoder state, owned.
///
/// Neither `Copy` nor `Clone`, and it hands out no copy of its pointer, because
/// the state is a heap allocation `opus_encoder_create` made and
/// `opus_encoder_destroy` must free exactly once.
pub struct Encoder {
    state: NonNull<sys::OpusEncoder>,
    channels: usize,
}

// SAFETY: an encoder state is one self-contained heap allocation. libopus keeps
// no thread-local state and consults no global, so a state created on one
// thread is sound to use from another, which is what a capture thread handing
// its encoder to a sender needs. `Sync` is deliberately absent: every call
// mutates the state and libopus takes no lock of its own, so two threads
// encoding through one state would corrupt it. Every method that reaches the C
// side takes `&mut self`, which is what makes that unreachable here.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(
        sample_rate: u32,
        channels: Channels,
        application: Application,
    ) -> Result<Encoder, ErrorCode> {
        // A rate that does not fit libopus's own `opus_int32` is out of range
        // for the argument, which is exactly what OPUS_BAD_ARG names. Casting
        // would wrap it into a different rate and hand the C side a number the
        // caller never chose.
        let Ok(rate) = c_int::try_from(sample_rate) else {
            return Err(ErrorCode::BadArg);
        };

        let mut error: c_int = 0;
        // SAFETY: `opus_encoder_create` allocates and initialises its own state
        // and writes its verdict through `error`, a live local for the whole
        // call. It answers a null pointer on every failure, so the pointer
        // decides and the code only explains: a state that exists has to be
        // destroyed however the code reads.
        let state = unsafe {
            sys::opus_encoder_create(
                rate,
                channels.count() as c_int,
                application.code(),
                &mut error,
            )
        };
        match NonNull::new(state) {
            Some(state) => Ok(Encoder {
                state,
                channels: channels.count(),
            }),
            None => Err(ErrorCode::from_code(error)),
        }
    }

    /// Performs a CTL whose documented argument is one `opus_int32`.
    fn set(&mut self, request: u32, value: c_int) -> Result<(), ErrorCode> {
        // SAFETY: `opus_encoder_ctl` is variadic, so nothing checks the
        // argument against the request. Every request reached through here is
        // an OPUS_SET_* whose argument is one `opus_int32`, and `value` is
        // exactly that, so the C side reads the bytes it expects to read.
        let code = unsafe { sys::opus_encoder_ctl(self.state.as_ptr(), request as c_int, value) };
        if code < 0 {
            return Err(ErrorCode::from_code(code));
        }
        Ok(())
    }

    /// Performs a CTL whose documented argument is one `opus_int32 *`.
    fn get(&mut self, request: u32) -> Result<c_int, ErrorCode> {
        let mut value: c_int = 0;
        // SAFETY: as in `set`, and the pointer addresses a live local of
        // exactly the width the request writes through it.
        let code = unsafe {
            sys::opus_encoder_ctl(
                self.state.as_ptr(),
                request as c_int,
                ptr::from_mut(&mut value),
            )
        };
        if code < 0 {
            return Err(ErrorCode::from_code(code));
        }
        Ok(value)
    }

    pub fn set_max_bandwidth(&mut self, bandwidth: Bandwidth) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_MAX_BANDWIDTH_REQUEST, bandwidth.0)
    }

    pub fn set_bandwidth(&mut self, bandwidth: Bandwidth) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_BANDWIDTH_REQUEST, bandwidth.0)
    }

    pub fn set_bitrate(&mut self, bits_per_second: i32) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_BITRATE_REQUEST, bits_per_second)
    }

    pub fn set_vbr(&mut self, on: bool) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_VBR_REQUEST, c_int::from(on))
    }

    pub fn set_vbr_constraint(&mut self, on: bool) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_VBR_CONSTRAINT_REQUEST, c_int::from(on))
    }

    pub fn set_dtx(&mut self, on: bool) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_DTX_REQUEST, c_int::from(on))
    }

    pub fn set_inband_fec(&mut self, on: bool) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_INBAND_FEC_REQUEST, c_int::from(on))
    }

    pub fn set_expert_frame_duration(&mut self, frame_size: FrameSize) -> Result<(), ErrorCode> {
        self.set(sys::OPUS_SET_EXPERT_FRAME_DURATION_REQUEST, frame_size.0)
    }

    pub fn get_application(&mut self) -> Result<Application, ErrorCode> {
        Ok(Application::from_code(
            self.get(sys::OPUS_GET_APPLICATION_REQUEST)?,
        ))
    }

    /// The bitrate the encoder says it is targeting, as libopus's own number.
    ///
    /// Its sentinels are passed through rather than translated: -1 is
    /// OPUS_BITRATE_MAX and -1000 is OPUS_AUTO, and a report that printed
    /// either as a bitrate nobody could have set would be worse than one that
    /// prints a number the header names.
    pub fn get_bitrate(&mut self) -> Result<i32, ErrorCode> {
        self.get(sys::OPUS_GET_BITRATE_REQUEST)
    }

    pub fn get_vbr(&mut self) -> Result<bool, ErrorCode> {
        Ok(self.get(sys::OPUS_GET_VBR_REQUEST)? != 0)
    }

    pub fn get_vbr_constraint(&mut self) -> Result<bool, ErrorCode> {
        Ok(self.get(sys::OPUS_GET_VBR_CONSTRAINT_REQUEST)? != 0)
    }

    pub fn get_dtx(&mut self) -> Result<bool, ErrorCode> {
        Ok(self.get(sys::OPUS_GET_DTX_REQUEST)? != 0)
    }

    pub fn get_inband_fec(&mut self) -> Result<bool, ErrorCode> {
        Ok(self.get(sys::OPUS_GET_INBAND_FEC_REQUEST)? != 0)
    }

    pub fn get_complexity(&mut self) -> Result<i32, ErrorCode> {
        self.get(sys::OPUS_GET_COMPLEXITY_REQUEST)
    }

    pub fn get_lookahead(&mut self) -> Result<i32, ErrorCode> {
        self.get(sys::OPUS_GET_LOOKAHEAD_REQUEST)
    }

    /// Encodes one frame of interleaved samples into `packet` and answers how
    /// many bytes it wrote.
    ///
    /// The frame duration is the length of `pcm` divided by the channel count
    /// the encoder was created with, because that is how `opus_encode_float`
    /// infers it. Nothing here checks that the quotient is a duration Opus
    /// permits: libopus refuses one that is not, and the caller that cares
    /// about which duration it asked for has to check the length itself.
    pub fn encode_float(&mut self, pcm: &[f32], packet: &mut [u8]) -> Result<usize, ErrorCode> {
        let Ok(frame_size) = c_int::try_from(pcm.len() / self.channels) else {
            return Err(ErrorCode::BadArg);
        };
        let Ok(max_bytes) = c_int::try_from(packet.len()) else {
            return Err(ErrorCode::BadArg);
        };

        // SAFETY: `frame_size` is samples per channel taken from `pcm`'s own
        // length, so `frame_size * channels` cannot exceed the samples that are
        // there; `max_bytes` is `packet`'s own length, so libopus cannot be
        // told there is more room to write than there is. Neither conversion
        // wraps, both slices are live for the call, and the state is this
        // encoder's alone.
        let written = unsafe {
            sys::opus_encode_float(
                self.state.as_ptr(),
                pcm.as_ptr(),
                frame_size,
                packet.as_mut_ptr(),
                max_bytes,
            )
        };

        // A negative answer is an error code and not a length. The branch is
        // the point: cast instead, and a refused encode becomes a claim that
        // libopus wrote billions of bytes into a four kilobyte buffer.
        if written < 0 {
            return Err(ErrorCode::from_code(written));
        }
        Ok(written as usize)
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the state came from `opus_encoder_create`, nothing else owns
        // it, and `drop` runs once, which together are the exactly-once free
        // the C side requires.
        unsafe { sys::opus_encoder_destroy(self.state.as_ptr()) };
    }
}

/// A libopus decoder state, owned, on the same terms as [`Encoder`].
pub struct Decoder {
    state: NonNull<sys::OpusDecoder>,
    channels: usize,
}

// SAFETY: the argument for the encoder holds unchanged for a decoder state,
// which is the same kind of self-contained allocation reached only through
// `&mut self`. `Sync` is absent for the same reason, and it matters more here:
// concealment reads and updates the decoder state, so a second thread decoding
// through it would be corrupting the history the next real frame is
// reconstructed from.
unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new(sample_rate: u32, channels: Channels) -> Result<Decoder, ErrorCode> {
        let Ok(rate) = c_int::try_from(sample_rate) else {
            return Err(ErrorCode::BadArg);
        };

        let mut error: c_int = 0;
        // SAFETY: as in `Encoder::new`: the state is libopus's own allocation,
        // `error` is a live local, and a null pointer is the failure.
        let state =
            unsafe { sys::opus_decoder_create(rate, channels.count() as c_int, &mut error) };
        match NonNull::new(state) {
            Some(state) => Ok(Decoder {
                state,
                channels: channels.count(),
            }),
            None => Err(ErrorCode::from_code(error)),
        }
    }

    /// Decodes one packet into `pcm` and answers the samples per channel it
    /// wrote.
    pub fn decode_float(&mut self, packet: &[u8], pcm: &mut [f32]) -> Result<usize, ErrorCode> {
        self.decode(Some(packet), pcm)
    }

    /// Runs libopus's concealer for a frame that did not arrive, filling `pcm`
    /// with the duration it holds.
    ///
    /// A call of its own rather than an empty packet handed to the one above.
    /// `opus_decode_float` reads a null pointer and a zero length as the same
    /// request, but the two callers are not the same thing at all: one has lost
    /// a frame and wants it invented, the other has been given a packet with no
    /// bytes in it, which is a defect upstream. A wrapper that spelled them the
    /// same way would make the second unobservable.
    pub fn conceal_float(&mut self, pcm: &mut [f32]) -> Result<usize, ErrorCode> {
        self.decode(None, pcm)
    }

    fn decode(&mut self, packet: Option<&[u8]>, pcm: &mut [f32]) -> Result<usize, ErrorCode> {
        let (data, len) = match packet {
            Some(packet) => {
                let Ok(len) = c_int::try_from(packet.len()) else {
                    return Err(ErrorCode::BadArg);
                };
                (packet.as_ptr(), len)
            }
            // The null pointer is how libopus is asked to conceal a loss.
            None => (ptr::null(), 0),
        };
        let Ok(frame_size) = c_int::try_from(pcm.len() / self.channels) else {
            return Err(ErrorCode::BadArg);
        };

        // SAFETY: `frame_size` is the room in `pcm` said the way
        // `opus_decode_float` asks for it, samples per channel, and it comes
        // from that buffer's own length rather than from the configuration that
        // sized it. That is what makes a packet holding more audio than fits a
        // refusal with OPUS_BUFFER_TOO_SMALL instead of a write past the end,
        // which libopus does not check and could not report. `data` and `len`
        // describe one live slice, or are a null pointer and a zero length
        // together, which is the concealment case.
        let returned = unsafe {
            sys::opus_decode_float(
                self.state.as_ptr(),
                data,
                len,
                pcm.as_mut_ptr(),
                frame_size,
                // In-band FEC is off in the encoder, so no packet here carries
                // a re-encoding of its predecessor to recover; asking for one
                // would decode the previous frame instead of this one.
                0,
            )
        };

        // Negative is an error code rather than a sample count, for the same
        // reason the encoder checks: a length taken from it would be a promise
        // about a buffer nothing wrote.
        if returned < 0 {
            return Err(ErrorCode::from_code(returned));
        }
        Ok(returned as usize)
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the state came from `opus_decoder_create`, nothing else owns
        // it, and `drop` runs once.
        unsafe { sys::opus_decoder_destroy(self.state.as_ptr()) };
    }
}
