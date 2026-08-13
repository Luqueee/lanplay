//! Reading physical key presses out of AppKit and handing them to a caller.
//!
//! Three event types, not two. A key down and a key up cover the keys that
//! produce characters, and modifiers produce neither: pressing shift emits an
//! `NSEventTypeFlagsChanged` and nothing else, so a capture listening only for
//! key down and key up would forward `w` and never forward the shift the player
//! was holding while they pressed it.
//!
//! Flags changed is also the one event whose meaning is not written on it. It
//! carries the modifier state as it now stands and the key code that changed,
//! but not whether that key went down or came up, so the direction has to be
//! derived. Reading it off the flag the key owns is the obvious way and it is
//! wrong: the left and right key of a pair share one flag on every macOS
//! version that matters, so releasing the left shift while the right is held
//! reports shift still asserted, and a capture that trusted the flag would send
//! a second press and never a release. The left shift would then be held down on
//! the host forever, which is the exact failure the protocol's snapshots and
//! release-all exist to repair and which this layer should not be producing in
//! the first place.
//!
//! What is correct is to keep the pressed set here and let it decide: a key
//! already believed held can only be coming up, and a key not held is going down
//! if its flag is now asserted. Caps lock needs the second half of that on its
//! own, because its flag reports the lock rather than the key and is therefore
//! clear on the press that turns the lock off.
//!
//! Auto-repeat is dropped. macOS synthesises a stream of key downs while a key
//! is held, and Windows synthesises its own from the single press this sends, so
//! forwarding both gives the player two streams of repeats interleaved. The
//! count is kept so an operator can see the suppression working.
//!
//! Two monitors for the same reason as the mouse: a global monitor never fires
//! for events delivered to this process and a local one only ever sees this
//! process's own, so both together see every key exactly once. The callback runs
//! on the event path, with no queue and no thread between it and the send,
//! because a key press that waits for a timer is a key press the player already
//! believes has happened.

use core::cell::{Cell, RefCell};
use core::fmt;
use core::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use lanplay_input_protocol::EventId;
use lanplay_telemetry::Timestamp;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags, NSEventType};

use crate::scancode::ScanCode;

/// Every event type a key arrives as.
pub const KEY_MASK: NSEventMask = NSEventMask::KeyDown
    .union(NSEventMask::KeyUp)
    .union(NSEventMask::FlagsChanged);

/// macOS virtual key code for caps lock, singled out because its flag reports
/// the lock and not the key.
const CAPS_LOCK: u16 = 0x39;

/// Highest virtual key code the pressed set can hold. Every key an `NSEvent`
/// reports from a keyboard is below this, and a code above it is refused rather
/// than folded into somebody else's bit.
const KEY_CODES: u16 = 128;

/// Why capture could not start.
///
/// One condition, not an enum: unlike the mouse, nothing here detaches a cursor
/// or holds anything the caller must be told about unwinding.
#[derive(Debug)]
pub struct MonitorRefused;

impl fmt::Display for MonitorRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "AppKit installed no keyboard monitor; Input Monitoring may be denied"
        )
    }
}

impl core::error::Error for MonitorRefused {}

/// One captured key, in the shape the wire wants it.
///
/// The field names match `Message::Key` so that a sender is a construction and
/// not a translation, and the id is here rather than left to the caller because
/// the host deduplicates on it: two callers minting ids from two counters would
/// hand the host the same id for different keys and it would inject one of them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub id: EventId,
    pub scan: ScanCode,
    /// True for a press. A release is never dropped anywhere in this path.
    pub down: bool,
    /// Read on this machine's monotonic clock, before anything else, so a
    /// caller measuring its own cost measures all of it.
    pub at: Timestamp,
}

/// What a flags-changed event turned out to mean.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transition {
    Pressed,
    Released,
    /// The event describes no change this capture can act on: a release for a
    /// key it does not believe is held, which is what an event that arrived
    /// before capture started, or after a release sweep, looks like. Sending a
    /// release the host has no press for would be harmless but sending it as a
    /// press would not, so nothing is sent.
    Ignored,
}

/// Which modifier keys this capture believes are physically down.
///
/// One bit per virtual key code rather than one per flag, because that is the
/// distinction AppKit's flags throw away and the whole reason this exists.
#[derive(Clone, Copy, Default, Debug)]
pub struct Modifiers {
    held: u128,
}

impl Modifiers {
    pub const fn new() -> Modifiers {
        Modifiers { held: 0 }
    }

    /// Folds one flags-changed event in and says what it was.
    ///
    /// `asserted` is whether the event's new flags still contain the flag this
    /// key owns. It is deliberately a `bool` rather than the flag set: the
    /// decision is about one key, and passing the whole set would invite a
    /// second reading of it here where the pressed set is the authority.
    pub fn apply(&mut self, virtual_key: u16, asserted: bool) -> Transition {
        if virtual_key >= KEY_CODES {
            return Transition::Ignored;
        }
        let bit = 1u128 << virtual_key;

        if self.held & bit != 0 {
            // Held keys can only be coming up. Trusting `asserted` here is the
            // bug this module is built around: the other half of the pair being
            // down keeps the flag set through this release.
            self.held &= !bit;
            Transition::Released
        } else if asserted || virtual_key == CAPS_LOCK {
            self.held |= bit;
            Transition::Pressed
        } else {
            Transition::Ignored
        }
    }

    /// How many modifier keys are believed down.
    pub const fn count(&self) -> u32 {
        self.held.count_ones()
    }

    /// Whether one key is believed down, for tests and diagnostics.
    pub const fn contains(&self, virtual_key: u16) -> bool {
        virtual_key < KEY_CODES && self.held & (1u128 << virtual_key) != 0
    }
}

/// The modifier flag a key owns, or `None` for a key that owns none.
///
/// A flags-changed event can report a key with no flag of its own, the globe and
/// fn keys being the ones a laptop produces, and those have no PC equivalent to
/// forward anyway.
const fn flag_of(virtual_key: u16) -> Option<NSEventModifierFlags> {
    Some(match virtual_key {
        0x38 | 0x3C => NSEventModifierFlags::Shift,
        0x3B | 0x3E => NSEventModifierFlags::Control,
        0x3A | 0x3D => NSEventModifierFlags::Option,
        0x36 | 0x37 => NSEventModifierFlags::Command,
        CAPS_LOCK => NSEventModifierFlags::CapsLock,
        _ => return None,
    })
}

/// Counters a caller reads after the run.
///
/// `Cell` rather than fields of the shared state, so the handle can read them
/// while the event path owns the borrow, and so reading a count can never be the
/// thing that fails inside an AppKit callback.
#[derive(Default)]
struct Counts {
    captured: Cell<u64>,
    repeats_suppressed: Cell<u64>,
}

/// What the two blocks share.
struct Shared<F> {
    /// The next reliable event id. Owned here because the host deduplicates on
    /// it and there must be exactly one source of it per session.
    next_id: EventId,
    modifiers: Modifiers,
    counts: Rc<Counts>,
    callback: F,
}

/// A running keyboard capture. Dropping it removes the monitors.
pub struct Keyboard {
    /// Retained only so they can be removed; AppKit owns the blocks behind them.
    global: Option<Retained<AnyObject>>,
    local: Option<Retained<AnyObject>>,
    counts: Rc<Counts>,
}

impl Keyboard {
    /// Starts capturing and calls `callback` once per key transition.
    ///
    /// Ids start at zero and increase by one per call, which is what lets the
    /// host tell a retransmission from a second press of the same key.
    ///
    /// AppKit only delivers these events while a run loop is turning, so the
    /// caller is responsible for running one. A global keyboard monitor also
    /// needs Input Monitoring, and a machine that has not granted it delivers
    /// nothing at all rather than failing here, so a caller that sees no keys
    /// should say so rather than report a quiet run.
    pub fn start<F>(callback: F) -> Result<Keyboard, MonitorRefused>
    where
        F: FnMut(KeyEvent) + 'static,
    {
        let counts = Rc::new(Counts::default());
        let shared = Rc::new(RefCell::new(Shared {
            next_id: EventId(0),
            modifiers: Modifiers::new(),
            counts: Rc::clone(&counts),
            callback,
        }));

        let watched = Rc::clone(&shared);
        let global_block = RcBlock::new(move |event: NonNull<NSEvent>| {
            // SAFETY: AppKit passes a live event that outlives this call, and
            // nothing here keeps a reference past the borrow inside `deliver`.
            deliver(&watched, unsafe { event.as_ref() });
        });
        let global =
            NSEvent::addGlobalMonitorForEventsMatchingMask_handler(KEY_MASK, &global_block)
                .ok_or(MonitorRefused)?;

        let watched = Rc::clone(&shared);
        let local_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: as above; the pointer is AppKit's own and is only read.
            deliver(&watched, unsafe { event.as_ref() });
            // Handing the event back keeps ordinary delivery intact. Monitoring
            // is not consuming, and swallowing a key here would stop this
            // process's own windows from ever seeing one.
            event.as_ptr()
        });
        // SAFETY: the block returns exactly the event pointer it was given,
        // which is the valid non-null return the method documents.
        let local = unsafe {
            NSEvent::addLocalMonitorForEventsMatchingMask_handler(KEY_MASK, &local_block)
        };

        Ok(Keyboard {
            global: Some(global),
            local,
            counts,
        })
    }

    /// Stops capturing. Idempotent, so `Drop` can call it after the caller has.
    ///
    /// Nothing is released on the host by this. Whatever the player was holding
    /// stays held there until the sender says otherwise, which is a `ReleaseAll`
    /// and belongs to whoever owns the socket.
    pub fn release(&mut self) {
        if let Some(monitor) = self.global.take() {
            // SAFETY: the object came from
            // `addGlobalMonitorForEventsMatchingMask:handler:`, which is what
            // `removeMonitor:` expects, and `take` guarantees it is removed
            // exactly once.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
        if let Some(monitor) = self.local.take() {
            // SAFETY: as above, for the local monitor.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    }

    /// How many key transitions reached the callback.
    pub fn captured(&self) -> u64 {
        self.counts.captured.get()
    }

    /// How many auto-repeat key downs were dropped.
    pub fn repeats_suppressed(&self) -> u64 {
        self.counts.repeats_suppressed.get()
    }

    /// Whether the monitors are still installed.
    pub const fn is_capturing(&self) -> bool {
        self.global.is_some()
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.release();
    }
}

/// The event path. Reads the clock first, because every cost the caller wants to
/// measure comes after it.
#[inline]
fn deliver<F: FnMut(KeyEvent)>(shared: &Rc<RefCell<Shared<F>>>, event: &NSEvent) {
    let at = Timestamp::now();
    let kind = event.r#type();
    let virtual_key = event.keyCode();

    // Resolved before anything is counted or recorded, so a key with no PC
    // position leaves no trace at all rather than a press the pressed set
    // believes in and the host never hears about.
    let Some(scan) = ScanCode::from_virtual_key(virtual_key) else {
        return;
    };
    let flag = flag_of(virtual_key);
    let asserted = flag.is_some_and(|flag| event.modifierFlags().contains(flag));
    let repeat = kind == NSEventType::KeyDown && event.isARepeat();

    // A callback that itself caused a key event would re-enter here. Dropping
    // that event is the only safe answer, and is preferable to the panic a blind
    // borrow would produce inside an AppKit callback.
    let Ok(mut shared) = shared.try_borrow_mut() else {
        return;
    };
    let shared = &mut *shared;

    let down = if kind == NSEventType::FlagsChanged {
        // A modifier without a flag has no press to derive, and guessing one
        // would put a key in the pressed set that nothing will ever clear.
        if flag.is_none() {
            return;
        }
        match shared.modifiers.apply(virtual_key, asserted) {
            Transition::Pressed => true,
            Transition::Released => false,
            Transition::Ignored => return,
        }
    } else {
        if repeat {
            shared
                .counts
                .repeats_suppressed
                .set(shared.counts.repeats_suppressed.get() + 1);
            return;
        }
        kind == NSEventType::KeyDown
    };

    let id = shared.next_id;
    shared.next_id = id.next();
    shared.counts.captured.set(shared.counts.captured.get() + 1);
    (shared.callback)(KeyEvent { id, scan, down, at });
}

#[cfg(test)]
mod tests {
    use super::{CAPS_LOCK, KEY_MASK, Modifiers, Transition, flag_of};
    use objc2_app_kit::{NSEventMask, NSEventModifierFlags};
    use std::collections::{HashMap, HashSet};

    /// Left and right of each pair, plus the keys they share a flag with.
    const LEFT_SHIFT: u16 = 0x38;
    const RIGHT_SHIFT: u16 = 0x3C;
    const LEFT_CONTROL: u16 = 0x3B;
    const RIGHT_CONTROL: u16 = 0x3E;
    const LEFT_OPTION: u16 = 0x3A;
    const RIGHT_OPTION: u16 = 0x3D;
    const LEFT_COMMAND: u16 = 0x37;
    const RIGHT_COMMAND: u16 = 0x36;

    /// Replays a physical sequence the way macOS reports it, and returns what
    /// the tracker made of each event.
    ///
    /// The flags are computed from what is physically held rather than from the
    /// key that just moved, which is the whole point: that is what makes a
    /// release with the flag still asserted appear in the sequence at all.
    fn replay(actions: &[(u16, bool)]) -> Vec<Transition> {
        let mut physical: HashSet<u16> = HashSet::new();
        let mut tracker = Modifiers::new();
        let mut seen = Vec::with_capacity(actions.len());

        for (key, down) in actions.iter().copied() {
            if down {
                physical.insert(key);
            } else {
                physical.remove(&key);
            }
            let flags = physical
                .iter()
                .filter_map(|held| flag_of(*held))
                .fold(NSEventModifierFlags::empty(), |all, flag| all.union(flag));
            let owned = flag_of(key).expect("the sequence only uses flag-carrying keys");
            seen.push(tracker.apply(key, flags.contains(owned)));
        }
        seen
    }

    /// The invariant the whole reliability design depends on: a modifier that
    /// went down comes back up exactly once, whatever else was held at the time.
    #[test]
    fn every_modifier_press_is_matched_by_exactly_one_release() {
        // Every ordering that has ever produced a stuck modifier: both sides of
        // a pair overlapping, one side released first, and two pairs nested.
        let actions = [
            (LEFT_SHIFT, true),
            (RIGHT_SHIFT, true),
            (LEFT_SHIFT, false),
            (RIGHT_SHIFT, false),
            (RIGHT_CONTROL, true),
            (LEFT_CONTROL, true),
            (RIGHT_CONTROL, false),
            (LEFT_COMMAND, true),
            (LEFT_OPTION, true),
            (RIGHT_OPTION, true),
            (LEFT_OPTION, false),
            (LEFT_CONTROL, false),
            (RIGHT_OPTION, false),
            (RIGHT_COMMAND, true),
            (LEFT_COMMAND, false),
            (RIGHT_COMMAND, false),
        ];

        let seen = replay(&actions);

        // Every event has to be read as the direction the key actually moved.
        // A tracker that read the flag instead would call the left shift
        // release at index 2 a press, and nothing would ever release it.
        for ((key, down), transition) in actions.iter().copied().zip(seen.iter().copied()) {
            let wanted = if down {
                Transition::Pressed
            } else {
                Transition::Released
            };
            assert_eq!(
                transition, wanted,
                "virtual key {key:#04X} moved the other way"
            );
        }

        // And the same thing counted, because that is the figure an operator
        // reads: a key left on the host is a press with no release.
        let mut net: HashMap<u16, i32> = HashMap::new();
        for ((key, _), transition) in actions.iter().copied().zip(seen.iter().copied()) {
            let entry = net.entry(key).or_default();
            match transition {
                Transition::Pressed => *entry += 1,
                Transition::Released => *entry -= 1,
                Transition::Ignored => {}
            }
            assert!(*entry >= 0, "virtual key {key:#04X} was released twice");
            assert!(*entry <= 1, "virtual key {key:#04X} was pressed twice");
        }
        for (key, count) in net {
            assert_eq!(count, 0, "virtual key {key:#04X} was left held");
        }
    }

    /// The specific case that makes a shared flag mask dangerous, asserted on
    /// its own so a failure names it.
    #[test]
    fn releasing_one_shift_while_the_other_is_held_is_a_release() {
        let mut tracker = Modifiers::new();
        assert_eq!(tracker.apply(LEFT_SHIFT, true), Transition::Pressed);
        assert_eq!(tracker.apply(RIGHT_SHIFT, true), Transition::Pressed);
        // Shift is still asserted, because the right one is down.
        assert_eq!(tracker.apply(LEFT_SHIFT, true), Transition::Released);
        assert!(!tracker.contains(LEFT_SHIFT));
        assert!(tracker.contains(RIGHT_SHIFT));
        assert_eq!(tracker.apply(RIGHT_SHIFT, false), Transition::Released);
        assert_eq!(tracker.count(), 0);
    }

    /// Caps lock's flag follows the lock, so the press that switches the lock
    /// off arrives with the flag already clear and must still be a press.
    #[test]
    fn caps_lock_alternates_however_its_flag_reads() {
        let mut tracker = Modifiers::new();
        // Locking on: down and up both report the lock as on.
        assert_eq!(tracker.apply(CAPS_LOCK, true), Transition::Pressed);
        assert_eq!(tracker.apply(CAPS_LOCK, true), Transition::Released);
        // Locking off: down and up both report it as off.
        assert_eq!(tracker.apply(CAPS_LOCK, false), Transition::Pressed);
        assert_eq!(tracker.apply(CAPS_LOCK, false), Transition::Released);
        assert_eq!(tracker.count(), 0);
    }

    /// A release for a key this capture never saw pressed is not turned into a
    /// press, which is what a naive reading of a clear flag would do.
    #[test]
    fn an_unheld_key_with_a_clear_flag_says_nothing() {
        let mut tracker = Modifiers::new();
        assert_eq!(tracker.apply(LEFT_CONTROL, false), Transition::Ignored);
        assert_eq!(tracker.count(), 0);
        // A code no keyboard reports cannot claim a bit either.
        assert_eq!(tracker.apply(0xFFFF, true), Transition::Ignored);
        assert_eq!(tracker.count(), 0);
    }

    /// Modifiers are watched, because they are the ones that produce no key
    /// down at all.
    #[test]
    fn the_mask_covers_presses_releases_and_modifiers() {
        for wanted in [
            NSEventMask::KeyDown,
            NSEventMask::KeyUp,
            NSEventMask::FlagsChanged,
        ] {
            assert!(KEY_MASK.contains(wanted));
        }
        // Nothing else, so a monitor sharing this mask cannot mistake pointer
        // motion for a key.
        assert!(!KEY_MASK.contains(NSEventMask::MouseMoved));
        assert!(!KEY_MASK.contains(NSEventMask::ScrollWheel));
    }

    /// The fn and globe keys reach a flags-changed handler with no flag of
    /// their own, and there is nothing to forward for them.
    #[test]
    fn a_key_with_no_flag_is_not_a_modifier() {
        assert!(flag_of(0x3F).is_none());
        assert!(flag_of(0x0D).is_none());
        assert!(flag_of(LEFT_SHIFT).is_some());
    }
}
