//! Reading relative mouse motion, buttons and the wheel out of AppKit and
//! handing them to a caller.
//!
//! `deltaX` and `deltaY` on an `NSEvent` are the movement the mouse reported,
//! not the movement the cursor was allowed to make, which is the only reason
//! this works at all once the cursor has been detached or pinned at a screen
//! edge. Four event types carry them: a plain move, and a move with the left,
//! right or any other button held, which AppKit calls a drag and reports
//! separately. Leaving the drags out would freeze the remote pointer for as
//! long as the user held a button, which is most of a game.
//!
//! Buttons arrive as their own down and up events, three pairs of them: left,
//! right, and one pair for everything else with `buttonNumber` saying which.
//! That number is already the order the wire format's `Button` uses, so a
//! button is a lookup rather than a translation. They are state, so what is
//! sent has to be a transition and never a level: a down with no up is a
//! button held on the host after the player has let go.
//!
//! The wheel is neither. It is reliable like a button, because a lost detent
//! changes a weapon, and stateless like nothing else here, because there is no
//! such thing as a wheel being held down. See [`crate::wheel`] for the
//! conversion its two reporting forms need.
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
use lanplay_input_protocol::Button;
use lanplay_telemetry::Timestamp;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventType};

use crate::cursor::{AssociateFailed, CursorLink};
use crate::residue::Residue;
use crate::wheel::{Notches, Scrolling};

/// Every event type that carries a relative mouse delta.
pub const MOTION_MASK: NSEventMask = NSEventMask::MouseMoved
    .union(NSEventMask::LeftMouseDragged)
    .union(NSEventMask::RightMouseDragged)
    .union(NSEventMask::OtherMouseDragged);

/// Every event type that is a button changing state. Three pairs rather than
/// five, because AppKit gives the left and right buttons their own events and
/// reports every other one through the same pair.
pub const BUTTON_MASK: NSEventMask = NSEventMask::LeftMouseDown
    .union(NSEventMask::LeftMouseUp)
    .union(NSEventMask::RightMouseDown)
    .union(NSEventMask::RightMouseUp)
    .union(NSEventMask::OtherMouseDown)
    .union(NSEventMask::OtherMouseUp);

/// The wheel and the trackpad, which share one event type and are told apart
/// by whether their deltas are precise.
pub const WHEEL_MASK: NSEventMask = NSEventMask::ScrollWheel;

/// Everything the monitors watch for.
pub const CAPTURE_MASK: NSEventMask = MOTION_MASK.union(BUTTON_MASK).union(WHEEL_MASK);

/// One thing the mouse did, in the shape the wire wants it.
///
/// One callback and one enum rather than three callbacks, because the three
/// share a monitor, a borrow and an ordering: a button down that overtook the
/// motion it was aimed with would be a shot fired at the wrong place.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseEvent {
    /// Whole pixels of relative movement, which may be zero.
    Motion { dx: i32, dy: i32 },
    /// A button changing state, never a button's level.
    Button { button: Button, down: bool },
    /// Whole notches, never zero of them.
    Wheel { dx: i16, dy: i16 },
}

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

/// What the two blocks share: the caller's callback, the fraction of a pixel
/// rounding has not yet spent, and the fraction of a notch beside it.
struct Shared<F> {
    residue: Residue,
    notches: Notches,
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
    /// mouse event with what the mouse did and the moment the event was seen.
    ///
    /// The timestamp is read on this machine's monotonic clock. It is never
    /// comparable with anything the host produces, and exists so the sender can
    /// measure its own cost.
    ///
    /// AppKit only delivers these events while a run loop is turning, so the
    /// caller is responsible for running one.
    pub fn start<F>(callback: F) -> Result<Capture, CaptureError>
    where
        F: FnMut(MouseEvent, Timestamp) + 'static,
    {
        let shared = Rc::new(RefCell::new(Shared {
            residue: Residue::new(),
            notches: Notches::new(),
            callback,
        }));

        let watched = Rc::clone(&shared);
        let global_block = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a live event that outlives this call, and
            // nothing here keeps a reference past the borrow below.
            deliver(&watched, unsafe { event.as_ref() });
        });
        let global =
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(CAPTURE_MASK, &global_block)
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
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(CAPTURE_MASK, &local_block)
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

    /// Lets the cursor follow the mouse again while the monitors stay
    /// installed, for a session that has lost control without ending.
    ///
    /// A cursor left detached is the worst state to hand back to a user: the
    /// mouse is plainly moving and nothing on the screen is, with no visible
    /// cause. So the association is given up the moment this process stops
    /// being the one the input is for, and taken again by [`Capture::detach`]
    /// if it becomes so once more.
    pub fn release_cursor(&mut self) -> Result<(), AssociateFailed> {
        self.link.attach()
    }

    /// Takes the cursor away from the mouse again. Idempotent, so a caller
    /// that cannot tell whether it already has may simply call it.
    pub fn detach_cursor(&mut self) -> Result<(), AssociateFailed> {
        self.link.detach()
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
/// A motion delta that rounds to nothing still reaches the callback. Filtering
/// it here would be a small piece of coalescing, and the whole point of this
/// path is to be the unfiltered baseline a coalescing one is later measured
/// against. A scroll that has not yet earned a notch is the opposite case and
/// is dropped: a wheel message is reliable, so an empty one would sit on the
/// retransmission ladder describing nothing.
#[inline]
fn deliver<F: FnMut(MouseEvent, Timestamp)>(shared: &Rc<RefCell<Shared<F>>>, event: &NSEvent) {
    let at = Timestamp::now();
    let kind = event.r#type();

    // Read before the borrow, so nothing that can re-enter happens while it is
    // held.
    let scrolling = if event_is(kind, WHEEL_MASK) && event.hasPreciseScrollingDeltas() {
        Scrolling::Precise
    } else {
        Scrolling::Discrete
    };

    // A callback that itself causes a mouse event would re-enter here. Dropping
    // that event is the only safe answer, and it is preferable to the panic a
    // blind borrow would produce inside an AppKit callback.
    let Ok(mut shared) = shared.try_borrow_mut() else {
        return;
    };
    let shared = &mut *shared;

    if event_is(kind, WHEEL_MASK) {
        let (dx, dy) =
            shared
                .notches
                .spend(event.scrollingDeltaX(), event.scrollingDeltaY(), scrolling);
        if (dx, dy) != (0, 0) {
            (shared.callback)(MouseEvent::Wheel { dx, dy }, at);
        }
        return;
    }

    if event_is(kind, BUTTON_MASK) {
        // A mouse with more buttons than the wire format names is not an error
        // and not forwarded either: folding a sixth button onto a fifth would
        // press one the player never touched.
        let index = event.buttonNumber();
        let Ok(index) = u8::try_from(index) else {
            return;
        };
        let Some(button) = Button::from_index(index) else {
            return;
        };
        let down = matches!(
            kind,
            NSEventType::LeftMouseDown | NSEventType::RightMouseDown | NSEventType::OtherMouseDown
        );
        (shared.callback)(MouseEvent::Button { button, down }, at);
        return;
    }

    let (dx, dy) = shared.residue.spend(event.deltaX(), event.deltaY());
    (shared.callback)(MouseEvent::Motion { dx, dy }, at);
}

/// Whether an event type is one of the types a mask covers. AppKit numbers the
/// types and bits the masks, so the two are related by a shift rather than by
/// equality.
#[inline]
fn event_is(kind: NSEventType, mask: NSEventMask) -> bool {
    mask.contains(NSEventMask(1 << kind.0))
}

#[cfg(test)]
mod tests {
    use super::{
        BUTTON_MASK, CAPTURE_MASK, Capture, CaptureError, MOTION_MASK, WHEEL_MASK, event_is,
    };
    use objc2_app_kit::{NSEventMask, NSEventType};

    /// The safety invariant end to end: whatever a capture detached, releasing
    /// it attaches again.
    #[test]
    fn a_released_capture_reattaches_the_cursor() {
        let mut capture = match Capture::start(|_, _| {}) {
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

    /// Losing focus gives the cursor back without giving up the monitors, and
    /// regaining it takes the cursor again, because a session that is still
    /// running must not leave a user with a mouse that appears broken.
    #[test]
    fn the_cursor_can_be_handed_back_and_taken_again_while_capture_continues() {
        let mut capture = match Capture::start(|_, _| {}) {
            Ok(capture) => capture,
            Err(CaptureError::MonitorRefused) => {
                panic!("AppKit installed no mouse monitor; grant input monitoring to run this")
            }
            Err(other) => panic!("{other}"),
        };

        capture.release_cursor().expect("the cursor goes back");
        assert!(!capture.cursor_detached());
        assert!(capture.is_capturing());

        capture.detach_cursor().expect("and can be taken again");
        assert!(capture.cursor_detached());
        assert!(capture.is_capturing());
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

    /// Five buttons on the wire, three pairs of events on the way in: the
    /// other-mouse pair is what carries the middle button and both side
    /// buttons, so leaving it out would lose three of the five.
    #[test]
    fn the_button_mask_covers_all_three_pairs_and_the_wheel_mask_only_the_wheel() {
        for wanted in [
            NSEventMask::LeftMouseDown,
            NSEventMask::LeftMouseUp,
            NSEventMask::RightMouseDown,
            NSEventMask::RightMouseUp,
            NSEventMask::OtherMouseDown,
            NSEventMask::OtherMouseUp,
        ] {
            assert!(BUTTON_MASK.contains(wanted));
        }
        assert!(!BUTTON_MASK.contains(NSEventMask::ScrollWheel));
        assert!(!BUTTON_MASK.contains(NSEventMask::LeftMouseDragged));
        assert!(WHEEL_MASK.contains(NSEventMask::ScrollWheel));
        assert!(!WHEEL_MASK.contains(NSEventMask::LeftMouseDown));
    }

    /// The event path routes on this, so a type that landed in the wrong arm
    /// would send a scroll as a movement or a click as nothing at all.
    #[test]
    fn an_event_type_is_matched_against_the_mask_it_belongs_to() {
        assert!(event_is(NSEventType::ScrollWheel, WHEEL_MASK));
        assert!(!event_is(NSEventType::ScrollWheel, BUTTON_MASK));
        assert!(!event_is(NSEventType::ScrollWheel, MOTION_MASK));

        assert!(event_is(NSEventType::OtherMouseUp, BUTTON_MASK));
        assert!(!event_is(NSEventType::OtherMouseUp, MOTION_MASK));

        assert!(event_is(NSEventType::LeftMouseDragged, MOTION_MASK));
        assert!(!event_is(NSEventType::LeftMouseDragged, BUTTON_MASK));

        // A key is not a mouse event at all, and the monitors must not be
        // asked for one.
        assert!(!event_is(NSEventType::KeyDown, CAPTURE_MASK));
    }
}
