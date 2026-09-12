//! Windows virtual-key code ↔ [`Key`] table — pure data, the "thin shim"
//! ADR-0007 describes. Uses the `VK_*` constants from the `windows`
//! crate directly rather than re-declaring numeric literals.

use kvm_protocol::Key;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_0, VK_1, VK_2, VK_3, VK_4, VK_5, VK_6, VK_7, VK_8, VK_9, VK_A, VK_ADD, VK_B,
    VK_BACK, VK_C, VK_CAPITAL, VK_D, VK_DECIMAL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_E, VK_END,
    VK_ESCAPE, VK_F, VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11,
    VK_F12, VK_G, VK_H, VK_HOME, VK_I, VK_J, VK_K, VK_L, VK_LCONTROL, VK_LEFT, VK_LMENU, VK_LSHIFT,
    VK_LWIN, VK_M, VK_MULTIPLY, VK_N, VK_NEXT, VK_NUMLOCK, VK_NUMPAD0, VK_NUMPAD1, VK_NUMPAD2,
    VK_NUMPAD3, VK_NUMPAD4, VK_NUMPAD5, VK_NUMPAD6, VK_NUMPAD7, VK_NUMPAD8, VK_NUMPAD9, VK_O,
    VK_OEM_1, VK_OEM_2, VK_OEM_3, VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA,
    VK_OEM_MINUS, VK_OEM_PERIOD, VK_OEM_PLUS, VK_P, VK_PRIOR, VK_Q, VK_R, VK_RCONTROL, VK_RETURN,
    VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_RWIN, VK_S, VK_SPACE, VK_SUBTRACT, VK_T, VK_TAB, VK_U, VK_UP,
    VK_V, VK_W, VK_X, VK_Y, VK_Z,
};

/// Translates a raw Windows virtual-key code into a normalized [`Key`].
/// Anything not in the table becomes [`Key::Unknown`] rather than being
/// dropped — see ADR-0007.
pub fn vk_to_key(vk: VIRTUAL_KEY) -> Key {
    match vk {
        VK_A => Key::A,
        VK_B => Key::B,
        VK_C => Key::C,
        VK_D => Key::D,
        VK_E => Key::E,
        VK_F => Key::F,
        VK_G => Key::G,
        VK_H => Key::H,
        VK_I => Key::I,
        VK_J => Key::J,
        VK_K => Key::K,
        VK_L => Key::L,
        VK_M => Key::M,
        VK_N => Key::N,
        VK_O => Key::O,
        VK_P => Key::P,
        VK_Q => Key::Q,
        VK_R => Key::R,
        VK_S => Key::S,
        VK_T => Key::T,
        VK_U => Key::U,
        VK_V => Key::V,
        VK_W => Key::W,
        VK_X => Key::X,
        VK_Y => Key::Y,
        VK_Z => Key::Z,

        VK_0 => Key::Digit0,
        VK_1 => Key::Digit1,
        VK_2 => Key::Digit2,
        VK_3 => Key::Digit3,
        VK_4 => Key::Digit4,
        VK_5 => Key::Digit5,
        VK_6 => Key::Digit6,
        VK_7 => Key::Digit7,
        VK_8 => Key::Digit8,
        VK_9 => Key::Digit9,

        VK_LSHIFT => Key::ShiftLeft,
        VK_RSHIFT => Key::ShiftRight,
        VK_LCONTROL => Key::ControlLeft,
        VK_RCONTROL => Key::ControlRight,
        VK_LMENU => Key::AltLeft,
        VK_RMENU => Key::AltRight,
        VK_LWIN => Key::MetaLeft,
        VK_RWIN => Key::MetaRight,
        VK_CAPITAL => Key::CapsLock,

        VK_F1 => Key::F1,
        VK_F2 => Key::F2,
        VK_F3 => Key::F3,
        VK_F4 => Key::F4,
        VK_F5 => Key::F5,
        VK_F6 => Key::F6,
        VK_F7 => Key::F7,
        VK_F8 => Key::F8,
        VK_F9 => Key::F9,
        VK_F10 => Key::F10,
        VK_F11 => Key::F11,
        VK_F12 => Key::F12,

        VK_UP => Key::ArrowUp,
        VK_DOWN => Key::ArrowDown,
        VK_LEFT => Key::ArrowLeft,
        VK_RIGHT => Key::ArrowRight,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,

        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_TAB => Key::Tab,
        VK_SPACE => Key::Space,

        // The `VK_OEM_*` codes name US-layout key *positions*, matching
        // `Key`'s own positional meaning; Windows applies the active layout.
        VK_OEM_MINUS => Key::Minus,
        VK_OEM_PLUS => Key::Equal,
        VK_OEM_4 => Key::BracketLeft,
        VK_OEM_6 => Key::BracketRight,
        VK_OEM_5 => Key::Backslash,
        VK_OEM_1 => Key::Semicolon,
        VK_OEM_7 => Key::Quote,
        VK_OEM_3 => Key::Backquote,
        VK_OEM_COMMA => Key::Comma,
        VK_OEM_PERIOD => Key::Period,
        VK_OEM_2 => Key::Slash,

        VK_NUMPAD0 => Key::Numpad0,
        VK_NUMPAD1 => Key::Numpad1,
        VK_NUMPAD2 => Key::Numpad2,
        VK_NUMPAD3 => Key::Numpad3,
        VK_NUMPAD4 => Key::Numpad4,
        VK_NUMPAD5 => Key::Numpad5,
        VK_NUMPAD6 => Key::Numpad6,
        VK_NUMPAD7 => Key::Numpad7,
        VK_NUMPAD8 => Key::Numpad8,
        VK_NUMPAD9 => Key::Numpad9,
        VK_DECIMAL => Key::NumpadDecimal,
        VK_MULTIPLY => Key::NumpadMultiply,
        VK_ADD => Key::NumpadAdd,
        VK_SUBTRACT => Key::NumpadSubtract,
        VK_DIVIDE => Key::NumpadDivide,
        VK_NUMLOCK => Key::NumLock,
        // No arm for `Key::NumpadEnter`: Windows has no distinct VK for it
        // (it's `VK_RETURN` plus the extended-key flag), so a captured
        // numpad Enter reads back as `Key::Enter` here.
        other => Key::Unknown(other.0 as u32),
    }
}

/// The inverse of [`vk_to_key`], for injection. Returns `None` for a
/// [`Key`] this table has no Windows code for — see the note on
/// [`crate::macos::keycode::key_to_keycode`] about cross-platform
/// `Unknown` codes not necessarily being portable.
pub fn key_to_vk(key: Key) -> Option<VIRTUAL_KEY> {
    Some(match key {
        Key::A => VK_A,
        Key::B => VK_B,
        Key::C => VK_C,
        Key::D => VK_D,
        Key::E => VK_E,
        Key::F => VK_F,
        Key::G => VK_G,
        Key::H => VK_H,
        Key::I => VK_I,
        Key::J => VK_J,
        Key::K => VK_K,
        Key::L => VK_L,
        Key::M => VK_M,
        Key::N => VK_N,
        Key::O => VK_O,
        Key::P => VK_P,
        Key::Q => VK_Q,
        Key::R => VK_R,
        Key::S => VK_S,
        Key::T => VK_T,
        Key::U => VK_U,
        Key::V => VK_V,
        Key::W => VK_W,
        Key::X => VK_X,
        Key::Y => VK_Y,
        Key::Z => VK_Z,

        Key::Digit0 => VK_0,
        Key::Digit1 => VK_1,
        Key::Digit2 => VK_2,
        Key::Digit3 => VK_3,
        Key::Digit4 => VK_4,
        Key::Digit5 => VK_5,
        Key::Digit6 => VK_6,
        Key::Digit7 => VK_7,
        Key::Digit8 => VK_8,
        Key::Digit9 => VK_9,

        Key::ShiftLeft => VK_LSHIFT,
        Key::ShiftRight => VK_RSHIFT,
        Key::ControlLeft => VK_LCONTROL,
        Key::ControlRight => VK_RCONTROL,
        Key::AltLeft => VK_LMENU,
        Key::AltRight => VK_RMENU,
        Key::MetaLeft => VK_LWIN,
        Key::MetaRight => VK_RWIN,
        Key::CapsLock => VK_CAPITAL,

        Key::F1 => VK_F1,
        Key::F2 => VK_F2,
        Key::F3 => VK_F3,
        Key::F4 => VK_F4,
        Key::F5 => VK_F5,
        Key::F6 => VK_F6,
        Key::F7 => VK_F7,
        Key::F8 => VK_F8,
        Key::F9 => VK_F9,
        Key::F10 => VK_F10,
        Key::F11 => VK_F11,
        Key::F12 => VK_F12,

        Key::ArrowUp => VK_UP,
        Key::ArrowDown => VK_DOWN,
        Key::ArrowLeft => VK_LEFT,
        Key::ArrowRight => VK_RIGHT,
        Key::Home => VK_HOME,
        Key::End => VK_END,
        Key::PageUp => VK_PRIOR,
        Key::PageDown => VK_NEXT,

        Key::Enter => VK_RETURN,
        Key::Escape => VK_ESCAPE,
        Key::Backspace => VK_BACK,
        Key::Delete => VK_DELETE,
        Key::Tab => VK_TAB,
        Key::Space => VK_SPACE,

        Key::Minus => VK_OEM_MINUS,
        Key::Equal => VK_OEM_PLUS,
        Key::BracketLeft => VK_OEM_4,
        Key::BracketRight => VK_OEM_6,
        Key::Backslash => VK_OEM_5,
        Key::Semicolon => VK_OEM_1,
        Key::Quote => VK_OEM_7,
        Key::Backquote => VK_OEM_3,
        Key::Comma => VK_OEM_COMMA,
        Key::Period => VK_OEM_PERIOD,
        Key::Slash => VK_OEM_2,

        Key::Numpad0 => VK_NUMPAD0,
        Key::Numpad1 => VK_NUMPAD1,
        Key::Numpad2 => VK_NUMPAD2,
        Key::Numpad3 => VK_NUMPAD3,
        Key::Numpad4 => VK_NUMPAD4,
        Key::Numpad5 => VK_NUMPAD5,
        Key::Numpad6 => VK_NUMPAD6,
        Key::Numpad7 => VK_NUMPAD7,
        Key::Numpad8 => VK_NUMPAD8,
        Key::Numpad9 => VK_NUMPAD9,
        Key::NumpadDecimal => VK_DECIMAL,
        Key::NumpadMultiply => VK_MULTIPLY,
        Key::NumpadAdd => VK_ADD,
        Key::NumpadSubtract => VK_SUBTRACT,
        Key::NumpadDivide => VK_DIVIDE,
        // See `vk_to_key`: injected as a plain Return, which types Enter.
        Key::NumpadEnter => VK_RETURN,
        Key::NumLock => VK_NUMLOCK,

        Key::Unknown(code) if code <= u16::MAX as u32 => VIRTUAL_KEY(code as u16),
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
        Key::NumLock,
        // `Key::NumpadEnter` deliberately excluded: it injects as
        // `VK_RETURN`, which reads back as `Key::Enter` — see
        // `numpad_enter_injects_as_return`.
    ];

    #[test]
    fn num_lock_maps_to_vk_numlock() {
        assert_eq!(key_to_vk(Key::NumLock), Some(VK_NUMLOCK));
        assert_eq!(vk_to_key(VK_NUMLOCK), Key::NumLock);
    }

    #[test]
    fn numpad_enter_injects_as_return() {
        assert_eq!(key_to_vk(Key::NumpadEnter), Some(VK_RETURN));
    }

    /// The Windows half of the Phase 4 acceptance-run keyboard regression
    /// (ADR-0007's 2026-09-12 update): each key must inject its own named
    /// VK, not the VK that happens to share the Mac's raw keycode number.
    #[test]
    fn keys_from_the_acceptance_log_inject_their_own_vk_not_the_mac_numbers() {
        assert_eq!(key_to_vk(Key::Backquote), Some(VK_OEM_3)); // was VK 0x32, the digit 2
        assert_eq!(key_to_vk(Key::NumpadEnter), Some(VK_RETURN)); // was VK 0x4C, L
        assert_eq!(key_to_vk(Key::Numpad9), Some(VK_NUMPAD9)); // was VK 0x5C, the Windows key
        assert_ne!(key_to_vk(Key::Numpad9), Some(VK_RWIN));
        assert_eq!(key_to_vk(Key::BracketLeft), Some(VK_OEM_4)); // was VK 0x21, Page Up
        assert_eq!(key_to_vk(Key::Minus), Some(VK_OEM_MINUS)); // was VK 0x1B, Escape
        assert_eq!(key_to_vk(Key::Numpad0), Some(VK_NUMPAD0)); // was VK 0x52, R
    }

    #[test]
    fn every_mapped_key_round_trips_through_the_windows_code() {
        for &key in ALL_MAPPED_KEYS {
            let vk = key_to_vk(key).unwrap_or_else(|| panic!("{key:?} has no Windows VK code"));
            assert_eq!(
                vk_to_key(vk),
                key,
                "VK {vk:?} for {key:?} did not round-trip"
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
            let left_vk = key_to_vk(left).unwrap();
            let right_vk = key_to_vk(right).unwrap();
            assert_ne!(
                left_vk, right_vk,
                "{left:?} and {right:?} must map to distinct Windows VK codes"
            );
        }
    }

    #[test]
    fn unrecognized_code_becomes_unknown_not_dropped() {
        // 0x07 is reserved/undefined in the VK_* table used above.
        assert_eq!(vk_to_key(VIRTUAL_KEY(0x07)), Key::Unknown(0x07));
    }

    #[test]
    fn unknown_with_in_range_code_maps_back_to_the_same_raw_code() {
        assert_eq!(key_to_vk(Key::Unknown(0x07)), Some(VIRTUAL_KEY(0x07)));
    }

    #[test]
    fn unknown_with_out_of_range_code_has_no_windows_code() {
        assert_eq!(key_to_vk(Key::Unknown(u32::MAX)), None);
    }
}
