//! Noticing that this process has stopped being the one the input is for.
//!
//! The safety invariant the whole input path is built around is that any loss
//! of control of the session converges the host to nothing held. Focus is the
//! loss that happens most and is the easiest to miss: the player switches away
//! mid-strafe, this process stops receiving events, and the `A` it last sent a
//! press for is held down on the host until something says otherwise. Nothing
//! else will, because the release it is waiting for was delivered to whatever
//! the player switched to.
//!
//! AppKit reports the loss twice. Deactivating the application posts one
//! notification and the key window resigning posts another, and an ordinary
//! switch away from a windowed application posts both. Two notifications are
//! one loss of control, so what is kept here is the edge rather than the
//! notification: the first one owes a `ReleaseAll` and the second owes nothing.
//! Sending two would not corrupt anything, since a host that receives a release
//! twice ends in the same empty state either way, but it would put a second
//! reliable event on the retransmission ladder for no reason and it would make
//! the count of releases an operator reads meaningless.
//!
//! The edge is a two-state machine and is kept apart from AppKit so that it can
//! be tested as one. [`FocusWatcher`] is the part that talks to the
//! notification centre, and it does nothing but feed this.

/// Whether this process is the one the input is for, and whether the last
/// notification was the transition.
///
/// Starts focused, which is the assumption a session begins under: a client
/// that opened a session was in the foreground when it did.
#[derive(Clone, Copy, Debug)]
pub struct FocusState {
    focused: bool,
    losses: u64,
}

impl Default for FocusState {
    fn default() -> FocusState {
        FocusState::new()
    }
}

impl FocusState {
    pub const fn new() -> FocusState {
        FocusState {
            focused: true,
            losses: 0,
        }
    }

    /// Reports a resign notification, from the application or from the window.
    /// True only for the one that was the transition out of focus, which is the
    /// one that owes a `ReleaseAll`.
    pub fn resigned(&mut self) -> bool {
        if !self.focused {
            return false;
        }
        self.focused = false;
        self.losses += 1;
        true
    }

    /// Reports a become-active or become-key notification. True only for the
    /// transition back, so a caller can take the cursor again exactly once.
    pub fn regained(&mut self) -> bool {
        if self.focused {
            return false;
        }
        self.focused = true;
        true
    }

    pub const fn is_focused(&self) -> bool {
        self.focused
    }

    /// How many times control has been lost, which is how many releases focus
    /// alone owes.
    pub const fn losses(&self) -> u64 {
        self.losses
    }
}

#[cfg(target_os = "macos")]
pub use watcher::FocusWatcher;

#[cfg(target_os = "macos")]
mod watcher {
    use core::cell::Cell;
    use core::ptr::NonNull;
    use std::rc::Rc;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{
        NSApplicationDidBecomeActiveNotification, NSApplicationDidResignActiveNotification,
        NSWindowDidBecomeKeyNotification, NSWindowDidResignKeyNotification,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter, NSNotificationName};

    use super::FocusState;

    /// What the observer blocks share with the handle.
    struct Shared {
        state: Cell<FocusState>,
        /// A loss the caller has not yet acted on. A flag rather than a
        /// callback, because the caller's response is to send a datagram and
        /// releasing the cursor, and doing either inside AppKit's notification
        /// dispatch would run a socket write underneath a window server call.
        pending_loss: Cell<bool>,
        pending_regain: Cell<bool>,
    }

    /// Watches the four notifications that say whether this process has the
    /// input. Dropping it removes the observers.
    pub struct FocusWatcher {
        observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
        shared: Rc<Shared>,
    }

    impl FocusWatcher {
        /// Starts watching. Never fails: an application that posts none of
        /// these simply never loses focus, and the other causes in the
        /// invariant still cover it.
        pub fn start() -> FocusWatcher {
            let shared = Rc::new(Shared {
                state: Cell::new(FocusState::new()),
                pending_loss: Cell::new(false),
                pending_regain: Cell::new(false),
            });
            let centre = NSNotificationCenter::defaultCenter();
            let mut observers = Vec::with_capacity(4);

            // SAFETY: these four statics are the notification names AppKit
            // documents, read only for the duration of the registration.
            let resigns = unsafe {
                [
                    NSApplicationDidResignActiveNotification,
                    NSWindowDidResignKeyNotification,
                ]
            };
            for name in resigns {
                let watched = Rc::clone(&shared);
                observers.push(observe(&centre, name, move || {
                    let mut state = watched.state.get();
                    if state.resigned() {
                        watched.pending_loss.set(true);
                    }
                    watched.state.set(state);
                }));
            }

            // SAFETY: as above, for the two that say control has come back.
            let regains = unsafe {
                [
                    NSApplicationDidBecomeActiveNotification,
                    NSWindowDidBecomeKeyNotification,
                ]
            };
            for name in regains {
                let watched = Rc::clone(&shared);
                observers.push(observe(&centre, name, move || {
                    let mut state = watched.state.get();
                    if state.regained() {
                        watched.pending_regain.set(true);
                    }
                    watched.state.set(state);
                }));
            }

            FocusWatcher { observers, shared }
        }

        /// Whether control has been lost since this was last asked, clearing
        /// the answer. Asked once per turn of the caller's loop, so a loss is
        /// acted on within a turn of the notification and exactly once.
        pub fn take_loss(&self) -> bool {
            self.shared.pending_loss.replace(false)
        }

        /// Whether control has come back since this was last asked.
        pub fn take_regain(&self) -> bool {
            self.shared.pending_regain.replace(false)
        }

        /// How many times control has been lost over the whole run.
        pub fn losses(&self) -> u64 {
            self.shared.state.get().losses()
        }

        pub fn is_focused(&self) -> bool {
            self.shared.state.get().is_focused()
        }

        /// Stops watching. Idempotent, so `Drop` can call it after the caller
        /// already has.
        pub fn stop(&mut self) {
            let centre = NSNotificationCenter::defaultCenter();
            for observer in self.observers.drain(..) {
                let observer: &AnyObject = observer.as_ref();
                // SAFETY: the object came from
                // `addObserverForName:object:queue:usingBlock:` on this same
                // centre, which is what `removeObserver:` expects, and draining
                // guarantees it is removed exactly once.
                unsafe { centre.removeObserver(observer) };
            }
        }
    }

    impl Drop for FocusWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// Registers one block against one notification name.
    ///
    /// A `None` queue is deliberate: the block then runs on the thread that
    /// posted the notification, which for these four is the main thread, and
    /// that is the thread the shared state and the caller's loop are on.
    fn observe(
        centre: &NSNotificationCenter,
        name: &NSNotificationName,
        on_post: impl Fn() + 'static,
    ) -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
        let block = RcBlock::new(move |_: NonNull<NSNotification>| on_post());
        // SAFETY: the name is one AppKit posts, no object filter is wanted, and
        // a `None` queue keeps the block on the posting thread, which is the
        // only thread the `Rc` it captures is ever touched from.
        unsafe { centre.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) }
    }
}

#[cfg(test)]
mod tests {
    use super::FocusState;
    use crate::Reliable;
    use lanplay_input_protocol::{Button, KeyBitset, Message};
    use lanplay_telemetry::Timestamp;

    use crate::ScanCode;

    const A: ScanCode = ScanCode {
        code: 0x1E,
        extended: false,
    };

    fn at(millis: u64) -> Timestamp {
        Timestamp::from_nanos(millis * 1_000_000)
    }

    /// The whole point of the edge. AppKit posts both notifications for one
    /// switch away, and one switch away is one loss of control.
    #[test]
    fn both_notifications_for_one_switch_away_owe_one_release() {
        let mut focus = FocusState::new();
        let mut reliable = Reliable::new(at(0));
        reliable.key(A, true, at(0));
        reliable.button(Button::Left, true, at(0));

        // The application resigning and the key window resigning, in the order
        // AppKit posts them.
        let mut releases = Vec::new();
        for _ in 0..2 {
            if focus.resigned() {
                releases.push(reliable.release_all(at(1)));
            }
        }

        assert_eq!(releases.len(), 1);
        assert!(matches!(releases[0], Message::ReleaseAll { .. }));
        assert_eq!(focus.losses(), 1);
        assert!(!focus.is_focused());

        // The client's own view is empty, so the snapshot that follows tells
        // the host the same thing the release did rather than contradicting
        // it a moment later.
        assert!(reliable.keys().is_empty());
        assert_eq!(reliable.buttons(), 0);
        let Some(Message::Snapshot { keys, buttons, .. }) = reliable.snapshot_due(at(51)) else {
            panic!("a snapshot is due fifty milliseconds after a release nobody acknowledged");
        };
        assert_eq!(keys, KeyBitset::EMPTY);
        assert_eq!(buttons, 0);
    }

    /// Coming back and losing it again is a second loss, and owes a second
    /// release: the edge must not latch.
    #[test]
    fn focus_regained_and_lost_again_owes_another_release() {
        let mut focus = FocusState::new();
        assert!(focus.resigned());
        assert!(!focus.resigned());

        assert!(focus.regained());
        // The second become-active of the pair is not a transition either.
        assert!(!focus.regained());
        assert!(focus.is_focused());

        assert!(focus.resigned());
        assert_eq!(focus.losses(), 2);
    }

    /// A release for a session that had nothing held still goes out, because
    /// the client's belief about what is held is not evidence about the host's:
    /// the release it is repairing may be the one that was lost.
    #[test]
    fn a_loss_with_nothing_held_still_sends_a_release() {
        let mut focus = FocusState::new();
        let mut reliable = Reliable::new(at(0));
        assert!(focus.resigned());
        let message = reliable.release_all(at(0));
        assert!(matches!(message, Message::ReleaseAll { .. }));
        assert_eq!(reliable.unacked(), 1);
    }

    /// The wiring itself, against the real notification centre, because the
    /// state machine above can be perfect while the observer is registered
    /// against a name AppKit never posts. Both resign notifications are posted
    /// here, exactly as a switch away posts them, and one loss is what the
    /// caller must see.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_watcher_turns_both_posted_resign_notifications_into_one_loss() {
        use objc2_app_kit::{
            NSApplicationDidBecomeActiveNotification, NSApplicationDidResignActiveNotification,
            NSWindowDidResignKeyNotification,
        };
        use objc2_foundation::NSNotificationCenter;

        let watcher = super::FocusWatcher::start();
        assert!(watcher.is_focused());
        assert!(!watcher.take_loss());

        let centre = NSNotificationCenter::defaultCenter();
        // SAFETY: posting a notification AppKit itself posts, with no object
        // and on this thread, which is the thread the observer blocks read
        // their state from.
        unsafe {
            centre.postNotificationName_object(NSApplicationDidResignActiveNotification, None);
            centre.postNotificationName_object(NSWindowDidResignKeyNotification, None);
        }

        assert_eq!(watcher.losses(), 1);
        assert!(!watcher.is_focused());
        // Taken once, and gone once taken, so a caller polling every turn of
        // its loop sends one release and not one per turn.
        assert!(watcher.take_loss());
        assert!(!watcher.take_loss());

        // SAFETY: as above, for the notification that says control came back.
        unsafe {
            centre.postNotificationName_object(NSApplicationDidBecomeActiveNotification, None);
        }
        assert!(watcher.take_regain());
        assert!(watcher.is_focused());
    }
}
