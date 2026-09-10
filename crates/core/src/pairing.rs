//! Pure pairing state machine — see ADR-0004 for the security model this
//! implements (permissive-TLS connection + human code confirmation).
//! Nothing here touches a socket, a keychain, or the trust store: it's
//! driven entirely by events and returns the next state, so the
//! networking glue ([`crate::pairing_session`]) and the trust-store
//! commit stay outside it and are separately testable.
//!
//! Only one pairing attempt is modeled at a time. There is no "reset"
//! event — a caller that wants to pair again after a session reaches a
//! terminal state ([`PairingState::Completed`], [`PairingState::Rejected`],
//! [`PairingState::Cancelled`], [`PairingState::TimedOut`]) starts a fresh
//! [`PairingState::Idle`] rather than reusing the old one.

use kvm_identity::pairing_code;
use kvm_protocol::DeviceId;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingState {
    Idle,
    /// We initiated; waiting for the peer's human to accept or reject.
    AwaitingRemoteResponse {
        peer_device_id: DeviceId,
        code: String,
    },
    /// The peer initiated; waiting for *our* human to accept or reject.
    AwaitingLocalConfirmation {
        peer_device_id: DeviceId,
        code: String,
        label: Option<String>,
    },
    Completed {
        peer_device_id: DeviceId,
    },
    Rejected {
        peer_device_id: DeviceId,
    },
    Cancelled,
    TimedOut,
}

impl PairingState {
    fn name(&self) -> &'static str {
        match self {
            PairingState::Idle => "Idle",
            PairingState::AwaitingRemoteResponse { .. } => "AwaitingRemoteResponse",
            PairingState::AwaitingLocalConfirmation { .. } => "AwaitingLocalConfirmation",
            PairingState::Completed { .. } => "Completed",
            PairingState::Rejected { .. } => "Rejected",
            PairingState::Cancelled => "Cancelled",
            PairingState::TimedOut => "TimedOut",
        }
    }

    fn in_flight_peer(&self) -> Option<&DeviceId> {
        match self {
            PairingState::AwaitingRemoteResponse { peer_device_id, .. }
            | PairingState::AwaitingLocalConfirmation { peer_device_id, .. } => {
                Some(peer_device_id)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingEvent {
    /// The human initiates pairing with a peer (discovered or manually
    /// entered) over an already-established pairing-mode connection.
    StartLocal {
        peer_device_id: DeviceId,
    },
    /// The peer sent a `PairingMessage::Request` on that connection.
    IncomingRequest {
        peer_device_id: DeviceId,
        label: Option<String>,
    },
    LocalAccept,
    LocalReject,
    RemoteAccept,
    RemoteReject,
    Cancel,
    Timeout,
}

impl PairingEvent {
    fn name(&self) -> &'static str {
        match self {
            PairingEvent::StartLocal { .. } => "StartLocal",
            PairingEvent::IncomingRequest { .. } => "IncomingRequest",
            PairingEvent::LocalAccept => "LocalAccept",
            PairingEvent::LocalReject => "LocalReject",
            PairingEvent::RemoteAccept => "RemoteAccept",
            PairingEvent::RemoteReject => "RemoteReject",
            PairingEvent::Cancel => "Cancel",
            PairingEvent::Timeout => "Timeout",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PairingError {
    /// `event` doesn't apply in `state` — e.g. `LocalAccept` while
    /// `Idle`, or any event once a session has already reached a
    /// terminal state (this is what makes a replayed pairing message
    /// after the session already resolved a hard error, not a silent
    /// no-op).
    #[error("event {event} is not valid in state {state}")]
    IllegalTransition {
        state: &'static str,
        event: &'static str,
    },
    /// An event named a different peer than the one already in flight —
    /// e.g. a second, distinct incoming request while one pairing
    /// attempt is already pending. Single-session by design (see module
    /// docs), so this is rejected rather than silently switching targets
    /// mid-flow.
    #[error("event referenced peer {actual:?}, but pairing is already in flight with {expected:?}")]
    PeerMismatch {
        expected: DeviceId,
        actual: DeviceId,
    },
}

/// Advances `state` given `event`. `our_device_id` is needed to derive
/// the human-comparable pairing code (see [`kvm_identity::pairing_code`])
/// when a session starts.
pub fn transition(
    state: &PairingState,
    event: PairingEvent,
    our_device_id: &DeviceId,
) -> Result<PairingState, PairingError> {
    if let Some(expected) = state.in_flight_peer()
        && let PairingEvent::IncomingRequest {
            peer_device_id: actual,
            ..
        } = &event
        && actual != expected
    {
        return Err(PairingError::PeerMismatch {
            expected: *expected,
            actual: *actual,
        });
    }

    match (state, event) {
        (PairingState::Idle, PairingEvent::StartLocal { peer_device_id }) => {
            let code = pairing_code(our_device_id, &peer_device_id);
            Ok(PairingState::AwaitingRemoteResponse {
                peer_device_id,
                code,
            })
        }
        (
            PairingState::Idle,
            PairingEvent::IncomingRequest {
                peer_device_id,
                label,
            },
        ) => {
            let code = pairing_code(our_device_id, &peer_device_id);
            Ok(PairingState::AwaitingLocalConfirmation {
                peer_device_id,
                code,
                label,
            })
        }
        (
            PairingState::AwaitingLocalConfirmation { peer_device_id, .. },
            PairingEvent::LocalAccept,
        ) => Ok(PairingState::Completed {
            peer_device_id: *peer_device_id,
        }),
        (
            PairingState::AwaitingLocalConfirmation { peer_device_id, .. },
            PairingEvent::LocalReject,
        ) => Ok(PairingState::Rejected {
            peer_device_id: *peer_device_id,
        }),
        (
            PairingState::AwaitingRemoteResponse { peer_device_id, .. },
            PairingEvent::RemoteAccept,
        ) => Ok(PairingState::Completed {
            peer_device_id: *peer_device_id,
        }),
        (
            PairingState::AwaitingRemoteResponse { peer_device_id, .. },
            PairingEvent::RemoteReject,
        ) => Ok(PairingState::Rejected {
            peer_device_id: *peer_device_id,
        }),
        (PairingState::AwaitingRemoteResponse { .. }, PairingEvent::Cancel)
        | (PairingState::AwaitingLocalConfirmation { .. }, PairingEvent::Cancel) => {
            Ok(PairingState::Cancelled)
        }
        (PairingState::AwaitingRemoteResponse { .. }, PairingEvent::Timeout)
        | (PairingState::AwaitingLocalConfirmation { .. }, PairingEvent::Timeout) => {
            Ok(PairingState::TimedOut)
        }
        (state, event) => Err(PairingError::IllegalTransition {
            state: state.name(),
            event: event.name(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const US: DeviceId = [1u8; 32];
    const PEER: DeviceId = [2u8; 32];
    const OTHER: DeviceId = [3u8; 32];

    #[test]
    fn valid_initiator_flow_to_completion() {
        let state = PairingState::Idle;
        let state = transition(
            &state,
            PairingEvent::StartLocal {
                peer_device_id: PEER,
            },
            &US,
        )
        .unwrap();
        assert!(matches!(
            state,
            PairingState::AwaitingRemoteResponse {
                peer_device_id: PEER,
                ..
            }
        ));

        let state = transition(&state, PairingEvent::RemoteAccept, &US).unwrap();
        assert_eq!(
            state,
            PairingState::Completed {
                peer_device_id: PEER
            }
        );
    }

    #[test]
    fn valid_initiator_flow_to_rejection() {
        let state = PairingState::Idle;
        let state = transition(
            &state,
            PairingEvent::StartLocal {
                peer_device_id: PEER,
            },
            &US,
        )
        .unwrap();
        let state = transition(&state, PairingEvent::RemoteReject, &US).unwrap();
        assert_eq!(
            state,
            PairingState::Rejected {
                peer_device_id: PEER
            }
        );
    }

    #[test]
    fn valid_acceptor_flow_to_completion() {
        let state = PairingState::Idle;
        let state = transition(
            &state,
            PairingEvent::IncomingRequest {
                peer_device_id: PEER,
                label: Some("A's device".to_string()),
            },
            &US,
        )
        .unwrap();
        assert!(matches!(
            state,
            PairingState::AwaitingLocalConfirmation {
                peer_device_id: PEER,
                ..
            }
        ));

        let state = transition(&state, PairingEvent::LocalAccept, &US).unwrap();
        assert_eq!(
            state,
            PairingState::Completed {
                peer_device_id: PEER
            }
        );
    }

    #[test]
    fn valid_acceptor_flow_to_rejection() {
        let state = PairingState::Idle;
        let state = transition(
            &state,
            PairingEvent::IncomingRequest {
                peer_device_id: PEER,
                label: None,
            },
            &US,
        )
        .unwrap();
        let state = transition(&state, PairingEvent::LocalReject, &US).unwrap();
        assert_eq!(
            state,
            PairingState::Rejected {
                peer_device_id: PEER
            }
        );
    }

    #[test]
    fn cancel_from_either_awaiting_state() {
        let awaiting_remote = transition(
            &PairingState::Idle,
            PairingEvent::StartLocal {
                peer_device_id: PEER,
            },
            &US,
        )
        .unwrap();
        assert_eq!(
            transition(&awaiting_remote, PairingEvent::Cancel, &US).unwrap(),
            PairingState::Cancelled
        );

        let awaiting_local = transition(
            &PairingState::Idle,
            PairingEvent::IncomingRequest {
                peer_device_id: PEER,
                label: None,
            },
            &US,
        )
        .unwrap();
        assert_eq!(
            transition(&awaiting_local, PairingEvent::Cancel, &US).unwrap(),
            PairingState::Cancelled
        );
    }

    #[test]
    fn timeout_from_either_awaiting_state() {
        let awaiting_remote = transition(
            &PairingState::Idle,
            PairingEvent::StartLocal {
                peer_device_id: PEER,
            },
            &US,
        )
        .unwrap();
        assert_eq!(
            transition(&awaiting_remote, PairingEvent::Timeout, &US).unwrap(),
            PairingState::TimedOut
        );

        let awaiting_local = transition(
            &PairingState::Idle,
            PairingEvent::IncomingRequest {
                peer_device_id: PEER,
                label: None,
            },
            &US,
        )
        .unwrap();
        assert_eq!(
            transition(&awaiting_local, PairingEvent::Timeout, &US).unwrap(),
            PairingState::TimedOut
        );
    }

    #[test]
    fn accepting_before_a_request_exists_is_illegal() {
        let err = transition(&PairingState::Idle, PairingEvent::LocalAccept, &US).unwrap_err();
        assert!(matches!(err, PairingError::IllegalTransition { .. }));
    }

    #[test]
    fn using_the_initiator_events_in_the_acceptor_role_is_illegal() {
        // We're the one being asked (AwaitingLocalConfirmation); only
        // LocalAccept/LocalReject/Cancel/Timeout are legal here, not the
        // initiator-side RemoteAccept/RemoteReject.
        let awaiting_local = transition(
            &PairingState::Idle,
            PairingEvent::IncomingRequest {
                peer_device_id: PEER,
                label: None,
            },
            &US,
        )
        .unwrap();
        assert!(matches!(
            transition(&awaiting_local, PairingEvent::RemoteAccept, &US),
            Err(PairingError::IllegalTransition { .. })
        ));
        assert!(matches!(
            transition(&awaiting_local, PairingEvent::RemoteReject, &US),
            Err(PairingError::IllegalTransition { .. })
        ));
    }

    #[test]
    fn every_event_is_illegal_once_a_session_is_terminal() {
        for terminal in [
            PairingState::Completed {
                peer_device_id: PEER,
            },
            PairingState::Rejected {
                peer_device_id: PEER,
            },
            PairingState::Cancelled,
            PairingState::TimedOut,
        ] {
            for event in [
                PairingEvent::StartLocal {
                    peer_device_id: PEER,
                },
                PairingEvent::IncomingRequest {
                    peer_device_id: PEER,
                    label: None,
                },
                PairingEvent::LocalAccept,
                PairingEvent::LocalReject,
                PairingEvent::RemoteAccept,
                PairingEvent::RemoteReject,
                PairingEvent::Cancel,
                PairingEvent::Timeout,
            ] {
                let result = transition(&terminal, event.clone(), &US);
                assert!(
                    result.is_err(),
                    "expected {event:?} on {terminal:?} to be illegal, got {result:?}"
                );
            }
        }
    }

    #[test]
    fn a_replayed_request_for_a_different_peer_is_rejected_not_silently_switched() {
        // "Replayed pairing code" adversarial case from the build plan:
        // once a session is in flight with PEER, an IncomingRequest
        // naming a different device must not silently redirect the
        // session to that new peer.
        let awaiting_remote = transition(
            &PairingState::Idle,
            PairingEvent::StartLocal {
                peer_device_id: PEER,
            },
            &US,
        )
        .unwrap();

        let err = transition(
            &awaiting_remote,
            PairingEvent::IncomingRequest {
                peer_device_id: OTHER,
                label: None,
            },
            &US,
        )
        .unwrap_err();
        assert_eq!(
            err,
            PairingError::PeerMismatch {
                expected: PEER,
                actual: OTHER,
            }
        );
        // The original session must be untouched by the rejected event.
        assert_eq!(awaiting_remote, awaiting_remote.clone());
    }

    #[test]
    fn the_code_is_order_independent_and_matches_identity_crate() {
        // Sanity check that the state machine is actually using
        // kvm_identity::pairing_code and not reinventing it.
        let state = transition(
            &PairingState::Idle,
            PairingEvent::StartLocal {
                peer_device_id: PEER,
            },
            &US,
        )
        .unwrap();
        let PairingState::AwaitingRemoteResponse { code, .. } = state else {
            panic!("expected AwaitingRemoteResponse");
        };
        assert_eq!(code, pairing_code(&US, &PEER));
        assert_eq!(code, pairing_code(&PEER, &US));
    }
}
