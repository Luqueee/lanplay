//! What the host believes is held, and what the OS must be told to make that
//! belief true.
//!
//! Nothing in this module touches an OS input API. Every message turns into a
//! sequence of [`Action`]s and the caller decides what to do with them, which
//! is what makes the interesting cases testable: a lost release, a reordered
//! snapshot and a retransmitted key press are all decided here, on any
//! platform, without a desktop and without moving anybody's pointer.
//!
//! Two other designs were considered and rejected. Replaying the client's
//! stream without keeping state at all is smaller, but then a
//! [`Message::ReleaseAll`] has nothing to release and a lost key-up is
//! permanent: the whole reason the protocol carries snapshots is that the
//! host must be able to converge on its own. Asking Windows what is held,
//! with `GetAsyncKeyState`, looks like it would avoid keeping state, but it
//! reports the physical keyboard attached to the host as well as the injected
//! keys, so a release sweep would fight whoever is sitting at the machine.
//! The host therefore tracks only what it injected.
//!
//! Held keys live in the protocol's own [`KeyBitset`], not in a set of wire
//! keys, so reconciling against a snapshot is a comparison rather than a
//! translation.

use lanplay_input_protocol::{Button, Datagram, EventId, KeyBitset, Message, SessionId};

/// Every button the protocol defines, in index order.
pub const BUTTONS: [Button; 5] = [
    Button::Left,
    Button::Right,
    Button::Middle,
    Button::X1,
    Button::X2,
];

/// One thing the OS input system is asked to do.
///
/// Deliberately smaller than the protocol's message set: a wheel message
/// carrying both axes becomes two actions, and a snapshot becomes as many
/// actions as it disagrees with the host about. Nothing here carries an event
/// id, because deduplication has already happened by the time an action
/// exists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    /// Additive pointer movement, in the units the client measured.
    Motion {
        dx: i32,
        dy: i32,
    },
    /// A physical key, by set-1 make code. `extended` asks for the 0xE0
    /// prefix that distinguishes, for instance, right control from left.
    Key {
        make: u8,
        extended: bool,
        down: bool,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// Wheel detents, positive away from the user vertically and to the right
    /// horizontally.
    Wheel {
        axis: WheelAxis,
        detents: i16,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WheelAxis {
    Vertical,
    Horizontal,
}

/// What happened to a message, and therefore what the caller owes the client.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Newly applied. Any actions it implies have been emitted, which for a
    /// heartbeat is none.
    Applied,
    /// A retransmission of a reliable event that was already applied. No
    /// actions were emitted, and an acknowledgement is still owed: the client
    /// retransmits until it hears one, so staying silent about a duplicate
    /// guarantees another copy.
    Duplicate,
    /// A snapshot no newer than the last one applied. Discarded, because
    /// reordering it into the present would resurrect keys that have since
    /// been released.
    Stale,
    /// Host-to-client traffic that arrived at the host. Nothing to inject.
    Ignored,
    /// A reliable event whose id precedes the last `ReleaseAll`. Acknowledged
    /// so the client stops retransmitting, and never applied.
    ///
    /// Without this a focus loss could paradoxically leave a key down: a press
    /// that was lost on the way, retransmitted, and delivered after the
    /// release carries an id the deduplication window has never seen, so it
    /// looks fresh and would be pressed. Deduplication cannot catch it because
    /// it never arrived the first time.
    Superseded,
    /// A datagram belonging to some other session. Nothing was injected and
    /// nothing is owed: that session is over, and an acknowledgement would
    /// tell a departed client that its event landed on this host.
    WrongSession,
}

/// Release sweeps, by what caused them.
///
/// Split apart because they say different things about the client and an
/// operator needs to tell them apart. A requested release is the client
/// saying goodbye: it lost focus, released capture or exited. An expired one
/// is the client saying nothing at all, which is a network or a crash. Both
/// end in the same empty state, and only one of them means something is
/// wrong.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Releases {
    /// `ReleaseAll` messages applied. A retransmission of one already applied
    /// is not counted again, because it swept nothing.
    pub requested: u64,
    /// Sweeps caused by the client falling silent for longer than the host
    /// waits.
    pub expired: u64,
    /// Sweeps caused by a new session displacing an old one.
    pub session_change: u64,
}

impl Outcome {
    /// Whether the message changed the host's idea of the world.
    pub fn is_applied(self) -> bool {
        matches!(self, Outcome::Applied)
    }

    /// Whether the client is owed an acknowledgement, given that this message
    /// was reliable.
    ///
    /// True for a duplicate and for a superseded event as well as for a fresh
    /// one. The client retransmits until it hears something, so silence about
    /// an event the host has decided never to apply buys five more copies of
    /// it and then an abandonment the client reports as possible loss.
    pub fn owes_ack(self) -> bool {
        matches!(
            self,
            Outcome::Applied | Outcome::Duplicate | Outcome::Superseded
        )
    }
}

/// What the host would tell the client it has, right now.
///
/// `top` is the highest event id the host has applied, and bit `i` of
/// `missing` says that `top - 1 - i` has not been. Anchored at the top so
/// that a hole delays only itself. A cumulative frontier plus a window of ids
/// above it stops dead at the first event that is lost for good, and every
/// later event then goes unacknowledged however many times the client sends
/// it, which is what fault injection at five per cent loss found.
///
/// An id below the oldest bit the window still holds is reported as applied
/// rather than as missing, because the evidence for it is gone. That is the
/// honest reading of what happens next: the host will never inject an id that
/// old, and the client's retransmission ladder has already run out for it, so
/// what repairs the state is the periodic snapshot rather than another copy of
/// the event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Acknowledgement {
    pub top: EventId,
    pub missing: u32,
}

/// Folds a wire key into the single byte that the snapshot bitset and
/// `SendInput` both address.
///
/// A set-1 make code fits in seven bits, and the extended keys repeat those
/// same codes behind a 0xE0 prefix, which is what the high half of the bitset
/// is for. The prefix never travels as part of the code here: `SendInput`
/// wants the make code plus a flag, so a client that puts 0xE0 in the high
/// byte of its `u16` and a client that only sets the flag fold to the same
/// slot.
pub fn key_slot(scancode: u16, extended: bool) -> u8 {
    (scancode as u8 & 0x7F) | if extended { 0x80 } else { 0 }
}

/// Unfolds a bitset slot back into what has to be injected.
pub fn slot_key(slot: u8) -> (u8, bool) {
    (slot & 0x7F, slot & 0x80 != 0)
}

/// The host's model of the client's keyboard and mouse.
///
/// One of these per session. It is the thing a `ReleaseAll` releases and the
/// thing a snapshot reconciles against, and it is deliberately independent of
/// whether the OS accepted the injection: see [`Action`] and the crate docs
/// for why a refused call is counted rather than rolled back.
pub struct HostState {
    /// The one session whose datagrams may reach the OS. Not an `Option`,
    /// because a state machine that does not yet know which session it serves
    /// would have to accept the first datagram that arrives to find out, and
    /// a stale one arriving first would then own the host.
    session: SessionId,
    keys: KeyBitset,
    /// The id of the last applied `ReleaseAll`, which is a barrier and not
    /// merely a sweep: nothing logically before it may recreate held state,
    /// however late it arrives.
    barrier: Option<EventId>,
    /// Bit per [`Button::index`], matching the snapshot's button field so the
    /// two can be compared directly.
    buttons: u8,
    reliable: Dedup,
    /// Absent until the first snapshot, because the client's generation
    /// counter starts wherever its session started and the host has no say in
    /// it.
    generation: Option<u32>,
    stale_snapshots: u64,
    releases: Releases,
}

impl HostState {
    /// Starts empty, for one session. There is no `Default`: a session id is
    /// minted by the control plane and inventing one here would give every
    /// host the same one.
    pub fn new(session: SessionId) -> Self {
        HostState {
            session,
            keys: KeyBitset::EMPTY,
            buttons: 0,
            reliable: Dedup::new(),
            barrier: None,
            generation: None,
            stale_snapshots: 0,
            releases: Releases::default(),
        }
    }

    /// Which session this state machine will accept datagrams for.
    pub fn session(&self) -> SessionId {
        self.session
    }

    /// Applies a decoded datagram, refusing whatever belongs to another
    /// session.
    ///
    /// The check lives here rather than in the caller because inertness is
    /// part of what this type guarantees. A stale `ReleaseAll` from a session
    /// that has ended would otherwise let go of keys the current user is
    /// holding, and a stale key-down would press a key nobody is touching:
    /// both are indistinguishable from a working host until somebody plays a
    /// game on it.
    pub fn apply_datagram(&mut self, datagram: &Datagram, emit: impl FnMut(Action)) -> Outcome {
        if datagram.session != self.session {
            return Outcome::WrongSession;
        }
        self.apply(&datagram.message, emit)
    }

    /// Hands the host to a new session, releasing whatever the old one held.
    ///
    /// Starting empty is the only safe reading of a new session: the keys the
    /// old client held are not the new client's business, and inheriting them
    /// would leave a key down that nobody ever pressed and that no snapshot
    /// from the new client will ever mention. The sweep goes to the OS,
    /// because the OS still believes those keys are down.
    pub fn begin_session(&mut self, session: SessionId, mut emit: impl FnMut(Action)) {
        if session == self.session {
            return;
        }
        self.releases.session_change += 1;
        self.release_all(&mut emit);
        self.session = session;
        // Event ids and snapshot generations are the client's own counters,
        // and a new client starts them wherever it likes. Carrying either
        // across would deduplicate a fresh press against a stranger's id, or
        // discard the first snapshot for being older than one the previous
        // client sent.
        self.reliable = Dedup::new();
        self.barrier = None;
        self.generation = None;
    }

    /// Releases everything because the client has gone quiet for too long.
    ///
    /// The same sweep a `ReleaseAll` performs, counted apart from it. Which
    /// of the two ended a run is the difference between a client that said
    /// goodbye and one that vanished, and the state it leaves behind is
    /// identical, so the counters are the only place that difference survives.
    pub fn expire(&mut self, mut emit: impl FnMut(Action)) {
        self.releases.expired += 1;
        self.release_all(&mut emit);
    }

    /// Release sweeps so far, by cause.
    pub fn releases(&self) -> Releases {
        self.releases
    }

    /// Decides what the OS must be told, records that it was told, and
    /// reports what the client is owed.
    ///
    /// `emit` is called once per action, in the order the actions must reach
    /// the OS. It takes a closure rather than filling a buffer because there
    /// is no queue anywhere on this path: an action is handed to the OS as it
    /// is decided, so one event in is one call out.
    pub fn apply(&mut self, message: &Message, mut emit: impl FnMut(Action)) -> Outcome {
        if let Some(id) = message.event_id() {
            // The barrier is checked before the deduplication window, because
            // the event this exists to stop has never been seen: a press lost
            // on the way, retransmitted, and arriving after the release that
            // was meant to end it. To the window it looks new.
            if self.barrier.is_some_and(|barrier| id < barrier) {
                // Marked, not merely refused. The acknowledgement is built
                // from this window, so an id left out of it reads as a hole:
                // the client retransmits until the id falls out of reach and
                // then reports it as possibly lost, for an event the host has
                // definitively decided about. Deciding and saying nothing is
                // worse than not deciding.
                self.reliable.mark(id);
                return Outcome::Superseded;
            }
            if !self.reliable.mark(id) {
                return Outcome::Duplicate;
            }
        }

        match *message {
            Message::Motion { dx, dy } => {
                // A zero delta is a call into the OS that moves nothing.
                if dx != 0 || dy != 0 {
                    emit(Action::Motion { dx, dy });
                }
            }
            Message::Key {
                scancode,
                down,
                extended,
                ..
            } => {
                // Injected even when the host already believes the key is in
                // that position: a repeated down is auto-repeat, which games
                // count, and a release of a key the host does not think is
                // held is how a host with a wrong model gets corrected.
                let slot = key_slot(scancode, extended);
                self.keys.set(slot, down);
                let (make, extended) = slot_key(slot);
                emit(Action::Key {
                    make,
                    extended,
                    down,
                });
            }
            Message::Button { button, down, .. } => {
                self.set_button(button, down);
                emit(Action::Button { button, down });
            }
            Message::Wheel { dx, dy, .. } => {
                if dy != 0 {
                    emit(Action::Wheel {
                        axis: WheelAxis::Vertical,
                        detents: dy,
                    });
                }
                if dx != 0 {
                    emit(Action::Wheel {
                        axis: WheelAxis::Horizontal,
                        detents: dx,
                    });
                }
            }
            Message::Snapshot {
                generation,
                keys,
                buttons,
            } => {
                if let Some(last) = self.generation
                    && generation <= last
                {
                    self.stale_snapshots += 1;
                    return Outcome::Stale;
                }
                self.generation = Some(generation);
                self.reconcile(keys, buttons, &mut emit);
            }
            Message::ReleaseAll { id } => {
                // A barrier and not merely a sweep. Everything the client sent
                // before this is stale by construction: it described a world
                // in which something was held, and the client has since said
                // nothing is.
                self.barrier = Some(id);
                self.releases.requested += 1;
                self.release_all(&mut emit);
            }
            Message::Heartbeat => {}
            Message::Ack { .. }
            | Message::GamepadAttach { .. }
            | Message::GamepadDetach { .. }
            | Message::GamepadState(_) => return Outcome::Ignored,
        }
        Outcome::Applied
    }

    /// What the client is owed, or `None` before any reliable event has been
    /// applied.
    ///
    /// Read out of the deduplication window rather than kept alongside it.
    /// The window already knows which ids have been applied, and a second
    /// record of the same fact would eventually disagree with the one that
    /// decides whether a key is injected.
    pub fn acknowledgement(&self) -> Option<Acknowledgement> {
        self.reliable.acknowledgement()
    }

    /// Which keys the host believes are held, in the same representation a
    /// snapshot arrives in.
    pub fn held_keys(&self) -> KeyBitset {
        self.keys
    }

    /// Which buttons the host believes are held, as snapshot button bits.
    pub fn held_buttons(&self) -> u8 {
        self.buttons
    }

    pub fn holds_key(&self, scancode: u16, extended: bool) -> bool {
        self.keys.contains(key_slot(scancode, extended))
    }

    pub fn holds_button(&self, button: Button) -> bool {
        self.buttons & button.mask() != 0
    }

    /// The invariant a `ReleaseAll` exists to restore.
    pub fn nothing_held(&self) -> bool {
        self.keys.is_empty() && self.buttons == 0
    }

    /// Snapshots discarded for being no newer than one already applied.
    pub fn stale_snapshots(&self) -> u64 {
        self.stale_snapshots
    }

    fn set_button(&mut self, button: Button, down: bool) {
        if down {
            self.buttons |= button.mask();
        } else {
            self.buttons &= !button.mask();
        }
    }

    fn release_all(&mut self, emit: &mut impl FnMut(Action)) {
        // Copied first because the sweep clears as it goes and the iterator
        // borrows the bitset it is walking.
        let held = self.keys;
        for slot in held.held() {
            let (make, extended) = slot_key(slot);
            emit(Action::Key {
                make,
                extended,
                down: false,
            });
        }
        self.keys = KeyBitset::EMPTY;

        for button in BUTTONS {
            if self.holds_button(button) {
                emit(Action::Button {
                    button,
                    down: false,
                });
            }
        }
        self.buttons = 0;
    }

    /// Moves the OS from what the host holds to what the client says it holds.
    ///
    /// Only the disagreements are injected. A snapshot that agrees with the
    /// host, which is the common case, costs nothing.
    fn reconcile(&mut self, keys: KeyBitset, buttons: u8, emit: &mut impl FnMut(Action)) {
        for slot in 0u8..=255 {
            let wanted = keys.contains(slot);
            if wanted != self.keys.contains(slot) {
                let (make, extended) = slot_key(slot);
                emit(Action::Key {
                    make,
                    extended,
                    down: wanted,
                });
            }
        }
        self.keys = keys;

        for button in BUTTONS {
            let wanted = buttons & button.mask() != 0;
            if wanted != self.holds_button(button) {
                emit(Action::Button {
                    button,
                    down: wanted,
                });
            }
        }
        // Masked to the buttons the protocol defines, so an unknown high bit
        // cannot make `nothing_held` permanently false.
        self.buttons = buttons & 0x1F;
    }
}

/// How many event ids the deduplication window covers.
///
/// A retransmission arrives within a round trip, so an id this far behind the
/// newest one has either been applied already or been abandoned by the client.
/// The window is a fixed 128 bytes: a growing set of seen ids would let a
/// long session, or a peer inventing ids, decide how much memory the host
/// spends.
const WINDOW: u64 = 1024;
const WORDS: usize = (WINDOW / 64) as usize;

/// Applied-once bookkeeping for reliable events.
///
/// `base` is the lowest id not known to be applied, so everything below it is
/// treated as already applied. That is the safe direction of error: injecting
/// a key twice is visible to the player, while discarding a message that the
/// window has already slid past costs at most one event, which the next
/// snapshot repairs.
struct Dedup {
    base: Option<u64>,
    applied: [u64; WORDS],
}

impl Dedup {
    fn new() -> Self {
        Dedup {
            base: None,
            applied: [0; WORDS],
        }
    }

    /// Records an id, reporting whether this was the first sighting.
    fn mark(&mut self, id: EventId) -> bool {
        let id = id.0;
        // The first id lands in the newest slot rather than the oldest, so
        // ids the client sent before it but that arrived after it are still
        // applied rather than mistaken for retransmissions.
        let mut base = self.base.unwrap_or_else(|| id.saturating_sub(WINDOW - 1));
        if id < base {
            return false;
        }

        let mut offset = id - base;
        if offset >= WINDOW {
            // The client has moved on further than the window is wide. Slide
            // so the new id is the newest slot rather than refuse it: refusing
            // would drop every event from here on.
            let shift = offset - (WINDOW - 1);
            self.slide(shift);
            base += shift;
            offset = WINDOW - 1;
        }

        let fresh = !self.get(offset);
        if fresh {
            self.set(offset);
            // Keep `base` at the lowest unapplied id, which is what makes the
            // window slide by itself under a contiguous stream and what lets
            // `id < base` mean "already applied".
            while self.get(0) {
                self.slide(1);
                base += 1;
            }
        }
        self.base = Some(base);
        fresh
    }

    /// The highest applied id, and which of the thirty-two ids below it are
    /// still missing, read straight out of the window.
    ///
    /// Both come from the bits that decide injection, so the acknowledgement
    /// cannot drift away from what was actually applied.
    ///
    /// An id below the oldest bit the window still holds is reported as
    /// applied. Its evidence has either been slid away or never existed,
    /// because the host joins the client's stream part-way through, and asking
    /// for it back would ask for a retransmission the client stopped making
    /// long ago.
    fn acknowledgement(&self) -> Option<Acknowledgement> {
        let base = self.base?;
        let top = match self.newest() {
            Some(offset) => base + offset,
            // A stream without holes slides every bit out from under itself,
            // which leaves the newest applied id just below `base`. A `base`
            // of zero with an empty window would mean no id was ever marked,
            // and then there is nothing to acknowledge.
            None => base.checked_sub(1)?,
        };
        let oldest = self.oldest().map_or(base, |offset| base + offset);
        let mut missing = 0u32;
        for bit in 0..32u32 {
            let Some(id) = top.checked_sub(bit as u64 + 1) else {
                break;
            };
            // Descending, so the first id past the oldest bit means every
            // lower one is past it too.
            if id < oldest {
                break;
            }
            if !self.get(id - base) {
                missing |= 1 << bit;
            }
        }
        Some(Acknowledgement {
            top: EventId(top),
            missing,
        })
    }

    /// Offset of the newest applied id the window still holds.
    fn newest(&self) -> Option<u64> {
        self.applied
            .iter()
            .enumerate()
            .rev()
            .find(|(_, word)| **word != 0)
            .map(|(index, word)| index as u64 * 64 + 63 - word.leading_zeros() as u64)
    }

    /// Offset of the oldest applied id the window still holds.
    fn oldest(&self) -> Option<u64> {
        self.applied
            .iter()
            .enumerate()
            .find(|(_, word)| **word != 0)
            .map(|(index, word)| index as u64 * 64 + word.trailing_zeros() as u64)
    }

    fn get(&self, offset: u64) -> bool {
        self.applied[(offset / 64) as usize] & (1u64 << (offset % 64)) != 0
    }

    fn set(&mut self, offset: u64) {
        self.applied[(offset / 64) as usize] |= 1u64 << (offset % 64);
    }

    /// Drops the lowest `shift` slots and moves the rest down.
    fn slide(&mut self, shift: u64) {
        if shift >= WINDOW {
            self.applied = [0; WORDS];
            return;
        }
        let words = (shift / 64) as usize;
        let bits = (shift % 64) as u32;
        // Ascending, because every source index is at or above the index being
        // written and so is still untouched when it is read.
        for index in 0..WORDS {
            let source = index + words;
            let mut value = if source < WORDS {
                self.applied[source] >> bits
            } else {
                0
            };
            if bits > 0 && source + 1 < WORDS {
                value |= self.applied[source + 1] << (64 - bits);
            }
            self.applied[index] = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lanplay_input_protocol::Sequence;

    /// Scan codes of a few real keys, so a failure reads as a key rather than
    /// as a number. 0x11 is W, 0x1D is left control, 0x1D extended is right
    /// control and 0x2A is left shift.
    const W: u16 = 0x11;
    const LEFT_CONTROL: u16 = 0x1D;
    const LEFT_SHIFT: u16 = 0x2A;

    /// The session every test's host serves, and one that has ended.
    const SESSION: SessionId = SessionId(7);
    const OLD_SESSION: SessionId = SessionId(6);

    fn apply(state: &mut HostState, message: &Message) -> (Outcome, Vec<Action>) {
        let mut actions = Vec::new();
        let outcome = state.apply(message, |action| actions.push(action));
        (outcome, actions)
    }

    /// Applies a message as it would arrive on the wire, carrying a session.
    fn deliver(
        state: &mut HostState,
        session: SessionId,
        message: Message,
    ) -> (Outcome, Vec<Action>) {
        let mut actions = Vec::new();
        let outcome = state.apply_datagram(
            &Datagram {
                session,
                sequence: Sequence(0),
                sent_at_ns: 0,
                message,
            },
            |action| actions.push(action),
        );
        (outcome, actions)
    }

    fn expire(state: &mut HostState) -> Vec<Action> {
        let mut actions = Vec::new();
        state.expire(|action| actions.push(action));
        actions
    }

    fn key(id: u64, scancode: u16, down: bool, extended: bool) -> Message {
        Message::Key {
            id: EventId(id),
            scancode,
            down,
            extended,
        }
    }

    fn button(id: u64, button: Button, down: bool) -> Message {
        Message::Button {
            id: EventId(id),
            button,
            down,
        }
    }

    /// Holds W, left shift and the left button, which is what a player walking
    /// and shooting is holding.
    fn hold_three(state: &mut HostState) {
        apply(state, &key(1, W, true, false));
        apply(state, &key(2, LEFT_SHIFT, true, false));
        apply(state, &button(3, Button::Left, true));
        assert!(!state.nothing_held());
    }

    fn ack(state: &HostState) -> Acknowledgement {
        state
            .acknowledgement()
            .expect("a reliable event has been applied")
    }

    #[test]
    fn a_contiguous_run_is_acknowledged_by_its_top() {
        let mut state = HostState::new(SESSION);
        for id in 1..=5 {
            assert!(
                apply(&mut state, &key(id, W, id % 2 == 1, false))
                    .0
                    .is_applied()
            );
        }
        assert_eq!(
            ack(&state),
            Acknowledgement {
                top: EventId(5),
                missing: 0
            }
        );
    }

    #[test]
    fn a_hole_shows_up_below_the_top_and_clears_when_it_is_filled() {
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, W, true, false));
        apply(&mut state, &key(2, W, false, false));
        // Four is applied, so it is the top, and the hole at three is the id
        // one below it: bit 0.
        apply(&mut state, &key(4, LEFT_CONTROL, true, false));
        assert_eq!(
            ack(&state),
            Acknowledgement {
                top: EventId(4),
                missing: 0b1
            }
        );

        // Filling the hole clears its bit and leaves the top where it was:
        // three is not the newest id, it was only the last one to arrive.
        apply(&mut state, &key(3, W, true, false));
        assert_eq!(
            ack(&state),
            Acknowledgement {
                top: EventId(4),
                missing: 0
            }
        );
    }

    /// The regression this acknowledgement shape exists for.
    ///
    /// The cumulative form reported a frontier of two here, one below the
    /// permanent hole, and a mask reaching only as far as thirty-four. It
    /// stayed there for the rest of the session however many times the client
    /// resent, so a client at id two hundred abandoned everything from
    /// thirty-five up.
    #[test]
    fn one_permanently_lost_event_does_not_stall_the_acknowledgement() {
        let mut state = HostState::new(SESSION);
        for id in (1..=200).filter(|id| *id != 3) {
            assert!(
                apply(&mut state, &key(id, W, id % 2 == 1, false))
                    .0
                    .is_applied()
            );
        }
        assert_eq!(
            ack(&state),
            Acknowledgement {
                top: EventId(200),
                // Three is far below the thirty-two ids the window reports on,
                // so it is no longer asked for: a retransmission that late
                // would arrive long after the client stopped sending it, and
                // the next snapshot is what repairs it.
                missing: 0
            }
        );
    }

    #[test]
    fn a_duplicate_is_still_acknowledged_and_moves_nothing() {
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, W, true, false));
        let before = ack(&state);

        let (outcome, actions) = apply(&mut state, &key(1, W, true, false));
        assert_eq!(outcome, Outcome::Duplicate);
        assert!(actions.is_empty());
        assert!(
            outcome.owes_ack(),
            "an unacknowledged duplicate is retransmitted forever"
        );
        assert_eq!(ack(&state), before);
    }

    #[test]
    fn an_id_far_beyond_the_window_does_not_corrupt_the_view() {
        let mut state = HostState::new(SESSION);
        for id in 1..=5 {
            apply(&mut state, &key(id, W, id % 2 == 1, false));
        }
        // A peer inventing an id, or a client whose counter jumped, moves the
        // top with it and reports no holes: the millions of ids in between are
        // below everything the window holds bits for, and claiming them
        // missing would ask for retransmissions of events that were never
        // sent.
        assert!(
            apply(&mut state, &key(5_000_000, W, false, false))
                .0
                .is_applied()
        );
        assert_eq!(
            ack(&state),
            Acknowledgement {
                top: EventId(5_000_000),
                missing: 0
            }
        );
    }

    #[test]
    fn nothing_reliable_means_nothing_to_acknowledge() {
        let mut state = HostState::new(SESSION);
        apply(&mut state, &Message::Motion { dx: 3, dy: 4 });
        apply(&mut state, &Message::Heartbeat);
        assert_eq!(state.acknowledgement(), None);
    }

    #[test]
    fn retransmitted_event_injects_once_and_is_acknowledged_twice() {
        let mut state = HostState::new(SESSION);
        let press = key(7, W, true, false);

        let (first, actions) = apply(&mut state, &press);
        assert_eq!(first, Outcome::Applied);
        assert_eq!(
            actions,
            vec![Action::Key {
                make: 0x11,
                extended: false,
                down: true
            }]
        );

        let (second, actions) = apply(&mut state, &press);
        assert_eq!(second, Outcome::Duplicate);
        assert!(actions.is_empty(), "a duplicate must not reach the OS");
        assert!(
            first.owes_ack() && second.owes_ack(),
            "the client stops retransmitting only once it hears about both"
        );
        assert!(state.holds_key(W, false));
    }

    #[test]
    fn distinct_event_ids_both_apply() {
        let mut state = HostState::new(SESSION);
        assert!(apply(&mut state, &key(7, W, true, false)).0.is_applied());
        assert!(apply(&mut state, &key(8, W, false, false)).0.is_applied());
        assert!(!state.holds_key(W, false));
    }

    #[test]
    fn out_of_order_ids_apply_and_still_deduplicate() {
        let mut state = HostState::new(SESSION);
        assert!(apply(&mut state, &key(10, W, true, false)).0.is_applied());
        // Arrives late but was never applied, so it must not be mistaken for
        // a retransmission of anything.
        assert!(
            apply(&mut state, &key(9, LEFT_CONTROL, true, false))
                .0
                .is_applied()
        );
        assert_eq!(
            apply(&mut state, &key(9, LEFT_CONTROL, true, false)).0,
            Outcome::Duplicate
        );
        assert_eq!(
            apply(&mut state, &key(10, W, true, false)).0,
            Outcome::Duplicate
        );
    }

    #[test]
    fn deduplication_survives_a_long_contiguous_stream() {
        let mut state = HostState::new(SESSION);
        for id in 0..(WINDOW * 4) {
            assert!(
                apply(&mut state, &key(id, W, id % 2 == 0, false))
                    .0
                    .is_applied(),
                "id {id} is fresh"
            );
        }
        assert_eq!(
            apply(&mut state, &key(WINDOW * 4 - 1, W, true, false)).0,
            Outcome::Duplicate
        );
        assert_eq!(
            apply(&mut state, &key(0, W, true, false)).0,
            Outcome::Duplicate
        );
    }

    #[test]
    fn release_all_converges_to_nothing_held() {
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, W, true, false));
        apply(&mut state, &key(2, LEFT_CONTROL, true, true));
        apply(
            &mut state,
            &Message::Button {
                id: EventId(3),
                button: Button::X2,
                down: true,
            },
        );
        assert!(!state.nothing_held());

        let (outcome, actions) = apply(&mut state, &Message::ReleaseAll { id: EventId(4) });
        assert_eq!(outcome, Outcome::Applied);
        assert!(state.nothing_held(), "the point of the message");
        assert!(
            actions.iter().all(|action| matches!(
                action,
                Action::Key { down: false, .. } | Action::Button { down: false, .. }
            )),
            "a release sweep may only release: {actions:?}"
        );
        assert_eq!(actions.len(), 3, "one per held key and button: {actions:?}");
        assert!(actions.contains(&Action::Key {
            make: 0x1D,
            extended: true,
            down: false
        }));
        assert!(actions.contains(&Action::Button {
            button: Button::X2,
            down: false
        }));

        // Nothing is held, so a second sweep has nothing to say.
        let (_, actions) = apply(&mut state, &Message::ReleaseAll { id: EventId(5) });
        assert!(actions.is_empty());
    }

    #[test]
    fn release_all_releases_a_key_the_client_never_released() {
        // The case that motivates host-side tracking: the client's key-up was
        // lost, so only the host knows the key is still down.
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, W, true, false));
        let (_, actions) = apply(&mut state, &Message::ReleaseAll { id: EventId(2) });
        assert_eq!(
            actions,
            vec![Action::Key {
                make: 0x11,
                extended: false,
                down: false
            }]
        );
    }

    #[test]
    fn stale_snapshot_generation_is_discarded() {
        let mut state = HostState::new(SESSION);
        let mut keys = KeyBitset::EMPTY;
        keys.set(key_slot(W, false), true);

        assert_eq!(
            apply(
                &mut state,
                &Message::Snapshot {
                    generation: 5,
                    keys,
                    buttons: 0
                }
            )
            .0,
            Outcome::Applied
        );
        apply(&mut state, &Message::ReleaseAll { id: EventId(1) });

        // An older snapshot, delivered late, describes a world where W is
        // still down. Applying it would press a key the user has let go of.
        let (outcome, actions) = apply(
            &mut state,
            &Message::Snapshot {
                generation: 4,
                keys,
                buttons: 0,
            },
        );
        assert_eq!(outcome, Outcome::Stale);
        assert!(actions.is_empty());
        assert!(state.nothing_held());
        assert_eq!(state.stale_snapshots(), 1);

        // Same generation as the last applied one is not newer either.
        assert_eq!(
            apply(
                &mut state,
                &Message::Snapshot {
                    generation: 5,
                    keys,
                    buttons: 0
                }
            )
            .0,
            Outcome::Stale
        );
        assert!(state.nothing_held());
    }

    #[test]
    fn snapshot_reconciles_both_directions() {
        let mut state = HostState::new(SESSION);
        // The host believes W and the left button are held.
        apply(&mut state, &key(1, W, true, false));
        apply(
            &mut state,
            &Message::Button {
                id: EventId(2),
                button: Button::Left,
                down: true,
            },
        );

        // The client says right control is held and nothing else is: W and
        // the button have to come up, right control has to go down.
        let mut keys = KeyBitset::EMPTY;
        keys.set(key_slot(LEFT_CONTROL, true), true);
        let (outcome, actions) = apply(
            &mut state,
            &Message::Snapshot {
                generation: 1,
                keys,
                buttons: 0,
            },
        );

        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(
            actions,
            vec![
                Action::Key {
                    make: 0x11,
                    extended: false,
                    down: false
                },
                Action::Key {
                    make: 0x1D,
                    extended: true,
                    down: true
                },
                Action::Button {
                    button: Button::Left,
                    down: false
                },
            ]
        );
        assert!(state.holds_key(LEFT_CONTROL, true));
        assert!(!state.holds_key(W, false));
        assert!(!state.holds_button(Button::Left));
    }

    #[test]
    fn agreeing_snapshot_injects_nothing() {
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, W, true, false));
        let mut keys = KeyBitset::EMPTY;
        keys.set(key_slot(W, false), true);
        let (outcome, actions) = apply(
            &mut state,
            &Message::Snapshot {
                generation: 9,
                keys,
                buttons: 0,
            },
        );
        assert_eq!(outcome, Outcome::Applied);
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn snapshot_button_bits_beyond_the_protocol_do_not_stick() {
        let mut state = HostState::new(SESSION);
        let (_, actions) = apply(
            &mut state,
            &Message::Snapshot {
                generation: 1,
                keys: KeyBitset::EMPTY,
                buttons: 0xFF,
            },
        );
        assert_eq!(actions.len(), BUTTONS.len());
        let (_, actions) = apply(
            &mut state,
            &Message::Snapshot {
                generation: 2,
                keys: KeyBitset::EMPTY,
                buttons: 0,
            },
        );
        assert_eq!(actions.len(), BUTTONS.len());
        assert!(state.nothing_held());
    }

    #[test]
    fn extended_and_plain_keys_are_different_keys() {
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, LEFT_CONTROL, true, false));
        apply(&mut state, &key(2, LEFT_CONTROL, true, true));
        assert!(state.holds_key(LEFT_CONTROL, false));
        assert!(state.holds_key(LEFT_CONTROL, true));

        apply(&mut state, &key(3, LEFT_CONTROL, false, true));
        assert!(
            state.holds_key(LEFT_CONTROL, false),
            "releasing right control must not release left control"
        );
    }

    #[test]
    fn a_prefixed_scancode_folds_onto_the_flag() {
        // A client that sends the full 0xE01D code and one that sends 0x1D
        // with the extended flag mean the same physical key, and the host must
        // not end up believing two keys are held.
        let mut state = HostState::new(SESSION);
        apply(&mut state, &key(1, 0xE01D, true, true));
        assert!(state.holds_key(LEFT_CONTROL, true));
        let (_, actions) = apply(&mut state, &Message::ReleaseAll { id: EventId(2) });
        assert_eq!(
            actions,
            vec![Action::Key {
                make: 0x1D,
                extended: true,
                down: false
            }]
        );
    }

    #[test]
    fn wheel_carries_one_action_per_moving_axis() {
        let mut state = HostState::new(SESSION);
        let (outcome, actions) = apply(
            &mut state,
            &Message::Wheel {
                id: EventId(1),
                dx: -2,
                dy: 3,
            },
        );
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(
            actions,
            vec![
                Action::Wheel {
                    axis: WheelAxis::Vertical,
                    detents: 3
                },
                Action::Wheel {
                    axis: WheelAxis::Horizontal,
                    detents: -2
                },
            ]
        );

        let (_, actions) = apply(
            &mut state,
            &Message::Wheel {
                id: EventId(2),
                dx: 0,
                dy: 0,
            },
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn motion_passes_through_unchanged_and_holds_nothing() {
        let mut state = HostState::new(SESSION);
        let (outcome, actions) = apply(&mut state, &Message::Motion { dx: 4, dy: -3 });
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(actions, vec![Action::Motion { dx: 4, dy: -3 }]);
        assert!(state.nothing_held());

        // Motion has no event id, so two identical deltas are two movements
        // and not a retransmission.
        let (outcome, actions) = apply(&mut state, &Message::Motion { dx: 4, dy: -3 });
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(actions, vec![Action::Motion { dx: 4, dy: -3 }]);
    }

    #[test]
    fn heartbeat_applies_and_an_ack_is_ignored() {
        let mut state = HostState::new(SESSION);
        let (outcome, actions) = apply(&mut state, &Message::Heartbeat);
        assert_eq!(outcome, Outcome::Applied);
        assert!(actions.is_empty());

        let (outcome, actions) = apply(
            &mut state,
            &Message::Ack {
                top: EventId(1),
                missing: 0,
            },
        );
        assert_eq!(outcome, Outcome::Ignored);
        assert!(actions.is_empty());
        assert!(!outcome.owes_ack(), "acknowledging an acknowledgement");
    }

    #[test]
    fn a_press_lost_before_a_release_cannot_arrive_after_it_and_hold() {
        // The dangerous ordering, and the one deduplication cannot catch. A
        // press is lost on the way, so the host has never seen its id. The
        // client loses focus and its ReleaseAll arrives. Then the press is
        // retransmitted and lands. To the deduplication window it looks new,
        // and applying it would leave a key held *after* a release - a focus
        // loss that paradoxically presses something.
        let mut state = HostState::new(SESSION);
        let mut actions = Vec::new();

        // Shift arrives and is held; W is lost in flight and never seen.
        state.apply(
            &Message::Key {
                id: EventId(10),
                scancode: 0x2A,
                down: true,
                extended: false,
            },
            |action| actions.push(action),
        );
        assert_eq!(
            actions.len(),
            1,
            "the shift press should have been injected"
        );

        // The client lets go. Its release carries a later id than the press
        // that went missing, which is what makes the ordering decidable.
        actions.clear();
        let outcome = state.apply(&Message::ReleaseAll { id: EventId(12) }, |action| {
            actions.push(action)
        });
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(actions.len(), 1, "the held shift should have been released");
        assert!(state.nothing_held());

        // And now the retransmission of the press that was lost.
        actions.clear();
        let outcome = state.apply(
            &Message::Key {
                id: EventId(11),
                scancode: 0x11,
                down: true,
                extended: false,
            },
            |action| actions.push(action),
        );
        assert_eq!(
            outcome,
            Outcome::Superseded,
            "a press from before the release must not be applied"
        );
        assert!(
            actions.is_empty(),
            "nothing may be injected for it: {actions:?}"
        );
        assert!(
            state.nothing_held(),
            "the release must hold: {:?}",
            state.held_keys()
        );
        assert!(
            outcome.owes_ack(),
            "it must still be acknowledged or the client retransmits forever"
        );
    }

    #[test]
    fn a_superseded_event_is_not_reported_as_a_hole() {
        // Found by a gate whose two criteria fought each other: refusing late
        // pre-barrier events and abandoning none cannot both hold if the
        // refusal leaves the id out of the acknowledgement. The client is
        // entitled to conclude from an unmarked id that the host never saw it.
        let mut state = HostState::new(SESSION);
        for id in 1..=5u64 {
            state.apply(
                &Message::Key {
                    id: EventId(id),
                    scancode: 0x11,
                    down: id % 2 == 1,
                    extended: false,
                },
                |_| {},
            );
        }
        state.apply(&Message::ReleaseAll { id: EventId(9) }, |_| {});
        // Id 7 precedes the barrier and never arrived until now.
        let outcome = state.apply(
            &Message::Key {
                id: EventId(7),
                scancode: 0x11,
                down: true,
                extended: false,
            },
            |_| {},
        );
        assert_eq!(outcome, Outcome::Superseded);
        let ack = state.acknowledgement().expect("something was applied");
        let below = ack.top.0 - 7;
        assert!(
            (1..=32).contains(&below),
            "id 7 should be inside the window below top {}",
            ack.top.0
        );
        assert_eq!(
            ack.missing & (1 << (below - 1)),
            0,
            "a decided event must not be advertised as missing: top {} missing {:#034b}",
            ack.top.0,
            ack.missing
        );
    }

    #[test]
    fn an_event_after_a_release_is_applied_normally() {
        // The barrier must not become a wall. A press the user makes after
        // recapturing carries a later id and has to work.
        let mut state = HostState::new(SESSION);
        state.apply(&Message::ReleaseAll { id: EventId(12) }, |_| {});
        let mut actions = Vec::new();
        let outcome = state.apply(
            &Message::Key {
                id: EventId(13),
                scancode: 0x11,
                down: true,
                extended: false,
            },
            |action| actions.push(action),
        );
        assert_eq!(outcome, Outcome::Applied);
        assert_eq!(actions.len(), 1);
        assert!(state.holds_key(0x11, false));
    }

    #[test]
    fn ten_release_alls_end_empty_and_only_the_first_says_anything() {
        let mut state = HostState::new(SESSION);
        hold_three(&mut state);

        for (attempt, id) in (100..110).enumerate() {
            let (outcome, actions) = apply(&mut state, &Message::ReleaseAll { id: EventId(id) });
            assert_eq!(outcome, Outcome::Applied, "attempt {attempt}");
            assert!(
                state.nothing_held(),
                "attempt {attempt} left something held"
            );
            let expected = if attempt == 0 { 3 } else { 0 };
            assert_eq!(
                actions.len(),
                expected,
                "attempt {attempt} emitted {actions:?}"
            );
        }
        assert_eq!(state.releases().requested, 10);
        assert_eq!(state.releases().expired, 0);
    }

    #[test]
    fn release_all_releases_each_held_thing_once_and_nothing_else() {
        let mut state = HostState::new(SESSION);
        hold_three(&mut state);

        let (_, actions) = apply(&mut state, &Message::ReleaseAll { id: EventId(4) });
        assert_eq!(
            actions,
            vec![
                Action::Key {
                    make: 0x11,
                    extended: false,
                    down: false
                },
                Action::Key {
                    make: 0x2A,
                    extended: false,
                    down: false
                },
                Action::Button {
                    button: Button::Left,
                    down: false
                },
            ]
        );
        assert!(state.nothing_held());
    }

    /// The wheel is reliable and deduplicated like a button, but it holds
    /// nothing, so the only evidence that a retransmission was discarded is
    /// the absence of a second notch.
    #[test]
    fn a_retransmitted_wheel_moves_by_one_notch() {
        let mut state = HostState::new(SESSION);
        let notch = Message::Wheel {
            id: EventId(1),
            dx: 0,
            dy: 1,
        };

        let (_, actions) = apply(&mut state, &notch);
        assert_eq!(
            actions,
            vec![Action::Wheel {
                axis: WheelAxis::Vertical,
                detents: 1
            }]
        );

        for _ in 0..2 {
            let (outcome, actions) = apply(&mut state, &notch);
            assert_eq!(outcome, Outcome::Duplicate);
            assert!(actions.is_empty(), "a third of a notch each: {actions:?}");
        }
        assert!(state.nothing_held(), "a wheel is never held");
    }

    #[test]
    fn a_datagram_from_an_old_session_is_inert() {
        let mut state = HostState::new(SESSION);
        hold_three(&mut state);
        let held_keys = state.held_keys();
        let held_buttons = state.held_buttons();

        // The stale client says goodbye. Its goodbye is not about this user's
        // hands.
        let (outcome, actions) = deliver(
            &mut state,
            OLD_SESSION,
            Message::ReleaseAll { id: EventId(50) },
        );
        assert_eq!(outcome, Outcome::WrongSession);
        assert!(actions.is_empty(), "{actions:?}");
        assert!(!outcome.owes_ack(), "answering a session that has ended");
        assert_eq!(state.held_keys(), held_keys);
        assert_eq!(state.held_buttons(), held_buttons);
        assert_eq!(state.releases(), Releases::default());

        // And it cannot press anything either.
        let (outcome, actions) =
            deliver(&mut state, OLD_SESSION, key(51, LEFT_CONTROL, true, false));
        assert_eq!(outcome, Outcome::WrongSession);
        assert!(actions.is_empty(), "{actions:?}");
        assert!(!state.holds_key(LEFT_CONTROL, false));

        // The current session is unaffected by any of it, including the
        // deduplication window: id 51 was never applied.
        let (outcome, _) = deliver(&mut state, SESSION, key(51, LEFT_CONTROL, true, false));
        assert_eq!(outcome, Outcome::Applied);
        assert!(state.holds_key(LEFT_CONTROL, false));
    }

    #[test]
    fn a_new_session_starts_empty() {
        let mut state = HostState::new(OLD_SESSION);
        hold_three(&mut state);
        // A snapshot under the old session, so the generation counter has a
        // value to be dragged across.
        let agreeing = Message::Snapshot {
            generation: 9,
            keys: state.held_keys(),
            buttons: state.held_buttons(),
        };
        apply(&mut state, &agreeing);

        let mut actions = Vec::new();
        state.begin_session(SESSION, |action| actions.push(action));
        assert_eq!(state.session(), SESSION);
        assert!(state.nothing_held(), "the new client pressed nothing");
        assert_eq!(
            actions.len(),
            3,
            "the OS still believed the old keys were down: {actions:?}"
        );
        assert_eq!(state.releases().session_change, 1);

        // The new client's counters are its own: an id and a generation the
        // old session already used must not be mistaken for stale.
        let (outcome, _) = deliver(&mut state, SESSION, key(1, W, true, false));
        assert_eq!(outcome, Outcome::Applied);
        let (outcome, _) = deliver(
            &mut state,
            SESSION,
            Message::Snapshot {
                generation: 1,
                keys: KeyBitset::EMPTY,
                buttons: 0,
            },
        );
        assert_eq!(outcome, Outcome::Applied);
        assert!(state.nothing_held());

        // Beginning the session already in progress is not a session change,
        // so it releases nothing.
        hold_three(&mut state);
        let mut actions = Vec::new();
        state.begin_session(SESSION, |action| actions.push(action));
        assert!(actions.is_empty(), "{actions:?}");
        assert!(!state.nothing_held());
        assert_eq!(state.releases().session_change, 1);
    }

    #[test]
    fn expiry_releases_everything_held() {
        let mut state = HostState::new(SESSION);
        hold_three(&mut state);

        let actions = expire(&mut state);
        assert_eq!(actions.len(), 3, "{actions:?}");
        assert!(
            actions.iter().all(|action| matches!(
                action,
                Action::Key { down: false, .. } | Action::Button { down: false, .. }
            )),
            "a vanished client may only cause releases: {actions:?}"
        );
        assert!(state.nothing_held());
        assert_eq!(
            state.releases(),
            Releases {
                requested: 0,
                expired: 1,
                session_change: 0
            },
            "a client that vanished must not read as one that said goodbye"
        );
    }

    #[test]
    fn expiry_with_nothing_held_emits_nothing() {
        let mut state = HostState::new(SESSION);
        assert!(expire(&mut state).is_empty());
        assert!(expire(&mut state).is_empty());
        assert!(state.nothing_held());
        // Still counted: the host decided twice that the client was gone, and
        // that is what the count is about, not how much it had to let go of.
        assert_eq!(state.releases().expired, 2);
    }
}
