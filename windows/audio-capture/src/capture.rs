//! WASAPI loopback capture of the default render endpoint.
//!
//! Shared mode with `AUDCLNT_STREAMFLAGS_LOOPBACK` on the render endpoint, which
//! is the whole of the mechanism: the audio engine copies what it is about to
//! play into this client's capture buffer. It is the endpoint mix, so it is
//! everything the machine is playing and not one process's audio. Capturing a
//! single process needs `ActivateAudioInterfaceAsync` with
//! `AUDIOCLIENT_ACTIVATION_PARAMS`, which is a different question with a
//! different set of failure modes, and it belongs to a later phase.
//!
//! Event-driven or polled. Microsoft's loopback recording page says that before
//! Windows 10 1703 a loopback client initialised with
//! `AUDCLNT_STREAMFLAGS_EVENTCALLBACK` was never signalled -- the call
//! succeeded and the events simply never arrived, and the documented workaround
//! was to run a render stream in event-driven mode and use its events to drive
//! the capture side -- and that from 1703 onwards event-driven loopback clients
//! are supported directly. The lab host is far past 1703, so this asks for
//! events. It does not trust that answer blindly: the wait carries a timeout of
//! several device periods, so an endpoint that never signals still reaches its
//! deadline and still reports, and `--poll` forces the other path so the two can
//! be compared on the same host. If the event-driven initialise is refused
//! outright the run falls back to polling and says which one produced the
//! numbers.
//!
//! The polling interval is half the default device period, which is the
//! coarsest interval that cannot systematically miss a packet, floored at one
//! millisecond because nothing finer is worth asking a Windows timer for.
//! Asking is not getting, so the interval the loop actually achieved is
//! measured and reported as a distribution; the requested figure appears in the
//! report only as the thing that was asked for. Polling also raises the system
//! timer resolution for the length of the run, since without that a sleep of
//! any length below about sixteen milliseconds returns after sixteen and the
//! distribution would describe the Windows tick rather than the audio stack.
//!
//! Timestamps come from `GetBuffer` and nowhere else. It reports the device
//! position and the performance counter for the *first frame of the packet*,
//! and a counter read after `GetBuffer` returns would instead describe when this
//! loop got round to asking -- a quantity that includes every scheduling delay
//! between the endpoint and this thread, which is precisely the quantity this
//! phase exists to measure rather than to bake into its own timestamps.
//!
//! The loop allocates nothing, logs nothing and takes no lock. Every store it
//! writes into was sized before `Start`, and one that fills up counts what it
//! could not take instead of growing, because a heap allocation on this path
//! would show up in the very distribution it was making room for. The thread
//! also does not join an MMCSS task: what this measures is what an ordinary
//! thread gets, which is the honest baseline to judge a later `Pro Audio`
//! registration against.

use core::ffi::c_void;
use core::fmt;
use core::ptr;
use core::slice;
use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_LOOPBACK, IAudioCaptureClient,
    IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::PCWSTR;

use crate::accounting::{Accounting, Packet, Samples};
use crate::analysis::analyse;
use crate::format::{FormatError, MixFormat, RawExtensible, RawWaveFormat, WAVE_FORMAT_EXTENSIBLE};
use crate::report::{Report, Wakeup};

/// What the caller wants captured.
pub struct Request {
    pub seconds: f64,
    /// Skip the event-driven path even where it works, so the two can be
    /// compared on one host.
    pub force_poll: bool,
    /// Keep every captured byte for a wav dump. Off unless somebody asked for
    /// a file, because it costs a copy of every packet.
    pub keep_pcm: bool,
}

pub struct Captured {
    pub report: Report,
    /// The captured bytes, exactly as the endpoint delivered them, empty unless
    /// the request asked for them.
    pub pcm: Vec<u8>,
}

/// Why a capture could not be set up or could not be believed.
#[derive(Debug)]
pub enum CaptureError {
    Api {
        stage: &'static str,
        error: windows::core::Error,
    },
    Format(FormatError),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Api { stage, error } => write!(f, "{stage}: {error}"),
            CaptureError::Format(error) => write!(f, "mix format: {error}"),
        }
    }
}

fn api(stage: &'static str) -> impl Fn(windows::core::Error) -> CaptureError {
    move |error| CaptureError::Api { stage, error }
}

/// Restores the timer resolution however the run ends, including a panic in the
/// middle of it, because leaving a machine at a one millisecond tick is a
/// change to every other process on it.
struct TimerResolution(u32);

impl Drop for TimerResolution {
    fn drop(&mut self) {
        // SAFETY: `timeEndPeriod` takes the value passed to a matching
        // `timeBeginPeriod`, which is the only way this type is constructed.
        unsafe {
            timeEndPeriod(self.0);
        }
    }
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: paired with the successful `CoInitializeEx` that produced
        // this value, on the same thread, and this type is neither `Send` nor
        // constructible elsewhere.
        unsafe {
            CoUninitialize();
        }
    }
}

/// Frees a block the audio engine allocated with the COM task allocator.
struct TaskMemory(*mut WAVEFORMATEX);

impl Drop for TaskMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from `GetMixFormat`, which documents the
            // caller as freeing it with `CoTaskMemFree`, and it is freed once
            // because this owns it.
            unsafe {
                CoTaskMemFree(Some(self.0 as *const c_void));
            }
        }
    }
}

struct EventHandle(HANDLE);

impl Drop for EventHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: the handle came from `CreateEventW` and nothing else
            // holds it; the audio client's reference was dropped first.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// An initialised client and everything the endpoint said while opening it.
struct Opened {
    client: IAudioClient,
    format: MixFormat,
    default_period_ms: f64,
    minimum_period_ms: f64,
    buffer_frames: u32,
}

/// Runs one capture and returns what it found.
pub fn run(request: &Request) -> Result<Captured, CaptureError> {
    // SAFETY: every call below is a COM call on this thread, made in the order
    // the audio client documents -- activate, initialise, get service, start,
    // read, stop -- with each allocation and handle owned by a guard that
    // releases it in the reverse order. The whole body is one unsafe block
    // because splitting a strictly ordered COM sequence into a dozen of them
    // would say the same thing a dozen times.
    unsafe { capture(request) }
}

unsafe fn capture(request: &Request) -> Result<Captured, CaptureError> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(api("CoInitializeEx"))?;
        let _apartment = ComApartment;

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(api("CoCreateInstance(MMDeviceEnumerator)"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(api("GetDefaultAudioEndpoint(eRender, eConsole)"))?;
        let endpoint = endpoint_name(&device);

        let (opened, wakeup, event_refused) = if request.force_poll {
            let opened = open(&device, false)?;
            let interval_ms = poll_interval_ms(opened.default_period_ms);
            (opened, Wakeup::Poll { interval_ms }, None)
        } else {
            match open(&device, true) {
                Ok(opened) => (opened, Wakeup::Event, None),
                // A refused initialise leaves the client unusable -- it may only
                // be initialised once -- so the fallback starts from a fresh
                // one rather than retrying this.
                Err(CaptureError::Api { error, .. }) => {
                    let opened = open(&device, false)?;
                    let interval_ms = poll_interval_ms(opened.default_period_ms);
                    (
                        opened,
                        Wakeup::Poll { interval_ms },
                        Some(error.to_string()),
                    )
                }
                Err(other) => return Err(other),
            }
        };

        let Opened {
            client,
            format,
            default_period_ms,
            minimum_period_ms,
            buffer_frames,
        } = opened;

        let event = match wakeup {
            Wakeup::Event => {
                let handle = CreateEventW(None, false, false, PCWSTR::null())
                    .map_err(api("CreateEventW"))?;
                let handle = EventHandle(handle);
                client
                    .SetEventHandle(handle.0)
                    .map_err(api("SetEventHandle"))?;
                Some(handle)
            }
            Wakeup::Poll { .. } => None,
        };

        let capture_client: IAudioCaptureClient = client.GetService().map_err(api("GetService"))?;

        let frame_bytes = format.frame_bytes();
        let period_seconds = (default_period_ms / 1_000.0).max(0.000_1);
        // Four times the number of periods the run should see, plus a floor, so
        // that a stream delivering unusually small packets still has its whole
        // distribution measured rather than a prefix of it.
        let expected_packets =
            ((request.seconds / period_seconds).ceil() as usize).saturating_mul(4) + 1_024;
        let mut packet_frames = Samples::with_capacity(expected_packets);
        let mut wakeup_intervals = Samples::with_capacity(expected_packets);

        // A tenth of a second of frames, which is a ten hertz bin spacing at
        // the usual rate: fine enough to separate the two contract tones by a
        // hundred bins and short enough to copy without noticing.
        let analysis_frames = (format.sample_rate as usize / 10).clamp(1_024, 16_384);
        let mut analysis = Vec::<u8>::with_capacity(analysis_frames * frame_bytes);

        let mut pcm = if request.keep_pcm {
            let frames = (request.seconds * f64::from(format.sample_rate)).ceil() as usize;
            Vec::<u8>::with_capacity(
                frames.saturating_add(format.sample_rate as usize) * frame_bytes,
            )
        } else {
            Vec::new()
        };

        let mut frequency = 0i64;
        QueryPerformanceFrequency(&mut frequency).map_err(api("QueryPerformanceFrequency"))?;
        let ticks_per_second = frequency.max(1) as f64;

        // Several device periods, so that an endpoint which never signals --
        // the pre-1703 behaviour, and whatever a driver does that the
        // documentation did not anticipate -- still reaches its deadline
        // instead of hanging the run.
        let wait_ms = (default_period_ms * 4.0).ceil().max(10.0) as u32;
        let poll_interval = match wakeup {
            Wakeup::Poll { interval_ms } => Some(Duration::from_secs_f64(interval_ms / 1_000.0)),
            Wakeup::Event => None,
        };
        let _resolution = poll_interval.map(|_| {
            timeBeginPeriod(1);
            TimerResolution(1)
        });

        let mut account = Accounting::new();
        let mut wakeup_timeouts = 0u64;
        let mut buffer_errors = 0u64;
        let mut first_buffer_error: Option<String> = None;
        let mut pcm_dropped = 0u64;

        client.Start().map_err(api("Start"))?;

        let started = counter();
        let deadline = started + (request.seconds * ticks_per_second) as i64;
        let mut previous_wakeup = started;

        while counter() < deadline {
            match (&event, poll_interval) {
                (Some(handle), _) => {
                    if WaitForSingleObject(handle.0, wait_ms) != WAIT_OBJECT_0 {
                        wakeup_timeouts += 1;
                    }
                }
                (None, Some(interval)) => sleep(interval),
                (None, None) => unreachable!("a run is either event driven or polled"),
            }

            let woke = counter();
            wakeup_intervals.record(micros(woke - previous_wakeup, ticks_per_second));
            previous_wakeup = woke;

            loop {
                let mut data: *mut u8 = ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                let mut device_position = 0u64;
                let mut qpc_position = 0u64;
                if let Err(error) = capture_client.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_position),
                ) {
                    buffer_errors += 1;
                    if first_buffer_error.is_none() {
                        first_buffer_error = Some(error.to_string());
                    }
                    break;
                }
                // An empty buffer is the ordinary end of a drain, and it leaves
                // the position and timestamp untouched, so nothing here may
                // read them.
                if frames == 0 {
                    break;
                }

                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                account.record(&Packet {
                    device_position,
                    frames,
                    qpc_100ns: qpc_position,
                    discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
                    silent,
                    timestamp_error: flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0,
                });
                packet_frames.record(u64::from(frames));

                let bytes = frames as usize * frame_bytes;
                // The silent flag means the buffer's contents are undefined and
                // are to be treated as silence, so it is zeroes that get stored
                // rather than whatever was left in the ring.
                if request.keep_pcm {
                    let room = pcm.capacity() - pcm.len();
                    let taken = bytes.min(room);
                    if silent {
                        pcm.resize(pcm.len() + taken, 0);
                    } else {
                        pcm.extend_from_slice(slice::from_raw_parts(data, taken));
                    }
                    pcm_dropped += (bytes - taken) as u64;
                }
                // Silence contributes nothing to a frequency measurement except
                // an attenuation of whatever came before it, so the analysis
                // window is filled from real packets only.
                if !silent && analysis.len() < analysis.capacity() {
                    let room = analysis.capacity() - analysis.len();
                    analysis.extend_from_slice(slice::from_raw_parts(data, bytes.min(room)));
                }

                capture_client
                    .ReleaseBuffer(frames)
                    .map_err(api("ReleaseBuffer"))?;
            }
        }

        client.Stop().map_err(api("Stop"))?;
        drop(event);

        let tone = analyse(&format, &analysis);
        let samples_dropped = packet_frames.dropped() + wakeup_intervals.dropped();
        let report = Report {
            endpoint,
            format,
            default_period_ms,
            minimum_period_ms,
            buffer_frames,
            wakeup,
            event_refused,
            requested_seconds: request.seconds,
            totals: account.totals(),
            packet_frames: packet_frames.percentiles(),
            wakeup_intervals_us: wakeup_intervals.percentiles(),
            wakeup_timeouts,
            tone,
            buffer_errors,
            first_buffer_error,
            samples_dropped,
            pcm_dropped,
        };
        Ok(Captured { report, pcm })
    }
}

/// Half the default device period, floored at a millisecond.
fn poll_interval_ms(default_period_ms: f64) -> f64 {
    (default_period_ms / 2.0).max(1.0)
}

fn counter() -> i64 {
    let mut now = 0i64;
    // SAFETY: writes one `i64` through a pointer to a live local. The call
    // cannot fail on any machine with a performance counter, which is every
    // machine this can run on.
    unsafe {
        let _ = QueryPerformanceCounter(&mut now);
    }
    now
}

fn micros(ticks: i64, ticks_per_second: f64) -> u64 {
    if ticks <= 0 {
        return 0;
    }
    (ticks as f64 * 1_000_000.0 / ticks_per_second) as u64
}

/// Activates a client on the endpoint and initialises a loopback stream on it.
unsafe fn open(device: &IMMDevice, event_driven: bool) -> Result<Opened, CaptureError> {
    unsafe {
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(api("Activate(IAudioClient)"))?;

        let mix = TaskMemory(client.GetMixFormat().map_err(api("GetMixFormat"))?);
        let format = MixFormat::from_raw(&read_wave_format(mix.0)).map_err(CaptureError::Format)?;

        let mut default_period = 0i64;
        let mut minimum_period = 0i64;
        client
            .GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period))
            .map_err(api("GetDevicePeriod"))?;

        let mut flags = AUDCLNT_STREAMFLAGS_LOOPBACK;
        if event_driven {
            flags |= AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        }
        // Zero duration and zero periodicity: shared mode has one engine period
        // and asking for a different one either changes what is being measured
        // or is refused, and the buffer the engine picks for itself is the
        // buffer every other client on this machine is living with.
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                flags,
                0,
                0,
                mix.0 as *const WAVEFORMATEX,
                None,
            )
            .map_err(api("Initialize(loopback)"))?;

        let buffer_frames = client.GetBufferSize().map_err(api("GetBufferSize"))?;

        Ok(Opened {
            client,
            format,
            default_period_ms: default_period as f64 / 10_000.0,
            minimum_period_ms: minimum_period as f64 / 10_000.0,
            buffer_frames,
        })
    }
}

/// Copies a `WAVEFORMATEX` out of engine-owned memory into plain fields.
///
/// Read whole and unaligned: the structure is byte packed, so borrowing a field
/// out of it would be a reference that the language is entitled to assume is
/// aligned and that in general is not.
unsafe fn read_wave_format(pointer: *const WAVEFORMATEX) -> RawWaveFormat {
    unsafe {
        let head = ptr::read_unaligned(pointer);
        let extensible = if head.wFormatTag == WAVE_FORMAT_EXTENSIBLE && head.cbSize >= 22 {
            let full = ptr::read_unaligned(pointer as *const WAVEFORMATEXTENSIBLE);
            // Copied out before anything borrows it: the GUID sits at an odd
            // offset in a packed structure, and `to_u128` takes it by
            // reference.
            let subformat = full.SubFormat;
            Some(RawExtensible {
                valid_bits: full.Samples.wValidBitsPerSample,
                channel_mask: full.dwChannelMask,
                subformat: subformat.to_u128(),
            })
        } else {
            None
        };
        RawWaveFormat {
            format_tag: head.wFormatTag,
            channels: head.nChannels,
            samples_per_sec: head.nSamplesPerSec,
            avg_bytes_per_sec: head.nAvgBytesPerSec,
            block_align: head.nBlockAlign,
            bits_per_sample: head.wBitsPerSample,
            extensible,
        }
    }
}

/// The name Windows shows for the endpoint, or a placeholder saying it could
/// not be read.
///
/// A missing name is not worth failing a run over, but it must not be reported
/// as an empty string either: a report whose endpoint line is blank looks like
/// a formatting bug rather than a property store that refused.
unsafe fn endpoint_name(device: &IMMDevice) -> String {
    unsafe {
        let Ok(store) = device.OpenPropertyStore(STGM_READ) else {
            return "<property store unavailable>".to_owned();
        };
        let Ok(mut value) = store.GetValue(&PKEY_Device_FriendlyName) else {
            return "<unnamed>".to_owned();
        };
        let name = if value.Anonymous.Anonymous.vt == VT_LPWSTR {
            value
                .Anonymous
                .Anonymous
                .Anonymous
                .pwszVal
                .to_string()
                .unwrap_or_else(|_| "<name is not text>".to_owned())
        } else {
            "<unnamed>".to_owned()
        };
        let _ = PropVariantClear(&mut value);
        name
    }
}
