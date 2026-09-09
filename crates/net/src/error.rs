//! Errors produced while establishing or using a peer connection.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum NetError {
    /// Building this device's self-signed identity certificate failed —
    /// an internal wiring problem (bad key encoding), not a network issue.
    #[error("failed to build identity certificate: {0}")]
    Identity(String),

    /// The peer's certificate doesn't correspond to a trusted device —
    /// either it isn't in the trust store, or its certificate is malformed
    /// in a way that prevents extracting a device identity at all.
    #[error("peer is not a trusted device")]
    UntrustedPeer,

    /// The QUIC/TLS transport itself failed (connect, handshake, or an
    /// established connection breaking).
    #[error("transport error: {0}")]
    Transport(String),

    /// The peer's declared protocol major version doesn't match ours.
    #[error(
        "incompatible protocol version: peer={peer_major}.{peer_minor}, ours={our_major}.{our_minor}"
    )]
    IncompatibleProtocolVersion {
        peer_major: u8,
        peer_minor: u8,
        our_major: u8,
        our_minor: u8,
    },

    /// The peer violated the connection-setup protocol — e.g. sent a
    /// duplicate handshake message, or a message on the wrong concern's
    /// stream. Distinct from a transport-level error: the connection was
    /// fine, the peer's behavior wasn't.
    #[error("protocol violation: {0}")]
    ProtocolViolation(String),

    /// A framing/serialization error from the `protocol` crate.
    #[error("protocol framing error: {0}")]
    Protocol(#[from] kvm_protocol::ProtocolError),

    /// An operation (handshake, heartbeat reply, stream setup) didn't
    /// complete within its allotted time.
    #[error("operation timed out")]
    Timeout,

    /// The connection was closed, locally or by the peer, and is no longer
    /// usable.
    #[error("connection closed")]
    ConnectionClosed,
}
