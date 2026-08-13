//! macOS virtual key codes to set 1 scan codes, the PC XT set.
//!
//! The host injects with `KEYEVENTF_SCANCODE`, so what travels is the position
//! of the key on the keyboard and not the character it produced. That is the
//! only translation a game can use. A player on a French layout who presses the
//! key where `W` sits on a US board wants the game to see that key, and a
//! character-based translation would send `Z` instead, so the two ends would
//! disagree about what the player just did.
//!
//! The table is therefore fixed. A macOS virtual key code is already
//! layout-independent: `kVK_ANSI_W` is the key one right of `Q` whatever the
//! user has installed, and a set 1 scan code names the same position on the
//! other convention. Nothing here reads the current layout, and nothing here
//! can be affected by the user changing it.
//!
//! An unknown code yields `None` rather than anything else. A missing key
//! presses nothing on the host, which the player notices and can work around;
//! a guessed key presses something else, which they cannot. That rule is why
//! print screen and pause are absent: on a real PC keyboard both are multi-byte
//! sequences rather than one prefixed code, so no single scan code reproduces
//! them and a partial sequence would press something unpredictable.
//!
//! The ANSI positions are what this asserts. The extra key an ISO board carries
//! next to the left shift, and the `§` key it puts left of the digit row, swap
//! roles between Apple and PC conventions in a way that depends on the physical
//! board rather than on anything a virtual key code reveals, so they are left
//! out instead of guessed at.

/// One physical key as the host wants it: a set 1 make code, and whether a PC
/// keyboard would have reached it through an 0xE0 prefix.
///
/// The prefix is not folded into `code`. `SendInput` takes the make code and
/// the flag separately, and the protocol's snapshot bitset folds the flag into
/// a bit of its own index, so carrying 0xE0 in a high byte here would leave two
/// representations of the same key for the host to reconcile.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ScanCode {
    pub code: u8,
    pub extended: bool,
}

impl ScanCode {
    const fn plain(code: u8) -> ScanCode {
        ScanCode {
            code,
            extended: false,
        }
    }

    const fn extended(code: u8) -> ScanCode {
        ScanCode {
            code,
            extended: true,
        }
    }

    /// Translates one `NSEvent` key code, or refuses to.
    ///
    /// `const` and a plain `match` so that the whole thing compiles to a jump
    /// table: this runs on the event path, once per key, and a search through a
    /// hundred entries there would be a cost paid for nothing.
    pub const fn from_virtual_key(virtual_key: u16) -> Option<ScanCode> {
        Some(match virtual_key {
            // The letters, in macOS code order, which is the order Apple's
            // header lists them in and therefore the order this can be checked
            // against it.
            0x00 => ScanCode::plain(0x1E), // A
            0x01 => ScanCode::plain(0x1F), // S
            0x02 => ScanCode::plain(0x20), // D
            0x03 => ScanCode::plain(0x21), // F
            0x04 => ScanCode::plain(0x23), // H
            0x05 => ScanCode::plain(0x22), // G
            0x06 => ScanCode::plain(0x2C), // Z
            0x07 => ScanCode::plain(0x2D), // X
            0x08 => ScanCode::plain(0x2E), // C
            0x09 => ScanCode::plain(0x2F), // V
            0x0B => ScanCode::plain(0x30), // B
            0x0C => ScanCode::plain(0x10), // Q
            0x0D => ScanCode::plain(0x11), // W
            0x0E => ScanCode::plain(0x12), // E
            0x0F => ScanCode::plain(0x13), // R
            0x10 => ScanCode::plain(0x15), // Y
            0x11 => ScanCode::plain(0x14), // T
            0x1F => ScanCode::plain(0x18), // O
            0x20 => ScanCode::plain(0x16), // U
            0x22 => ScanCode::plain(0x17), // I
            0x23 => ScanCode::plain(0x19), // P
            0x25 => ScanCode::plain(0x26), // L
            0x26 => ScanCode::plain(0x24), // J
            0x28 => ScanCode::plain(0x25), // K
            0x2D => ScanCode::plain(0x31), // N
            0x2E => ScanCode::plain(0x32), // M

            // The digit row. Both conventions run left to right, but macOS
            // numbers 5 and 6 the other way round, which is the one place a
            // reader is likely to assume a pattern that is not there.
            0x12 => ScanCode::plain(0x02), // 1
            0x13 => ScanCode::plain(0x03), // 2
            0x14 => ScanCode::plain(0x04), // 3
            0x15 => ScanCode::plain(0x05), // 4
            0x17 => ScanCode::plain(0x06), // 5
            0x16 => ScanCode::plain(0x07), // 6
            0x1A => ScanCode::plain(0x08), // 7
            0x1C => ScanCode::plain(0x09), // 8
            0x19 => ScanCode::plain(0x0A), // 9
            0x1D => ScanCode::plain(0x0B), // 0

            // The punctuation a US board carries.
            0x1B => ScanCode::plain(0x0C), // -
            0x18 => ScanCode::plain(0x0D), // =
            0x21 => ScanCode::plain(0x1A), // [
            0x1E => ScanCode::plain(0x1B), // ]
            0x2A => ScanCode::plain(0x2B), // \
            0x29 => ScanCode::plain(0x27), // ;
            0x27 => ScanCode::plain(0x28), // '
            0x32 => ScanCode::plain(0x29), // `
            0x2B => ScanCode::plain(0x33), // ,
            0x2F => ScanCode::plain(0x34), // .
            0x2C => ScanCode::plain(0x35), // /

            // Whitespace and editing. Apple's `kVK_Delete` is the key above
            // return, which every PC calls backspace, and the one Apple calls
            // `kVK_ForwardDelete` is the one a PC calls delete.
            0x24 => ScanCode::plain(0x1C),    // return
            0x30 => ScanCode::plain(0x0F),    // tab
            0x31 => ScanCode::plain(0x39),    // space
            0x33 => ScanCode::plain(0x0E),    // backspace
            0x35 => ScanCode::plain(0x01),    // escape
            0x75 => ScanCode::extended(0x53), // delete forward

            // Modifiers. The right-hand control and alt live behind the prefix
            // because a PC keyboard has only one of each in the base set.
            0x38 => ScanCode::plain(0x2A),    // left shift
            0x3C => ScanCode::plain(0x36),    // right shift
            0x3B => ScanCode::plain(0x1D),    // left control
            0x3E => ScanCode::extended(0x1D), // right control
            0x3A => ScanCode::plain(0x38),    // left option, the host's left alt
            0x3D => ScanCode::extended(0x38), // right option, the host's right alt
            0x39 => ScanCode::plain(0x3A),    // caps lock

            // The command keys become the Windows keys, which is where a
            // player's thumb already is and which the host's own shortcuts
            // expect. Both are extended: the base set 1 table has nothing at
            // 0x5B or 0x5C, so an unprefixed one would press nothing at all.
            0x37 => ScanCode::extended(0x5B), // left command
            0x36 => ScanCode::extended(0x5C), // right command

            // The function keys, whose scan codes stop being contiguous after
            // F10 because F11 and F12 were added to the keyboard later.
            0x7A => ScanCode::plain(0x3B), // F1
            0x78 => ScanCode::plain(0x3C), // F2
            0x63 => ScanCode::plain(0x3D), // F3
            0x76 => ScanCode::plain(0x3E), // F4
            0x60 => ScanCode::plain(0x3F), // F5
            0x61 => ScanCode::plain(0x40), // F6
            0x62 => ScanCode::plain(0x41), // F7
            0x64 => ScanCode::plain(0x42), // F8
            0x65 => ScanCode::plain(0x43), // F9
            0x6D => ScanCode::plain(0x44), // F10
            0x67 => ScanCode::plain(0x57), // F11
            0x6F => ScanCode::plain(0x58), // F12

            // The navigation cluster, all of it prefixed. These share their
            // make codes with the numpad, and the prefix is the only thing that
            // tells the host an arrow from a numpad digit.
            0x7B => ScanCode::extended(0x4B), // left
            0x7C => ScanCode::extended(0x4D), // right
            0x7D => ScanCode::extended(0x50), // down
            0x7E => ScanCode::extended(0x48), // up
            0x73 => ScanCode::extended(0x47), // home
            0x77 => ScanCode::extended(0x4F), // end
            0x74 => ScanCode::extended(0x49), // page up
            0x79 => ScanCode::extended(0x51), // page down
            // Apple's `kVK_Help` is the key sitting where a PC puts insert,
            // and it is that position the host has to be told about.
            0x72 => ScanCode::extended(0x52), // insert

            // The numpad, unprefixed, which is what makes it the numpad rather
            // than the navigation cluster.
            0x52 => ScanCode::plain(0x52), // 0
            0x53 => ScanCode::plain(0x4F), // 1
            0x54 => ScanCode::plain(0x50), // 2
            0x55 => ScanCode::plain(0x51), // 3
            0x56 => ScanCode::plain(0x4B), // 4
            0x57 => ScanCode::plain(0x4C), // 5
            0x58 => ScanCode::plain(0x4D), // 6
            0x59 => ScanCode::plain(0x47), // 7
            0x5B => ScanCode::plain(0x48), // 8
            0x5C => ScanCode::plain(0x49), // 9
            0x41 => ScanCode::plain(0x53), // .
            0x43 => ScanCode::plain(0x37), // *
            0x4E => ScanCode::plain(0x4A), // -
            0x45 => ScanCode::plain(0x4E), // +
            // Divide and enter are prefixed, which is the only thing that
            // stops the host reading them as the slash on the main row and as
            // the return key.
            0x4B => ScanCode::extended(0x35), // divide
            0x4C => ScanCode::extended(0x1C), // enter
            // Apple's clear occupies the position a PC gives num lock, and
            // sending it as num lock is what physical reproduction means here:
            // the host's numpad changes mode, exactly as it would for someone
            // pressing that key on a PC keyboard.
            0x47 => ScanCode::plain(0x45), // clear, the host's num lock

            // Everything else, which is every key this build has no position
            // for: F13 upwards, the volume and brightness keys, the fn key,
            // the JIS keys, and whatever a future keyboard adds.
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ScanCode;
    use std::collections::HashMap;

    /// The whole domain of an `NSEvent` key code, so every test below is
    /// exhaustive rather than a sample of the entries somebody remembered.
    fn table() -> Vec<(u16, ScanCode)> {
        (0..=u16::MAX)
            .filter_map(|key| ScanCode::from_virtual_key(key).map(|scan| (key, scan)))
            .collect()
    }

    /// The four keys every player notices in the first second of moving.
    #[test]
    fn wasd_lands_on_the_set_every_player_knows() {
        for (key, code) in [(0x0Du16, 0x11u8), (0x00, 0x1E), (0x01, 0x1F), (0x02, 0x20)] {
            assert_eq!(
                ScanCode::from_virtual_key(key),
                Some(ScanCode {
                    code,
                    extended: false
                }),
                "virtual key {key:#04X} must be scan code {code:#04X}"
            );
        }
    }

    /// Two keys sharing a code and a flag would be one key to the host, so one
    /// of them would press the other's key and neither could ever be released
    /// independently.
    #[test]
    fn no_two_keys_share_a_code_and_flag() {
        let mut seen: HashMap<ScanCode, u16> = HashMap::new();
        for (key, scan) in table() {
            if let Some(other) = seen.insert(scan, key) {
                panic!("virtual keys {other:#04X} and {key:#04X} both map to {scan:?}");
            }
        }
    }

    /// The extended flag is the whole difference between an arrow and a numpad
    /// digit, and between the two controls, so the set that carries it is
    /// pinned by name rather than by count.
    #[test]
    fn exactly_the_prefixed_keys_are_extended() {
        let mut wanted = vec![
            0x3Eu16, // right control
            0x3D,    // right option
            0x7B,    // left
            0x7C,    // right
            0x7D,    // down
            0x7E,    // up
            0x72,    // insert
            0x75,    // delete forward
            0x73,    // home
            0x77,    // end
            0x74,    // page up
            0x79,    // page down
            0x4B,    // numpad divide
            0x4C,    // numpad enter
            0x37,    // left command
            0x36,    // right command
        ];
        wanted.sort_unstable();

        let mut found: Vec<u16> = table()
            .into_iter()
            .filter(|(_, scan)| scan.extended)
            .map(|(key, _)| key)
            .collect();
        found.sort_unstable();

        assert_eq!(found, wanted);
    }

    /// A key with no position on a PC keyboard has to press nothing, because
    /// the alternative is pressing something the player did not.
    #[test]
    fn an_unknown_key_presses_nothing() {
        // F13, the fn key, mute, the JIS yen key, and a code no keyboard emits.
        for key in [0x69u16, 0x3F, 0x4A, 0x5D, 0x0A, 0x80, 0xFFFF] {
            assert_eq!(
                ScanCode::from_virtual_key(key),
                None,
                "virtual key {key:#04X} has no set 1 position and must not be guessed"
            );
        }
    }
}
