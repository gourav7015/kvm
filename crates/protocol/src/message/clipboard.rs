//! Clipboard concern: cross-device clipboard content updates.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipboardContent {
    Text(String),
    Url(String),
    /// Raw image bytes plus a format hint (e.g. `"png"`).
    Image {
        format: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipboardMessage {
    Update { content: ClipboardContent },
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
    fn text_round_trips_including_empty_and_unicode() {
        round_trip(Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Text(String::new()),
        }));
        round_trip(Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Text("héllo wörld 🎉".to_string()),
        }));
    }

    #[test]
    fn url_round_trips() {
        round_trip(Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Url("https://example.com/path?q=1".to_string()),
        }));
    }

    #[test]
    fn image_round_trips_including_empty_bytes() {
        round_trip(Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Image {
                format: "png".to_string(),
                bytes: vec![],
            },
        }));
        round_trip(Message::Clipboard(ClipboardMessage::Update {
            content: ClipboardContent::Image {
                format: "png".to_string(),
                bytes: vec![0, 1, 2, 255, 254],
            },
        }));
    }
}
