//! Input concern: keyboard and mouse events.
//!
//! Keyboard events carry a normalized, OS-independent [`Key`] rather than
//! a raw platform key code — see ADR-0007. A device's `input` crate is
//! responsible for translating its own OS's raw codes to and from `Key`;
//! nothing OS-specific belongs on the wire. The one exception is
//! [`Key::Unknown`], which preserves an unrecognized platform code
//! untranslated rather than silently dropping or corrupting it.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Any additional mouse button, identified by platform-reported index.
    Other(u8),
}

/// Which OS a [`InputMessage::Key`] event originated on — needed by the
/// receiving side to correctly translate modifier roles (e.g. macOS
/// Command vs. Windows Control) for its own platform. See ADR-0007.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformKind {
    MacOs,
    Windows,
    Linux,
}

/// A normalized, OS-independent key identity — a physical/logical key,
/// not a character. Modifier keys distinguish left/right where the
/// capturing OS reports it, since translation rules can differ per side
/// (and the DoD explicitly wants left/right covered).
///
/// This list is deliberately not exhaustive. An unrecognized platform key
/// becomes [`Key::Unknown`] carrying the raw platform code, rather than
/// being dropped or misrepresented as some other key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Key {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,
    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,

    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    /// Command on macOS, Windows key on Windows, Super/Meta on Linux.
    MetaLeft,
    MetaRight,
    CapsLock,

    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,

    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,

    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    Space,

    /// A key this device's `input` crate doesn't yet map to a normalized
    /// variant, carrying its raw platform code so it isn't lost — see the
    /// module docs.
    Unknown(u32),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputMessage {
    Key {
        key: Key,
        state: ButtonState,
        /// True if this is an OS-generated auto-repeat event from a held
        /// key, not a fresh press.
        repeat: bool,
        /// The OS this event was captured on — see [`PlatformKind`].
        source_os: PlatformKind,
    },
    MouseMove {
        dx: i32,
        dy: i32,
    },
    MouseButton {
        button: MouseButton,
        state: ButtonState,
    },
    MouseScroll {
        dx: i32,
        dy: i32,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::{DecodeStatus, decode_frame, encode_frame};

    fn round_trip(message: Message) {
        let frame = encode_frame(&message).unwrap();
        match decode_frame(&frame).unwrap() {
            DecodeStatus::Ready {
                message: decoded,
                consumed,
            } => {
                assert_eq!(decoded, message);
                assert_eq!(consumed, frame.len());
            }
            DecodeStatus::Incomplete => panic!("expected a complete frame"),
        }
    }

    #[test]
    fn key_round_trips_pressed_and_released() {
        round_trip(Message::Input(InputMessage::Key {
            key: Key::A,
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::MacOs,
        }));
        round_trip(Message::Input(InputMessage::Key {
            key: Key::Z,
            state: ButtonState::Released,
            repeat: false,
            source_os: PlatformKind::Windows,
        }));
    }

    #[test]
    fn key_round_trips_with_repeat_set() {
        round_trip(Message::Input(InputMessage::Key {
            key: Key::Space,
            state: ButtonState::Pressed,
            repeat: true,
            source_os: PlatformKind::Linux,
        }));
    }

    #[test]
    fn key_round_trips_every_modifier_including_left_right_variants() {
        for key in [
            Key::ShiftLeft,
            Key::ShiftRight,
            Key::ControlLeft,
            Key::ControlRight,
            Key::AltLeft,
            Key::AltRight,
            Key::MetaLeft,
            Key::MetaRight,
            Key::CapsLock,
        ] {
            round_trip(Message::Input(InputMessage::Key {
                key,
                state: ButtonState::Pressed,
                repeat: false,
                source_os: PlatformKind::MacOs,
            }));
        }
    }

    #[test]
    fn key_round_trips_unknown_with_raw_code_preserved() {
        round_trip(Message::Input(InputMessage::Key {
            key: Key::Unknown(0xDEAD_BEEF),
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::Windows,
        }));
        round_trip(Message::Input(InputMessage::Key {
            key: Key::Unknown(0),
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::Windows,
        }));
    }

    #[test]
    fn mouse_move_round_trips_negative_and_positive_deltas() {
        round_trip(Message::Input(InputMessage::MouseMove {
            dx: i32::MIN,
            dy: i32::MAX,
        }));
        round_trip(Message::Input(InputMessage::MouseMove { dx: 0, dy: 0 }));
    }

    #[test]
    fn mouse_button_round_trips_every_variant() {
        for button in [
            MouseButton::Left,
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Other(255),
        ] {
            round_trip(Message::Input(InputMessage::MouseButton {
                button,
                state: ButtonState::Pressed,
            }));
        }
    }

    #[test]
    fn mouse_scroll_round_trips() {
        round_trip(Message::Input(InputMessage::MouseScroll { dx: -5, dy: 5 }));
    }
}
