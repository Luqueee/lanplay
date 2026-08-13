//! The window, the swap chain and the raw input registration.
//!
//! One borderless window filling the IDD-LAB output, one flat colour, one
//! present loop. It has no content of its own because content would be
//! something else to blame: every microsecond between the input message and
//! the present returning belongs to Windows, the driver and DXGI, and to
//! nothing this file does.
//!
//! Rejected: rendering with GDI, which would give a number about `BitBlt`
//! rather than about the path a game takes; going exclusive fullscreen, which
//! changes what Desktop Duplication sees on the other side of the pipeline and
//! would measure a different thing; and driving the loop from `GetMessage`,
//! which would only present when something happened and would fold a
//! thread wake-up into every interval.
//!
//! The two instants are read with [`lanplay_telemetry::Timestamp`], which is
//! `QueryPerformanceCounter` on Windows. Both are taken on this machine, so
//! the interval is host-local.

use std::cell::RefCell;

use lanplay_capture::{CaptureDevice, output_named, outputs};
use lanplay_telemetry::{Nanos, Timestamp};
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11DepthStencilView, ID3D11RenderTargetView, ID3D11Texture2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    DXGI_FEATURE_PRESENT_ALLOW_TEARING, DXGI_MWA_NO_ALT_ENTER, DXGI_PRESENT,
    DXGI_PRESENT_ALLOW_TEARING, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING, DXGI_SWAP_EFFECT_FLIP_DISCARD,
    DXGI_USAGE_RENDER_TARGET_OUTPUT, IDXGIDevice1, IDXGIFactory2, IDXGIFactory5, IDXGIOutput,
    IDXGISwapChain1,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{RAWINPUTDEVICE, RIDEV_INPUTSINK, RegisterRawInputDevices};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetForegroundWindow,
    HWND_TOPMOST, IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PeekMessageW, PostQuitMessage,
    RegisterClassExW, SW_SHOW, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow,
    UnregisterClassW, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_INPUT, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    WM_XBUTTONDOWN, WM_XBUTTONUP, WNDCLASSEXW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::{BOOL, HRESULT, Interface, PCWSTR};

use crate::cli::{self, Cli};
use crate::flash::Flash;
use crate::report::{Display, Observed, Tally};

/// The window class, unique to this target so that nothing else in the
/// interactive session can be mistaken for it.
const CLASS_NAME: &str = "LanPlayInputLatencyTarget";

/// `DXGI_STATUS_OCCLUDED`, which the `windows` crate does not name.
///
/// A success code, so a present that returns it did not fail, but the frame
/// went somewhere nobody could see it. Worth counting: an occluded present is
/// a measured interval whose other end never reached the display.
const OCCLUDED: HRESULT = HRESULT(0x087A_0001u32 as i32);

const HID_USAGE_PAGE_GENERIC: u16 = 0x01;
const HID_USAGE_GENERIC_MOUSE: u16 = 0x02;
const HID_USAGE_GENERIC_KEYBOARD: u16 = 0x06;

/// Which queue delivered an event.
///
/// Keyboard and mouse are separated on the window message side only. Telling
/// a raw keyboard event from a raw mouse one needs `GetRawInputData`, and that
/// call would run inside the interval being measured for the sake of a number
/// nobody is going to act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Raw,
    Key,
    Mouse,
}

thread_local! {
    /// Events the window procedure has handled and the loop has not yet seen.
    ///
    /// A thread-local rather than a pointer stashed in `GWLP_USERDATA`: the
    /// window procedure only ever runs on the thread that dispatches to it,
    /// which is the same thread that runs the loop, so the two already share a
    /// lifetime and there is no raw pointer to get wrong.
    static INBOX: RefCell<Vec<(Timestamp, Source)>> = const { RefCell::new(Vec::new()) };
}

/// Records that an input event was handled, now.
///
/// The clock is read before anything else, because the interval starts when
/// the message was handled and every instruction ahead of the read would be
/// charged to somebody further down the pipeline.
fn note(source: Source) {
    let at = Timestamp::now();
    INBOX.with_borrow_mut(|inbox| inbox.push((at, source)));
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            note(Source::Raw);
            // The system reclaims the buffer behind a WM_INPUT only once the
            // default handler has seen the message, so this one is forwarded
            // even though nothing here reads its payload.
            //
            // SAFETY: the arguments are the ones this procedure was called
            // with, passed on unchanged.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        // Key-up as well as key-down, so that the two paths can be compared
        // event for event: `SendInput` of one keystroke produces a press and a
        // release, and raw input reports both.
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
            note(Source::Key);
            // Deliberately not forwarded. The default handler would open the
            // window menu on Alt and close the window on Alt+F4, and a target
            // that can be dismissed mid-run by an injected keystroke is a
            // target that will be.
            LRESULT(0)
        }
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP
        | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_XBUTTONDOWN | WM_XBUTTONUP | WM_MOUSEWHEEL
        | WM_MOUSEHWHEEL => {
            note(Source::Mouse);
            LRESULT(0)
        }
        // Claimed, and nothing painted: the swap chain owns every pixel, and
        // letting GDI erase the window would put a grey frame in front of
        // whatever the capture side is looking for.
        WM_ERASEBKGND => LRESULT(1),
        WM_CLOSE | WM_DESTROY => {
            // SAFETY: no arguments to get wrong, and no failure mode.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: the arguments are the ones this procedure was called with.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Destroys the window and unregisters its class however the run ends.
///
/// A class left registered would make the next `RegisterClassEx` in this
/// process fail, which is the sort of thing that only surfaces on the retry
/// after a failure, when nobody is looking for it.
struct WindowGuard {
    hwnd: HWND,
    instance: HINSTANCE,
    class: Vec<u16>,
}

impl Drop for WindowGuard {
    fn drop(&mut self) {
        // SAFETY: both handles were produced by the calls being undone here,
        // the class name still owns its NUL-terminated buffer, and neither has
        // been released already.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
            let _ = UnregisterClassW(PCWSTR(self.class.as_ptr()), Some(self.instance));
        }
    }
}

/// Accumulates what the run observed as the window procedure feeds it.
struct Recorder {
    flash: Flash,
    observed: Observed,
    /// The event waiting for the present that will first show white. At most
    /// one, because only the event that found the window at rest caused a
    /// transition worth timing.
    pending: Option<(Timestamp, Source)>,
}

impl Recorder {
    fn tally_mut(&mut self, source: Source) -> &mut Tally {
        match source {
            Source::Raw => &mut self.observed.raw,
            Source::Key | Source::Mouse => &mut self.observed.messages,
        }
    }

    /// Takes one handled event, answering whether it armed the flash.
    fn accept(&mut self, at: Timestamp, source: Source) -> bool {
        match source {
            Source::Raw => self.observed.raw.seen += 1,
            Source::Key => {
                self.observed.messages.seen += 1;
                self.observed.key_messages += 1;
            }
            Source::Mouse => {
                self.observed.messages.seen += 1;
                self.observed.mouse_messages += 1;
            }
        }
        if self.flash.arm() {
            self.pending = Some((at, source));
            true
        } else {
            self.tally_mut(source).during_flash += 1;
            false
        }
    }

    /// Empties the window procedure's inbox, answering whether anything in it
    /// armed the flash.
    fn drain(&mut self) -> bool {
        INBOX.with_borrow_mut(|inbox| {
            let mut armed = false;
            for (at, source) in inbox.drain(..) {
                armed |= self.accept(at, source);
            }
            armed
        })
    }

    /// One present of the current colour returned at `returned`, and whether it
    /// closed a timed interval so that a caller can attribute something to the
    /// event rather than to the present. Every present calls this and only the
    /// one that first carried the change closes anything.
    fn presented(&mut self, returned: Timestamp) -> bool {
        if self.flash.presented()
            && let Some((at, source)) = self.pending.take()
        {
            let interval = returned.saturating_since(at);
            self.tally_mut(source)
                .latency
                .saturating_record(interval.get());
            return true;
        }
        false
    }
}

pub fn run(cli: &Cli) -> Result<Observed, String> {
    let index = output_named(cli::DISPLAY).map_err(|error| error.to_string())?;
    let info = outputs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|output| output.index == index)
        .ok_or_else(|| format!("output {index} disappeared between being named and being used"))?;

    // Opened before the window exists, because this is also what makes the
    // process per-monitor DPI aware, and a window created before that would be
    // placed and sized in virtualised coordinates on a scaled display.
    let device = CaptureDevice::open(index).map_err(|error| error.to_string())?;
    // SAFETY: the output came from DXGI's own enumeration and `GetDesc` only
    // fills a caller-owned struct.
    let desc = unsafe { device.output().GetDesc() }
        .map_err(|error| hresult("IDXGIOutput::GetDesc", &error))?;
    let bounds = desc.DesktopCoordinates;

    let display = Display {
        index,
        device_name: info.device_name,
        monitor_name: info.monitor_name,
        adapter_name: info.adapter_name,
        left: bounds.left,
        top: bounds.top,
        width: (bounds.right - bounds.left) as u32,
        height: (bounds.bottom - bounds.top) as u32,
    };

    // SAFETY: a null module name asks for the running executable, which always
    // has a handle.
    let module =
        unsafe { GetModuleHandleW(None) }.map_err(|error| hresult("GetModuleHandleW", &error))?;
    let instance = HINSTANCE(module.0);

    let class_name = wide(CLASS_NAME);
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        // SAFETY: a null module with a predefined cursor id is the documented
        // way to ask for a system cursor.
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised, its size is in `cbSize`, and the
    // class name outlives the registration.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(format!(
            "RegisterClassEx failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let title = wide("lanplay input latency target");

    // Borderless and exactly the size of the output: this window exists to be
    // captured, and a caption bar or a border would be pixels the capture side
    // has to be told to ignore.
    //
    // SAFETY: the class was just registered under this instance and both wide
    // strings are NUL-terminated and outlive the call.
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP | WS_VISIBLE,
            display.left,
            display.top,
            display.width as i32,
            display.height as i32,
            None,
            None,
            Some(instance),
            None,
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(error) => {
            // SAFETY: undoing the registration a few lines above, with the
            // name it was made under.
            unsafe {
                let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(instance));
            }
            return Err(hresult("CreateWindowEx", &error));
        }
    };
    // Declared before anything that depends on the window so that it is
    // dropped after all of them.
    let _guard = WindowGuard {
        hwnd,
        instance,
        class: class_name,
    };

    // SAFETY: `hwnd` is the window just created and every argument is a
    // constant the API defines.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            display.left,
            display.top,
            display.width as i32,
            display.height as i32,
            SWP_SHOWWINDOW,
        );
    }
    // SAFETY: no arguments.
    let foreground_at_start = unsafe { GetForegroundWindow() } == hwnd;

    // SAFETY: the adapter is the one the device was built on, and `GetParent`
    // only queries an interface off it.
    let factory: IDXGIFactory2 = unsafe { device.adapter().GetParent() }
        .map_err(|error| hresult("IDXGIAdapter1::GetParent(IDXGIFactory2)", &error))?;
    let tearing = tearing_allowed(&factory);

    // Two buffers and flip-discard, which is the only swap effect that reaches
    // the display without going through the desktop compositor's redirection
    // surface. The tearing flag is what lets a present with a sync interval of
    // zero actually not wait.
    let swap_desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: display.width,
        Height: display.height,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: if tearing {
            DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING.0 as u32
        } else {
            0
        },
    };
    // SAFETY: the device and the window are both alive, the description is
    // fully initialised, and no fullscreen description or output restriction
    // is being asked for.
    let swap_chain: IDXGISwapChain1 = unsafe {
        factory.CreateSwapChainForHwnd(
            device.device(),
            hwnd,
            &swap_desc,
            None,
            None::<&IDXGIOutput>,
        )
    }
    .map_err(|error| hresult("IDXGIFactory2::CreateSwapChainForHwnd", &error))?;

    // SAFETY: the window belongs to this process and the flag is a constant.
    unsafe {
        // Alt+Enter would drop the target into exclusive fullscreen, which is
        // not the thing being measured and would change what the capture side
        // sees halfway through a run.
        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);
    }

    // One frame of queueing rather than the default three. A present that has
    // to wait for a buffer waits inside the interval being measured, and three
    // frames of slack is three frames of somebody else's rendering charged to
    // this one.
    if let Ok(dxgi_device) = device.device().cast::<IDXGIDevice1>() {
        // SAFETY: the device is alive and one is a legal latency.
        unsafe {
            let _ = dxgi_device.SetMaximumFrameLatency(1);
        }
    }

    // SAFETY: buffer zero of a flip-model swap chain is the back buffer, and
    // in D3D11 the same texture is returned for it every time, so one render
    // target view built here stays valid for the whole run.
    let back_buffer: ID3D11Texture2D = unsafe { swap_chain.GetBuffer(0) }
        .map_err(|error| hresult("IDXGISwapChain1::GetBuffer", &error))?;
    let mut view: Option<ID3D11RenderTargetView> = None;
    // SAFETY: the back buffer is a render-target-capable texture and `view` is
    // a valid out-parameter.
    unsafe {
        device
            .device()
            .CreateRenderTargetView(&back_buffer, None, Some(&mut view))
    }
    .map_err(|error| hresult("ID3D11Device::CreateRenderTargetView", &error))?;
    let view = view.ok_or("CreateRenderTargetView returned no view")?;
    // Built once. Handing `OMSetRenderTargets` a freshly built array every
    // iteration would be a pair of atomic refcount operations per present for
    // nothing.
    let targets = [Some(view.clone())];

    let devices = [
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GENERIC_MOUSE,
            // An input sink, so raw input arrives whether or not this window
            // holds the foreground. Without it a run that failed to take focus
            // would report nothing on either path and look like a dead input
            // chain rather than a focus problem.
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
        RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_GENERIC,
            usUsage: HID_USAGE_GENERIC_KEYBOARD,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        },
    ];
    // SAFETY: both entries name a valid usage page and usage and target the
    // window just created, and the element size is passed alongside.
    unsafe { RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32) }
        .map_err(|error| hresult("RegisterRawInputDevices", &error))?;

    let mut recorder = Recorder {
        flash: Flash::new(cli.flash_presents),
        observed: Observed::new(display, cli.flash_presents),
        pending: None,
    };
    recorder.observed.tearing = tearing;

    let present_flags = if tearing {
        DXGI_PRESENT_ALLOW_TEARING
    } else {
        DXGI_PRESENT(0)
    };
    let started = Timestamp::now();
    let deadline = started.add(Nanos::from_millis_f64(cli.seconds * 1_000.0));
    let mut quit = false;
    let mut msg = MSG::default();

    while !quit && Timestamp::now() < deadline {
        let mut armed = false;
        // Pumping stops the moment an event arms the flash. The present that
        // answers that event is the thing being timed, and draining the rest
        // of the queue first would put somebody else's messages inside the
        // interval.
        while !armed {
            // SAFETY: `msg` is a valid out-parameter and a null window filter
            // asks for every message on this thread.
            if !unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                break;
            }
            if msg.message == WM_QUIT {
                quit = true;
                break;
            }
            // No `TranslateMessage`: nothing here reads characters, and the
            // `WM_CHAR` it would post is one more message to dispatch for a
            // measurement that does not want it.
            //
            // SAFETY: `msg` was filled by `PeekMessageW` on this thread.
            unsafe { DispatchMessageW(&msg) };
            armed = recorder.drain();
        }

        let colour = recorder.flash.colour();
        // SAFETY: the view and the context both belong to the device opened
        // above and outlive this call; a flip-model present unbinds the back
        // buffer, so the target is rebound before every clear.
        unsafe {
            device
                .context()
                .OMSetRenderTargets(Some(&targets), None::<&ID3D11DepthStencilView>);
            device
                .context()
                .ClearRenderTargetView(&view, &colour.rgba());
        }

        // A sync interval of zero, deliberately. Waiting for vertical blank
        // would fold up to a whole refresh period of doing nothing into every
        // figure, and that period is a property of the panel rather than of
        // the software this is measuring. It makes the result a lower bound:
        // a vsynced game does all of this and then waits.
        //
        // SAFETY: the swap chain is alive and both arguments are values the
        // API defines.
        let result = unsafe { swap_chain.Present(0, present_flags) };
        let returned = Timestamp::now();
        recorder.observed.presents += 1;

        if result.is_err() {
            recorder.observed.present_failures += 1;
            recorder
                .observed
                .first_present_failure
                .get_or_insert(result.0);
            // The flash is left armed on purpose. Nothing was shown, so there
            // is no instant to measure against and the next present is still
            // the one that will first carry white.
            continue;
        }
        if result == OCCLUDED {
            recorder.observed.occluded_presents += 1;
        }
        if !recorder.presented(returned) {
            continue;
        }

        // Sampled here, once per timed event and after the interval has closed,
        // rather than at the two ends of the run. A console process launched
        // between them takes the foreground and gives it back when it exits, so
        // both end samples read true while every injected keystroke landed
        // somewhere else. That is not a property of Windows and it is the
        // difference between "SendInput does not reach the window message queue"
        // and "this window was not the one being typed into" - which is the
        // question the two paths exist to answer.
        //
        // SAFETY: no arguments.
        if unsafe { GetForegroundWindow() } != hwnd {
            recorder.observed.timed_while_background += 1;
        }
    }

    // SAFETY: no arguments.
    let foreground_at_end = unsafe { GetForegroundWindow() } == hwnd;
    // Sampled at the two ends rather than polled. A `GetForegroundWindow` per
    // present is a syscall inside the loop being measured, and what the report
    // needs to answer is whether the window ever had focus at all.
    recorder.observed.foreground = foreground_at_start || foreground_at_end;
    recorder.observed.elapsed = Timestamp::now().saturating_since(started);
    Ok(recorder.observed)
}

/// Whether this DXGI can present without waiting for vertical blank.
fn tearing_allowed(factory: &IDXGIFactory2) -> bool {
    let Ok(factory) = factory.cast::<IDXGIFactory5>() else {
        return false;
    };
    let mut allowed = BOOL::from(false);
    // SAFETY: the feature being asked about is documented to answer into a
    // `BOOL`, and the size of that out-parameter is passed alongside it.
    let asked = unsafe {
        factory.CheckFeatureSupport(
            DXGI_FEATURE_PRESENT_ALLOW_TEARING,
            (&raw mut allowed).cast(),
            size_of::<BOOL>() as u32,
        )
    };
    asked.is_ok() && allowed.as_bool()
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(core::iter::once(0)).collect()
}

fn hresult(call: &str, error: &windows::core::Error) -> String {
    format!("{call} failed: 0x{:08X}", error.code().0 as u32)
}
