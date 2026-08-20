use lanplay_input_protocol::{Dpad, GamepadStateV1};

use crate::gamepad::buttons;

pub const BLUETOOTH_INPUT_REPORT_ID: u8 = 0x01;
pub const INPUT_REPORT_LEN: usize = 10;

pub fn parse_bluetooth_input(
    report: &[u8],
    session_generation: u32,
    controller_slot: u8,
    sequence: u32,
) -> Option<GamepadStateV1> {
    let (
        left_x,
        left_y,
        right_x,
        right_y,
        buttons_0,
        buttons_1,
        buttons_2,
        left_trigger,
        right_trigger,
    ) = if report.first() == Some(&BLUETOOTH_INPUT_REPORT_ID) && report.len() >= INPUT_REPORT_LEN {
        (
            report[1], report[2], report[3], report[4], report[5], report[6], report[7], report[8],
            report[9],
        )
    } else if report.first() == Some(&0x11) && report.len() >= 12 {
        (
            report[3], report[4], report[5], report[6], report[7], report[8], report[9],
            report[10], report[11],
        )
    } else {
        return None;
    };
    let mut buttons = 0;
    for (mask, pressed) in [
        (buttons::WEST, buttons_0 & 0x10 != 0),
        (buttons::SOUTH, buttons_0 & 0x20 != 0),
        (buttons::EAST, buttons_0 & 0x40 != 0),
        (buttons::NORTH, buttons_0 & 0x80 != 0),
        (buttons::LEFT_SHOULDER, buttons_1 & 0x01 != 0),
        (buttons::RIGHT_SHOULDER, buttons_1 & 0x02 != 0),
        (buttons::VIEW, buttons_1 & 0x10 != 0),
        (buttons::MENU, buttons_1 & 0x20 != 0),
        (buttons::LEFT_STICK, buttons_1 & 0x40 != 0),
        (buttons::RIGHT_STICK, buttons_1 & 0x80 != 0),
        (buttons::GUIDE, buttons_2 & 0x01 != 0),
    ] {
        if pressed {
            buttons |= mask;
        }
    }
    Some(GamepadStateV1 {
        session_generation,
        controller_slot,
        sequence,
        buttons,
        dpad: dpad(buttons_0 & 0x0f),
        left_x: axis(left_x),
        left_y: axis(left_y),
        right_x: axis(right_x),
        right_y: axis(right_y),
        left_trigger: trigger(left_trigger),
        right_trigger: trigger(right_trigger),
    })
}

fn axis(value: u8) -> i16 {
    (f32::from(value) * 65534.0 / 255.0 - 32767.0).round() as i16
}

fn trigger(value: u8) -> u16 {
    u16::from(value) * 257
}

fn dpad(value: u8) -> Dpad {
    match value {
        0 => Dpad::North,
        1 => Dpad::NorthEast,
        2 => Dpad::East,
        3 => Dpad::SouthEast,
        4 => Dpad::South,
        5 => Dpad::SouthWest,
        6 => Dpad::West,
        7 => Dpad::NorthWest,
        _ => Dpad::Neutral,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_a_report_without_platform_input_apis() {
        let report = [1, 0, 255, 128, 64, 0x22, 0xd3, 1, 0, 255];
        let state = parse_bluetooth_input(&report, 4, 0, 9).expect("DS4 report");
        assert_eq!(state.left_x, -32767);
        assert_eq!(state.left_y, i16::MAX);
        assert_eq!(state.right_x, 128);
        assert_eq!(state.right_y, -16319);
        assert_eq!(state.left_trigger, 0);
        assert_eq!(state.right_trigger, u16::MAX);
        assert_eq!(state.dpad, Dpad::East);
        assert_eq!(
            state.buttons,
            buttons::SOUTH
                | buttons::LEFT_SHOULDER
                | buttons::RIGHT_SHOULDER
                | buttons::VIEW
                | buttons::LEFT_STICK
                | buttons::RIGHT_STICK
                | buttons::GUIDE
        );
    }

    #[test]
    fn maps_a_bluetooth_report_common_section() {
        let mut report = [0u8; 78];
        report[0] = 0x11;
        report[3] = 255;
        report[4] = 0;
        report[5] = 128;
        report[6] = 64;
        report[7] = 8 | 0x20 | 0x40;
        report[8] = 0x01;
        report[9] = 0x01;
        report[10] = 0;
        report[11] = 255;
        let state = parse_bluetooth_input(&report, 4, 0, 9).expect("Bluetooth DS4 report");
        assert_eq!(state.left_x, i16::MAX);
        assert_eq!(state.left_y, -32767);
        assert_eq!(state.left_trigger, 0);
        assert_eq!(state.right_trigger, u16::MAX);
        assert_eq!(state.dpad, Dpad::Neutral);
        assert_eq!(
            state.buttons,
            buttons::SOUTH | buttons::EAST | buttons::LEFT_SHOULDER | buttons::GUIDE
        );
    }
}
