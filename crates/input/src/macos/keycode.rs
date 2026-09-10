//! macOS virtual keycode ↔ [`Key`] table — pure data, the "thin shim"
//! ADR-0007 describes. Codes are the standard Carbon/HIToolbox
//! `kVK_*` virtual keycodes, stable across macOS versions.

use kvm_protocol::Key;

/// Translates a raw macOS virtual keycode into a normalized [`Key`].
/// Anything not in the table becomes [`Key::Unknown`] rather than being
/// dropped — see ADR-0007.
pub fn keycode_to_key(code: u16) -> Key {
    match code {
        0x00 => Key::A,
        0x0B => Key::B,
        0x08 => Key::C,
        0x02 => Key::D,
        0x0E => Key::E,
        0x03 => Key::F,
        0x05 => Key::G,
        0x04 => Key::H,
        0x22 => Key::I,
        0x26 => Key::J,
        0x28 => Key::K,
        0x25 => Key::L,
        0x2E => Key::M,
        0x2D => Key::N,
        0x1F => Key::O,
        0x23 => Key::P,
        0x0C => Key::Q,
        0x0F => Key::R,
        0x01 => Key::S,
        0x11 => Key::T,
        0x20 => Key::U,
        0x09 => Key::V,
        0x0D => Key::W,
        0x07 => Key::X,
        0x10 => Key::Y,
        0x06 => Key::Z,

        0x1D => Key::Digit0,
        0x12 => Key::Digit1,
        0x13 => Key::Digit2,
        0x14 => Key::Digit3,
        0x15 => Key::Digit4,
        0x17 => Key::Digit5,
        0x16 => Key::Digit6,
        0x1A => Key::Digit7,
        0x1C => Key::Digit8,
        0x19 => Key::Digit9,

        0x38 => Key::ShiftLeft,
        0x3C => Key::ShiftRight,
        0x3B => Key::ControlLeft,
        0x3E => Key::ControlRight,
        0x3A => Key::AltLeft,
        0x3D => Key::AltRight,
        0x37 => Key::MetaLeft,
        0x36 => Key::MetaRight,
        0x39 => Key::CapsLock,

        0x7A => Key::F1,
        0x78 => Key::F2,
        0x63 => Key::F3,
        0x76 => Key::F4,
        0x60 => Key::F5,
        0x61 => Key::F6,
        0x62 => Key::F7,
        0x64 => Key::F8,
        0x65 => Key::F9,
        0x6D => Key::F10,
        0x67 => Key::F11,
        0x6F => Key::F12,

        0x7E => Key::ArrowUp,
        0x7D => Key::ArrowDown,
        0x7B => Key::ArrowLeft,
        0x7C => Key::ArrowRight,
        0x73 => Key::Home,
        0x77 => Key::End,
        0x74 => Key::PageUp,
        0x79 => Key::PageDown,

        0x24 => Key::Enter,
        0x35 => Key::Escape,
        0x33 => Key::Backspace,
        0x75 => Key::Delete,
        0x30 => Key::Tab,
        0x31 => Key::Space,

        other => Key::Unknown(other as u32),
    }
}

/// The inverse of [`keycode_to_key`], for injection. Returns `None` for
/// a [`Key`] this table has no macOS code for (should only happen for a
/// `Key::Unknown` whose original code came from a *different* platform —
/// see ADR-0007's note on cross-platform `Unknown` codes not being
/// portable).
pub fn key_to_keycode(key: Key) -> Option<u16> {
    Some(match key {
        Key::A => 0x00,
        Key::B => 0x0B,
        Key::C => 0x08,
        Key::D => 0x02,
        Key::E => 0x0E,
        Key::F => 0x03,
        Key::G => 0x05,
        Key::H => 0x04,
        Key::I => 0x22,
        Key::J => 0x26,
        Key::K => 0x28,
        Key::L => 0x25,
        Key::M => 0x2E,
        Key::N => 0x2D,
        Key::O => 0x1F,
        Key::P => 0x23,
        Key::Q => 0x0C,
        Key::R => 0x0F,
        Key::S => 0x01,
        Key::T => 0x11,
        Key::U => 0x20,
        Key::V => 0x09,
        Key::W => 0x0D,
        Key::X => 0x07,
        Key::Y => 0x10,
        Key::Z => 0x06,

        Key::Digit0 => 0x1D,
        Key::Digit1 => 0x12,
        Key::Digit2 => 0x13,
        Key::Digit3 => 0x14,
        Key::Digit4 => 0x15,
        Key::Digit5 => 0x17,
        Key::Digit6 => 0x16,
        Key::Digit7 => 0x1A,
        Key::Digit8 => 0x1C,
        Key::Digit9 => 0x19,

        Key::ShiftLeft => 0x38,
        Key::ShiftRight => 0x3C,
        Key::ControlLeft => 0x3B,
        Key::ControlRight => 0x3E,
        Key::AltLeft => 0x3A,
        Key::AltRight => 0x3D,
        Key::MetaLeft => 0x37,
        Key::MetaRight => 0x36,
        Key::CapsLock => 0x39,

        Key::F1 => 0x7A,
        Key::F2 => 0x78,
        Key::F3 => 0x63,
        Key::F4 => 0x76,
        Key::F5 => 0x60,
        Key::F6 => 0x61,
        Key::F7 => 0x62,
        Key::F8 => 0x64,
        Key::F9 => 0x65,
        Key::F10 => 0x6D,
        Key::F11 => 0x67,
        Key::F12 => 0x6F,

        Key::ArrowUp => 0x7E,
        Key::ArrowDown => 0x7D,
        Key::ArrowLeft => 0x7B,
        Key::ArrowRight => 0x7C,
        Key::Home => 0x73,
        Key::End => 0x77,
        Key::PageUp => 0x74,
        Key::PageDown => 0x79,

        Key::Enter => 0x24,
        Key::Escape => 0x35,
        Key::Backspace => 0x33,
        Key::Delete => 0x75,
        Key::Tab => 0x30,
        Key::Space => 0x31,

        Key::Unknown(code) if code <= u16::MAX as u32 => code as u16,
        Key::Unknown(_) => return None,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const ALL_MAPPED_KEYS: &[Key] = &[
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
        Key::Digit0,
        Key::Digit1,
        Key::Digit2,
        Key::Digit3,
        Key::Digit4,
        Key::Digit5,
        Key::Digit6,
        Key::Digit7,
        Key::Digit8,
        Key::Digit9,
        Key::ShiftLeft,
        Key::ShiftRight,
        Key::ControlLeft,
        Key::ControlRight,
        Key::AltLeft,
        Key::AltRight,
        Key::MetaLeft,
        Key::MetaRight,
        Key::CapsLock,
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::ArrowUp,
        Key::ArrowDown,
        Key::ArrowLeft,
        Key::ArrowRight,
        Key::Home,
        Key::End,
        Key::PageUp,
        Key::PageDown,
        Key::Enter,
        Key::Escape,
        Key::Backspace,
        Key::Delete,
        Key::Tab,
        Key::Space,
    ];

    #[test]
    fn every_mapped_key_round_trips_through_the_macos_code() {
        for &key in ALL_MAPPED_KEYS {
            let code = key_to_keycode(key).unwrap_or_else(|| panic!("{key:?} has no macOS code"));
            assert_eq!(
                keycode_to_key(code),
                key,
                "keycode {code:#x} for {key:?} did not round-trip"
            );
        }
    }

    #[test]
    fn left_and_right_modifier_variants_map_to_distinct_codes() {
        let pairs = [
            (Key::ShiftLeft, Key::ShiftRight),
            (Key::ControlLeft, Key::ControlRight),
            (Key::AltLeft, Key::AltRight),
            (Key::MetaLeft, Key::MetaRight),
        ];
        for (left, right) in pairs {
            let left_code = key_to_keycode(left).unwrap();
            let right_code = key_to_keycode(right).unwrap();
            assert_ne!(
                left_code, right_code,
                "{left:?} and {right:?} must map to distinct macOS codes"
            );
        }
    }

    #[test]
    fn unrecognized_code_becomes_unknown_not_dropped() {
        // 0xFF is not assigned by any kVK_* constant used above.
        assert_eq!(keycode_to_key(0xFF), Key::Unknown(0xFF));
    }

    #[test]
    fn unknown_with_in_range_code_maps_back_to_the_same_raw_code() {
        assert_eq!(key_to_keycode(Key::Unknown(0xFF)), Some(0xFF));
    }

    #[test]
    fn unknown_with_out_of_range_code_has_no_macos_code() {
        // A Key::Unknown that originated on a platform with wider raw
        // codes than macOS's u16 keycode space has no macOS equivalent —
        // must report that explicitly rather than truncating silently.
        assert_eq!(key_to_keycode(Key::Unknown(u32::MAX)), None);
    }
}
