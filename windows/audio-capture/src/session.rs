//! A loopback stream something else can drive, one packet at a time.
//!
//! [`crate::capture`] is an instrument: it runs the endpoint for a while and
//! answers with a report. That shape is right for the phase that asked what the
//! endpoint delivers and wrong for everything built on the answer, because a
//! component that has to encode a packet and put it on a socket cannot be handed
//! its packets after the run is over. So the session is separated from the run.
//! The setup is the same setup -- the same activation, the same shared-mode
//! loopback initialise, the same event handle, the same fallback to polling if
//! events are refused -- and it is the same code rather than a copy of it, which
//! matters because the pre-1703 event behaviour and the timer-resolution
//! requirement are exactly the sort of thing a second implementation quietly
//! omits.
//!
//! Packets are handed over by calling a closure rather than by returning a
//! borrowed handle. The engine's buffer is only valid between `GetBuffer` and
//! `ReleaseBuffer`, and a returned handle would have to release in `Drop`, where
//! the one thing `ReleaseBuffer` can do wrong -- refuse -- could only be
//! swallowed. Here the release happens on the way out of [`Loopback::next`] and
//! its failure reaches the caller.
//!
//! Nothing here converts, copies or accumulates a sample. The bytes the caller
//! sees are the engine's own, in the endpoint's own mix format, and what to do
//! about a format that is not the one the caller can encode is the caller's
//! decision to make and to report.

use core::ptr;
use core::slice;
use std::thread::sleep;
use std::time::Duration;

use windows::Win32::Foundation::WAIT_OBJECT_0;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
    MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::Media::timeBeginPeriod;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::PCWSTR;

use crate::accounting::Packet;
use crate::capture::{
    CaptureError, ComApartment, EventHandle, Opened, TimerResolution, api, endpoint_name, open,
    poll_interval_ms,
};
use crate::format::MixFormat;
use crate::report::Wakeup;

/// An open, running loopback stream on the default render endpoint.
pub struct Loopback {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    /// Held for the length of the session; the event is what the endpoint
    /// signals, so it must outlive every wait.
    event: Option<EventHandle>,
    poll_interval: Option<Duration>,
    wait_ms: u32,
    endpoint: String,
    format: MixFormat,
    default_period_ms: f64,
    minimum_period_ms: f64,
    buffer_frames: u32,
    wakeup: Wakeup,
    event_refused: Option<String>,
    running: bool,
    /// Declared last, so both are dropped after every COM interface above them:
    /// Rust drops fields in declaration order, and uninitialising the apartment
    /// while a client still held a reference would be a use after free.
    _resolution: Option<TimerResolution>,
    _apartment: ComApartment,
}

impl Loopback {
    /// Opens the default render endpoint in loopback and prepares to be driven.
    ///
    /// Event-driven where the endpoint allows it, polled where it does not, and
    /// the session says which -- a caller reporting wakeup intervals has to
    /// state what woke it or the distribution describes an unknown mechanism.
    pub fn open() -> Result<Loopback, CaptureError> {
        // SAFETY: the COM sequence below is the one `IAudioClient` documents,
        // in order, on this thread, with every allocation and handle owned by a
        // guard that releases it in the reverse order. One block, because
        // splitting a strictly ordered sequence into a dozen would say the same
        // thing a dozen times.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(api("CoInitializeEx"))?;
            let apartment = ComApartment;

            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .map_err(api("CoCreateInstance(MMDeviceEnumerator)"))?;
            let device = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(api("GetDefaultAudioEndpoint(eRender, eConsole)"))?;
            let endpoint = endpoint_name(&device);

            let (opened, wakeup, event_refused) = match open(&device, true) {
                Ok(opened) => (opened, Wakeup::Event, None),
                // A refused initialise leaves the client unusable -- it may
                // only be initialised once -- so the fallback starts from a
                // fresh one rather than retrying this.
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
                    let handle = EventHandle(
                        CreateEventW(None, false, false, PCWSTR::null())
                            .map_err(api("CreateEventW"))?,
                    );
                    client
                        .SetEventHandle(handle.0)
                        .map_err(api("SetEventHandle"))?;
                    Some(handle)
                }
                Wakeup::Poll { .. } => None,
            };

            let capture: IAudioCaptureClient = client.GetService().map_err(api("GetService"))?;

            let poll_interval = match wakeup {
                Wakeup::Poll { interval_ms } => {
                    Some(Duration::from_secs_f64(interval_ms / 1_000.0))
                }
                Wakeup::Event => None,
            };
            // Without this a sleep of any length under about sixteen
            // milliseconds returns after sixteen, and a polled session would
            // then collect packets at the Windows tick rate rather than at the
            // device period.
            let resolution = poll_interval.map(|_| {
                timeBeginPeriod(1);
                TimerResolution(1)
            });

            Ok(Loopback {
                client,
                capture,
                event,
                poll_interval,
                // Several device periods, so that an endpoint which never
                // signals -- the pre-1703 behaviour, and whatever a driver does
                // that the documentation did not anticipate -- reaches a
                // deadline instead of hanging its caller.
                wait_ms: (default_period_ms * 4.0).ceil().max(10.0) as u32,
                endpoint,
                format,
                default_period_ms,
                minimum_period_ms,
                buffer_frames,
                wakeup,
                event_refused,
                running: false,
                _resolution: resolution,
                _apartment: apartment,
            })
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn format(&self) -> MixFormat {
        self.format
    }

    pub fn default_period_ms(&self) -> f64 {
        self.default_period_ms
    }

    pub fn minimum_period_ms(&self) -> f64 {
        self.minimum_period_ms
    }

    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    pub fn wakeup(&self) -> Wakeup {
        self.wakeup
    }

    /// The error an event-driven initialise gave before the session fell back to
    /// polling, when it did.
    pub fn event_refused(&self) -> Option<&str> {
        self.event_refused.as_deref()
    }

    pub fn start(&mut self) -> Result<(), CaptureError> {
        // SAFETY: the client is initialised and has not been started, which is
        // the only state this method is reachable in.
        unsafe { self.client.Start() }.map_err(api("Start"))?;
        self.running = true;
        Ok(())
    }

    /// Waits for the endpoint to say a packet is ready, answering whether it
    /// said so before the deadline.
    ///
    /// A polled session has nothing to be signalled by, so it sleeps its
    /// interval and answers that the wait was ordinary: a timeout is a statement
    /// about an event that did not arrive, and there was none to arrive.
    pub fn wait(&self) -> bool {
        match (&self.event, self.poll_interval) {
            (Some(handle), _) => {
                // SAFETY: the handle is live for the session's lifetime and the
                // wait is bounded.
                let waited = unsafe { WaitForSingleObject(handle.0, self.wait_ms) };
                waited == WAIT_OBJECT_0
            }
            (None, Some(interval)) => {
                sleep(interval);
                true
            }
            (None, None) => unreachable!("a session is either event driven or polled"),
        }
    }

    /// Hands the next packet to `take`, or answers `None` when the endpoint has
    /// none waiting.
    ///
    /// The slice is the engine's buffer in the endpoint's own mix format and is
    /// valid only for the duration of the call. Where the packet is flagged
    /// silent its contents are undefined and are to be read as silence, which is
    /// why the flag reaches the caller in the [`Packet`] rather than being
    /// resolved here: substituting zeroes would be a conversion, and a caller
    /// that has to encode silence knows better than this does what silence looks
    /// like to it.
    pub fn next<R>(
        &self,
        take: impl FnOnce(&Packet, &[u8]) -> R,
    ) -> Result<Option<R>, CaptureError> {
        let mut data: *mut u8 = ptr::null_mut();
        let mut frames = 0u32;
        let mut flags = 0u32;
        let mut device_position = 0u64;
        let mut qpc_position = 0u64;
        // SAFETY: five live locals are written by the call, and the buffer it
        // reports is read only between here and the release below.
        unsafe {
            self.capture.GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                Some(&mut device_position),
                Some(&mut qpc_position),
            )
        }
        .map_err(api("GetBuffer"))?;

        // An empty buffer is the ordinary end of a drain, and it leaves the
        // position and the timestamp untouched, so nothing here may read them.
        if frames == 0 {
            return Ok(None);
        }

        let described = packet(device_position, frames, qpc_position, flags);
        // SAFETY: `GetBuffer` reported this many frames at this pointer, and the
        // frame size is the endpoint's own block alignment.
        let bytes =
            unsafe { slice::from_raw_parts(data, frames as usize * self.format.frame_bytes()) };
        let taken = take(&described, bytes);

        // SAFETY: exactly the frame count `GetBuffer` reported is released, once.
        unsafe { self.capture.ReleaseBuffer(frames) }.map_err(api("ReleaseBuffer"))?;
        Ok(Some(taken))
    }

    pub fn stop(&mut self) -> Result<(), CaptureError> {
        if !self.running {
            return Ok(());
        }
        self.running = false;
        // SAFETY: the client was started and is stopped once, since the flag is
        // cleared first.
        unsafe { self.client.Stop() }.map_err(api("Stop"))
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        // A stream left running holds the engine's loopback tap after whoever
        // asked for it has gone, including when the run ended in a panic.
        let _ = self.stop();
    }
}

/// What `GetBuffer` said about one packet, with the flags decoded.
///
/// Shared with [`crate::capture`] so that the two agree about which bit means
/// what: discontinuity and silence are different statements -- the engine losing
/// data against the host playing nothing -- and a second decoding of the same
/// flags is a second chance to conflate them.
pub(crate) fn packet(device_position: u64, frames: u32, qpc_100ns: u64, flags: u32) -> Packet {
    Packet {
        device_position,
        frames,
        qpc_100ns,
        discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
        silent: flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0,
        timestamp_error: flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0,
    }
}
