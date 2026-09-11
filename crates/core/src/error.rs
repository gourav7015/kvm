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

    /// A [`crate::router::Effect`] named a device with no entry in the
    /// session's connected-peers map. `Router` can only ever name a
    /// device previously registered via `Session::add_peer` (itself only
    /// ever fed an authenticated `net::Peer`), so this indicates the
    /// session's own bookkeeping fell out of sync with the router's —
    /// a bug, not something a remote peer can trigger.
    #[error("router named device {0:?} which has no registered peer connection")]
    UnknownPeer(kvm_protocol::DeviceId),
}
