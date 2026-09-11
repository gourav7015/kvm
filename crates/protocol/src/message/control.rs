//! Control concern: liveness, active-device switching, disconnect.

use serde::{Deserialize, Serialize};

use super::handshake::DeviceId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlMessage {
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// Sent to `device_id` at the moment it becomes the active input
    /// target (see ADR-0009). `cursor_position` is where the sender has
    /// computed the pointer should land on the receiver's own screen —
    /// resolution-aware, already accounting for the two devices'
    /// differing screen sizes — so the receiver only has to warp its
    /// local cursor there, not do any of that math itself.
    SwitchActive {
        device_id: DeviceId,
        cursor_position: (i32, i32),
    },
    /// Sent once after a peer connection is established (and again if a
    /// device's screen configuration changes) so the other side's edge-
    /// switching math has real bounds instead of a guess — see ADR-0009
    /// §6. One combined screen boundary per device; multi-monitor detail
    /// is out of scope for Phase 4.
    ScreenInfo {
        width: u32,
        height: u32,
    },
    Disconnect {
        reason: String,
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
    fn ping_pong_round_trip() {
        round_trip(Message::Control(ControlMessage::Ping { nonce: 0 }));
        round_trip(Message::Control(ControlMessage::Ping { nonce: u64::MAX }));
        round_trip(Message::Control(ControlMessage::Pong { nonce: 42 }));
    }

    #[test]
    fn switch_active_round_trips() {
        round_trip(Message::Control(ControlMessage::SwitchActive {
            device_id: [1u8; 32],
            cursor_position: (0, 0),
        }));
        round_trip(Message::Control(ControlMessage::SwitchActive {
            device_id: [2u8; 32],
            cursor_position: (-1, 4000),
        }));
    }

    #[test]
    fn screen_info_round_trips() {
        round_trip(Message::Control(ControlMessage::ScreenInfo {
            width: 1920,
            height: 1080,
        }));
        round_trip(Message::Control(ControlMessage::ScreenInfo {
            width: 0,
            height: 0,
        }));
    }

    #[test]
    fn disconnect_round_trips_including_empty_reason() {
        round_trip(Message::Control(ControlMessage::Disconnect {
            reason: String::new(),
        }));
        round_trip(Message::Control(ControlMessage::Disconnect {
            reason: "peer requested shutdown".to_string(),
        }));
    }
}
