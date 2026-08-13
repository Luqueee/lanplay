//! Relative mouse motion and physical key presses on macOS, for a pointer and
//! a keyboard that live on another machine.
//!
//! The one thing this crate exists to get right is that movement is reported
//! as a delta and never as a position. A remote pointer is driven by how far
//! the mouse moved, and the moment the cursor on this machine reaches a screen
//! edge its position stops changing while the mouse keeps moving. Differencing
//! absolute positions would therefore report zero for exactly the input a game
//! cares about most, which is why nothing here reads a cursor location.
//!
//! Two pieces, because they fail differently. [`residue`] is arithmetic and is
//! tested as arithmetic: AppKit reports fractional deltas and a naive cast to
//! an integer throws away every slow movement. [`cursor`] and [`mouse`] talk
//! to the window server, and their hazard is a cursor left detached from the
//! mouse after a crash, which looks to the user like a broken machine rather
//! than a broken program.
//!
//! The keyboard has a hazard of its own, and it is the mirror of that one: a key
//! reported as pressed and never as released is held down on the host until
//! something else says otherwise. [`scancode`] is a table and is tested as a
//! table, since a wrong entry presses a key the player did not touch, and
//! [`keyboard`] derives modifier presses from a set it keeps rather than from
//! the flags AppKit reports, because those flags cannot tell the two shifts
//! apart.
//!
//! Keys travel as set 1 scan codes, the PC XT set, so the host reproduces the
//! key that was pressed rather than the character this machine's layout would
//! have produced. That choice belongs here rather than in the wire format
//! because it is what makes the capture layout-independent.
//!
//! A third piece, [`reliable`], is neither of those: it is what makes a lost
//! key release survive at all. It reads no clock and owns no socket, so the
//! retransmission ladder and the snapshot cadence are tested by advancing a
//! number rather than by sleeping through them.
//!
//! Rejected: `CGEventTap`, which sees more but asks the user for accessibility
//! permission and can be disabled by the system for being slow, and
//! `IOHIDManager`, which reports per-device counts in device units and would
//! make this crate responsible for acceleration curves the OS already applies.
//! Rejected too: any queue between the event and the caller. The callback runs
//! on the event path so that a lost datagram cannot turn into a backlog of
//! pending motion.
//!
//! ```no_run
//! use lanplay_input_capture::{Capture, Keyboard};
//!
//! let mut capture = Capture::start(|dx, dy, _at| println!("{dx} {dy}")).unwrap();
//! let mut keys = Keyboard::start(|key| println!("{:#04X} {}", key.scan.code, key.down)).unwrap();
//! // ... an AppKit run loop turns ...
//! keys.release();
//! capture.release().unwrap();
//! ```

pub mod reliable;
pub mod residue;
pub mod scancode;

#[cfg(target_os = "macos")]
pub mod cursor;
#[cfg(target_os = "macos")]
pub mod keyboard;
#[cfg(target_os = "macos")]
pub mod mouse;

pub use reliable::Reliable;
pub use residue::Residue;
pub use scancode::ScanCode;

#[cfg(target_os = "macos")]
pub use cursor::{AssociateFailed, CursorLink};
#[cfg(target_os = "macos")]
pub use keyboard::{KEY_MASK, KeyEvent, Keyboard, Modifiers, MonitorRefused, Transition};
#[cfg(target_os = "macos")]
pub use mouse::{Capture, CaptureError, MOTION_MASK};

/// The UDP port the whole project reserves for input, as opposed to 5004 for
/// media and 5005 for control. Here rather than in the probe because the port
/// is a property of the protocol, not of one binary.
pub const INPUT_PORT: u16 = 5006;
