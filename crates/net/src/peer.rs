//! Establishes an authenticated peer connection: QUIC connect/accept
//! (TLS mutual auth already pins identity), then a lightweight post-TLS
//! handshake that cross-checks the peer's claimed identity against what
//! TLS actually verified, before the connection is handed to callers.

use std::net::SocketAddr;

use kvm_protocol::{ControlMessage, DeviceId, HandshakeMessage, Message};

use crate::config;
use crate::error::NetError;
use crate::streams::{self, Streams};
use crate::tls::extract_device_id;

/// An established, mutually-authenticated connection to one peer.
pub struct Peer {
    pub connection: quinn::Connection,
    /// The peer's device identity, as verified by TLS (not merely what it
    /// claimed in its handshake message).
    pub remote_device_id: DeviceId,
    pub streams: Streams,
}

impl Peer {
    /// Reads the next message on the control stream, rejecting anything
    /// other than a [`ControlMessage`] — in particular, this is what makes
    /// a replayed/duplicate [`HandshakeMessage`] on an already-established
    /// connection a protocol violation instead of being silently accepted.
    pub async fn recv_control(&mut self) -> Result<ControlMessage, NetError> {
        match self.streams.control.recv().await? {
            Message::Control(control) => Ok(control),
            other => Err(NetError::ProtocolViolation(format!(
                "expected a Control message on the control stream, got {other:?}"
            ))),
        }
    }
}

/// Dials out to `addr` using the endpoint's default client config (see
/// [`crate::new_endpoint`]), expecting the peer to be a trusted device.
pub async fn connect(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    our_device_id: DeviceId,
) -> Result<Peer, NetError> {
    let connecting = endpoint
        .connect(addr, config::IGNORED_SERVER_NAME)
        .map_err(|e| NetError::Transport(e.to_string()))?;
    let connection = connecting
        .await
        .map_err(|e| NetError::Transport(e.to_string()))?;

    finish_handshake(connection, our_device_id, true).await
}

/// Completes accepting an incoming connection (already authenticated by
/// the endpoint's server-role TLS config).
pub async fn accept(incoming: quinn::Incoming, our_device_id: DeviceId) -> Result<Peer, NetError> {
    let connection = incoming
        .await
        .map_err(|e| NetError::Transport(e.to_string()))?;
    finish_handshake(connection, our_device_id, false).await
}

async fn finish_handshake(
    connection: quinn::Connection,
    our_device_id: DeviceId,
    is_initiator: bool,
) -> Result<Peer, NetError> {
    let remote_device_id = tls_verified_peer_identity(&connection)?;

    let mut streams = if is_initiator {
        streams::open_streams(&connection).await?
    } else {
        streams::accept_streams(&connection).await?
    };

    streams
        .control
        .send(&Message::Handshake(HandshakeMessage::Hello {
            device_id: our_device_id,
        }))
        .await?;

    match streams.control.recv().await? {
        Message::Handshake(HandshakeMessage::Hello { device_id }) => {
            if device_id != remote_device_id {
                return Err(NetError::ProtocolViolation(
                    "peer's claimed device_id doesn't match its TLS-verified identity".to_string(),
                ));
            }
        }
        other => {
            return Err(NetError::ProtocolViolation(format!(
                "expected a Handshake Hello, got {other:?}"
            )));
        }
    }

    Ok(Peer {
        connection,
        remote_device_id,
        streams,
    })
}

/// Pulls the peer's device identity from the certificate TLS actually
/// verified during the handshake — the ground truth, independent of
/// anything the peer later claims over the control stream.
fn tls_verified_peer_identity(connection: &quinn::Connection) -> Result<DeviceId, NetError> {
    let identity = connection
        .peer_identity()
        .ok_or_else(|| NetError::Transport("connection has no peer identity".to_string()))?;

    let chain = identity
        .downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
        .map_err(|_| NetError::Transport("unexpected peer identity type".to_string()))?;

    let end_entity = chain.first().ok_or_else(|| {
        NetError::Transport("peer presented an empty certificate chain".to_string())
    })?;

    extract_device_id(end_entity).map_err(|e| NetError::Transport(e.to_string()))
}
