//! File transfer concern: offer/accept/reject, chunked data, completion.

use serde::{Deserialize, Serialize};

pub type TransferId = u64;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransferMessage {
    Offer {
        id: TransferId,
        file_name: String,
        size: u64,
    },
    Accept {
        id: TransferId,
    },
    Reject {
        id: TransferId,
    },
    Chunk {
        id: TransferId,
        offset: u64,
        data: Vec<u8>,
    },
    Complete {
        id: TransferId,
    },
    Cancel {
        id: TransferId,
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
    fn offer_round_trips() {
        round_trip(Message::Transfer(TransferMessage::Offer {
            id: 1,
            file_name: "report.pdf".to_string(),
            size: 123_456,
        }));
    }

    #[test]
    fn accept_reject_complete_cancel_round_trip() {
        round_trip(Message::Transfer(TransferMessage::Accept { id: 1 }));
        round_trip(Message::Transfer(TransferMessage::Reject { id: 1 }));
        round_trip(Message::Transfer(TransferMessage::Complete { id: 1 }));
        round_trip(Message::Transfer(TransferMessage::Cancel { id: 1 }));
    }

    #[test]
    fn chunk_round_trips_including_empty_data() {
        round_trip(Message::Transfer(TransferMessage::Chunk {
            id: 1,
            offset: 0,
            data: vec![],
        }));
        round_trip(Message::Transfer(TransferMessage::Chunk {
            id: 1,
            offset: u64::MAX,
            data: vec![1, 2, 3, 4, 5],
        }));
    }
}
