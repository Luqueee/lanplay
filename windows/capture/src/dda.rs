//! Desktop Duplication.
//!
//! The older of the two APIs and the one with the sharper edges. It hands back
//! the desktop image itself rather than a copy, which is why it insists the
//! image is given back before the next one is asked for, and why the window
//! between those two calls is the only part of this file worth reading
//! carefully.
//!
//! It also reports something Windows.Graphics.Capture does not:
//! `AccumulatedFrames`, the number of desktop updates that happened since the
//! last acquire. Anything above one is the API saying the consumer is behind,
//! which for a streamer is the measurement, not an aside. It is counted here
//! and exposed as a running total.

#![cfg(windows)]

use lanplay_telemetry::Timestamp;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_INVALID_CALL, DXGI_ERROR_NOT_CURRENTLY_AVAILABLE,
    DXGI_ERROR_UNSUPPORTED, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIOutput1,
    IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource,
};
use windows::core::Interface;

use crate::backend::{
    Acquired, CaptureBackend, CaptureConfig, CaptureError, CapturedFrame, FrameMetadata, SourceMark,
};
use crate::device::CaptureDevice;

/// Which entry point creates the duplication.
///
/// `DuplicateOutput1` is preferred because it is the only one that lets us
/// state a format: without it the driver picks, and on an HDR-capable output
/// it can pick a 10-bit or float format, at which point a comparison against
/// Windows.Graphics.Capture would partly be a comparison between pixel
/// formats. `IDXGIOutput5` is Windows 10 1703 and later; the fallback exists
/// for the machine that does not have it, and the report says which was used.
enum Duplicator {
    WithFormatList(IDXGIOutput5),
    Legacy(IDXGIOutput1),
}

pub struct DesktopDuplication {
    /// The shared capture device, by interface pointer. Cloning an
    /// `ID3D11Device` bumps a refcount; it does not make a second device, and
    /// a second device is the one thing this backend must not have.
    device: ID3D11Device,
    duplicator: Duplicator,
    api: &'static str,
    duplication: Option<IDXGIOutputDuplication>,
    /// The desktop image of the frame we currently owe DXGI a `ReleaseFrame`
    /// for. `Some` exactly when a frame is outstanding.
    held: Option<ID3D11Texture2D>,
    config: Option<CaptureConfig>,
    access_lost: u64,
    accumulated_over_one: u64,
}

impl DesktopDuplication {
    /// Binds to the output the shared device was opened for.
    ///
    /// Duplication is only legal from the device that owns the output;
    /// [`CaptureDevice::open`] picks the adapter from the output, so the
    /// pairing holds by construction and there is nothing to check here.
    pub fn new(device: &CaptureDevice) -> Result<DesktopDuplication, CaptureError> {
        let (duplicator, api) = match device.output_as::<IDXGIOutput5>() {
            Ok(output) => (Duplicator::WithFormatList(output), "DuplicateOutput1"),
            // Only the cast falling back is a fallback. A `DuplicateOutput1`
            // that fails on a supported cast is a real failure and is
            // reported, not papered over with a call that would silently
            // give us a different format.
            Err(_) => (
                Duplicator::Legacy(device.output_as::<IDXGIOutput1>()?),
                "DuplicateOutput",
            ),
        };

        Ok(DesktopDuplication {
            device: device.device().clone(),
            duplicator,
            api,
            duplication: None,
            held: None,
            config: None,
            access_lost: 0,
            accumulated_over_one: 0,
        })
    }

    /// `"DuplicateOutput1"` or `"DuplicateOutput"`, for the report header.
    pub fn api(&self) -> &'static str {
        self.api
    }

    /// How many times the duplication became invalid.
    ///
    /// Mode changes, desktop switches and some fullscreen transitions all do
    /// this. It is a normal event on a machine someone is using, and the count
    /// belongs in the result rather than in an error log.
    pub fn access_lost(&self) -> u64 {
        self.access_lost
    }

    /// Frames that arrived with `AccumulatedFrames > 1`, i.e. the desktop
    /// changed more than once while we were busy with the previous frame.
    pub fn accumulated_over_one(&self) -> u64 {
        self.accumulated_over_one
    }

    fn duplicate(&self) -> Result<IDXGIOutputDuplication, CaptureError> {
        // SAFETY: both calls take a live device interface and, for the format
        // list, a slice whose length the binding derives itself. The returned
        // duplication is refcounted by `windows`.
        unsafe {
            match &self.duplicator {
                Duplicator::WithFormatList(output) => output
                    // The flags argument is documented as reserved and must be
                    // zero.
                    .DuplicateOutput1(&self.device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM])
                    .map_err(|e| creation_error("IDXGIOutput5::DuplicateOutput1", e)),
                Duplicator::Legacy(output) => output
                    .DuplicateOutput(&self.device)
                    .map_err(|e| creation_error("IDXGIOutput1::DuplicateOutput", e)),
            }
        }
    }
}

impl CaptureBackend for DesktopDuplication {
    fn name(&self) -> &'static str {
        "desktop-duplication"
    }

    /// Creates the duplication.
    ///
    /// `CaptureConfig::buffers` has no counterpart here — Desktop Duplication
    /// owns exactly one desktop image and does not queue — and `cursor` has
    /// none either: the cursor is always reported separately and never
    /// composited into the frame. Only `acquire_timeout_ms` reaches the API.
    fn start(&mut self, config: CaptureConfig) -> Result<(), CaptureError> {
        // A second duplication on the same output from the same device fails,
        // so an already-started backend is torn down first and `start` stays
        // idempotent.
        self.stop();
        let duplication = self.duplicate()?;
        self.duplication = Some(duplication);
        self.config = Some(config);
        Ok(())
    }

    fn acquire(&mut self) -> Result<Acquired<'_>, CaptureError> {
        let Some(config) = self.config else {
            return Err(CaptureError::NotStarted);
        };

        // Our own reference to the previous desktop image goes first: once
        // `ReleaseFrame` returns, the surface is the compositor's again and a
        // pointer we kept would name something being overwritten.
        let holding = self.held.take().is_some();

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;

        let Some(duplication) = self.duplication.as_ref() else {
            return Err(CaptureError::NotStarted);
        };

        // SAFETY: `duplication` is live, and both out-parameters are local and
        // correctly typed for the binding.
        //
        // Nothing may be inserted between these two calls. While the client
        // does not own the frame, the OS copies every desktop update into the
        // duplication surface, and that cost lands on the machine running the
        // game we are trying not to disturb. Microsoft asks for the gap to be
        // minimal; this is where that is honoured.
        let outcome = unsafe {
            if holding {
                // A release that fails means the frame is already gone. The
                // acquire below is what decides whether the duplication still
                // works, so its result is the one that matters.
                let _ = duplication.ReleaseFrame();
            }
            duplication.AcquireNextFrame(config.acquire_timeout_ms, &mut info, &mut resource)
        };

        if let Err(err) = outcome {
            return match err.code() {
                // A static desktop produces nothing to hand over. Not a fault.
                DXGI_ERROR_WAIT_TIMEOUT => Ok(Acquired::Timeout),
                DXGI_ERROR_ACCESS_LOST => {
                    self.access_lost += 1;
                    Ok(Acquired::Lost)
                }
                code => Err(CaptureError::Api {
                    call: "IDXGIOutputDuplication::AcquireNextFrame",
                    hresult: code.0,
                }),
            };
        }

        let texture = match resource {
            Some(resource) => resource
                .cast::<ID3D11Texture2D>()
                .map_err(|e| CaptureError::Api {
                    call: "IDXGIResource::QueryInterface(ID3D11Texture2D)",
                    hresult: e.code().0,
                }),
            None => Err(CaptureError::Api {
                call: "IDXGIOutputDuplication::AcquireNextFrame",
                hresult: DXGI_ERROR_INVALID_CALL.0,
            }),
        };

        let texture = match texture {
            Ok(texture) => texture,
            Err(err) => {
                // We own a frame we cannot use. Hand it back now: leaving it
                // outstanding would fail every subsequent acquire.
                if let Some(duplication) = self.duplication.as_ref() {
                    // SAFETY: live duplication, no arguments.
                    unsafe {
                        let _ = duplication.ReleaseFrame();
                    }
                }
                return Err(err);
            }
        };

        // Asking the texture rather than the duplication description, because
        // a rotated output makes the two disagree and the consumer will be
        // handed this texture, not that description.
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: live texture, valid out-parameter.
        unsafe { texture.GetDesc(&mut desc) };

        if frame_is_behind(info.AccumulatedFrames) {
            self.accumulated_over_one += 1;
        }

        let metadata = FrameMetadata {
            accumulated_frames: Some(info.AccumulatedFrames),
            // Desktop Duplication keeps no pool, so there is no pool pressure
            // to report. Reporting zero would be a claim we cannot make.
            pending: None,
            duplicate: frame_is_duplicate(info.LastPresentTime, info.AccumulatedFrames),
        };

        // Passed through as the API gave it, zero included: a cursor-only
        // update carries no present time, and `CapturedFrame::delivery_delay`
        // already declines to call a zero mark a delay.
        let source = SourceMark::DesktopPresented(Timestamp::from_qpc_ticks(info.LastPresentTime));

        let texture = &*self.held.insert(texture);
        let acquired = Timestamp::now();

        Ok(Acquired::Frame(CapturedFrame {
            width: desc.Width,
            height: desc.Height,
            source,
            acquired,
            metadata,
            texture,
        }))
    }

    fn stop(&mut self) {
        if self.held.take().is_some() {
            if let Some(duplication) = self.duplication.as_ref() {
                // SAFETY: live duplication, no arguments.
                unsafe {
                    let _ = duplication.ReleaseFrame();
                }
            }
        }
        self.duplication = None;
    }

    /// Rebuilds after [`Acquired::Lost`] with the configuration `start` was
    /// given, which is the whole recovery path: whatever invalidated the
    /// duplication also invalidated nothing we chose.
    fn restart(&mut self) -> Result<(), CaptureError> {
        let config = self.config.ok_or(CaptureError::NotStarted)?;
        self.start(config)
    }
}

impl Drop for DesktopDuplication {
    fn drop(&mut self) {
        // Not the release path for a frame in flight — that happens at the
        // head of the next `acquire`, and the `&mut self` borrow guarantees
        // no frame outlives it. This only covers being dropped mid-stream.
        self.stop();
    }
}

/// A frame that says the desktop image did not change.
///
/// No present time and no accumulated updates together mean the duplication
/// woke us for something other than new desktop content — a cursor move, or a
/// desktop-image-unchanged notification. The pixels are the ones we already
/// had.
const fn frame_is_duplicate(last_present_time: i64, accumulated_frames: u32) -> bool {
    last_present_time == 0 && accumulated_frames == 0
}

/// Whether the API is telling us it updated the desktop more than once while
/// we were busy, i.e. we dropped content we never saw.
const fn frame_is_behind(accumulated_frames: u32) -> bool {
    accumulated_frames > 1
}

fn creation_error(call: &'static str, err: windows::core::Error) -> CaptureError {
    match err.code() {
        DXGI_ERROR_UNSUPPORTED => CaptureError::Unsupported(format!(
            "{call}: this output cannot be duplicated as B8G8R8A8_UNORM"
        )),
        DXGI_ERROR_NOT_CURRENTLY_AVAILABLE => CaptureError::Unsupported(format!(
            "{call}: the maximum number of duplications on this output already exists"
        )),
        code => CaptureError::Api {
            call,
            hresult: code.0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_present_and_no_accumulation_is_the_unchanged_notification() {
        assert!(frame_is_duplicate(0, 0));
    }

    #[test]
    fn a_present_time_means_the_desktop_changed() {
        // The mouse moved and the desktop was redrawn: still a real frame.
        assert!(!frame_is_duplicate(1_234_567, 0));
    }

    #[test]
    fn accumulation_without_a_present_time_is_still_a_real_update() {
        // A protected-content or cross-adapter path can hand back updates
        // whose present time is unreported; the accumulation says content
        // moved, so calling it a duplicate would discard a real frame.
        assert!(!frame_is_duplicate(0, 3));
    }

    #[test]
    fn only_more_than_one_accumulated_frame_means_we_are_behind() {
        // One accumulated frame is the steady state: exactly the update we
        // are being handed. Counting it would report every frame as a miss.
        assert!(!frame_is_behind(0));
        assert!(!frame_is_behind(1));
        assert!(frame_is_behind(2));
    }
}
