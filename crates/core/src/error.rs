//! Errors produced by `core`'s orchestration logic.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Net(#[from] kvm_net::NetError),

    #[error(transparent)]
    Identity(#[from] kvm_identity::IdentityError),

    #[error(transparent)]
    Input(#[from] kvm_input::InputError),

    #[error(transparent)]
    Pairing(#[from] crate::pairing::PairingError),

    /// A message arrived where a specific [`kvm_protocol::PairingMessage`]
    /// variant was expected.
    #[error("unexpected message during pairing: {0}")]
    UnexpectedPairingMessage(String),
}
