use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSScreen, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{MTLDevice, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;

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
