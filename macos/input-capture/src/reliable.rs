//! Keeping the messages that may not be lost alive until the host admits it
//! has them.
//!
//! The wire format divides input into what may be dropped and what may not,
//! but a format cannot retransmit anything. Without something here, a single
//! lost key release leaves a key held down on the host for as long as the
//! session lasts, which is the exact failure the split into two classes
//! exists to prevent. So this is where a reliable message is remembered after
//! it has been sent, offered again when its deadline passes, and forgotten
//! when an acknowledgement covers it.
//!
//! Nothing here opens a socket or reads a clock. The caller passes the time in
//! and sends what it is handed back, which is what makes every deadline in
//! this module testable by advancing a number instead of by sleeping: a test
//! for the five-attempt ladder that slept would take most of a second and
//! would still only prove that the machine it ran on was not busy.
//!
//! Acknowledgement is consumed in the form the host sends: the highest id it
//! has applied, plus a bit for each of the thirty-two ids below that one it has
//! not, so a single datagram retires a burst and names the holes in it.
//! Anchoring at the top rather than at a cumulative frontier is what keeps one
//! permanently lost event from stalling every event after it. A frontier stops
//! at the hole, and with only thirty-two ids of reach above it nothing further
//! up can ever be retired however many times it arrives; measured under five
//! per cent loss that abandoned almost every event a run sent. Anchored at the
//! top, a hole delays only itself.
//!
//! An id that drops below that window without ever having been shown applied
//! has outlived the ladder here, so it is counted as abandoned rather than
//! quietly retired. Calling it acknowledged would claim the host applied input
//! it may never have seen, and the count of events the snapshots have to repair
//! is the figure a fault-injection run is read on.
//!
//! This also holds what a snapshot describes, because the two cannot be kept
//! apart: the snapshot is the repair of last resort for anything the ladder
//! above abandoned, and it is only a repair if it says what the client
//! believes is held at the moment it is sent. The generation rises whenever
//! that belief changes, which is what lets the host throw away a snapshot that
//! overtook a newer one instead of resurrecting a key from it.

use lanplay_input_protocol::{Button, EventId, KeyBitset, Message, Reliability};
use lanplay_telemetry::{Nanos, Timestamp};

use crate::ScanCode;

/// How long the first retransmission waits. Every figure in this block is a
/// starting value chosen so that a fault-injection run has something to move,
/// not a tuned number: none of them has yet been measured against a link that
/// drops datagrams, and the harness that will do the measuring is what should
/// change them.
pub const FIRST_BACKOFF: Nanos = Nanos::from_millis(20);

/// Ceiling on the doubling. Past this the wait is longer than any plausible
/// round trip on a local network, so doubling further would only delay the
/// repair without reducing the load that caused the loss.
pub const MAX_BACKOFF: Nanos = Nanos::from_millis(100);

/// Retransmissions before an event is given up on.
///
/// Abandoning rather than retrying forever is deliberate. A client that keeps
/// hammering a link that is not carrying its datagrams helps nobody, and the
/// periodic snapshot repairs the held state without needing this event, so the
/// only thing lost by giving up is a key press that the player has long since
/// finished making.
pub const MAX_RETRANSMISSIONS: u32 = 5;

/// Snapshot cadence while anything is held or anything is unacknowledged.
pub const SNAPSHOT_BUSY: Nanos = Nanos::from_millis(50);

/// Snapshot cadence when nothing is held and nothing is outstanding. Slower
/// because with an empty held set a snapshot only tells the host something it
/// already believes, and the reason to keep sending them at all is that the
/// host cannot tell an idle client from a departed one.
pub const SNAPSHOT_IDLE: Nanos = Nanos::from_millis(500);

/// How many ids below the acknowledged top the `missing` bits can speak about.
/// An id that falls past this is beyond anything a retransmission can fix,
/// because the host can no longer say whether it arrived.
const WINDOW_WIDTH: u64 = 32;

/// The six cumulative figures a run reports. Separate from the live
/// [`Reliable::unacked`] count, which is not a total and is the figure a
/// fault-injection run is actually judged on.
///
/// Every reliable event sent ends in exactly one of three places, so
/// `reliable_sent` must equal `acknowledged + abandoned` plus whatever is still
/// outstanding. A run whose figures do not close that way has misrouted an
/// outcome, and a report that gave only the abandoned figure would leave a
/// reader to assume the remainder was fine.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Counts {
    pub reliable_sent: u64,
    pub retransmissions: u64,
    /// Events an acknowledgement showed the host had applied.
    pub acknowledged: u64,
    /// Events given up on: out of retransmissions, or fallen below the window
    /// without ever having been shown applied. Calling either acknowledged
    /// would claim the host applied input it may never have seen.
    pub abandoned: u64,
    /// Acknowledgement datagrams received, which is a count of what the host
    /// sent rather than of events and does not enter the identity above.
    pub acks: u64,
    pub snapshots: u64,
}

/// One reliable message that has been sent and not yet acknowledged.
#[derive(Clone, Copy)]
struct Pending {
    id: EventId,
    message: Message,
    /// Retransmissions so far, not counting the original send.
    attempts: u32,
    /// Wait that produced the current deadline, doubled on each attempt.
    backoff: Nanos,
    due: Timestamp,
}

/// The client's reliable send state: the id counter, what is outstanding, and
/// what a snapshot would say.
///
/// One per session. Two of these sharing a session would mint the same id for
/// two different events, and the host deduplicates on that id, so the second
/// key press of a pair would be discarded as a retransmission of the first.
pub struct Reliable {
    next_id: EventId,
    pending: Vec<Pending>,
    keys: KeyBitset,
    buttons: u8,
    generation: u32,
    last_snapshot: Timestamp,
    counts: Counts,
}

impl Reliable {
    pub fn new(now: Timestamp) -> Reliable {
        Reliable {
            next_id: EventId(0),
            pending: Vec::new(),
            keys: KeyBitset::EMPTY,
            buttons: 0,
            generation: 0,
            last_snapshot: now,
            counts: Counts::default(),
        }
    }

    /// Mints an id, folds the key into the held set and returns the message to
    /// send. The send itself is the caller's, so this can be driven from an
    /// event callback without a socket anywhere near it.
    pub fn key(&mut self, scan: ScanCode, down: bool, now: Timestamp) -> Message {
        let slot = key_slot(scan);
        if self.keys.contains(slot) != down {
            self.keys.set(slot, down);
            self.bump();
        }
        self.track(now, |id| Message::Key {
            id,
            scancode: u16::from(scan.code),
            down,
            extended: scan.extended,
        })
    }

    pub fn button(&mut self, button: Button, down: bool, now: Timestamp) -> Message {
        let held = self.buttons & button.mask() != 0;
        if held != down {
            if down {
                self.buttons |= button.mask();
            } else {
                self.buttons &= !button.mask();
            }
            self.bump();
        }
        self.track(now, |id| Message::Button { id, button, down })
    }

    /// A wheel detent changes nothing about what is held, so it leaves the
    /// generation alone: a snapshot cannot describe a detent and reordering one
    /// against a snapshot has nothing to decide.
    pub fn wheel(&mut self, dx: i16, dy: i16, now: Timestamp) -> Message {
        self.track(now, |id| Message::Wheel { id, dx, dy })
    }

    /// Drops everything from the held set and returns the message that tells
    /// the host to do the same. Reliable like any other event, because it is
    /// the one message whose loss is the failure the rest of this exists to
    /// avoid.
    pub fn release_all(&mut self, now: Timestamp) -> Message {
        if !self.keys.is_empty() || self.buttons != 0 {
            self.keys = KeyBitset::EMPTY;
            self.buttons = 0;
            self.bump();
        }
        self.track(now, |id| Message::ReleaseAll { id })
    }

    /// The next message whose deadline has passed, or `None` when nothing is
    /// due. Called in a loop, so a caller that was late sends everything that
    /// fell due while it was away rather than one per turn.
    ///
    /// An event that has used its attempts is dropped here rather than at the
    /// moment of its last retransmission, so that the fifth attempt is given
    /// the same window to be acknowledged as the four before it.
    pub fn next_due(&mut self, now: Timestamp) -> Option<Message> {
        while let Some(index) = self.pending.iter().position(|entry| entry.due <= now) {
            if self.pending[index].attempts >= MAX_RETRANSMISSIONS {
                self.pending.remove(index);
                self.counts.abandoned += 1;
                continue;
            }
            let entry = &mut self.pending[index];
            entry.attempts += 1;
            entry.backoff = Nanos(entry.backoff.get().saturating_mul(2).min(MAX_BACKOFF.get()));
            // Measured from now rather than from the deadline that just
            // passed, because the wait is meant to cover a round trip from
            // this send, and compounding a late poll would shorten the next
            // one to nothing.
            entry.due = now.add(entry.backoff);
            self.counts.retransmissions += 1;
            return Some(entry.message);
        }
        None
    }

    /// Retires everything this acknowledgement shows the host has applied, and
    /// gives up on whatever it has carried out of reach.
    ///
    /// An acknowledgement for something already retired is not an error and is
    /// not ignored either: the host acknowledges duplicates, so this arrives
    /// routinely and simply finds nothing left to retire.
    pub fn ack(&mut self, top: EventId, missing: u32) {
        self.counts.acks += 1;
        // Split borrows because the verdict on one entry moves a figure the
        // whole struct owns.
        let counts = &mut self.counts;
        self.pending
            .retain(|entry| match verdict(top, missing, entry.id) {
                Verdict::Applied => {
                    counts.acknowledged += 1;
                    false
                }
                Verdict::Outstanding => true,
                Verdict::OutOfReach => {
                    counts.abandoned += 1;
                    false
                }
            });
    }

    /// The snapshot to send if one is due. The cadence tightens whenever
    /// something is held or outstanding, which is evaluated here rather than
    /// scheduled ahead so that a key pressed a millisecond ago does not have
    /// to wait out an idle interval that was already running.
    pub fn snapshot_due(&mut self, now: Timestamp) -> Option<Message> {
        if now.saturating_since(self.last_snapshot) < self.snapshot_interval() {
            return None;
        }
        self.last_snapshot = now;
        self.counts.snapshots += 1;
        Some(Message::Snapshot {
            generation: self.generation,
            keys: self.keys,
            buttons: self.buttons,
        })
    }

    fn snapshot_interval(&self) -> Nanos {
        if self.keys.is_empty() && self.buttons == 0 && self.pending.is_empty() {
            SNAPSHOT_IDLE
        } else {
            SNAPSHOT_BUSY
        }
    }

    /// Reliable events sent and not yet acknowledged. The figure a
    /// fault-injection run turns on: anything other than zero at exit is an
    /// event the host may never have applied.
    pub fn unacked(&self) -> usize {
        self.pending.len()
    }

    pub fn counts(&self) -> Counts {
        self.counts
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn keys(&self) -> KeyBitset {
        self.keys
    }

    pub fn buttons(&self) -> u8 {
        self.buttons
    }

    /// Whether the client believes this key is down, by the same fold the host
    /// applies, so a caller counting its own presses and this module agree
    /// about which slot a key occupies.
    pub fn holds_key(&self, scan: ScanCode) -> bool {
        self.keys.contains(key_slot(scan))
    }

    fn bump(&mut self) {
        // Wrapping, because the host compares generations for recency and a
        // session long enough to overflow this has bigger problems than one
        // misordered snapshot.
        self.generation = self.generation.wrapping_add(1);
    }

    fn track(&mut self, now: Timestamp, build: impl FnOnce(EventId) -> Message) -> Message {
        let id = self.next_id;
        self.next_id = id.next();
        let message = build(id);
        debug_assert_eq!(
            message.reliability(),
            Reliability::Reliable,
            "only reliable messages belong in the retransmission set"
        );
        self.pending.push(Pending {
            id,
            message,
            attempts: 0,
            backoff: FIRST_BACKOFF,
            due: now.add(FIRST_BACKOFF),
        });
        self.counts.reliable_sent += 1;
        message
    }
}

/// Folds a key into the byte the snapshot bitset addresses, which is the fold
/// the host applies too. The 0xE0 prefix is a bit of the index rather than part
/// of the code, so an arrow and a numpad digit do not share a slot.
pub fn key_slot(scan: ScanCode) -> u8 {
    (scan.code & 0x7F) | if scan.extended { 0x80 } else { 0 }
}

/// What one acknowledgement says about one outstanding id.
enum Verdict {
    /// The host has applied it, so it is finished with.
    Applied,
    /// Not applied and still worth sending: either above the acknowledged top
    /// and therefore in flight, or named by a `missing` bit and therefore on
    /// the ladder.
    Outstanding,
    /// Below the window, and this is reached only for an id no acknowledgement
    /// ever showed applied, because one that had would already be retired. The
    /// host can no longer say anything about it, so no further retransmission
    /// can settle it.
    OutOfReach,
}

fn verdict(top: EventId, missing: u32, id: EventId) -> Verdict {
    let Some(below) = top.0.checked_sub(id.0) else {
        return Verdict::Outstanding;
    };
    // The top is applied by definition, which is why the bits start one below
    // it: bit `i` speaks about `top - 1 - i`.
    if below == 0 {
        return Verdict::Applied;
    }
    if below > WINDOW_WIDTH {
        return Verdict::OutOfReach;
    }
    if missing & (1 << (below - 1)) != 0 {
        Verdict::Outstanding
    } else {
        Verdict::Applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: ScanCode = ScanCode {
        code: 0x11,
        extended: false,
    };
    const A: ScanCode = ScanCode {
        code: 0x1E,
        extended: false,
    };

    /// The fake clock: a number of milliseconds since an arbitrary origin.
    fn at(millis: u64) -> Timestamp {
        Timestamp::from_nanos(millis * 1_000_000)
    }

    fn ids(messages: &[Message]) -> Vec<u64> {
        messages
            .iter()
            .map(|message| message.event_id().expect("reliable messages carry an id").0)
            .collect()
    }

    /// Everything due at this instant, in the order it would be sent.
    fn drain(reliable: &mut Reliable, now: Timestamp) -> Vec<Message> {
        let mut out = Vec::new();
        while let Some(message) = reliable.next_due(now) {
            out.push(message);
        }
        out
    }

    /// The ids still on the ladder, in the order they were sent.
    fn outstanding(reliable: &Reliable) -> Vec<u64> {
        reliable.pending.iter().map(|entry| entry.id.0).collect()
    }

    /// A millisecond sweep rather than a jump to each expected deadline: the
    /// question is not only that a retransmission happens by then but that
    /// nothing happens before, which is what a caller polling every
    /// millisecond would see.
    #[test]
    fn an_unacknowledged_event_walks_the_ladder_and_is_then_abandoned() {
        let mut reliable = Reliable::new(at(0));
        reliable.key(W, true, at(0));

        let mut retransmitted_at = Vec::new();
        let mut abandoned_at = None;
        for millis in 1..=1_000 {
            let before = reliable.counts().abandoned;
            if !drain(&mut reliable, at(millis)).is_empty() {
                retransmitted_at.push(millis);
            }
            if reliable.counts().abandoned > before {
                abandoned_at = Some(millis);
            }
        }

        // Twenty, then forty, eighty and the cap twice over.
        assert_eq!(retransmitted_at, vec![20, 60, 140, 240, 340]);
        assert_eq!(reliable.counts().retransmissions, 5);
        assert_eq!(abandoned_at, Some(440));
        assert_eq!(reliable.counts().abandoned, 1);
        assert_eq!(reliable.unacked(), 0);
    }

    #[test]
    fn an_acknowledgement_above_a_pending_id_with_its_bit_clear_retires_it() {
        let mut reliable = Reliable::new(at(0));
        for _ in 0..4 {
            reliable.key(W, true, at(0));
        }
        assert_eq!(reliable.unacked(), 4);

        // A top of 2 with nothing missing below it, so 0, 1 and 2 are applied
        // and only 3 is still in flight.
        reliable.ack(EventId(2), 0);
        assert_eq!(outstanding(&reliable), vec![3]);
        assert_eq!(reliable.counts().acknowledged, 3);
        assert_eq!(reliable.counts().abandoned, 0);
        assert_eq!(reliable.counts().acks, 1);
    }

    #[test]
    fn a_missing_bit_keeps_an_event_on_the_ladder_until_a_later_acknowledgement_clears_it() {
        let mut reliable = Reliable::new(at(0));
        for _ in 0..6 {
            reliable.key(W, true, at(0));
        }

        // Bit 1 under a top of 5 is id 3, the one hole in a run the host has
        // otherwise applied. Retiring 4 and 5 rather than waiting for the hole
        // below them to be filled is the point of the whole arrangement: that
        // hole may be a message the host will never receive.
        reliable.ack(EventId(5), 0b10);
        assert_eq!(outstanding(&reliable), vec![3]);
        assert_eq!(reliable.counts().acknowledged, 5);

        // The hole is still retransmitted, and only the hole.
        assert_eq!(ids(&drain(&mut reliable, at(20))), &[3]);

        // That retransmission arrived, so the same top now names nothing.
        reliable.ack(EventId(5), 0);
        assert_eq!(reliable.unacked(), 0);
        assert_eq!(reliable.counts().acknowledged, 6);
        assert_eq!(reliable.counts().abandoned, 0);
    }

    #[test]
    fn an_id_above_the_top_is_untouched() {
        let mut reliable = Reliable::new(at(0));
        for _ in 0..3 {
            reliable.key(W, true, at(0));
        }
        // Every bit set, and it still says nothing about 1 and 2: the bits run
        // downwards from the top, so an id above it has not been spoken about
        // and is in flight rather than missing.
        reliable.ack(EventId(0), u32::MAX);
        assert_eq!(outstanding(&reliable), vec![1, 2]);
        assert_eq!(reliable.counts().acknowledged, 1);
        assert_eq!(reliable.counts().abandoned, 0);
    }

    #[test]
    fn an_id_carried_below_the_window_unshown_is_abandoned_and_not_acknowledged() {
        let mut reliable = Reliable::new(at(0));
        for _ in 0..40 {
            reliable.key(W, true, at(0));
        }
        // A top of 36 with every bit set holds 4 through 35 on the ladder and
        // carries 0 through 3 out of reach below it. None of those four was
        // ever shown applied, so counting them as acknowledged would claim the
        // host applied input it never saw; the periodic snapshot is the repair.
        reliable.ack(EventId(36), u32::MAX);
        let expected: Vec<u64> = (4..=35).chain([37, 38, 39]).collect();
        assert_eq!(outstanding(&reliable), expected);
        assert_eq!(reliable.counts().abandoned, 4);
        assert_eq!(reliable.counts().acknowledged, 1);
    }

    #[test]
    fn an_acknowledgement_for_a_retired_event_changes_nothing() {
        let mut reliable = Reliable::new(at(0));
        reliable.key(W, true, at(0));
        reliable.key(W, false, at(0));
        reliable.ack(EventId(1), 0);
        assert_eq!(reliable.unacked(), 0);
        let before = reliable.counts();

        // The host acknowledges duplicates, so this is the ordinary case and
        // not a fault: only the count of acknowledgements received moves. The
        // second still names 0 as missing, which is what an acknowledgement
        // sent before a retransmission landed looks like arriving after it.
        reliable.ack(EventId(1), 0);
        reliable.ack(EventId(1), 0b1);
        assert_eq!(reliable.unacked(), 0);
        assert_eq!(reliable.counts().acks, before.acks + 2);
        assert_eq!(reliable.counts().acknowledged, before.acknowledged);
        assert_eq!(reliable.counts().retransmissions, before.retransmissions);
        assert_eq!(reliable.counts().abandoned, before.abandoned);
        assert_eq!(reliable.counts().reliable_sent, before.reliable_sent);
        assert_eq!(ids(&drain(&mut reliable, at(10_000))), &[] as &[u64]);
    }

    /// A host that applies everything the client hands it except the ids the
    /// link swallows, and describes what it has applied the way the wire format
    /// does. Nothing here is a socket: the loss is a predicate on an id, which
    /// is what makes a permanently lost event reproducible.
    struct Host {
        applied: Vec<bool>,
        loses: fn(u64) -> bool,
    }

    impl Host {
        fn new(loses: fn(u64) -> bool) -> Host {
            Host {
                applied: Vec::new(),
                loses,
            }
        }

        fn receive(&mut self, message: Message) {
            let id = message.event_id().expect("reliable messages carry an id").0;
            if (self.loses)(id) {
                return;
            }
            let slot = id as usize;
            if self.applied.len() <= slot {
                self.applied.resize(slot + 1, false);
            }
            self.applied[slot] = true;
        }

        /// The acknowledgement this host would send, or nothing at all until it
        /// has applied something to anchor one on.
        fn ack(&self) -> Option<(EventId, u32)> {
            let top = self.applied.iter().rposition(|applied| *applied)? as u64;
            let mut missing = 0u32;
            for bit in 0..WINDOW_WIDTH {
                let Some(id) = top.checked_sub(bit + 1) else {
                    break;
                };
                if !self.applied[id as usize] {
                    missing |= 1 << bit;
                }
            }
            Some((EventId(top), missing))
        }
    }

    /// The regression the top-anchored acknowledgement exists for. Anchored at
    /// a cumulative frontier instead, one event lost for good stops that
    /// frontier, nothing more than thirty-two ids above it can ever be retired,
    /// and the outstanding set therefore grows for as long as the run lasts.
    /// That is what fault injection measured: at five per cent loss almost
    /// every event a run sent was abandoned.
    #[test]
    fn one_permanently_lost_event_does_not_pile_up_the_ids_after_it() {
        let mut reliable = Reliable::new(at(0));
        let mut host = Host::new(|id| id == 3);
        let mut worst = 0;

        for millis in 1..=500 {
            host.receive(reliable.key(W, millis % 2 == 1, at(millis)));
            for message in drain(&mut reliable, at(millis)) {
                host.receive(message);
            }
            if let Some((top, missing)) = host.ack() {
                reliable.ack(top, missing);
            }
            worst = worst.max(reliable.unacked());
        }

        let counts = reliable.counts();
        assert_eq!(counts.reliable_sent, 500);
        // One outstanding at a time, and that one is the hole itself. A figure
        // that grew with the length of the run would be the frontier stalling.
        assert_eq!(worst, 1);
        assert_eq!(reliable.unacked(), 0);
        assert_eq!(counts.acknowledged, 499);
        assert_eq!(counts.abandoned, 1);
        // Only the hole was ever retransmitted, because every id after it was
        // retired by acknowledgement inside its first deadline.
        assert_eq!(counts.retransmissions, 1);
    }

    /// The figures a run reports have to close, and this is the test that says
    /// so: it cannot pass while any one outcome is misrouted, whereas a report
    /// of the abandoned figure alone leaves a reader to assume the remainder
    /// was fine.
    #[test]
    fn every_reliable_event_sent_ends_acknowledged_abandoned_or_outstanding() {
        let mut reliable = Reliable::new(at(0));
        // Every eleventh event swallowed for good, so the run mixes ids the
        // host applied, an id it is still naming as missing, and ids its
        // acknowledgements have carried out of reach.
        let mut host = Host::new(|id| id % 11 == 0);

        for millis in 1..=300 {
            host.receive(reliable.key(W, millis % 2 == 1, at(millis)));
            for message in drain(&mut reliable, at(millis)) {
                host.receive(message);
            }
            if let Some((top, missing)) = host.ack() {
                reliable.ack(top, missing);
            }
        }

        let counts = reliable.counts();
        assert_eq!(counts.reliable_sent, 300);
        assert_eq!(
            counts.reliable_sent,
            counts.acknowledged + counts.abandoned + reliable.unacked() as u64
        );
        // And all three outcomes actually happened, so the identity above is
        // not holding because one of them stood in for the others.
        assert!(counts.acknowledged > 0);
        assert!(counts.abandoned > 0);
        assert!(reliable.unacked() > 0);
    }

    #[test]
    fn a_snapshot_is_due_every_fifty_milliseconds_while_a_key_is_held() {
        let mut reliable = Reliable::new(at(0));
        reliable.key(W, true, at(0));
        reliable.ack(EventId(0), 0);

        assert!(reliable.snapshot_due(at(49)).is_none());
        let Some(Message::Snapshot { keys, .. }) = reliable.snapshot_due(at(50)) else {
            panic!("a snapshot is due at fifty milliseconds with a key held");
        };
        assert!(keys.contains(key_slot(W)));

        assert!(reliable.snapshot_due(at(99)).is_none());
        assert!(reliable.snapshot_due(at(100)).is_some());
        assert_eq!(reliable.counts().snapshots, 2);
    }

    #[test]
    fn a_snapshot_is_due_every_five_hundred_milliseconds_while_nothing_is_held() {
        let mut reliable = Reliable::new(at(0));
        assert!(reliable.snapshot_due(at(499)).is_none());
        assert!(reliable.snapshot_due(at(500)).is_some());
        assert!(reliable.snapshot_due(at(999)).is_none());
        assert!(reliable.snapshot_due(at(1_000)).is_some());
        assert_eq!(reliable.counts().snapshots, 2);
    }

    #[test]
    fn an_outstanding_event_alone_is_enough_to_tighten_the_cadence() {
        let mut reliable = Reliable::new(at(0));
        // Pressed and released, so nothing is held, but neither has been
        // acknowledged and the snapshot is what will repair them.
        reliable.key(W, true, at(0));
        reliable.key(W, false, at(0));
        assert!(reliable.keys().is_empty());
        assert!(reliable.snapshot_due(at(50)).is_some());
    }

    #[test]
    fn the_cadence_relaxes_once_everything_is_acknowledged_and_released() {
        let mut reliable = Reliable::new(at(0));
        reliable.key(W, true, at(0));
        reliable.key(W, false, at(0));
        reliable.ack(EventId(1), 0);
        assert!(reliable.snapshot_due(at(499)).is_none());
        assert!(reliable.snapshot_due(at(500)).is_some());
    }

    #[test]
    fn the_generation_rises_when_the_held_set_changes_and_not_otherwise() {
        let mut reliable = Reliable::new(at(0));
        assert_eq!(reliable.generation(), 0);

        reliable.key(W, true, at(0));
        assert_eq!(reliable.generation(), 1);

        // A repeat of a key already held describes the same set.
        reliable.key(W, true, at(0));
        assert_eq!(reliable.generation(), 1);

        reliable.key(A, true, at(0));
        assert_eq!(reliable.generation(), 2);

        // A release of a key that was never down, and a detent, change
        // nothing about what is held.
        reliable.key(
            ScanCode {
                code: 0x20,
                extended: false,
            },
            false,
            at(0),
        );
        reliable.wheel(0, 1, at(0));
        assert_eq!(reliable.generation(), 2);

        reliable.key(W, false, at(0));
        assert_eq!(reliable.generation(), 3);

        reliable.button(Button::Left, true, at(0));
        assert_eq!(reliable.generation(), 4);
        reliable.button(Button::Left, true, at(0));
        assert_eq!(reliable.generation(), 4);
        reliable.button(Button::Left, false, at(0));
        assert_eq!(reliable.generation(), 5);

        // A release-all with something held changes the set; a second one has
        // nothing left to change.
        reliable.key(A, false, at(0));
        assert_eq!(reliable.generation(), 6);
        assert!(reliable.keys().is_empty());
        reliable.release_all(at(0));
        assert_eq!(reliable.generation(), 6);

        reliable.key(W, true, at(0));
        assert_eq!(reliable.generation(), 7);
        reliable.release_all(at(0));
        assert_eq!(reliable.generation(), 8);
        assert!(reliable.keys().is_empty());
        assert_eq!(reliable.buttons(), 0);
    }

    /// The line between a button and a detent, which is the one thing a
    /// snapshot cannot be allowed to get wrong. A button is state and belongs
    /// in the mask a snapshot describes; a wheel is an event and nothing is
    /// ever a held wheel, so a detent must move neither the mask nor the
    /// generation that says the mask changed.
    #[test]
    fn a_button_moves_the_held_mask_and_a_wheel_notch_moves_nothing() {
        let mut reliable = Reliable::new(at(0));

        reliable.button(Button::Right, true, at(0));
        assert_eq!(reliable.buttons(), Button::Right.mask());
        assert_eq!(reliable.generation(), 1);

        let generation = reliable.generation();
        let buttons = reliable.buttons();
        for notch in [1i16, -1, 3] {
            let message = reliable.wheel(0, notch, at(0));
            assert!(matches!(message, Message::Wheel { dy, .. } if dy == notch));
        }
        assert_eq!(reliable.buttons(), buttons);
        assert_eq!(reliable.generation(), generation);

        reliable.button(Button::Right, false, at(0));
        assert_eq!(reliable.buttons(), 0);
        assert_eq!(reliable.generation(), 2);

        // And what a snapshot says is the mask itself, so a detent can never
        // reach the host as something it has to hold.
        reliable.button(Button::X2, true, at(0));
        reliable.wheel(0, 1, at(0));
        let Some(Message::Snapshot {
            generation,
            buttons,
            ..
        }) = reliable.snapshot_due(at(50))
        else {
            panic!("a snapshot is due with a button held");
        };
        assert_eq!(buttons, Button::X2.mask());
        assert_eq!(generation, 3);
    }

    /// The safety invariant says a loss of control converges the host to
    /// nothing held, and converging means the second and the tenth release
    /// leave the same state as the first. This is that property on the client's
    /// side: every cause the session has sends its own release, several of them
    /// can land in the same instant, and none of them may resurrect anything or
    /// leave the held set describing something other than empty.
    #[test]
    fn releasing_everything_ten_times_ends_where_releasing_it_once_does() {
        let mut reliable = Reliable::new(at(0));
        reliable.key(W, true, at(0));
        reliable.button(Button::Middle, true, at(0));

        let mut ids = Vec::new();
        for _ in 0..10 {
            let Message::ReleaseAll { id } = reliable.release_all(at(1)) else {
                panic!("release_all returns a release");
            };
            ids.push(id.0);
            assert!(reliable.keys().is_empty());
            assert_eq!(reliable.buttons(), 0);
        }

        // Ten distinct events rather than one repeated: the host deduplicates
        // on the id, so a release that reused one would be discarded as a
        // retransmission of a release the link may have swallowed.
        assert_eq!(ids, (2..12).collect::<Vec<u64>>());
        // And only the first of them described a change, so a snapshot that
        // overtook the rest cannot be mistaken for a newer one.
        assert_eq!(reliable.generation(), 3);
    }

    #[test]
    fn the_extended_flag_is_part_of_the_slot() {
        let mut reliable = Reliable::new(at(0));
        let numpad_enter = ScanCode {
            code: 0x1C,
            extended: true,
        };
        let return_key = ScanCode {
            code: 0x1C,
            extended: false,
        };
        reliable.key(numpad_enter, true, at(0));
        assert!(reliable.holds_key(numpad_enter));
        assert!(!reliable.holds_key(return_key));
    }

    #[test]
    fn every_tracked_message_carries_the_next_id_in_order() {
        let mut reliable = Reliable::new(at(0));
        let messages = vec![
            reliable.key(W, true, at(0)),
            reliable.button(Button::Right, true, at(0)),
            reliable.wheel(0, -1, at(0)),
            reliable.release_all(at(0)),
        ];
        assert_eq!(ids(&messages), vec![0, 1, 2, 3]);
        assert_eq!(reliable.counts().reliable_sent, 4);
        assert_eq!(reliable.unacked(), 4);
    }
}
