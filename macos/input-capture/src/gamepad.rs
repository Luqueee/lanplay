//! GameController reads normalized to the transport's device-neutral controller state.
//!
//! This module does not create a policy deadzone. GameController's raw normalized values
//! travel unchanged in integer form, leaving game-specific treatment at the Windows API
//! boundary where a virtual controller declares its own range.

use lanplay_input_protocol::{Dpad, GamepadStateV1};

pub mod buttons {
    pub const SOUTH: u16 = 1 << 0;
    pub const EAST: u16 = 1 << 1;
    pub const WEST: u16 = 1 << 2;
    pub const NORTH: u16 = 1 << 3;
    pub const LEFT_SHOULDER: u16 = 1 << 4;
    pub const RIGHT_SHOULDER: u16 = 1 << 5;
    pub const LEFT_STICK: u16 = 1 << 6;
    pub const RIGHT_STICK: u16 = 1 << 7;
    pub const VIEW: u16 = 1 << 8;
    pub const MENU: u16 = 1 << 9;
    pub const GUIDE: u16 = 1 << 10;
}

pub fn normalize_axis(value: f32) -> i16 {
    let scaled = (value.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round();
    scaled as i16
}

pub fn normalize_trigger(value: f32) -> u16 {
    let scaled = (value.clamp(0.0, 1.0) * f32::from(u16::MAX)).round();
    scaled as u16
}

pub fn dpad(up: bool, down: bool, left: bool, right: bool) -> Dpad {
    match (up, down, left, right) {
        (true, false, false, false) => Dpad::North,
        (true, false, false, true) => Dpad::NorthEast,
        (false, false, false, true) => Dpad::East,
        (false, true, false, true) => Dpad::SouthEast,
        (false, true, false, false) => Dpad::South,
        (false, true, true, false) => Dpad::SouthWest,
        (false, false, true, false) => Dpad::West,
        (true, false, true, false) => Dpad::NorthWest,
        _ => Dpad::Neutral,
    }
}

#[cfg(target_os = "macos")]
pub fn snapshot(
    profile: &objc2_game_controller::GCExtendedGamepad,
    session_generation: u32,
    controller_slot: u8,
    sequence: u32,
) -> GamepadStateV1 {
    use objc2::rc::Retained;
    use objc2_game_controller::GCControllerButtonInput;

    unsafe {
        let pressed = |button: Retained<GCControllerButtonInput>| button.isPressed();
        let mut buttons = 0;
        for (bit, down) in [
            (buttons::SOUTH, pressed(profile.buttonA())),
            (buttons::EAST, pressed(profile.buttonB())),
            (buttons::WEST, pressed(profile.buttonX())),
            (buttons::NORTH, pressed(profile.buttonY())),
            (buttons::LEFT_SHOULDER, pressed(profile.leftShoulder())),
            (buttons::RIGHT_SHOULDER, pressed(profile.rightShoulder())),
            (
                buttons::LEFT_STICK,
                profile.leftThumbstickButton().is_some_and(&pressed),
            ),
            (
                buttons::RIGHT_STICK,
                profile.rightThumbstickButton().is_some_and(&pressed),
            ),
            (buttons::VIEW, profile.buttonOptions().is_some_and(&pressed)),
            (buttons::MENU, pressed(profile.buttonMenu())),
            (buttons::GUIDE, profile.buttonHome().is_some_and(&pressed)),
        ] {
            if down {
                buttons |= bit;
            }
        }
        let left = profile.leftThumbstick();
        let right = profile.rightThumbstick();
        let pad = profile.dpad();
        GamepadStateV1 {
            session_generation,
            controller_slot,
            sequence,
            buttons,
            dpad: dpad(
                pressed(pad.up()),
                pressed(pad.down()),
                pressed(pad.left()),
                pressed(pad.right()),
            ),
            left_x: normalize_axis(left.xAxis().value()),
            left_y: normalize_axis(left.yAxis().value()),
            right_x: normalize_axis(right.xAxis().value()),
            right_y: normalize_axis(right.yAxis().value()),
            left_trigger: normalize_trigger(profile.leftTrigger().value()),
            right_trigger: normalize_trigger(profile.rightTrigger().value()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_axis_preserves_both_endpoints_and_neutral() {
        assert_eq!(normalize_axis(-1.0), -32767);
        assert_eq!(normalize_axis(0.0), 0);
        assert_eq!(normalize_axis(1.0), 32767);
    }

    #[test]
    fn normalized_trigger_preserves_both_endpoints_and_neutral() {
        assert_eq!(normalize_trigger(0.0), 0);
        assert_eq!(normalize_trigger(1.0), u16::MAX);
    }

    #[test]
    fn a_dpad_reports_one_hat_position() {
        assert_eq!(dpad(true, false, true, false), Dpad::NorthWest);
        assert_eq!(dpad(true, true, false, false), Dpad::Neutral);
    }
}
