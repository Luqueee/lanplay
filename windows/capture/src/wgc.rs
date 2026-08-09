//! Windows.Graphics.Capture.
//!
//! The compositor's own capture path. It hands out surfaces from a frame pool
//! it owns and recycles, which is why nothing here returns an owned texture
//! and why `acquire` releases the previous frame before asking for the next
//! one: a frame we still hold is a buffer the pool cannot refill.
//!
//! Two choices below decide whether the phase 3 comparison is honest.
//!
//! The pool is created with `CreateFreeThreaded`, so `FrameArrived` runs on
//! the pool's own worker thread and no `DispatcherQueue` — that is, no message
//! pump belonging to somebody else — sits between the compositor and us. The
//! handler does nothing but set a flag and wake the consumer; every cost that
//! could be attributed to the API is paid inside `acquire`, on the consumer's
//! thread, where the benchmark is measuring.
//!
//! And `acquire` drains. The pool is a queue, so asking it once for "the next
//! frame" after being busy hands back a frame that is already several frames
//! old, and a latency measured from that is a measurement of our own lateness.
//! Everything queued is pulled, the newest is delivered, the rest are closed
//! immediately and counted as superseded. The counter is public because a run
//! where most frames were thrown away is a run whose latency number needs that
//! sentence next to it.

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::RO_E_CLOSED;
use windows::Win32::Graphics::Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D};
use windows::Win32::Graphics::Dxgi::{
    DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, IDXGIDevice,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::core::{Error, IInspectable, Interface, Ref};

use lanplay_telemetry::Timestamp;

use crate::backend::{
    Acquired, CaptureBackend, CaptureConfig, CaptureError, CapturedFrame, FrameMetadata, SourceMark,
};
use crate::device::CaptureDevice;

/// Pinned so the comparison with Desktop Duplication is not a comparison
/// between pixel formats.
const FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

/// What the pool's worker thread and the consumer thread share.
///
/// Only a flag and a lost marker: the signal path runs on the compositor's
/// capture thread and must not allocate, block on us, or do anything that
/// would show up in the frame it just announced.
#[derive(Default)]
struct Shared {
    /// Set by `FrameArrived`, consumed by `acquire` before it looks in the
    /// pool. A flag rather than a count because the consumer drains
    /// everything each time, so the only cost of a coalesced wake is one
    /// redundant look in an empty pool.
    arrived: Mutex<bool>,
    wake: Condvar,
    /// Every `FrameArrived`, not just the ones a consumer was waiting for.
    /// The flag above coalesces, so it cannot answer the one question that
    /// separates a slow consumer from a slow producer: whether the pool
    /// offered a frame we failed to take, or never offered it at all.
    signals: AtomicU64,
    /// Set by the item's `Closed` event: the monitor went away, the session
    /// was torn down under us, or the mode changed. Expected, not an error.
    lost: AtomicBool,
}

impl Shared {
    /// Clears the flag and reports whether it was set.
    ///
    /// Called before draining, never after: a frame that arrives while we are
    /// draining must leave the flag set, or the wait below would sleep
    /// through a frame that is already sitting in the pool.
    fn take(&self) -> bool {
        let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        core::mem::replace(&mut arrived, false)
    }

    /// Blocks until `FrameArrived` fires or the deadline passes. Returns
    /// whether a frame was announced.
    fn wait(&self, timeout: Duration) -> bool {
        let arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut arrived, _) = self
            .wake
            .wait_timeout_while(arrived, timeout, |arrived| !*arrived)
            .unwrap_or_else(PoisonError::into_inner);
        core::mem::replace(&mut arrived, false)
    }

    fn signal(&self) {
        self.signals.fetch_add(1, Ordering::Relaxed);
        let mut arrived = self.arrived.lock().unwrap_or_else(PoisonError::into_inner);
        *arrived = true;
        drop(arrived);
        self.wake.notify_one();
    }
}

/// How much of what the API produced actually reached the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counters {
    delivered: u64,
    superseded: u64,
    drained_total: u64,
    pool_recreations: u64,
}

impl Counters {
    /// Books one acquire that ended in a delivery.
    ///
    /// `drained` is every frame pulled out of the pool during that acquire,
    /// the delivered one included, so the three counters keep the invariant
    /// `drained_total == delivered + superseded`.
    fn delivered_from(&mut self, drained: u32) {
        self.delivered += 1;
        self.drained_total += u64::from(drained);
        self.superseded += u64::from(drained.saturating_sub(1));
    }
}

/// Why a pool call failed, split the way `acquire` has to answer.
#[derive(Debug, PartialEq, Eq)]
enum Fault {
    /// The capture is gone and must be rebuilt. The harness restarts; it does
    /// not report a failure.
    Lost,
    Api {
        call: &'static str,
        hresult: i32,
    },
}

/// A capture that has stopped being one is not the same event as a call that
/// went wrong, and treating a mode change as an error would end a run that had
/// only been interrupted.
fn classify(call: &'static str, error: &Error) -> Fault {
    let code = error.code();
    if code == RO_E_CLOSED
        || code == DXGI_ERROR_DEVICE_REMOVED
        || code == DXGI_ERROR_DEVICE_RESET
        || code == DXGI_ERROR_ACCESS_LOST
    {
        Fault::Lost
    } else {
        Fault::Api {
            call,
            hresult: code.0,
        }
    }
}

fn api(call: &'static str) -> impl Fn(Error) -> CaptureError {
    move |error| CaptureError::Api {
        call,
        hresult: error.code().0,
    }
}

/// The objects that exist only while capturing.
struct Active {
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    frame_arrived: i64,
    item_closed: i64,
    /// What the pool's surfaces currently measure. Compared against each
    /// frame's `ContentSize` to notice a resize.
    pool_size: SizeInt32,
    buffers: i32,
}

/// The frame the caller is looking at, and the texture inside it.
struct Held {
    frame: Direct3D11CaptureFrame,
    texture: ID3D11Texture2D,
}

/// Windows.Graphics.Capture, as a backend.
///
/// Not `Send`: the D3D11 and DXGI interfaces it holds are Win32 COM and
/// windows-rs does not mark them thread-safe. Build it and drive it on one
/// thread. That costs nothing here, because the frame pool is free-threaded
/// and needs no message pump on the consumer's thread.
pub struct GraphicsCapture {
    /// The shared D3D11 device, seen through WinRT's eyes. The pool renders
    /// into this device's memory, so the frames need no cross-device copy.
    winrt_device: IDirect3DDevice,
    monitor: HMONITOR,
    config: Option<CaptureConfig>,
    active: Option<Active>,
    held: Option<Held>,
    shared: Arc<Shared>,
    counters: Counters,
    border_suppressed: bool,
}

impl GraphicsCapture {
    /// Wraps the shared capture device for WinRT and finds the monitor to
    /// capture. Nothing starts until [`CaptureBackend::start`].
    pub fn new(device: &CaptureDevice) -> Result<GraphicsCapture, CaptureError> {
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err(CaptureError::Unsupported(
                "Windows.Graphics.Capture is not supported on this machine".into(),
            ));
        }

        let dxgi_device: IDXGIDevice = device.device().cast().map_err(|e| {
            CaptureError::Unsupported(format!(
                "the capture device does not expose IDXGIDevice: 0x{:08X}",
                e.code().0 as u32
            ))
        })?;

        // SAFETY: `dxgi_device` is a live interface on the shared device, and
        // the returned object is refcounted by `windows`.
        let inspectable: IInspectable = unsafe {
            CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
                .map_err(api("CreateDirect3D11DeviceFromDXGIDevice"))?
        };
        let winrt_device: IDirect3DDevice = inspectable
            .cast()
            .map_err(api("IInspectable::QueryInterface(IDirect3DDevice)"))?;

        // SAFETY: `output` is the live output the device was built for.
        let desc = unsafe { device.output().GetDesc() }.map_err(api("IDXGIOutput::GetDesc"))?;
        if desc.Monitor.is_invalid() {
            return Err(CaptureError::Unsupported(
                "the selected output is not attached to a monitor".into(),
            ));
        }

        Ok(GraphicsCapture {
            winrt_device,
            monitor: desc.Monitor,
            config: None,
            active: None,
            held: None,
            shared: Arc::new(Shared::default()),
            counters: Counters::default(),
            border_suppressed: false,
        })
    }

    /// Frames delivered to the caller.
    pub fn delivered(&self) -> u64 {
        self.counters.delivered
    }

    /// Frames pulled out of the pool and thrown away because a newer one was
    /// already queued behind them.
    pub fn superseded(&self) -> u64 {
        self.counters.superseded
    }

    /// Every frame taken out of the pool, delivered and superseded together.
    pub fn drained_total(&self) -> u64 {
        self.counters.drained_total
    }

    /// How many times the item resized under us and the pool was rebuilt in
    /// place.
    /// Every `FrameArrived` the pool raised, whether or not a frame was taken
    /// for it. Compared against `delivered`, this is what tells a slow
    /// consumer apart from a pool that never offered the frame.
    pub fn signals(&self) -> u64 {
        self.shared.signals.load(Ordering::Relaxed)
    }

    pub fn pool_recreations(&self) -> u64 {
        self.counters.pool_recreations
    }

    /// Whether the compositor's capture border was successfully turned off.
    ///
    /// False means the border is being drawn into the captured frames, which
    /// is content this benchmark did not ask for and Desktop Duplication does
    /// not produce. A report comparing pixels has to say so.
    pub fn border_suppressed(&self) -> bool {
        self.border_suppressed
    }

    /// Builds item, pool and session for `config` and starts the capture.
    fn build(&mut self, config: CaptureConfig) -> Result<(), CaptureError> {
        if config.buffers == 0 {
            return Err(CaptureError::Unsupported(
                "the frame pool needs at least one buffer".into(),
            ));
        }
        let buffers = i32::try_from(config.buffers).map_err(|_| {
            CaptureError::Unsupported(format!("{} is not a usable buffer count", config.buffers))
        })?;

        // The activation factory's interop interface, not the picker: a
        // benchmark that needs a human to choose a monitor is not a benchmark.
        let interop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| {
                CaptureError::Unsupported(format!(
                    "IGraphicsCaptureItemInterop is unavailable: 0x{:08X}",
                    e.code().0 as u32
                ))
            })?;
        // SAFETY: `self.monitor` came from the live output's description and
        // was checked for validity when this backend was constructed.
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(self.monitor) }
            .map_err(api("IGraphicsCaptureItemInterop::CreateForMonitor"))?;

        let size = item.Size().map_err(api("GraphicsCaptureItem::Size"))?;

        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &self.winrt_device,
            FORMAT,
            buffers,
            size,
        )
        .map_err(api("Direct3D11CaptureFramePool::CreateFreeThreaded"))?;

        self.shared.lost.store(false, Ordering::Release);
        let _ = self.shared.take();

        let on_frame = {
            let shared = Arc::clone(&self.shared);
            // Runs on the pool's worker thread. It must stay this short: any
            // work here is work the compositor waits on, and it would show up
            // in the very latency this backend exists to measure.
            TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
                move |_pool: Ref<Direct3D11CaptureFramePool>, _args: Ref<IInspectable>| {
                    shared.signal();
                    Ok(())
                },
            )
        };
        let frame_arrived = pool
            .FrameArrived(&on_frame)
            .map_err(api("Direct3D11CaptureFramePool::FrameArrived"))?;

        let on_closed = {
            let shared = Arc::clone(&self.shared);
            TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
                move |_item: Ref<GraphicsCaptureItem>, _args: Ref<IInspectable>| {
                    shared.lost.store(true, Ordering::Release);
                    // Wake a consumer parked in `acquire`, or it would sit
                    // there until its timeout for a frame that can never come.
                    shared.signal();
                    Ok(())
                },
            )
        };
        let item_closed = item
            .Closed(&on_closed)
            .map_err(api("GraphicsCaptureItem::Closed"))?;

        let session = pool
            .CreateCaptureSession(&item)
            .map_err(api("Direct3D11CaptureFramePool::CreateCaptureSession"))?;

        // The cursor defaults to on, so only the request to turn it off can
        // go unmet. When it does, the frames carry content the experiment did
        // not ask for and which Desktop Duplication composites elsewhere:
        // that is a different picture, not a slower one, and the run should
        // not start.
        if let Err(error) = session.SetIsCursorCaptureEnabled(config.cursor)
            && !config.cursor
        {
            return Err(CaptureError::Unsupported(format!(
                "this build cannot disable cursor capture: 0x{:08X}",
                error.code().0 as u32
            )));
        }

        // Best effort: builds that require the capture border refuse to drop
        // it without a declared capability. Recorded rather than ignored,
        // because the border lands in the pixels.
        self.border_suppressed = session.SetIsBorderRequired(false).is_ok();

        session
            .StartCapture()
            .map_err(api("GraphicsCaptureSession::StartCapture"))?;

        self.active = Some(Active {
            item,
            pool,
            session,
            frame_arrived,
            item_closed,
            pool_size: size,
            buffers,
        });
        Ok(())
    }

    /// Gives the pool its buffer back.
    ///
    /// The texture reference goes first so that `Close` is the call that
    /// actually frees the surface, rather than leaving it alive until this
    /// function returns.
    fn release_held(&mut self) {
        if let Some(held) = self.held.take() {
            drop(held.texture);
            let _ = held.frame.Close();
        }
    }
}

/// Pulls everything the pool has queued, keeping only the newest.
///
/// Returns the newest frame and how many were taken out in total. The older
/// ones are closed as they are found: they are buffers the pool needs back,
/// and holding them while we look for a newer frame would shrink the pool to
/// nothing within a few acquires.
fn drain(
    pool: &Direct3D11CaptureFramePool,
) -> Result<(Option<Direct3D11CaptureFrame>, u32), Fault> {
    let mut newest: Option<Direct3D11CaptureFrame> = None;
    let mut taken = 0u32;
    loop {
        match pool.TryGetNextFrame() {
            Ok(frame) => {
                taken += 1;
                if let Some(stale) = newest.replace(frame) {
                    let _ = stale.Close();
                }
            }
            // An empty pool is reported as a success carrying no object, which
            // windows-rs surfaces as an error whose HRESULT is S_OK.
            Err(error) if error.code().is_ok() => return Ok((newest, taken)),
            Err(error) => {
                if let Some(stale) = newest {
                    let _ = stale.Close();
                }
                return Err(classify(
                    "Direct3D11CaptureFramePool::TryGetNextFrame",
                    &error,
                ));
            }
        }
    }
}

/// The D3D11 texture behind a WinRT surface.
fn texture_of(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D, CaptureError> {
    let surface = frame
        .Surface()
        .map_err(api("Direct3D11CaptureFrame::Surface"))?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(api(
        "IDirect3DSurface::QueryInterface(IDirect3DDxgiInterfaceAccess)",
    ))?;
    // SAFETY: `access` is a live interface on the frame's surface and the
    // requested IID matches the type parameter.
    unsafe { access.GetInterface::<ID3D11Texture2D>() }
        .map_err(api("IDirect3DDxgiInterfaceAccess::GetInterface"))
}

impl CaptureBackend for GraphicsCapture {
    fn name(&self) -> &'static str {
        "windows-graphics-capture"
    }

    fn start(&mut self, config: CaptureConfig) -> Result<(), CaptureError> {
        self.stop();
        self.config = Some(config);
        self.build(config)
    }

    fn acquire(&mut self) -> Result<Acquired<'_>, CaptureError> {
        // First, always: the pool cannot refill a buffer we are still holding,
        // and doing this in a destructor would put the release wherever the
        // caller happened to finish reading instead of immediately before the
        // next request.
        self.release_held();

        if self.shared.lost.load(Ordering::Acquire) {
            return Ok(Acquired::Lost);
        }

        let timeout = Duration::from_millis(u64::from(
            self.config
                .ok_or(CaptureError::NotStarted)?
                .acquire_timeout_ms,
        ));
        let Some(active) = self.active.as_mut() else {
            return Err(CaptureError::NotStarted);
        };

        // Consumed before the first look so that a frame arriving mid-drain
        // still counts as a wake for the wait below.
        let _ = self.shared.take();

        let deadline = Instant::now() + timeout;
        let mut pending = 0u32;
        let frame = loop {
            match drain(&active.pool) {
                Ok((Some(frame), taken)) => {
                    pending += taken;
                    break frame;
                }
                Ok((None, taken)) => pending += taken,
                Err(Fault::Lost) => return Ok(Acquired::Lost),
                Err(Fault::Api { call, hresult }) => {
                    return Err(CaptureError::Api { call, hresult });
                }
            }

            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !self.shared.wait(remaining) {
                return Ok(Acquired::Timeout);
            }
            if self.shared.lost.load(Ordering::Acquire) {
                return Ok(Acquired::Lost);
            }
        };

        let texture = match texture_of(&frame) {
            Ok(texture) => texture,
            Err(error) => {
                let _ = frame.Close();
                return Err(error);
            }
        };
        let system_relative = match frame.SystemRelativeTime() {
            Ok(span) => span,
            Err(error) => {
                let _ = frame.Close();
                return Err(api("Direct3D11CaptureFrame::SystemRelativeTime")(error));
            }
        };

        // The texture, not the content size: after a resize the pool's
        // surfaces are still the old size for one more frame, and reporting
        // dimensions the texture does not have would mislead whoever copies
        // out of it.
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `texture` is a live ID3D11Texture2D and `desc` is a valid
        // writable D3D11_TEXTURE2D_DESC.
        unsafe { texture.GetDesc(&mut desc) };

        if let Ok(content) = frame.ContentSize()
            && content != active.pool_size
        {
            // Recreating while still holding this frame is sound: the frame
            // owns a reference to its surface, so the pool dropping its
            // buffers cannot take the texture out from under the caller.
            active
                .pool
                .Recreate(&self.winrt_device, FORMAT, active.buffers, content)
                .map_err(api("Direct3D11CaptureFramePool::Recreate"))?;
            active.pool_size = content;
            self.counters.pool_recreations += 1;
        }

        self.counters.delivered_from(pending);
        self.held = Some(Held { frame, texture });
        let held = self.held.as_ref().expect("just stored");

        // Last thing before returning, so the delay this feeds is the delay
        // the caller actually experienced.
        let acquired = Timestamp::now();
        Ok(Acquired::Frame(CapturedFrame {
            width: desc.Width,
            height: desc.Height,
            source: SourceMark::CompositorRendered(Timestamp::from_time_span(
                system_relative.Duration,
            )),
            acquired,
            metadata: FrameMetadata {
                // WGC does not report how far behind we are; only the drain
                // count says anything about that.
                accumulated_frames: None,
                pending: Some(pending),
                duplicate: false,
            },
            texture: &held.texture,
        }))
    }

    fn stop(&mut self) {
        self.release_held();
        let Some(active) = self.active.take() else {
            return;
        };
        // Handlers off before the objects close, so the pool's worker thread
        // cannot be inside our closure while the session is being torn down.
        let _ = active.pool.RemoveFrameArrived(active.frame_arrived);
        let _ = active.item.RemoveClosed(active.item_closed);
        let _ = active.session.Close();
        let _ = active.pool.Close();
        // `GraphicsCaptureItem` has no Close; dropping the last reference is
        // how it goes away.
        drop(active.item);
        let _ = self.shared.take();
    }

    fn restart(&mut self) -> Result<(), CaptureError> {
        let config = self.config.ok_or(CaptureError::NotStarted)?;
        self.stop();
        self.build(config)
    }
}

impl Drop for GraphicsCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::E_FAIL;

    #[test]
    fn a_lone_frame_is_delivered_and_supersedes_nothing() {
        let mut counters = Counters::default();
        counters.delivered_from(1);
        assert_eq!(counters.delivered, 1);
        assert_eq!(counters.superseded, 0);
        assert_eq!(counters.drained_total, 1);
    }

    #[test]
    fn a_burst_delivers_one_and_discards_the_rest() {
        let mut counters = Counters::default();
        counters.delivered_from(4);
        assert_eq!(counters.delivered, 1);
        assert_eq!(counters.superseded, 3);
        assert_eq!(counters.drained_total, 4);
    }

    /// The three numbers are printed side by side in the report, so a reader
    /// must be able to add two of them and get the third.
    #[test]
    fn every_drained_frame_is_either_delivered_or_superseded() {
        let mut counters = Counters::default();
        for taken in [1, 3, 1, 1, 7, 2] {
            counters.delivered_from(taken);
        }
        assert_eq!(counters.drained_total, 15);
        assert_eq!(counters.delivered, 6);
        assert_eq!(counters.superseded, 9);
        assert_eq!(
            counters.drained_total,
            counters.delivered + counters.superseded
        );
    }

    #[test]
    fn a_closed_item_is_a_restart_not_a_failure() {
        assert_eq!(
            classify("call", &Error::from_hresult(RO_E_CLOSED)),
            Fault::Lost
        );
        assert_eq!(
            classify("call", &Error::from_hresult(DXGI_ERROR_DEVICE_REMOVED)),
            Fault::Lost
        );
        assert_eq!(
            classify("call", &Error::from_hresult(DXGI_ERROR_ACCESS_LOST)),
            Fault::Lost
        );
    }

    #[test]
    fn an_unexpected_hresult_keeps_its_code_and_call_site() {
        assert_eq!(
            classify("TryGetNextFrame", &Error::from_hresult(E_FAIL)),
            Fault::Api {
                call: "TryGetNextFrame",
                hresult: E_FAIL.0
            }
        );
    }
}
