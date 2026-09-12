//! Mouse buttons in the protocol's platform-neutral numbering — see
//! ADR-0007's 2026-09-13 mouse update.
//!
//! `MouseButton::Other(n)` carries the mouse's own **USB HID button
//! number**: 1 left, 2 right, 3 middle (those three also have named
//! variants), **4 back, 5 forward**, 6 and up further buttons. Every OS
//! numbers the same physical buttons differently, so each backend converts
//! through these functions and the button pressed on one machine is the
//! button pressed on the other.
//!
//! Real hardware: before this, each backend put its *own* number on the
//! wire — the macOS capture reported every extra button as a middle click,
//! the Windows capture ignored them, the Windows injector always pressed
//! Back — so a Mac mouse's Back button middle-clicked on Linux and Windows.

use kvm_protocol::MouseButton;

/// The Back button's HID number.
pub const BACK: u8 = 4;
/// The Forward button's HID number.
pub const FORWARD: u8 = 5;

/// macOS's `kCGMouseEventButtonNumber` is the HID number minus one:
/// 0 left, 1 right, 2 middle, 3 back, 4 forward, and so on.
pub fn from_macos_button_number(number: i64) -> MouseButton {
    match number {
        0 => MouseButton::Left,
        1 => MouseButton::Right,
        2 => MouseButton::Middle,
        n => MouseButton::Other(n.saturating_add(1).clamp(0, i64::from(u8::MAX)) as u8),
    }
}

/// The inverse of [`from_macos_button_number`].
pub fn to_macos_button_number(button: MouseButton) -> i64 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Other(n) => i64::from(n.max(1)) - 1,
    }
}

/// X11 button numbers: 1 left, 2 middle, 3 right, 4–7 the scroll wheel
/// (not buttons — `None`), then 8 back, 9 forward, 10 and up further
/// buttons: HID button `n` (from 4) is X11 button `n + 4`.
pub fn from_x11_button(detail: u32) -> Option<MouseButton> {
    match detail {
        1 => Some(MouseButton::Left),
        2 => Some(MouseButton::Middle),
        3 => Some(MouseButton::Right),
        4..=7 => None,
        d => Some(MouseButton::Other(
            d.saturating_sub(4).min(u32::from(u8::MAX)) as u8,
        )),
    }
}

/// The inverse of [`from_x11_button`].
pub fn to_x11_button(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left | MouseButton::Other(0 | 1) => 1,
        MouseButton::Right | MouseButton::Other(2) => 3,
        MouseButton::Middle | MouseButton::Other(3) => 2,
        MouseButton::Other(n) => n.saturating_add(4),
    }
}

/// Windows has only two extra buttons, identified in `mouseData` as
/// `XBUTTON1` (1) = Back and `XBUTTON2` (2) = Forward.
pub fn from_windows_xbutton(xbutton: u16) -> Option<MouseButton> {
    match xbutton {
        1 => Some(MouseButton::Other(BACK)),
        2 => Some(MouseButton::Other(FORWARD)),
        _ => None,
    }
}

/// The inverse of [`from_windows_xbutton`]; `None` for any button Windows
/// cannot inject (it has nothing beyond Back and Forward).
pub fn to_windows_xbutton(button: MouseButton) -> Option<u16> {
    match button {
        MouseButton::Other(BACK) => Some(1),
        MouseButton::Other(FORWARD) => Some(2),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Regression test for the Back button middle-clicking on the other
    /// machine: Back pressed on a Mac mouse is Back on Linux and Windows.
    #[test]
    fn a_macs_back_and_forward_buttons_are_back_and_forward_everywhere() {
        let back = from_macos_button_number(3);
        let forward = from_macos_button_number(4);
        assert_eq!(back, MouseButton::Other(BACK));
        assert_eq!(forward, MouseButton::Other(FORWARD));
        assert_eq!(to_x11_button(back), 8);
        assert_eq!(to_x11_button(forward), 9);
        assert_eq!(to_windows_xbutton(back), Some(1));
        assert_eq!(to_windows_xbutton(forward), Some(2));
    }

    #[test]
    fn every_platforms_numbering_round_trips() {
        for number in 0..=10 {
            assert_eq!(
                to_macos_button_number(from_macos_button_number(number)),
                number
            );
        }
        for detail in [1, 2, 3, 8, 9, 10, 20] {
            let button = from_x11_button(detail).unwrap();
            assert_eq!(u32::from(to_x11_button(button)), detail);
        }
        for xbutton in [1, 2] {
            let button = from_windows_xbutton(xbutton).unwrap();
            assert_eq!(to_windows_xbutton(button), Some(xbutton));
        }
    }

    #[test]
    fn linux_and_windows_back_and_forward_reach_the_mac_as_back_and_forward() {
        assert_eq!(to_macos_button_number(from_x11_button(8).unwrap()), 3);
        assert_eq!(to_macos_button_number(from_windows_xbutton(2).unwrap()), 4);
    }

    #[test]
    fn the_x11_scroll_wheel_is_never_a_button() {
        for detail in 4..=7 {
            assert_eq!(from_x11_button(detail), None);
        }
    }

    #[test]
    fn windows_cannot_inject_buttons_beyond_forward() {
        assert_eq!(to_windows_xbutton(MouseButton::Other(6)), None);
        assert_eq!(to_windows_xbutton(MouseButton::Middle), None);
        assert_eq!(from_windows_xbutton(3), None);
    }

    #[test]
    fn named_buttons_keep_their_platform_numbers() {
        assert_eq!(to_macos_button_number(MouseButton::Left), 0);
        assert_eq!(to_macos_button_number(MouseButton::Right), 1);
        assert_eq!(to_macos_button_number(MouseButton::Middle), 2);
        assert_eq!(to_x11_button(MouseButton::Left), 1);
        assert_eq!(to_x11_button(MouseButton::Middle), 2);
        assert_eq!(to_x11_button(MouseButton::Right), 3);
    }
}
