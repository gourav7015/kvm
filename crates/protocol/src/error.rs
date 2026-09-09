//! Errors produced while encoding or decoding protocol frames.

use thiserror::Error;

/// Everything that can go wrong turning a [`crate::Message`] into bytes and
/// back, including hostile/corrupted input.
///
/// Decoding never panics — every failure mode surfaces here instead.
#[derive(Debug, Error, PartialEq)]
pub enum ProtocolError {
    /// The peer's major protocol version doesn't match ours.
    #[error(
        "incompatible protocol version: peer={peer_major}.{peer_minor}, ours={our_major}.{our_minor}"
    )]
    IncompatibleVersion {
        peer_major: u8,
        peer_minor: u8,
        our_major: u8,
        our_minor: u8,
    },

    /// The frame header parsed, but the payload bytes didn't decode into a
    /// valid [`crate::Message`].
    #[error("failed to decode message payload: {0}")]
    Decode(String),

    /// A [`crate::Message`] failed to serialize (should not happen for any
    /// value our own types can construct; kept as an explicit error rather
    /// than an unwrap so a future message type with a fallible encoding
    /// can't panic).
    #[error("failed to encode message payload: {0}")]
    Encode(String),

    /// The frame header declares a payload larger than
    /// [`crate::MAX_FRAME_PAYLOAD_SIZE`] — rejected before attempting to
    /// buffer or decode it, so a corrupted/adversarial length field can't be
    /// used to force unbounded allocation.
    #[error("frame payload size {size} exceeds maximum allowed size {max}")]
    FrameTooLarge { size: u32, max: u32 },
}
