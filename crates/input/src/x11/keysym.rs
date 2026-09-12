//! X11 keysym ↔ [`Key`] table — pure data, the "thin shim" ADR-0007
//! describes, extended to X11 by ADR-0008. Values verified against
//! `X11/keysymdef.h`, a header whose core-key values have been stable
//! for decades.
//!
//! Keysyms, not keycodes, are the portable layer here: an X11 keycode
//! is a per-server, per-keyboard-driver number with no fixed meaning
//! (unlike macOS's Carbon keycodes or Windows' `VK_*` constants), so
//! the keycode↔keysym mapping has to be queried from the running X
//! server at capture/inject start — see [`super::keymap`]. This module
//! only handles the portable keysym↔[`Key`] half.

use kvm_protocol::Key;

/// Translates an X11 keysym into a normalized [`Key`]. Anything not in
/// the table becomes [`Key::Unknown`] rather than being dropped — see
/// ADR-0007. Unlike the macOS/Windows tables (raw codes are `u16`
/// there), X11 keysyms are natively `u32`, so no code is ever out of
/// range here.
pub fn keysym_to_key(keysym: u32) -> Key {
    match keysym {
        0x0061..=0x007a => letter(keysym as u8), // a-z (the base/unshifted keysym for a letter key)
        0x0041..=0x005a => letter((keysym as u8) + 0x20), // A-Z, normalized to the same Key as its lowercase
        0x0030..=0x0039 => digit(keysym as u8),

        0xffe1 => Key::ShiftLeft,
        0xffe2 => Key::ShiftRight,
        0xffe3 => Key::ControlLeft,
        0xffe4 => Key::ControlRight,
        0xffe9 => Key::AltLeft,
        0xffea => Key::AltRight,
        0xffeb => Key::MetaLeft,
        0xffec => Key::MetaRight,
        0xffe5 => Key::CapsLock,

        0xffbe => Key::F1,
        0xffbf => Key::F2,
        0xffc0 => Key::F3,
        0xffc1 => Key::F4,
        0xffc2 => Key::F5,
        0xffc3 => Key::F6,
        0xffc4 => Key::F7,
        0xffc5 => Key::F8,
        0xffc6 => Key::F9,
        0xffc7 => Key::F10,
        0xffc8 => Key::F11,
        0xffc9 => Key::F12,

        0xff51 => Key::ArrowLeft,
        0xff52 => Key::ArrowUp,
        0xff53 => Key::ArrowRight,
        0xff54 => Key::ArrowDown,
        0xff50 => Key::Home,
        0xff57 => Key::End,
        0xff55 => Key::PageUp,
        0xff56 => Key::PageDown,

        0xff0d => Key::Enter,
        0xff1b => Key::Escape,
        0xff08 => Key::Backspace,
        0xffff => Key::Delete,
        0xff09 => Key::Tab,
        0x0020 => Key::Space,

        0x002d => Key::Minus,        // XK_minus
        0x003d => Key::Equal,        // XK_equal
        0x005b => Key::BracketLeft,  // XK_bracketleft
        0x005d => Key::BracketRight, // XK_bracketright
        0x005c => Key::Backslash,    // XK_backslash
        0x003b => Key::Semicolon,    // XK_semicolon
        0x0027 => Key::Quote,        // XK_apostrophe
        0x0060 => Key::Backquote,    // XK_grave
        0x002c => Key::Comma,        // XK_comma
        0x002e => Key::Period,       // XK_period
        0x002f => Key::Slash,        // XK_slash

        0xffb0 => Key::Numpad0, // XK_KP_0
        0xffb1 => Key::Numpad1,
        0xffb2 => Key::Numpad2,
        0xffb3 => Key::Numpad3,
        0xffb4 => Key::Numpad4,
        0xffb5 => Key::Numpad5,
        0xffb6 => Key::Numpad6,
        0xffb7 => Key::Numpad7,
        0xffb8 => Key::Numpad8,
        0xffb9 => Key::Numpad9,        // XK_KP_9
        0xffae => Key::NumpadDecimal,  // XK_KP_Decimal
        0xffaa => Key::NumpadMultiply, // XK_KP_Multiply
        0xffab => Key::NumpadAdd,      // XK_KP_Add
        0xffad => Key::NumpadSubtract, // XK_KP_Subtract
        0xffaf => Key::NumpadDivide,   // XK_KP_Divide
        0xff8d => Key::NumpadEnter,    // XK_KP_Enter

        other => Key::Unknown(other),
    }
}

/// The inverse of [`keysym_to_key`], for injection. Always returns a
/// keysym for a [`Key`] this table maps — including `Key::Unknown`,
/// whose raw code passes straight through, since X11 keysyms are
/// natively `u32` (unlike macOS/Windows' narrower raw code spaces,
/// there's no truncation case to report `None` for here).
pub fn key_to_keysym(key: Key) -> u32 {
    match key {
        Key::A => 0x0061,
        Key::B => 0x0062,
        Key::C => 0x0063,
        Key::D => 0x0064,
        Key::E => 0x0065,
        Key::F => 0x0066,
        Key::G => 0x0067,
        Key::H => 0x0068,
        Key::I => 0x0069,
        Key::J => 0x006a,
        Key::K => 0x006b,
        Key::L => 0x006c,
        Key::M => 0x006d,
        Key::N => 0x006e,
        Key::O => 0x006f,
        Key::P => 0x0070,
        Key::Q => 0x0071,
        Key::R => 0x0072,
        Key::S => 0x0073,
        Key::T => 0x0074,
        Key::U => 0x0075,
        Key::V => 0x0076,
        Key::W => 0x0077,
        Key::X => 0x0078,
        Key::Y => 0x0079,
        Key::Z => 0x007a,

        Key::Digit0 => 0x0030,
        Key::Digit1 => 0x0031,
        Key::Digit2 => 0x0032,
        Key::Digit3 => 0x0033,
        Key::Digit4 => 0x0034,
        Key::Digit5 => 0x0035,
        Key::Digit6 => 0x0036,
        Key::Digit7 => 0x0037,
        Key::Digit8 => 0x0038,
        Key::Digit9 => 0x0039,

        Key::ShiftLeft => 0xffe1,
        Key::ShiftRight => 0xffe2,
        Key::ControlLeft => 0xffe3,
        Key::ControlRight => 0xffe4,
        Key::AltLeft => 0xffe9,
        Key::AltRight => 0xffea,
        Key::MetaLeft => 0xffeb,
        Key::MetaRight => 0xffec,
        Key::CapsLock => 0xffe5,

        Key::F1 => 0xffbe,
        Key::F2 => 0xffbf,
        Key::F3 => 0xffc0,
        Key::F4 => 0xffc1,
        Key::F5 => 0xffc2,
        Key::F6 => 0xffc3,
        Key::F7 => 0xffc4,
        Key::F8 => 0xffc5,
        Key::F9 => 0xffc6,
        Key::F10 => 0xffc7,
        Key::F11 => 0xffc8,
        Key::F12 => 0xffc9,

        Key::ArrowLeft => 0xff51,
        Key::ArrowUp => 0xff52,
        Key::ArrowRight => 0xff53,
        Key::ArrowDown => 0xff54,
        Key::Home => 0xff50,
        Key::End => 0xff57,
        Key::PageUp => 0xff55,
        Key::PageDown => 0xff56,

        Key::Enter => 0xff0d,
        Key::Escape => 0xff1b,
        Key::Backspace => 0xff08,
        Key::Delete => 0xffff,
        Key::Tab => 0xff09,
        Key::Space => 0x0020,

        Key::Minus => 0x002d,
        Key::Equal => 0x003d,
        Key::BracketLeft => 0x005b,
        Key::BracketRight => 0x005d,
        Key::Backslash => 0x005c,
        Key::Semicolon => 0x003b,
        Key::Quote => 0x0027,
        Key::Backquote => 0x0060,
        Key::Comma => 0x002c,
        Key::Period => 0x002e,
        Key::Slash => 0x002f,

        Key::Numpad0 => 0xffb0,
        Key::Numpad1 => 0xffb1,
        Key::Numpad2 => 0xffb2,
        Key::Numpad3 => 0xffb3,
        Key::Numpad4 => 0xffb4,
        Key::Numpad5 => 0xffb5,
        Key::Numpad6 => 0xffb6,
        Key::Numpad7 => 0xffb7,
        Key::Numpad8 => 0xffb8,
        Key::Numpad9 => 0xffb9,
        Key::NumpadDecimal => 0xffae,
        Key::NumpadMultiply => 0xffaa,
        Key::NumpadAdd => 0xffab,
        Key::NumpadSubtract => 0xffad,
        Key::NumpadDivide => 0xffaf,
        Key::NumpadEnter => 0xff8d,

        Key::Unknown(code) => code,
    }
}

fn letter(ascii_lower: u8) -> Key {
    match ascii_lower {
        b'a' => Key::A,
        b'b' => Key::B,
        b'c' => Key::C,
        b'd' => Key::D,
        b'e' => Key::E,
        b'f' => Key::F,
        b'g' => Key::G,
        b'h' => Key::H,
        b'i' => Key::I,
        b'j' => Key::J,
        b'k' => Key::K,
        b'l' => Key::L,
        b'm' => Key::M,
        b'n' => Key::N,
        b'o' => Key::O,
        b'p' => Key::P,
        b'q' => Key::Q,
        b'r' => Key::R,
        b's' => Key::S,
        b't' => Key::T,
        b'u' => Key::U,
        b'v' => Key::V,
        b'w' => Key::W,
        b'x' => Key::X,
        b'y' => Key::Y,
        b'z' => Key::Z,
        other => Key::Unknown(other as u32),
    }
}

fn digit(ascii_digit: u8) -> Key {
    match ascii_digit {
        b'0' => Key::Digit0,
        b'1' => Key::Digit1,
        b'2' => Key::Digit2,
        b'3' => Key::Digit3,
        b'4' => Key::Digit4,
        b'5' => Key::Digit5,
        b'6' => Key::Digit6,
        b'7' => Key::Digit7,
        b'8' => Key::Digit8,
        b'9' => Key::Digit9,
        other => Key::Unknown(other as u32),
    }
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
    ];

    #[test]
    fn every_mapped_key_round_trips_through_the_x11_keysym() {
        for &key in ALL_MAPPED_KEYS {
            let keysym = key_to_keysym(key);
            assert_eq!(
                keysym_to_key(keysym),
                key,
                "keysym {keysym:#x} for {key:?} did not round-trip"
            );
        }
    }

    #[test]
    fn left_and_right_modifier_variants_map_to_distinct_keysyms() {
        let pairs = [
            (Key::ShiftLeft, Key::ShiftRight),
            (Key::ControlLeft, Key::ControlRight),
            (Key::AltLeft, Key::AltRight),
            (Key::MetaLeft, Key::MetaRight),
        ];
        for (left, right) in pairs {
            assert_ne!(
                key_to_keysym(left),
                key_to_keysym(right),
                "{left:?} and {right:?} must map to distinct keysyms"
            );
        }
    }

    #[test]
    fn uppercase_ascii_keysym_normalizes_to_the_same_key_as_lowercase() {
        // A keyboard mapping could plausibly report the shifted
        // (uppercase) keysym at the queried column; both must resolve
        // to the same physical-key identity.
        assert_eq!(keysym_to_key(0x0041), Key::A); // XK_A
        assert_eq!(keysym_to_key(0x0061), Key::A); // XK_a
    }

    #[test]
    fn unrecognized_keysym_becomes_unknown_not_dropped() {
        // 0x1008ff02 is XF86XK_PowerOff, not in the mapped table.
        assert_eq!(keysym_to_key(0x1008_ff02), Key::Unknown(0x1008_ff02));
    }

    #[test]
    fn unknown_keysym_round_trips_exactly_since_x11_keysyms_are_native_u32() {
        assert_eq!(key_to_keysym(Key::Unknown(0x1008_ff02)), 0x1008_ff02);
        assert_eq!(key_to_keysym(Key::Unknown(u32::MAX)), u32::MAX);
    }
}
