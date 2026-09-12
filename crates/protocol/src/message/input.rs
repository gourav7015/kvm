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
    /// module docs. The code is only meaningful on the platform that
    /// produced it (the message's `source_os`); injectors refuse a foreign
    /// one rather than reinterpreting it — see ADR-0007's 2026-09-12 update.
    Unknown(u32),

    // ---- Added in protocol 1.1 --------------------------------------
    //
    // Appended *after* `Unknown` deliberately: postcard encodes an enum
    // variant as its declaration index, so inserting these anywhere
    // earlier would silently renumber `Unknown` (and every key declared
    // after the insertion point) on the wire. `key_wire_indices_are_stable`
    // pins this. Names follow the same W3C `KeyboardEvent.code` scheme as
    // the rest of this enum (`Digit0`, `ArrowUp`, `ShiftLeft`): each names
    // a physical key *position* on a US layout, not the character it types
    // — the receiving OS applies its own keyboard layout, exactly as it
    // already does for letters.
    Minus,
    Equal,
    BracketLeft,
    BracketRight,
    Backslash,
    Semicolon,
    Quote,
    /// The `` ` ``/`~` key.
    Backquote,
    Comma,
    Period,
    Slash,

    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadDecimal,
    NumpadMultiply,
    NumpadAdd,
    NumpadSubtract,
    NumpadDivide,
    NumpadEnter,

    // ---- Added in protocol 1.2 (appended, same reasoning as above) ----
    /// The PC Num Lock key. The same physical key as the Mac keypad's
    /// Clear key: USB HID usage 0x53 is literally "Keypad Num Lock and
    /// Clear".
    NumLock,
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
    /// `dx`/`dy` in 1/120 of a wheel notch (Windows' `WHEEL_DELTA`; a notch
    /// is 3 lines, so one line is 40). Every backend converts to and from
    /// this unit — see `kvm_input::scroll` and ADR-0007's scroll update.
    MouseScroll {
        dx: i32,
        dy: i32,
    },
    /// Protocol 1.3, appended (postcard encodes a variant as its index):
    /// the sender's own Caps Lock state, sent when it becomes the active
    /// source for this receiver and on every change. Caps Lock is a
    /// toggle each machine keeps separately, so forwarding presses let the
    /// two drift apart (real hardware: a press made while controlling the
    /// sender's own screen left the target's Caps Lock reversed). The
    /// receiver sets its own Caps Lock to `on` — see ADR-0007.
    CapsLockState {
        on: bool,
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
    fn key_round_trips_every_protocol_1_1_punctuation_and_numpad_key() {
        for key in [
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
        ] {
            round_trip(Message::Input(InputMessage::Key {
                key,
                state: ButtonState::Pressed,
                repeat: false,
                source_os: PlatformKind::MacOs,
            }));
        }
    }

    /// Pins `Key`'s wire encoding. postcard encodes an enum variant as its
    /// declaration index, so reordering or inserting a variant anywhere but
    /// the end silently changes what every later key means to a peer built
    /// from an older revision. The protocol 1.1 keys were appended after
    /// `Unknown` for exactly this reason; this test is what keeps the next
    /// addition honest too.
    #[test]
    fn key_wire_indices_are_stable() {
        let index = |key: Key| postcard::to_allocvec(&key).unwrap()[0];
        // Protocol 1.0 keys, unchanged.
        assert_eq!(index(Key::A), 0);
        assert_eq!(index(Key::Digit0), 26);
        assert_eq!(index(Key::ShiftLeft), 36);
        assert_eq!(index(Key::F1), 45);
        assert_eq!(index(Key::ArrowUp), 57);
        assert_eq!(index(Key::Space), 70);
        assert_eq!(
            postcard::to_allocvec(&Key::Unknown(5)).unwrap(),
            vec![71, 5],
            "Key::Unknown must keep wire index 71 and its payload"
        );
        // Protocol 1.1 keys, appended after Unknown.
        assert_eq!(index(Key::Minus), 72);
        assert_eq!(index(Key::Slash), 82);
        assert_eq!(index(Key::Numpad0), 83);
        assert_eq!(index(Key::NumpadEnter), 98);
        // Protocol 1.2.
        assert_eq!(index(Key::NumLock), 99);
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

    #[test]
    fn caps_lock_state_round_trips_both_states() {
        round_trip(Message::Input(InputMessage::CapsLockState { on: true }));
        round_trip(Message::Input(InputMessage::CapsLockState { on: false }));
    }

    /// Same reasoning as `key_wire_indices_are_stable`, for `InputMessage`
    /// itself: the protocol 1.3 variant must be appended, never inserted.
    #[test]
    fn input_message_wire_indices_are_stable() {
        let index = |message: InputMessage| postcard::to_allocvec(&message).unwrap()[0];
        assert_eq!(
            index(InputMessage::Key {
                key: Key::A,
                state: ButtonState::Pressed,
                repeat: false,
                source_os: PlatformKind::MacOs,
            }),
            0
        );
        assert_eq!(index(InputMessage::MouseMove { dx: 0, dy: 0 }), 1);
        assert_eq!(
            index(InputMessage::MouseButton {
                button: MouseButton::Left,
                state: ButtonState::Pressed,
            }),
            2
        );
        assert_eq!(index(InputMessage::MouseScroll { dx: 0, dy: 0 }), 3);
        assert_eq!(index(InputMessage::CapsLockState { on: true }), 4);
    }
}
