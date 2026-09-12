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

        0x1B => Key::Minus,
        0x18 => Key::Equal,
        0x21 => Key::BracketLeft,
        0x1E => Key::BracketRight,
        0x2A => Key::Backslash,
        0x29 => Key::Semicolon,
        0x27 => Key::Quote,
        0x32 => Key::Backquote,
        0x2B => Key::Comma,
        0x2F => Key::Period,
        0x2C => Key::Slash,

        0x52 => Key::Numpad0,
        0x53 => Key::Numpad1,
        0x54 => Key::Numpad2,
        0x55 => Key::Numpad3,
        0x56 => Key::Numpad4,
        0x57 => Key::Numpad5,
        0x58 => Key::Numpad6,
        0x59 => Key::Numpad7,
        0x5B => Key::Numpad8,
        0x5C => Key::Numpad9,
        0x41 => Key::NumpadDecimal,
        0x43 => Key::NumpadMultiply,
        0x45 => Key::NumpadAdd,
        0x4E => Key::NumpadSubtract,
        0x4B => Key::NumpadDivide,
        0x4C => Key::NumpadEnter,
        // kVK_ANSI_KeypadClear: the key a PC keyboard labels Num Lock
        // (USB HID usage 0x53, "Keypad Num Lock and Clear").
        0x47 => Key::NumLock,

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

        Key::Minus => 0x1B,
        Key::Equal => 0x18,
        Key::BracketLeft => 0x21,
        Key::BracketRight => 0x1E,
        Key::Backslash => 0x2A,
        Key::Semicolon => 0x29,
        Key::Quote => 0x27,
        Key::Backquote => 0x32,
        Key::Comma => 0x2B,
        Key::Period => 0x2F,
        Key::Slash => 0x2C,

        Key::Numpad0 => 0x52,
        Key::Numpad1 => 0x53,
        Key::Numpad2 => 0x54,
        Key::Numpad3 => 0x55,
        Key::Numpad4 => 0x56,
        Key::Numpad5 => 0x57,
        Key::Numpad6 => 0x58,
        Key::Numpad7 => 0x59,
        Key::Numpad8 => 0x5B,
        Key::Numpad9 => 0x5C,
        Key::NumpadDecimal => 0x41,
        Key::NumpadMultiply => 0x43,
        Key::NumpadAdd => 0x45,
        Key::NumpadSubtract => 0x4E,
        Key::NumpadDivide => 0x4B,
        Key::NumpadEnter => 0x4C,
        Key::NumLock => 0x47,

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
        Key::Minus,
        Key::Equal,
        Key::BracketLeft,
        Key::BracketRight,
        Key::Backslash,
        Key::Semicolon,
        Key::Quote,
        Key::Backquote,
        Key::Comma,
        Key::Period,
        Key::Slash,
        Key::Numpad0,
        Key::Numpad1,
        Key::Numpad2,
        Key::Numpad3,
        Key::Numpad4,
        Key::Numpad5,
        Key::Numpad6,
        Key::Numpad7,
        Key::Numpad8,
        Key::Numpad9,
        Key::NumpadDecimal,
        Key::NumpadMultiply,
        Key::NumpadAdd,
        Key::NumpadSubtract,
        Key::NumpadDivide,
        Key::NumpadEnter,
        Key::NumLock,
    ];

    /// **Regression test for the Phase 4 acceptance-run keyboard failure**
    /// (ADR-0007's 2026-09-12 update), using the raw codes from the real
    /// hub log verbatim. Every one of these left the Mac as
    /// `Key::Unknown(code)`, and the Windows injector pressed whatever key
    /// Windows assigns that same *number* — noted per line. Each must now
    /// be captured as its named key.
    #[test]
    fn punctuation_and_numpad_codes_from_the_acceptance_log_are_named_not_unknown() {
        let observed = [
            (50, Key::Backquote),      // logged x14; Windows typed the digit 2
            (76, Key::NumpadEnter),    // logged x32; Windows typed L
            (92, Key::Numpad9),        // Windows pressed the Windows key
            (33, Key::BracketLeft),    // Windows pressed Page Up
            (27, Key::Minus),          // Windows pressed Escape
            (39, Key::Quote),          // Windows pressed Right arrow
            (44, Key::Slash),          // Windows pressed Print Screen
            (43, Key::Comma),          // Windows: no-op VK, "not working"
            (47, Key::Period),         // Windows: no-op VK
            (41, Key::Semicolon),      // Windows: no-op VK
            (30, Key::BracketRight),   // Windows: no-op VK
            (42, Key::Backslash),      // Windows: no-op VK
            (24, Key::Equal),          // Windows: no-op VK
            (82, Key::Numpad0),        // Windows typed R
            (83, Key::Numpad1),        // S
            (84, Key::Numpad2),        // T
            (85, Key::Numpad3),        // U
            (86, Key::Numpad4),        // V
            (88, Key::Numpad6),        // X
            (89, Key::Numpad7),        // Y
            (67, Key::NumpadMultiply), // C
            (69, Key::NumpadAdd),      // E
            (75, Key::NumpadDivide),   // K
            (78, Key::NumpadSubtract), // N
        ];
        for (code, expected) in observed {
            assert_eq!(
                keycode_to_key(code),
                expected,
                "macOS keycode {code} ({code:#x}) must be captured as {expected:?}, not Unknown"
            );
        }

        // Also in the log (x4; Windows typed G): keypad Clear -- the key a
        // PC keyboard labels Num Lock (USB HID usage 0x53, "Keypad Num
        // Lock and Clear"). Named since protocol 1.2.
        assert_eq!(keycode_to_key(0x47), Key::NumLock);
    }

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
