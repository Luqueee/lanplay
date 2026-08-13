//! Handing one [`Action`] to the Windows input system.
//!
//! `SendInput` is the whole backend. It puts events into the same queue a real
//! device feeds, which is why relative motion arrives shaped by the user's
//! pointer speed and acceleration settings rather than as the delta the client
//! measured; the crate docs say why that is left alone.
//!
//! One action is one call. Batching several actions into a single `SendInput`
//! array would be cheaper per event and is explicitly not done: this path
//! exists as the no-pacing, no-queue baseline that later work is measured
//! against, and an array is a queue with a very short life.

use core::mem::size_of;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
    MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{WHEEL_DELTA, XBUTTON1, XBUTTON2};

use crate::state::{Action, WheelAxis};

use lanplay_input_protocol::Button;

/// The Windows input system, as much of it as an injected event needs.
pub struct Injector {
    calls: u64,
    refused: u64,
}

impl Default for Injector {
    fn default() -> Self {
        Injector::new()
    }
}

impl Injector {
    pub fn new() -> Self {
        Injector {
            calls: 0,
            refused: 0,
        }
    }

    /// Injects one action.
    ///
    /// A failure is counted rather than returned, because there is nothing a
    /// caller could usefully do with it. `SendInput` reports only how many
    /// events it inserted, so a refusal by User Interface Privilege Isolation
    /// -- which is what happens whenever the foreground window belongs to a
    /// process at a higher integrity level, an elevated console or the secure
    /// desktop being the everyday cases -- is indistinguishable from any other
    /// failure. Reporting [`Injector::refused`] is honest about that: the
    /// number says how many events the host asked for and did not get, and the
    /// fix is to run the host elevated, not to retry.
    pub fn deliver(&mut self, action: Action) {
        let input = encode(action);
        self.calls += 1;
        // SAFETY: `SendInput` reads one `INPUT` from the slice, and the size it
        // is told to expect is that type's own size, so it cannot read past
        // the value; the slice outlives the call and the call keeps nothing.
        let inserted = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
        if inserted == 0 {
            self.refused += 1;
        }
    }

    /// How many events were handed to `SendInput`.
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// How many of those it declined to insert. See [`Injector::deliver`].
    pub fn refused(&self) -> u64 {
        self.refused
    }
}

/// The one `INPUT` an action becomes.
///
/// Separate from [`Injector::deliver`] so that what is handed to Windows can
/// be examined without a desktop, an input queue or a pointer to move.
fn encode(action: Action) -> INPUT {
    match action {
        // No `MOUSEEVENTF_ABSOLUTE`: the delta is a delta, and the client
        // never learns where the host's pointer is.
        Action::Motion { dx, dy } => mouse(MOUSEEVENTF_MOVE, dx, dy, 0),
        Action::Key {
            make,
            extended,
            down,
        } => key(make, extended, down),
        Action::Button { button, down } => {
            let (flags, data) = button_event(button, down);
            mouse(flags, 0, 0, data)
        }
        Action::Wheel { axis, detents } => {
            let flags = match axis {
                WheelAxis::Vertical => MOUSEEVENTF_WHEEL,
                WheelAxis::Horizontal => MOUSEEVENTF_HWHEEL,
            };
            // One notch per detent. The field is unsigned in the header and
            // signed in meaning, so a backwards rotation travels as its two's
            // complement.
            let notches = detents as i32 * WHEEL_DELTA as i32;
            mouse(flags, 0, 0, notches as u32)
        }
    }
}

/// `time: 0` asks the system to timestamp the event itself. Supplying a
/// timestamp of ours would be inventing one on a clock the input stack does
/// not share, and there is no schedule here to express anyway.
fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Always by scan code, never by virtual key: the client sends the physical
/// key that was pressed, and a virtual key would ask Windows to reproduce the
/// character that the *client's* layout made of it. A player whose two
/// machines disagree about their keyboard layout would otherwise find their
/// movement keys somewhere else.
fn key(make: u8, extended: bool, down: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: make as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// The flag for a button, and the `mouseData` that says which X button is
/// meant when the flag cannot say it by itself.
fn button_event(button: Button, down: bool) -> (MOUSE_EVENT_FLAGS, u32) {
    match (button, down) {
        (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (Button::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
        (Button::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
        (Button::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
        (Button::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The X buttons share one flag pair, so the button they mean lives
    /// entirely in `mouseData`. Getting this wrong is silent: the click lands,
    /// on the wrong button.
    #[test]
    fn x_buttons_are_told_apart_by_data_and_not_by_flag() {
        let (down_flags, x1) = button_event(Button::X1, true);
        let (up_flags, x2) = button_event(Button::X2, false);
        assert_eq!(down_flags, MOUSEEVENTF_XDOWN);
        assert_eq!(up_flags, MOUSEEVENTF_XUP);
        assert_ne!(x1, x2);
        assert_eq!(button_event(Button::X2, true).1, x2);
    }

    #[test]
    fn a_release_carries_keyup_and_still_carries_scancode() {
        let up = key(0x11, false, false);
        // SAFETY: the union was written as a keyboard event one line above.
        let flags = unsafe { up.Anonymous.ki }.dwFlags;
        assert_eq!(flags, KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP);

        let right_control = key(0x1D, true, true);
        // SAFETY: as above.
        let extended = unsafe { right_control.Anonymous.ki };
        assert_eq!(extended.dwFlags, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY);
        assert_eq!(extended.wScan, 0x1D);
        assert_eq!(
            extended.wVk,
            VIRTUAL_KEY(0),
            "a virtual key would override the scan code"
        );
    }

    /// A detent is a notch, and a backwards notch is a negative one in a field
    /// the header declares unsigned. A wheel that scrolls the wrong way, or by
    /// a fifth of a line, is this arithmetic.
    #[test]
    fn a_wheel_detent_becomes_one_notch_on_the_axis_it_names() {
        let up = encode(Action::Wheel {
            axis: WheelAxis::Vertical,
            detents: 1,
        });
        // SAFETY: `encode` wrote the mouse arm of the union for a wheel action.
        let up = unsafe { up.Anonymous.mi };
        assert_eq!(up.dwFlags, MOUSEEVENTF_WHEEL);
        assert_eq!(up.mouseData, 120);

        let left = encode(Action::Wheel {
            axis: WheelAxis::Horizontal,
            detents: -2,
        });
        // SAFETY: as above.
        let left = unsafe { left.Anonymous.mi };
        assert_eq!(left.dwFlags, MOUSEEVENTF_HWHEEL);
        assert_eq!(left.mouseData as i32, -240);
    }

    #[test]
    fn motion_is_relative_and_carries_the_delta_it_was_given() {
        let input = encode(Action::Motion { dx: -7, dy: 3 });
        assert_eq!(input.r#type, INPUT_MOUSE);
        // SAFETY: `encode` wrote the mouse arm of the union for motion.
        let motion = unsafe { input.Anonymous.mi };
        assert_eq!(motion.dwFlags, MOUSEEVENTF_MOVE);
        assert_eq!((motion.dx, motion.dy), (-7, 3));
    }
}
