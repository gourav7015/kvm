//! The top-level message envelope and its per-concern sub-messages.

mod clipboard;
mod control;
mod handshake;
mod input;
mod pairing;
mod transfer;

pub use clipboard::{ClipboardContent, ClipboardMessage};
pub use control::ControlMessage;
pub use handshake::{DeviceId, HandshakeMessage};
pub use input::{ButtonState, InputMessage, MouseButton};
pub use pairing::PairingMessage;
pub use transfer::{TransferId, TransferMessage};

use serde::{Deserialize, Serialize};

/// The single message type every frame carries, tagged by concern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    Handshake(HandshakeMessage),
    Control(ControlMessage),
    Input(InputMessage),
    Clipboard(ClipboardMessage),
    Transfer(TransferMessage),
    Pairing(PairingMessage),
}
