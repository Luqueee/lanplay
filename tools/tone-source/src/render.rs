//! The render loop.
//!
//! Shared mode, event driven, default render endpoint, and no buffer duration
//! of this program's choosing beyond one deliberate doubling. Every one of those
//! is forced by what the run has to prove rather than chosen for comfort:
//! shared mode because loopback capture reads the shared mixer and an exclusive
//! stream would not be captured at all; event driven because a polled renderer's
//! report would be a report about its own sleep granularity; the default
//! endpoint because that is the one a loopback probe with no arguments will
//! open.
//!
//! The doubling is the underrun detector. WASAPI tells a render client nothing
//! about glitches — there is no glitch count on `IAudioClient`, and by the time
//! `GetCurrentPadding` is asked, a buffer sized to exactly one period is empty
//! at every single wake, healthy or not. Asking for two periods instead makes
//! the padding informative: healthy operation leaves about a period of audio
//! unplayed at each wake, so finding the buffer empty means the engine consumed
//! everything and had nothing left. That is the strongest honest statement a
//! render client can make about its own gaps, and it is why the count is
//! reported rather than inferred from a short frame total.
//!
//! The thread joins the `Audio` MMCSS task. Without it an ordinary scheduling
//! hiccup on a busy machine shows up as an underrun, and the capture side would
//! be handed a gap that says nothing about capture.

#![cfg(windows)]

use core::ffi::c_void;
use core::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use lanplay_telemetry::{Nanos, Timestamp};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, IAudioClient, IAudioRenderClient,
    IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, WAVEFORMATEX, eConsole, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
    WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::core::{BOOL, w};

use crate::format::MixFormat;
use crate::report::Report;
use crate::tone::{CONTRACT, Tone};
use crate::{Error, api};

/// How long a wake is waited for before the device is called gone.
///
/// Not pacing: a healthy event arrives every device period, some ten
/// milliseconds. This is the point at which silence stops being a hiccup and
/// becomes a departure, and it is deliberately far past any period a shared-mode
/// endpoint uses.
const WAIT_MS: u32 = 2_000;

/// Set from the console control handler, read by the loop.
///
/// A run with `--seconds 0` ends when the operator ends it, and a source killed
/// outright would take its report with it. The handler only stores a flag; the
/// loop is woken by the device every period, so it notices within one.
static STOP: AtomicBool = AtomicBool::new(false);

/// Everything the run needs from the command line.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// 0 means run until the console says to stop.
    pub seconds: u64,
}

/// Plays the contract tone until `--seconds` elapses or the console interrupts,
/// and reports what the run actually rendered.
pub fn run(options: Options) -> Result<Report, Error> {
    // SAFETY: the handler is a plain function that stores into a static
    // `AtomicBool` and returns; it touches nothing that could be mid-update
    // when the console calls it on its own thread.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(on_console_control), true);
    }

    // SAFETY: no arguments to get wrong. An apartment already initialised is
    // reported as a failure HRESULT and is not one: something else in the
    // process got there first, which is fine for a render client.
    let _com = unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        Apartment
    };

    // SAFETY: every call below is checked before its result is used, and every
    // COM object is refcounted by `windows`. The two raw pointers in play, the
    // mix format and the device buffer, are each owned by a guard or released
    // within the same statement.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                .map_err(api("CoCreateInstance(MMDeviceEnumerator)"))?;
        let device = enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(api("IMMDeviceEnumerator::GetDefaultAudioEndpoint"))?;
        let endpoint = endpoint_name(&device);

        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(api("IMMDevice::Activate(IAudioClient)"))?;
        let mix = MixFormatMemory(
            client
                .GetMixFormat()
                .map_err(api("IAudioClient::GetMixFormat"))?,
        );
        let format = MixFormat::read(mix.0);
        if !format.carries(&CONTRACT) {
            return Err(Error::MixFormat {
                endpoint,
                found: format,
            });
        }

        let mut default_period = 0i64;
        let mut minimum_period = 0i64;
        client
            .GetDevicePeriod(Some(&mut default_period), Some(&mut minimum_period))
            .map_err(api("IAudioClient::GetDevicePeriod"))?;

        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                2 * default_period,
                // Shared mode takes its periodicity from the engine, and passing
                // anything but zero here is documented to fail.
                0,
                mix.0,
                None,
            )
            .map_err(api("IAudioClient::Initialize"))?;

        let buffer_frames = client
            .GetBufferSize()
            .map_err(api("IAudioClient::GetBufferSize"))?;
        let renderer: IAudioRenderClient = client
            .GetService()
            .map_err(api("IAudioClient::GetService(IAudioRenderClient)"))?;
        let event = Event(CreateEventW(None, false, false, None).map_err(api("CreateEventW"))?);
        client
            .SetEventHandle(event.0)
            .map_err(api("IAudioClient::SetEventHandle"))?;

        let _mmcss = Mmcss::join();

        // Everything descriptive goes to stderr: stdout carries the report and
        // nothing else, so a joint run can consume it without parsing around a
        // banner.
        eprintln!(
            "tone-source: {endpoint} mixes at {format}, device period {:.3} ms default, \
             {:.3} ms minimum; buffer {buffer_frames} frames",
            hundred_nanos_as_millis(default_period),
            hundred_nanos_as_millis(minimum_period),
        );

        let mut tone = Tone::new(CONTRACT);
        let mut buffers_filled = 0u64;
        let mut frames_rendered = 0u64;
        let mut underruns = 0u64;

        // Pre-roll before `Start`, so the engine's first read finds audio. A
        // renderer that started empty would put a period of silence at the head
        // of the stream, and the capture side counts silent packets: a leading
        // gap this program chose to create would be indistinguishable there from
        // one the capture lost.
        frames_rendered += u64::from(fill(&renderer, &mut tone, buffer_frames)?);
        buffers_filled += 1;

        client.Start().map_err(api("IAudioClient::Start"))?;
        let started = Timestamp::now();
        let deadline = (options.seconds > 0)
            .then(|| started.add(Nanos(options.seconds.saturating_mul(1_000_000_000))));
        let mut announced = false;

        while !STOP.load(Ordering::Relaxed) {
            if WaitForSingleObject(event.0, WAIT_MS) != WAIT_OBJECT_0 {
                return Err(Error::Stalled {
                    buffers_filled,
                    waited_ms: WAIT_MS,
                });
            }

            let padding = client
                .GetCurrentPadding()
                .map_err(api("IAudioClient::GetCurrentPadding"))?;
            if padding == 0 {
                underruns += 1;
            }
            let available = buffer_frames - padding;
            if available == 0 {
                // A wake with a full buffer. Nothing to fill, and not a gap.
                continue;
            }

            frames_rendered += u64::from(fill(&renderer, &mut tone, available)?);
            buffers_filled += 1;

            if !announced {
                // One line, once, so an operator can tell a started source from
                // a stalled one long before the report exists.
                eprintln!("tone-source: playing, first buffer of {available} frames filled");
                announced = true;
            }

            if deadline.is_some_and(|deadline| Timestamp::now() >= deadline) {
                break;
            }
        }

        // Let what was written finish playing before the count is reported. The
        // buffer holds two periods, so stopping the moment the loop ends would
        // report up to twenty milliseconds of frames the endpoint never reached,
        // and the capture side would read the difference as loss.
        let unplayed = client.GetCurrentPadding().unwrap_or(0);
        if unplayed > 0 {
            thread::sleep(Duration::from_nanos(
                u64::from(unplayed) * 1_000_000_000 / u64::from(format.rate),
            ));
        }
        client.Stop().map_err(api("IAudioClient::Stop"))?;
        let span = Timestamp::now().saturating_since(started);

        if frames_rendered == 0 {
            return Err(Error::Unsupported(format!(
                "rendered nothing on {endpoint}: the run produced no audio, so there was nothing \
                 for a loopback capture to find"
            )));
        }

        Ok(Report {
            endpoint,
            format,
            buffers_filled,
            frames_rendered,
            underruns,
            span,
        })
    }
}

/// Writes `frames` frames of tone into the device buffer and releases it.
///
/// No allocation, no logging and no lock: the device is waiting on the far side
/// of this, and anything that could block here would become the underrun it is
/// meant to avoid.
///
/// # Safety
///
/// The caller must have checked that the stream's format is two-channel 32-bit
/// float, which is what makes the returned bytes a slice of `f32` pairs.
unsafe fn fill(renderer: &IAudioRenderClient, tone: &mut Tone, frames: u32) -> Result<u32, Error> {
    // SAFETY: `frames` never exceeds the space the padding said was free, which
    // is what `GetBuffer` requires.
    let raw =
        unsafe { renderer.GetBuffer(frames) }.map_err(api("IAudioRenderClient::GetBuffer"))?;

    // SAFETY: `GetBuffer` returned room for `frames` frames of eight bytes, and
    // the format check pinned the layout at two `f32` per frame. WASAPI buffers
    // are allocated for the audio engine's own vector loads, so the pointer is
    // aligned far beyond the four bytes an `f32` needs. Nothing else can hold a
    // reference to this memory between here and `ReleaseBuffer`.
    let samples =
        unsafe { core::slice::from_raw_parts_mut(raw.cast::<f32>(), frames as usize * 2) };
    let written = tone.fill_stereo(samples);

    // SAFETY: exactly the frames just written are released, with no flags: the
    // buffer carries real audio, so `AUDCLNT_BUFFERFLAGS_SILENT` would be a lie.
    unsafe { renderer.ReleaseBuffer(written, 0) }
        .map_err(api("IAudioRenderClient::ReleaseBuffer"))?;
    Ok(written)
}

/// The endpoint's friendly name, falling back to its id.
///
/// A missing name is worth a fallback rather than a failure: a run that played
/// the tone and could not read a display string is still a run, and the endpoint
/// id identifies the device just as precisely to anyone who has to find it
/// again.
///
/// # Safety
///
/// `device` must be a live endpoint, which is what the enumerator returned.
unsafe fn endpoint_name(device: &IMMDevice) -> String {
    // SAFETY: the property store and the variant are both released before this
    // returns, and the string is copied out while the variant still owns it.
    unsafe {
        if let Ok(store) = device.OpenPropertyStore(STGM_READ)
            && let Ok(mut value) = store.GetValue(&PKEY_Device_FriendlyName)
        {
            let name = (value.Anonymous.Anonymous.vt == VT_LPWSTR)
                .then(|| value.Anonymous.Anonymous.Anonymous.pwszVal.to_string().ok())
                .flatten();
            let _ = PropVariantClear(&mut value);
            if let Some(name) = name {
                return name;
            }
        }

        device
            .GetId()
            .ok()
            .and_then(|id| id.to_string().ok())
            .unwrap_or_else(|| "unnamed endpoint".to_string())
    }
}

fn hundred_nanos_as_millis(hundred_nanos: i64) -> f64 {
    hundred_nanos as f64 / 10_000.0
}

/// Console control events that mean stop.
///
/// Returning handled for `CTRL_CLOSE_EVENT` buys the few seconds Windows allows
/// before it kills the process, which is more than the loop needs to notice the
/// flag and print its report.
unsafe extern "system" fn on_console_control(kind: u32) -> BOOL {
    match kind {
        CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT => {
            STOP.store(true, Ordering::Relaxed);
            true.into()
        }
        _ => false.into(),
    }
}

/// Undoes `CoInitializeEx` however the run ends, including through the `?` on
/// the format refusal.
struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances exactly one successful or already-initialised
        // `CoInitializeEx` on this thread, and nothing here outlives it.
        unsafe { CoUninitialize() };
    }
}

struct Event(HANDLE);

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateEventW` and is closed once. The
        // client that was given it is dropped first, in reverse declaration
        // order, so nothing can signal it after this.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// The mix format exactly as `GetMixFormat` allocated it.
///
/// Held rather than copied because `Initialize` has to be handed the same
/// structure, trailing extensible fields and all, and a hand-built copy that
/// dropped them would ask the engine for a format subtly unlike the one that
/// was inspected.
struct MixFormatMemory(*mut WAVEFORMATEX);

impl Drop for MixFormatMemory {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `GetMixFormat`, which allocates with the
        // COM task allocator, and is freed once.
        unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) };
    }
}

/// Membership of the `Audio` MMCSS task, for as long as the run lasts.
struct Mmcss(Option<HANDLE>);

impl Mmcss {
    fn join() -> Mmcss {
        let mut index = 0u32;
        // SAFETY: the task name is a static wide string and the index is a live
        // local; the call either returns a handle or reports why not.
        match unsafe { AvSetMmThreadCharacteristicsW(w!("Audio"), &mut index) } {
            Ok(handle) => Mmcss(Some(handle)),
            Err(error) => {
                // Not fatal, but worth saying: without MMCSS an ordinary
                // scheduling hiccup becomes an underrun in this report, and an
                // underrun here is read on the capture side as capture loss.
                eprintln!(
                    "tone-source: not in the Audio MMCSS task (0x{:08X}); underruns become likelier",
                    error.code().0 as u32
                );
                Mmcss(None)
            }
        }
    }
}

impl Drop for Mmcss {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // SAFETY: the handle came from `AvSetMmThreadCharacteristicsW` on
            // this thread and is reverted once.
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(handle);
            }
        }
    }
}
