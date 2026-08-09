use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSFloatingWindowLevel, NSScreen, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{MTLDevice, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;

use crate::environment::{Environment, WindowState};
use crate::error::RendererError;

/// The window, its Metal layer and what the display underneath can do.
pub(crate) struct Surface {
    pub(crate) window: Retained<NSWindow>,
    pub(crate) layer: Retained<CAMetalLayer>,
    pub(crate) display_name: String,
    /// What the chosen display advertises, before anything is measured.
    pub(crate) nominal_hz: f64,
    pub(crate) drawable_width: u32,
    pub(crate) drawable_height: u32,
}

impl Surface {
    pub(crate) fn open(
        mtm: MainThreadMarker,
        device: &ProtocolObject<dyn MTLDevice>,
        width: u32,
        height: u32,
        title: &str,
    ) -> Result<Surface, RendererError> {
        let screen = fastest_screen(mtm).ok_or(RendererError::NoScreen)?;
        let content = fit_within(screen.visibleFrame().size, width as f64, height as f64);
        let frame = centred(screen.visibleFrame(), content);

        // SAFETY: the window is created outside a window controller, so
        // release-when-closed is turned off immediately below and this handle
        // stays the sole owner.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer_screen(
                NSWindow::alloc(mtm),
                frame,
                // No `Resizable`: a resize would change the drawable size
                // halfway through a measurement run.
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
                Some(&screen),
            )
        };
        // SAFETY: matches the ownership assumption above.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));

        // A covered window has its display link suspended: macOS sees nothing
        // worth drawing and stops calling back. Measured here, that turned a
        // 120 Hz link into 75 callbacks a second and made a healthy Wi-Fi look
        // like it was stalling for a second at a time. Floating above other
        // windows, and following whichever Space is in front, is what keeps a
        // presentation measurement about presentation.
        window.setLevel(NSFloatingWindowLevel);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );

        // The screen's factor, not the window's: `NSWindow` only reports its
        // own once it has been ordered onto a screen, and before that it
        // answers for whichever display happens to be main.
        let scale = screen.backingScaleFactor();
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // Nothing ever reads the drawable back, so let the compositor keep it
        // in the cheapest form it can.
        layer.setFramebufferOnly(true);
        layer.setMaximumDrawableCount(3);
        layer.setContentsScale(scale);
        layer.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), content));
        // The drawable is sized in physical pixels; the layer's frame is in
        // points, and on a Retina display the two differ by the backing scale.
        let drawable_width = (content.width * scale).round().max(1.0);
        let drawable_height = (content.height * scale).round().max(1.0);
        layer.setDrawableSize(NSSize::new(drawable_width, drawable_height));

        let view = window
            .contentView()
            .expect("a titled NSWindow always has a content view");
        // Order matters: assigning the layer before asking for one makes this
        // a layer-hosting view, which is what stops AppKit drawing over us.
        view.setLayer(Some(&layer));
        view.setWantsLayer(true);

        Ok(Surface {
            display_name: screen.localizedName().to_string(),
            nominal_hz: screen.maximumFramesPerSecond() as f64,
            window,
            layer,
            drawable_width: drawable_width as u32,
            drawable_height: drawable_height as u32,
        })
    }

    /// The window's situation right now, for the preflight and for the stats.
    ///
    /// The rate is re-read from whichever screen the window is actually on
    /// rather than reused from `open`: a window can be dragged, or the display
    /// it opened on can change mode, and a run that needs 120 Hz must be told
    /// so before it starts rather than be judged against a stale figure.
    pub(crate) fn environment(&self, display_hz: f64) -> Environment {
        let state = WindowState::read(&self.window);
        Environment {
            display_name: state.screen.as_ref().map_or_else(
                || self.display_name.clone(),
                |screen| screen.localizedName().to_string(),
            ),
            display_hz,
            maximum_frames_per_second: state.screen.as_ref().map_or(self.nominal_hz, |screen| {
                screen.maximumFramesPerSecond() as f64
            }),
            on_active_space: state.on_active_space,
            occluded: state.occluded,
            miniaturised: state.miniaturised,
            drawable: (self.drawable_width, self.drawable_height),
        }
    }
}

/// The point of this renderer is to observe presentation cadence, so it opens
/// on the display that can actually show 120 Hz rather than on whichever
/// monitor happens to be primary.
fn fastest_screen(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    NSScreen::screens(mtm)
        .iter()
        .max_by_key(|screen| screen.maximumFramesPerSecond())
}

/// Largest box with the source aspect ratio that leaves a margin on `limit`.
fn fit_within(limit: NSSize, width: f64, height: f64) -> NSSize {
    let budget_w = limit.width * 0.9;
    let budget_h = limit.height * 0.9;
    let scale = (budget_w / width).min(budget_h / height).min(1.0);
    NSSize::new((width * scale).round(), (height * scale).round())
}

fn centred(within: NSRect, size: NSSize) -> NSRect {
    NSRect::new(
        NSPoint::new(
            within.origin.x + (within.size.width - size.width) / 2.0,
            within.origin.y + (within.size.height - size.height) / 2.0,
        ),
        size,
    )
}
