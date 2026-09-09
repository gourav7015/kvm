//! Handshake concern: peer identity exchange when a QUIC connection opens.

use serde::{Deserialize, Serialize};

/// A device's stable identifier: the raw bytes of its Ed25519 public key.
pub type DeviceId = [u8; 32];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HandshakeMessage {
    /// Sent by the connecting peer to introduce itself.
    Hello { device_id: DeviceId },
    /// Sent in response once the peer's identity has been checked against
    /// the local trust store.
    HelloAck { device_id: DeviceId, trusted: bool },
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::message::Message;
    use crate::{DecodeStatus, decode_frame, encode_frame};

    fn round_trip(message: Message) {
        let frame = encode_frame(&message).unwrap();
        let status = decode_frame(&frame).unwrap();
        match status {
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
    fn hello_round_trips() {
        round_trip(Message::Handshake(HandshakeMessage::Hello {
            device_id: [7u8; 32],
        }));
    }

    #[test]
    fn hello_ack_round_trips_trusted_and_untrusted() {
        round_trip(Message::Handshake(HandshakeMessage::HelloAck {
            device_id: [0u8; 32],
            trusted: true,
        }));
        round_trip(Message::Handshake(HandshakeMessage::HelloAck {
            device_id: [255u8; 32],
            trusted: false,
        }));
    }
}
