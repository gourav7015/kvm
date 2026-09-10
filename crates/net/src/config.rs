//! Builds the QUIC client/server configs used for every connection: TLS
//! 1.3 only, ALPN-tagged, mutually authenticated via the pinned-identity
//! verifiers in `tls`.

use std::sync::Arc;
use std::time::Duration;

use rustls::crypto::CryptoProvider;

use crate::error::NetError;
use crate::tls::{IdentityCert, PinnedClientVerifier, PinnedServerVerifier, TrustCheck};

/// ALPN identifier for this application's QUIC protocol. Required for
/// interop even though our own verifier ignores hostnames — QUIC/TLS
/// peers commonly reject a handshake with no/mismatched ALPN.
const ALPN_PROTOCOL: &[u8] = b"universal-kvm/1";

/// A dummy `server_name` passed to `quinn::Endpoint::connect_with`. It
/// must parse as a syntactically valid `ServerName`, but its value is
/// never inspected — [`PinnedServerVerifier`] checks the certificate's
/// embedded device identity instead of any hostname.
pub const IGNORED_SERVER_NAME: &str = "peer.universal-kvm.invalid";

/// Default idle timeout: how long a connection can go without any
/// traffic (including keep-alives) before it's declared dead. This is
/// the mechanism that detects a peer vanishing without a graceful close —
/// a hard-killed process, a crashed machine, a severed network link.
pub const DEFAULT_MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default keep-alive interval: how often an idle connection sends a
/// keep-alive frame to reset the peer's idle timer, so a healthy but
/// quiet connection doesn't get mistaken for a dead one.
pub const DEFAULT_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(3);

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport_config(
    max_idle_timeout: Duration,
    keep_alive_interval: Duration,
) -> Result<Arc<quinn::TransportConfig>, NetError> {
    let mut cfg = quinn::TransportConfig::default();
    let idle_timeout = quinn::IdleTimeout::try_from(max_idle_timeout)
        .map_err(|e| NetError::Identity(e.to_string()))?;
    cfg.max_idle_timeout(Some(idle_timeout));
    cfg.keep_alive_interval(Some(keep_alive_interval));
    Ok(Arc::new(cfg))
}

/// Builds the client-role config used when this device dials out to a
/// peer.
pub fn client_config(
    identity: &IdentityCert,
    trust_check: TrustCheck,
    max_idle_timeout: Duration,
    keep_alive_interval: Duration,
) -> Result<quinn::ClientConfig, NetError> {
    let provider = provider();
    let verifier = Arc::new(PinnedServerVerifier::new(provider.clone(), trust_check));

    let mut rustls_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Identity(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(
            vec![identity.cert_der.clone()],
            identity.key_der.clone_key().into(),
        )
        .map_err(|e| NetError::Identity(e.to_string()))?;
    rustls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];

    let quic_config = quinn::crypto::rustls::QuicClientConfig::try_from(rustls_config)
        .map_err(|e| NetError::Identity(e.to_string()))?;
    let mut client_cfg = quinn::ClientConfig::new(Arc::new(quic_config));
    client_cfg.transport_config(transport_config(max_idle_timeout, keep_alive_interval)?);
    Ok(client_cfg)
}

/// Builds the server-role config used when this device accepts an
/// incoming connection.
pub fn server_config(
    identity: &IdentityCert,
    trust_check: TrustCheck,
    max_idle_timeout: Duration,
    keep_alive_interval: Duration,
) -> Result<quinn::ServerConfig, NetError> {
    let provider = provider();
    let verifier = Arc::new(PinnedClientVerifier::new(provider.clone(), trust_check));

    let mut rustls_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| NetError::Identity(e.to_string()))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(
            vec![identity.cert_der.clone()],
            identity.key_der.clone_key().into(),
        )
        .map_err(|e| NetError::Identity(e.to_string()))?;
    rustls_config.alpn_protocols = vec![ALPN_PROTOCOL.to_vec()];
    // Session resumption would let a client skip the client-cert
    // verifier entirely on a later connection by reusing an
    // already-issued ticket — which would mean a revoked device could
    // keep reconnecting until its cached ticket happened to expire. Our
    // trust store can change between any two connections, so every
    // connection must run the verifier fresh. rustls's default of 2 is
    // built for the (unrelated) common case where resumption is a safe
    // perf optimization because trust doesn't change per-connection.
    rustls_config.send_tls13_tickets = 0;

    let quic_config = quinn::crypto::rustls::QuicServerConfig::try_from(rustls_config)
        .map_err(|e| NetError::Identity(e.to_string()))?;
    let mut server_cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_config));
    server_cfg.transport_config(transport_config(max_idle_timeout, keep_alive_interval)?);
    Ok(server_cfg)
}
