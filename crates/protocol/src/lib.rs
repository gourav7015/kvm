//! Wire message types and versioned framing shared by every peer.
//!
//! Pure data definitions only — no I/O, no networking, no crypto. This is the
//! contract that `net`, `core`, and every other crate serialize against.

mod error;
mod framing;
mod message;
mod version;

pub use error::ProtocolError;
pub use framing::{DecodeStatus, MAX_FRAME_PAYLOAD_SIZE, decode_frame, encode_frame};
pub use message::{
    ButtonState, ClipboardContent, ClipboardMessage, ControlMessage, DeviceId, HandshakeMessage,
    InputMessage, Message, MouseButton, PairingMessage, TransferId, TransferMessage,
};
pub use version::{PROTOCOL_MAJOR, PROTOCOL_MINOR, is_compatible};
