//! Orchestrator: device state, active-device switching, screen layout, and
//! pairing state machines. Wires `input`, `clipboard`, `transfer`, and `net`
//! together via channels; owns no sockets or OS input APIs directly.

mod error;
mod pairing;
mod pairing_session;

pub use error::CoreError;
pub use pairing::{PairingError, PairingEvent, PairingState, transition};
pub use pairing_session::{commit_if_completed, run_acceptor, run_initiator};
