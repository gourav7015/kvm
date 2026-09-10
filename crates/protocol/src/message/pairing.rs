//! Pairing concern: establishing trust with a device for the first time.
//!
//! Sent only over a pairing-mode connection (see `kvm-net`'s
//! `connect_for_pairing`/`accept_for_pairing`), never over an ordinary
//! trusted connection. The actual trust decision is made by a human
//! comparing a locally-derived code (see `kvm-identity::pairing_code`) —
//! these messages just carry the request/response, no code or secret.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PairingMessage {
    /// Sent by the connecting device once the pairing connection is up.
    Request {
        label: Option<String>,
    },
    Accept,
    Reject,
    Cancel,
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
    fn request_round_trips_with_and_without_a_label() {
        round_trip(Message::Pairing(PairingMessage::Request { label: None }));
        round_trip(Message::Pairing(PairingMessage::Request {
            label: Some("Gourav's MacBook".to_string()),
        }));
    }

    #[test]
    fn accept_reject_cancel_round_trip() {
        round_trip(Message::Pairing(PairingMessage::Accept));
        round_trip(Message::Pairing(PairingMessage::Reject));
        round_trip(Message::Pairing(PairingMessage::Cancel));
    }
}
