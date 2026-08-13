//! Telling the host the session is still there when the player is not touching
//! anything.
//!
//! A heartbeat and a snapshot answer different questions and are kept apart
//! here for that reason alone, not because their cadences differ. A heartbeat
//! says the session is alive; it carries no state and repairs nothing. A
//! snapshot says what the client believes is held, and is the repair of last
//! resort for a release the retransmission ladder gave up on. Folding them into
//! one timer would tie the proof of liveness to the size of the held set, so a
//! client sitting idle with nothing held would prove itself alive four times
//! less often than one in the middle of a firefight, which is the wrong way
//! round: the idle client is the one a host cannot otherwise tell from a
//! departed one.
//!
//! Nothing here reads a clock or opens a socket. The caller passes the time in
//! and sends what it is handed back, so the cadence is tested by advancing a
//! number rather than by sleeping through half a second of it.

use lanplay_input_protocol::Message;
use lanplay_telemetry::{Nanos, Timestamp};

/// How long the host waits before it declares a silent session gone.
///
/// Not a production figure. It is here so that both ends can be read against
/// the same number while the real one is still unmeasured, and two seconds was
/// chosen because it is far above the fifty millisecond stalls already measured
/// on this Wi-Fi: a stall that long is a bad moment on a link, not a departure,
/// and an expiry near it would release the player's keys mid-corner. The
/// harness that measures a real link is what should replace it.
pub const HOST_EXPIRY: Nanos = Nanos::from_millis(2_000);

/// How often a heartbeat leaves.
///
/// Chosen against the expiry above rather than picked for its own sake: at this
/// interval five heartbeats fall inside every expiry window, so losing one
/// leaves the host waiting eight hundred milliseconds out of two thousand and a
/// live session cannot be expired by a single dropped datagram. Halving the
/// expiry later would still leave two of these inside it.
pub const HEARTBEAT_INTERVAL: Nanos = Nanos::from_millis(400);

// The property above is the whole reason for the figure, so it is checked here
// rather than left to whoever edits one of the two constants next.
const _: () = assert!(
    HEARTBEAT_INTERVAL.get() * 2 < HOST_EXPIRY.get(),
    "a single lost heartbeat must not be able to expire a live session"
);

/// The heartbeat timer: when the last one left, and how many have.
///
/// Its own type rather than a field beside the snapshot cadence, because the
/// two must not end up sharing a deadline by accident when somebody later
/// notices how similar their intervals are.
pub struct Heartbeat {
    last: Timestamp,
    sent: u64,
}

impl Heartbeat {
    pub fn new(now: Timestamp) -> Heartbeat {
        Heartbeat { last: now, sent: 0 }
    }

    /// The heartbeat to send if one is due, and nothing otherwise.
    ///
    /// Unconditional: a heartbeat is not suppressed because other traffic has
    /// gone out recently. Deciding that would mean this module knowing what
    /// else the session has sent and when, which is exactly the coupling that
    /// keeps liveness and state repair apart.
    pub fn due(&mut self, now: Timestamp) -> Option<Message> {
        if now.saturating_since(self.last) < HEARTBEAT_INTERVAL {
            return None;
        }
        self.last = now;
        self.sent += 1;
        Some(Message::Heartbeat)
    }

    pub const fn sent(&self) -> u64 {
        self.sent
    }
}

#[cfg(test)]
mod tests {
    use super::{HEARTBEAT_INTERVAL, HOST_EXPIRY, Heartbeat};
    use crate::Reliable;
    use lanplay_input_protocol::Message;
    use lanplay_telemetry::Timestamp;

    /// The fake clock: a number of milliseconds since an arbitrary origin.
    fn at(millis: u64) -> Timestamp {
        Timestamp::from_nanos(millis * 1_000_000)
    }

    #[test]
    fn a_heartbeat_leaves_on_its_interval_and_not_before() {
        let mut heartbeat = Heartbeat::new(at(0));
        assert!(heartbeat.due(at(399)).is_none());
        assert_eq!(heartbeat.due(at(400)), Some(Message::Heartbeat));
        assert!(heartbeat.due(at(799)).is_none());
        assert_eq!(heartbeat.due(at(800)), Some(Message::Heartbeat));
        assert_eq!(heartbeat.sent(), 2);
    }

    /// The two timers are independent, which is the contract this module
    /// exists to keep. Advancing to a heartbeat deadline must produce a
    /// heartbeat and no snapshot, and advancing to a snapshot deadline must
    /// produce a snapshot and no heartbeat: one timer serving both would make
    /// each of these pairs move together.
    #[test]
    fn the_heartbeat_timer_and_the_snapshot_timer_do_not_move_together() {
        let mut heartbeat = Heartbeat::new(at(0));
        let mut reliable = Reliable::new(at(0));

        // Nothing held and nothing outstanding, so the snapshot cadence is the
        // idle five hundred milliseconds and the heartbeat's four hundred
        // falls first.
        assert_eq!(heartbeat.due(at(400)), Some(Message::Heartbeat));
        assert!(reliable.snapshot_due(at(400)).is_none());

        // And the other way round at the snapshot's own deadline, which the
        // heartbeat that just went out has moved past.
        assert!(matches!(
            reliable.snapshot_due(at(500)),
            Some(Message::Snapshot { .. })
        ));
        assert!(heartbeat.due(at(500)).is_none());

        assert_eq!(heartbeat.sent(), 1);
        assert_eq!(reliable.counts().snapshots, 1);
    }

    /// A heartbeat is proof of life and nothing else: it must never be the
    /// thing that repairs held state, which is what a reader would assume if
    /// it carried any.
    #[test]
    fn a_heartbeat_is_unreliable_and_carries_no_state() {
        assert!(Message::Heartbeat.event_id().is_none());
        assert_eq!(
            Message::Heartbeat.reliability(),
            lanplay_input_protocol::Reliability::Unreliable
        );
    }

    /// The margin the interval was chosen for, stated as a test as well as in
    /// the comment, so that changing one of the two constants fails here
    /// rather than in a session that quietly expires.
    #[test]
    fn a_single_lost_heartbeat_cannot_expire_a_live_session() {
        let mut heartbeat = Heartbeat::new(at(0));
        let mut arrived = at(0);
        let mut lost_one = false;
        for millis in 1..=2_000 {
            if heartbeat.due(at(millis)).is_some() {
                // The first one goes missing, and the host hears nothing until
                // the next.
                if lost_one {
                    let silence = at(millis).saturating_since(arrived);
                    assert!(silence < HOST_EXPIRY, "the host waited {silence:?}");
                    arrived = at(millis);
                } else {
                    lost_one = true;
                }
            }
        }
        assert!(lost_one);
        assert_eq!(
            heartbeat.sent(),
            2_000 / (HEARTBEAT_INTERVAL.get() / 1_000_000)
        );
    }
}
