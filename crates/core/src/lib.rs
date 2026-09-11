//! Orchestrator: device state, active-device switching, screen layout, and
//! pairing state machines. Wires `input`, `clipboard`, `transfer`, and `net`
//! together via channels; owns no sockets or OS input APIs directly.

mod error;
mod input_bridge;
mod layout;
mod ownership;
mod pairing;
mod pairing_session;
mod router;
mod session;

pub use error::CoreError;
pub use input_bridge::{forward_capture_to_peer, inject_from_peer};
pub use layout::{Edge, Layout, LayoutDevice, LayoutError};
pub use ownership::{
    OwnershipEvent, OwnershipState, SwitchContext, entry_position,
    transition as ownership_transition,
};
pub use pairing::{PairingError, PairingEvent, PairingState, transition};
pub use pairing_session::{commit_if_completed, run_acceptor, run_initiator};
pub use router::{Effect, Router};
pub use session::{Session, run_target};
