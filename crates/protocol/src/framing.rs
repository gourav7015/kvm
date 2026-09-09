//! Length-prefixed, version-tagged framing around encoded [`Message`]s.
//!
//! This is pure byte-buffer logic — no sockets, no async. `net` reads bytes
//! off a QUIC stream and hands them here incrementally; [`decode_frame`] is
//! designed to be called repeatedly as more bytes arrive.

use crate::error::ProtocolError;
use crate::message::Message;
use crate::version::{PROTOCOL_MAJOR, PROTOCOL_MINOR, is_compatible};

/// Maximum allowed encoded payload size for a single frame (16 MiB).
///
/// Bounds how much a corrupted or adversarial length field can claim,
/// independent of how many bytes have actually arrived — decoding rejects
/// an over-large declared size immediately rather than waiting to buffer
/// that many bytes.
pub const MAX_FRAME_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

/// `payload_len: u32` + `major: u8` + `minor: u8`.
const HEADER_LEN: usize = 4 + 1 + 1;

/// Outcome of attempting to decode one frame from the front of a buffer.
#[derive(Debug, PartialEq)]
pub enum DecodeStatus {
    /// Not enough bytes are available yet to decode a complete frame; call
    /// again once more bytes have arrived.
    Incomplete,
    /// A full frame was decoded from the front of the buffer.
    Ready {
        message: Message,
        /// Number of bytes consumed from the front of the buffer — the
        /// caller should advance past this many bytes before decoding
        /// again.
        consumed: usize,
    },
}

/// Encodes a message into a length-prefixed, version-tagged frame.
pub fn encode_frame(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    let payload =
        postcard::to_allocvec(message).map_err(|e| ProtocolError::Encode(e.to_string()))?;

    let payload_len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| ProtocolError::FrameTooLarge {
            size: u32::MAX,
            max: MAX_FRAME_PAYLOAD_SIZE,
        })?;

    if payload_len > MAX_FRAME_PAYLOAD_SIZE {
        return Err(ProtocolError::FrameTooLarge {
            size: payload_len,
            max: MAX_FRAME_PAYLOAD_SIZE,
        });
    }

    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&payload_len.to_le_bytes());
    frame.push(PROTOCOL_MAJOR);
    frame.push(PROTOCOL_MINOR);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Attempts to decode one frame from the front of `buf`.
///
/// Never panics, regardless of the contents of `buf`: malformed, truncated,
/// or adversarial input always produces `Ok(DecodeStatus::Incomplete)` or
/// `Err(ProtocolError)`, never a panic or an out-of-bounds access.
pub fn decode_frame(buf: &[u8]) -> Result<DecodeStatus, ProtocolError> {
    if buf.len() < HEADER_LEN {
        return Ok(DecodeStatus::Incomplete);
    }

    let mut len_bytes = [0u8; 4];
    len_bytes.copy_from_slice(&buf[0..4]);
    let payload_len = u32::from_le_bytes(len_bytes);

    if payload_len > MAX_FRAME_PAYLOAD_SIZE {
        return Err(ProtocolError::FrameTooLarge {
            size: payload_len,
            max: MAX_FRAME_PAYLOAD_SIZE,
        });
    }

    let peer_major = buf[4];
    let peer_minor = buf[5];

    let total_len = HEADER_LEN + payload_len as usize;
    if buf.len() < total_len {
        return Ok(DecodeStatus::Incomplete);
    }

    if !is_compatible(peer_major) {
        return Err(ProtocolError::IncompatibleVersion {
            peer_major,
            peer_minor,
            our_major: PROTOCOL_MAJOR,
            our_minor: PROTOCOL_MINOR,
        });
    }

    let payload = &buf[HEADER_LEN..total_len];
    let message: Message =
        postcard::from_bytes(payload).map_err(|e| ProtocolError::Decode(e.to_string()))?;

    Ok(DecodeStatus::Ready {
        message,
        consumed: total_len,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::message::{ControlMessage, HandshakeMessage};

    fn sample_message() -> Message {
        Message::Handshake(HandshakeMessage::Hello {
            device_id: [1u8; 32],
        })
    }

    #[test]
    fn empty_buffer_is_incomplete() {
        assert_eq!(decode_frame(&[]).unwrap(), DecodeStatus::Incomplete);
    }

    #[test]
    fn buffer_shorter_than_header_is_incomplete() {
        assert_eq!(decode_frame(&[0, 0, 0]).unwrap(), DecodeStatus::Incomplete);
    }

    #[test]
    fn buffer_with_header_but_missing_payload_is_incomplete() {
        let frame = encode_frame(&sample_message()).unwrap();
        // Header claims a payload that isn't fully present yet.
        let truncated = &frame[..frame.len() - 1];
        assert_eq!(decode_frame(truncated).unwrap(), DecodeStatus::Incomplete);
    }

    #[test]
    fn decode_reports_exact_bytes_consumed_and_ignores_trailing_bytes() {
        let message = sample_message();
        let mut frame = encode_frame(&message).unwrap();
        let frame_len = frame.len();
        frame.extend_from_slice(&[0xAA, 0xBB, 0xCC]); // trailing next-frame bytes

        match decode_frame(&frame).unwrap() {
            DecodeStatus::Ready {
                message: decoded,
                consumed,
            } => {
                assert_eq!(decoded, message);
                assert_eq!(consumed, frame_len);
            }
            DecodeStatus::Incomplete => panic!("expected a complete frame"),
        }
    }

    #[test]
    fn oversized_declared_payload_is_rejected_without_buffering() {
        let mut header = Vec::new();
        header.extend_from_slice(&(MAX_FRAME_PAYLOAD_SIZE + 1).to_le_bytes());
        header.push(PROTOCOL_MAJOR);
        header.push(PROTOCOL_MINOR);
        // No payload bytes at all — rejection must happen from the header alone.
        let err = decode_frame(&header).unwrap_err();
        assert_eq!(
            err,
            ProtocolError::FrameTooLarge {
                size: MAX_FRAME_PAYLOAD_SIZE + 1,
                max: MAX_FRAME_PAYLOAD_SIZE,
            }
        );
    }

    #[test]
    fn incompatible_major_version_is_rejected() {
        let mut frame = encode_frame(&sample_message()).unwrap();
        frame[4] = PROTOCOL_MAJOR + 1; // corrupt the major-version byte
        let err = decode_frame(&frame).unwrap_err();
        assert_eq!(
            err,
            ProtocolError::IncompatibleVersion {
                peer_major: PROTOCOL_MAJOR + 1,
                peer_minor: PROTOCOL_MINOR,
                our_major: PROTOCOL_MAJOR,
                our_minor: PROTOCOL_MINOR,
            }
        );
    }

    #[test]
    fn different_minor_version_is_accepted() {
        let mut frame = encode_frame(&sample_message()).unwrap();
        frame[5] = PROTOCOL_MINOR.wrapping_add(1); // corrupt only the minor-version byte
        let status = decode_frame(&frame).unwrap();
        assert!(matches!(status, DecodeStatus::Ready { .. }));
    }

    #[test]
    fn malformed_payload_is_a_decode_error_not_a_panic() {
        // A hand-built frame whose payload is not a valid encoding of any
        // `Message` variant (rather than corrupting a real encoding, which
        // can coincidentally still decode if the flipped byte lands inside
        // fixed-size data rather than a structural tag).
        let payload = [0xFFu8; 8];
        let mut frame = Vec::new();
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.push(PROTOCOL_MAJOR);
        frame.push(PROTOCOL_MINOR);
        frame.extend_from_slice(&payload);

        let result = decode_frame(&frame);
        assert!(
            result.is_err(),
            "payload of 0xFF bytes must not decode as a valid Message, got {result:?}"
        );
    }

    #[test]
    fn back_to_back_frames_decode_in_sequence() {
        let a = Message::Control(ControlMessage::Ping { nonce: 1 });
        let b = Message::Control(ControlMessage::Pong { nonce: 2 });

        let mut buf = encode_frame(&a).unwrap();
        buf.extend_from_slice(&encode_frame(&b).unwrap());

        let (first_message, first_consumed) = match decode_frame(&buf).unwrap() {
            DecodeStatus::Ready { message, consumed } => (message, consumed),
            DecodeStatus::Incomplete => panic!("expected a complete frame"),
        };
        assert_eq!(first_message, a);

        let (second_message, second_consumed) = match decode_frame(&buf[first_consumed..]).unwrap()
        {
            DecodeStatus::Ready { message, consumed } => (message, consumed),
            DecodeStatus::Incomplete => panic!("expected a complete frame"),
        };
        assert_eq!(second_message, b);
        assert_eq!(first_consumed + second_consumed, buf.len());
    }
}
