//! Drives the pure [`crate::pairing`] state machine over a real,
//! already-connected pairing-mode [`kvm_net::Peer`] (see
//! [`kvm_net::connect_for_pairing`]/[`kvm_net::accept_for_pairing`] and
//! ADR-0004). This is the thin, I/O-touching glue the module docs on
//! `pairing` describe — the state machine itself stays pure and is
//! tested without any of this.

use kvm_identity::TrustStore;
use kvm_net::Peer;
use kvm_protocol::{DeviceId, PairingMessage};

use crate::error::CoreError;
use crate::pairing::{PairingEvent, PairingState, transition};

/// Runs the initiator side of one pairing attempt to completion: sends a
/// `Request`, waits for the peer's `Accept`/`Reject`, and returns the
/// resulting terminal (or cancelled/timed-out, if the caller wraps this
/// in a timeout) state alongside the still-open `Peer`. `on_code` is
/// called once the code is known (for display — e.g. "your code: 123-456,
/// waiting for the other device..."); the initiator doesn't act on it,
/// only the acceptor's human decision does.
///
/// Does **not** touch the trust store — call [`commit_if_completed`]
/// afterward if the caller wants that.
pub async fn run_initiator<F>(
    mut peer: Peer,
    our_device_id: DeviceId,
    label: Option<String>,
    on_code: F,
) -> Result<(PairingState, Peer), CoreError>
where
    F: FnOnce(&str),
{
    let peer_device_id = peer.remote_device_id;
    let state = PairingState::Idle;
    let state = transition(
        &state,
        PairingEvent::StartLocal { peer_device_id },
        &our_device_id,
    )?;
    if let PairingState::AwaitingRemoteResponse { code, .. } = &state {
        on_code(code);
    }

    peer.send_pairing(&PairingMessage::Request { label })
        .await?;

    let event = match peer.recv_pairing().await? {
        PairingMessage::Accept => PairingEvent::RemoteAccept,
        PairingMessage::Reject => PairingEvent::RemoteReject,
        other => {
            return Err(CoreError::UnexpectedPairingMessage(format!("{other:?}")));
        }
    };
    let state = transition(&state, event, &our_device_id)?;

    Ok((state, peer))
}

/// Runs the acceptor side of one pairing attempt: waits for the peer's
/// `Request`, calls `decide(code, label)` for a human (or, in a headless
/// test/harness, a scripted) decision, sends the response, and returns
/// the resulting state alongside the still-open `Peer`.
pub async fn run_acceptor<F>(
    mut peer: Peer,
    our_device_id: DeviceId,
    decide: F,
) -> Result<(PairingState, Peer), CoreError>
where
    F: FnOnce(&str, Option<&str>) -> bool,
{
    let peer_device_id = peer.remote_device_id;
    let state = PairingState::Idle;

    let label = match peer.recv_pairing().await? {
        PairingMessage::Request { label } => label,
        other => {
            return Err(CoreError::UnexpectedPairingMessage(format!("{other:?}")));
        }
    };
    let state = transition(
        &state,
        PairingEvent::IncomingRequest {
            peer_device_id,
            label: label.clone(),
        },
        &our_device_id,
    )?;

    let code = match &state {
        PairingState::AwaitingLocalConfirmation { code, .. } => code.clone(),
        _ => unreachable!("transition just produced AwaitingLocalConfirmation"),
    };

    let accepted = decide(&code, label.as_deref());
    let event = if accepted {
        PairingEvent::LocalAccept
    } else {
        PairingEvent::LocalReject
    };
    let state = transition(&state, event, &our_device_id)?;

    let reply = if accepted {
        PairingMessage::Accept
    } else {
        PairingMessage::Reject
    };
    peer.send_pairing(&reply).await?;

    Ok((state, peer))
}

/// If `state` is [`PairingState::Completed`], pins the peer as trusted
/// and returns `true`; otherwise leaves the trust store untouched and
/// returns `false`. Idempotent — trusting an already-trusted device is a
/// no-op (see `identity::TrustStore::trust`).
pub fn commit_if_completed(
    state: &PairingState,
    trust_store: &mut TrustStore,
) -> Result<bool, CoreError> {
    if let PairingState::Completed { peer_device_id } = state {
        trust_store.trust(*peer_device_id)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
