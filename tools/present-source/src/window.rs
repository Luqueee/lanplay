//! The window the producer presents into.
//!
//! Deliberately not resizable. A resize would invalidate the swap chain and
//! the render target view, and handling that well means a code path that
//! changes the frame size in the middle of a measurement. The operator asks
//! for a size on the command line and gets exactly that size, or a borderless
//! window covering one monitor.

#![cfg(windows)]

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CS_HREDRAW, CS_OWNDC, CS_VREDRAW, CreateWindowExW, DefWindowProcW,
    DestroyWindow, DispatchMessageW, IDC_ARROW, LoadCursorW, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassExW, SW_SHOW, SetForegroundWindow, ShowWindow, TranslateMessage, WM_CLOSE,
    WM_DESTROY, WM_KEYDOWN, WNDCLASSEXW, WS_CAPTION, WS_EX_APPWINDOW, WS_MINIMIZEBOX,
    WS_OVERLAPPED, WS_POPUP, WS_SYSMENU,
};
use windows::core::{PCWSTR, w};

use crate::gpu::Monitor;
use crate::{Error, api};

const CLASS_NAME: PCWSTR = w!("LanplayPresentSource");

/// Escape, spelled out rather than pulled from `Win32_UI_Input_KeyboardAndMouse`
/// for one constant. A borderless full-screen window has no close button, so
/// it needs a key that ends the run.
const VK_ESCAPE: usize = 0x1B;

/// Set by the window procedure, read by the present loop.
///
/// A process-wide flag rather than per-window state because this tool creates
/// exactly one window and lives only as long as it; threading the state
/// through `GWLP_USERDATA` would buy generality nothing here needs.
static CLOSE_REQUESTED: AtomicBool = AtomicBool::new(false);

static REGISTER_CLASS: Once = Once::new();

pub struct Window {
    hwnd: HWND,
    width: u32,
    height: u32,
}

impl Window {
    /// Opens the window on `monitor`, either at `width` x `height` or covering
    /// the whole monitor.
    ///
    /// The returned size is the client area, which is what the swap chain must
    /// match; in full-screen mode it is the monitor's own resolution and the
    /// requested width and height are ignored.
    pub fn open(
        monitor: &Monitor,
        width: u32,
        height: u32,
        fullscreen: bool,
    ) -> Result<Window, Error> {
        CLOSE_REQUESTED.store(false, Ordering::Relaxed);

        // Without this, Windows lies about the monitor's size on any scaled
        // display and hands back a stretched, blurred surface: the capture
        // would then be measured against a resample nobody asked for.
        // Failure only means the awareness was already set, which is fine.
        //
        // SAFETY: the context value is one of the documented constants.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

        // SAFETY: the class and window descriptions are fully initialised and
        // every returned handle is checked before use.
        unsafe {
            let instance = HINSTANCE(
                GetModuleHandleW(PCWSTR::null())
                    .map_err(api("GetModuleHandleW"))?
                    .0,
            );

            let mut registration = Ok(());
            REGISTER_CLASS.call_once(|| {
                let cursor = LoadCursorW(None, IDC_ARROW).unwrap_or_default();
                let class = WNDCLASSEXW {
                    cbSize: size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW | CS_OWNDC,
                    lpfnWndProc: Some(window_proc),
                    hInstance: instance,
                    hCursor: cursor,
                    lpszClassName: CLASS_NAME,
                    ..Default::default()
                };
                if RegisterClassExW(&class) == 0 {
                    registration = Err(Error::Api {
                        call: "RegisterClassExW",
                        hresult: windows::core::Error::from_thread().code().0,
                    });
                }
            });
            registration?;

            let (style, x, y, outer_width, outer_height, client_width, client_height) =
                if fullscreen {
                    (
                        WS_POPUP,
                        monitor.left,
                        monitor.top,
                        monitor.width as i32,
                        monitor.height as i32,
                        monitor.width,
                        monitor.height,
                    )
                } else {
                    // No WS_THICKFRAME and no WS_MAXIMIZEBOX: the size is
                    // fixed for the life of the swap chain.
                    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
                    let mut rect = RECT {
                        left: 0,
                        top: 0,
                        right: width as i32,
                        bottom: height as i32,
                    };
                    AdjustWindowRectEx(&mut rect, style, false, WS_EX_APPWINDOW)
                        .map_err(api("AdjustWindowRectEx"))?;
                    (
                        style,
                        monitor.left + 64,
                        monitor.top + 64,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        width,
                        height,
                    )
                };

            let hwnd = CreateWindowExW(
                WS_EX_APPWINDOW,
                CLASS_NAME,
                w!("lanplay present-source"),
                style,
                x,
                y,
                outer_width,
                outer_height,
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(api("CreateWindowExW"))?;

            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);

            Ok(Window {
                hwnd,
                width: client_width,
                height: client_height,
            })
        }
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Drains the queue. Returns false once the window has been asked to
    /// close, which ends the run.
    pub fn pump(&self) -> bool {
        let mut message = MSG::default();
        // SAFETY: `message` is a valid, fully initialised MSG for the length
        // of each call, and the window belongs to this thread.
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        !CLOSE_REQUESTED.load(Ordering::Relaxed)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateWindowExW` and is destroyed
        // exactly once, here.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// WM_CLOSE only records the request; the window is destroyed by [`Window`]'s
/// destructor once the present loop has stopped drawing into it. Letting the
/// default handler destroy it mid-loop would leave the swap chain bound to a
/// dead HWND.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE | WM_DESTROY => {
            CLOSE_REQUESTED.store(true, Ordering::Relaxed);
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == VK_ESCAPE => {
            CLOSE_REQUESTED.store(true, Ordering::Relaxed);
            LRESULT(0)
        }
        // SAFETY: forwarding the message the system just delivered, unchanged.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
