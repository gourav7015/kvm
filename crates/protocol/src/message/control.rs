//! Control concern: liveness, active-device switching, disconnect.

use serde::{Deserialize, Serialize};

use super::handshake::DeviceId;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ControlMessage {
    Ping { nonce: u64 },
    Pong { nonce: u64 },
    SwitchActive { device_id: DeviceId },
    Disconnect { reason: String },
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
