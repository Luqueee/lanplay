//! Reading relative mouse motion out of AppKit and handing it to a caller.
//!
//! `deltaX` and `deltaY` on an `NSEvent` are the movement the mouse reported,
//! not the movement the cursor was allowed to make, which is the only reason
//! this works at all once the cursor has been detached or pinned at a screen
//! edge. Four event types carry them: a plain move, and a move with the left,
//! right or any other button held, which AppKit calls a drag and reports
//! separately. Leaving the drags out would freeze the remote pointer for as
//! long as the user held a button, which is most of a game.
//!
//! Two monitors, not one. A global monitor never fires for events delivered to
//! this process, and a local monitor only ever sees this process's events, so
//! installing both covers the cursor being over any application including this
//! one and still reports every event exactly once.
//!
//! Rejected: a channel to a worker thread. The callback runs on the event path
//! precisely so that the send is one event in and one datagram out, with
//! nothing in between that could grow while the network is unhappy. There is no
//! allocation here after `start` returns, and the only synchronisation is the
//! borrow flag that lets two blocks share one callback on one thread.

use core::cell::RefCell;
use core::fmt;
use core::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use lanplay_telemetry::Timestamp;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask};

use crate::cursor::{AssociateFailed, CursorLink};
use crate::residue::Residue;

/// Every event type that carries a relative mouse delta.
pub const MOTION_MASK: NSEventMask = NSEventMask::MouseMoved
    .union(NSEventMask::LeftMouseDragged)
    .union(NSEventMask::RightMouseDragged)
    .union(NSEventMask::OtherMouseDragged);

/// Why capture could not start.
#[derive(Debug)]
pub enum CaptureError {
    /// AppKit returned no monitor. On a machine where the user has refused
    /// input monitoring, or in a process the system does not consider an
    /// application, this is what refusal looks like.
    MonitorRefused,
    /// The cursor could not be detached, so capture was abandoned rather than
    /// run with a cursor that still walks across the desktop.
    Cursor(AssociateFailed),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::MonitorRefused => write!(
                f,
                "AppKit installed no mouse monitor; input monitoring may be denied"
            ),
            CaptureError::Cursor(failure) => write!(f, "{failure}"),
        }
    }
}

impl core::error::Error for CaptureError {}

impl From<AssociateFailed> for CaptureError {
    fn from(failure: AssociateFailed) -> CaptureError {
        CaptureError::Cursor(failure)
    }
}

/// What the two blocks share: the caller's callback and the fraction of a pixel
/// rounding has not yet spent.
struct Shared<F> {
    residue: Residue,
    callback: F,
}

/// A running capture. Dropping it releases, so a caller that forgets cannot
/// leave the cursor detached.
pub struct Capture {
    /// Retained only so they can be removed; AppKit owns the blocks behind them.
    global: Option<Retained<AnyObject>>,
    local: Option<Retained<AnyObject>>,
    link: CursorLink,
}

impl Capture {
    /// Starts capturing, detaches the cursor, and calls `callback` once per
    /// mouse event with the whole-pixel delta and the moment the event was
    /// seen.
    ///
    /// The timestamp is read on this machine's monotonic clock. It is never
    /// comparable with anything the host produces, and exists so the sender can
    /// measure its own cost.
    ///
    /// AppKit only delivers these events while a run loop is turning, so the
    /// caller is responsible for running one.
    pub fn start<F>(callback: F) -> Result<Capture, CaptureError>
    where
        F: FnMut(i32, i32, Timestamp) + 'static,
    {
        let shared = Rc::new(RefCell::new(Shared {
            residue: Residue::new(),
            callback,
        }));

        let watched = Rc::clone(&shared);
        let global_block = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a live event that outlives this call, and
            // nothing here keeps a reference past the borrow below.
            deliver(&watched, unsafe { event.as_ref() });
        });
        let global =
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(MOTION_MASK, &global_block)
                .ok_or(CaptureError::MonitorRefused)?;

        let watched = Rc::clone(&shared);
        let local_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: as above; the pointer is AppKit's own and is only read.
            deliver(&watched, unsafe { event.as_ref() });
            // Handing the event straight back keeps ordinary delivery intact.
            // Monitoring is not consuming, and swallowing a drag here would
            // break every control in this process's own windows.
            event.as_ptr()
        });
        // SAFETY: the block returns exactly the event pointer it was given,
        // which is the valid non-null return the method documents.
        let local = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(MOTION_MASK, &local_block)
        };

        // Built before the cursor is touched so that a refused detach unwinds
        // through `Drop` and takes the monitors with it.
        let mut capture = Capture {
            global: Some(global),
            local,
            link: CursorLink::new(),
        };
        capture.link.detach()?;
        Ok(capture)
    }

    /// Stops capturing and lets the cursor follow the mouse again. Idempotent,
    /// so `Drop` can call it after the caller already has.
    pub fn release(&mut self) -> Result<(), AssociateFailed> {
        if let Some(monitor) = self.global.take() {
            // SAFETY: the object came from `addGlobalMonitorForEventsMatchingMask:handler:`,
            // which is what `removeMonitor:` expects, and `take` guarantees it
            // is removed exactly once.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        if let Some(monitor) = self.local.take() {
            // SAFETY: as above, for the local monitor.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        self.link.attach()
    }

    /// Whether the cursor is currently held away from the mouse.
    pub const fn cursor_detached(&self) -> bool {
        self.link.is_detached()
    }

    /// Whether the monitors are still installed.
    pub const fn is_capturing(&self) -> bool {
        self.global.is_some()
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// The event path. Reads the clock first, because every cost the caller wants
/// to measure comes after it.
///
/// A delta that rounds to nothing still reaches the callback. Filtering it here
/// would be a small piece of coalescing, and the whole point of this path is to
/// be the unfiltered baseline a coalescing one is later measured against.
#[inline]
fn deliver<F: FnMut(i32, i32, Timestamp)>(shared: &Rc<RefCell<Shared<F>>>, event: &NSEvent) {
    let at = Timestamp::now();
    let dx = event.deltaX();
    let dy = event.deltaY();
    // A callback that itself causes a mouse event would re-enter here. Dropping
    // that event is the only safe answer, and it is preferable to the panic a
    // blind borrow would produce inside an AppKit callback.
    let Ok(mut shared) = shared.try_borrow_mut() else {
        return;
    };
    let shared = &mut *shared;
    let (dx, dy) = shared.residue.spend(dx, dy);
    (shared.callback)(dx, dy, at);
}

#[cfg(test)]
mod tests {
    use super::{Capture, CaptureError, MOTION_MASK};
    use objc2_app_kit::NSEventMask;

    /// The safety invariant end to end: whatever a capture detached, releasing
    /// it attaches again.
    #[test]
    fn a_released_capture_reattaches_the_cursor() {
        let mut capture = match Capture::start(|_, _, _| {}) {
            Ok(capture) => capture,
            // A machine that refuses input monitoring cannot exercise this, and
            // saying so is better than passing on a capture that never started.
            Err(CaptureError::MonitorRefused) => {
                panic!("AppKit installed no mouse monitor; grant input monitoring to run this")
            }
            Err(other) => panic!("{other}"),
        };
        assert!(capture.cursor_detached());
        assert!(capture.is_capturing());

        capture.release().expect("releasing reattaches the cursor");
        assert!(!capture.cursor_detached());
        assert!(!capture.is_capturing());

        // Releasing twice must stay harmless, because `Drop` is about to.
        capture.release().expect("a second release does nothing");
    }

    /// Drags are watched, not just plain moves, because a button held down is
    /// the normal state of a mouse in a game.
    #[test]
    fn the_mask_covers_moves_and_all_three_drags() {
        for wanted in [
            NSEventMask::MouseMoved,
            NSEventMask::LeftMouseDragged,
            NSEventMask::RightMouseDragged,
            NSEventMask::OtherMouseDragged,
        ] {
            assert!(MOTION_MASK.contains(wanted));
        }
        // Nothing else, so a key press or a scroll cannot be mistaken for
        // motion by a monitor that shares this mask.
        assert!(!MOTION_MASK.contains(NSEventMask::KeyDown));
        assert!(!MOTION_MASK.contains(NSEventMask::ScrollWheel));
    }
}
