//! Input concern: keyboard and mouse events.
//!
//! These carry raw key codes and deltas only — modifier translation and
//! edge-switching logic live in the `input`/`core` crates, not here.

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputMessage {
    Key {
        keycode: u32,
        state: ButtonState,
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
            keycode: 0,
            state: ButtonState::Pressed,
        }));
        round_trip(Message::Input(InputMessage::Key {
            keycode: u32::MAX,
            state: ButtonState::Released,
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
