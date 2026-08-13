//! Whether an event came from a hand or from a program.
//!
//! A capture has no use for this. It forwards what the mouse did, and an event
//! posted by a test generator deserves exactly the same treatment as one from a
//! device under a palm: filtering by origin would build a remote session that
//! stops working the moment the player uses an assistive tool or a macro pad.
//!
//! A measurement cannot do without it. Every synthetic run in this project
//! compares what a generator posted against what the host injected, and that
//! comparison is only sound when both sides describe the same events. A hand
//! resting on the trackpad for a moment adds real movement that no generator
//! posted, and the cross-check then reports a discrepancy in a pipeline that
//! carried every event it was given perfectly. One such run already produced a
//! total the pipeline could not have caused by any loss, which is what this
//! module exists to explain rather than to argue about.
//!
//! `kCGEventSourceUnixProcessID` is the identifier of the process that posted an
//! event, and zero when it came from hardware. That makes the distinction
//! answerable from the event itself rather than from an assumption about who
//! was in the room.

/// Where an event came from.
///
/// The process id is kept rather than reduced to a flag because a run has more
/// than one program posting events, and knowing that the movement came from the
/// generator instead of from some other automation is the whole point of asking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// A device. Something moved in the physical world.
    Device,
    /// Posted by the process with this identifier.
    Posted { by: i64 },
}

impl Origin {
    /// Whether this came from a program rather than from a device.
    pub fn is_posted(self) -> bool {
        matches!(self, Origin::Posted { .. })
    }
}

/// Reading the field needs AppKit; the accounting above does not, and keeping
/// them apart is what lets the accounting be tested on either machine.
#[cfg(target_os = "macos")]
impl Origin {
    /// Reads the origin of an event.
    ///
    /// An event with no `CGEvent` behind it is reported as a device: AppKit
    /// synthesises a few of its own, none of them mouse movement, and calling a
    /// physical origin the default keeps an unknown from being counted as a
    /// generator's work and quietly balancing a cross-check.
    pub fn of(event: &objc2_app_kit::NSEvent) -> Origin {
        use objc2_core_graphics::{CGEvent, CGEventField};

        let Some(event) = event.CGEvent() else {
            return Origin::Device;
        };
        match CGEvent::integer_value_field(Some(&event), CGEventField::EventSourceUnixProcessID) {
            0 => Origin::Device,
            by => Origin::Posted { by },
        }
    }
}

/// Movement accumulated from one kind of origin.
///
/// The sums are what a cross-check compares, and the count is what tells a run
/// that reported no movement from a silent one that reported none because
/// nothing arrived.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Movement {
    /// Events seen.
    pub events: u64,
    /// How many of them arrived with a button held down.
    ///
    /// AppKit reports movement with a button held as a drag rather than a move,
    /// and the difference separates a hand on the mouse from a button that was
    /// pressed and never released. Both look like unexplained movement in a
    /// total and they call for opposite investigations.
    pub dragged: u64,
    /// Their total horizontal movement, in whole pixels as sent.
    pub dx: i64,
    /// Their total vertical movement.
    pub dy: i64,
}

impl Movement {
    /// Records one event's contribution.
    pub fn record(&mut self, dx: i32, dy: i32, dragged: bool) {
        self.events += 1;
        self.dragged += u64::from(dragged);
        self.dx += i64::from(dx);
        self.dy += i64::from(dy);
    }
}

/// Motion split by where it came from.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Origins {
    /// Movement a program posted.
    pub posted: Movement,
    /// Movement a device produced.
    pub device: Movement,
}

impl Origins {
    /// Records one event against the half its origin names.
    pub fn record(&mut self, origin: Origin, dx: i32, dy: i32, dragged: bool) {
        if origin.is_posted() {
            self.posted.record(dx, dy, dragged);
        } else {
            self.device.record(dx, dy, dragged);
        }
    }

    /// Whether a device contributed any movement at all.
    ///
    /// A synthetic run wants this to be false, and wants to say so rather than
    /// to have the fact inferred from two totals that happen to match.
    pub fn device_intruded(&self) -> bool {
        self.device.events > 0
    }
}

#[cfg(test)]
mod tests {
    use super::{Movement, Origin, Origins};

    /// The two halves never mix, which is the only property the accounting has.
    #[test]
    fn each_origin_keeps_its_own_total() {
        let mut origins = Origins::default();
        origins.record(Origin::Posted { by: 42 }, 6, 0, false);
        origins.record(Origin::Posted { by: 42 }, -6, 0, false);
        origins.record(Origin::Device, 100, -200, false);

        assert_eq!(
            origins.posted,
            Movement {
                events: 2,
                dragged: 0,
                dx: 0,
                dy: 0
            }
        );
        assert_eq!(
            origins.device,
            Movement {
                events: 1,
                dragged: 0,
                dx: 100,
                dy: -200
            }
        );
    }

    /// A hand that moved and returned still counts as having been there. The
    /// intrusion is the event, not the displacement, because a generator's total
    /// can be balanced by a device that ended where it started.
    #[test]
    fn a_device_that_returned_to_where_it_started_still_intruded() {
        let mut origins = Origins::default();
        origins.record(Origin::Device, 40, 40, false);
        origins.record(Origin::Device, -40, -40, false);

        assert_eq!(origins.device.dx, 0);
        assert_eq!(origins.device.dy, 0);
        assert!(origins.device_intruded());
    }

    /// Nothing at all is not an intrusion, which is the case a synthetic run is
    /// trying to establish.
    #[test]
    fn a_run_nothing_touched_reports_no_intrusion() {
        let mut origins = Origins::default();
        origins.record(Origin::Posted { by: 7 }, 1, 1, false);

        assert!(!origins.device_intruded());
    }

    /// A drag is counted apart from a move, because a button nobody released and
    /// a hand on the mouse both show up as movement and need opposite fixes.
    #[test]
    fn a_drag_is_counted_apart_from_a_move() {
        let mut origins = Origins::default();
        origins.record(Origin::Device, 1, 0, false);
        origins.record(Origin::Device, 1, 0, true);
        origins.record(Origin::Device, 1, 0, true);

        assert_eq!(origins.device.events, 3);
        assert_eq!(origins.device.dragged, 2);
    }

    /// Any non-zero identifier is a program, and zero is the only value that
    /// means hardware.
    #[test]
    fn only_a_zero_process_id_means_a_device() {
        assert!(Origin::Posted { by: 1 }.is_posted());
        assert!(Origin::Posted { by: 99999 }.is_posted());
        assert!(!Origin::Device.is_posted());
    }
}
