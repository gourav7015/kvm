//! Builds the one QUIC endpoint each device binds — every device is
//! symmetric, acting as both a QUIC server (accepting incoming peers) and
//! a QUIC client (dialing out), over a single UDP socket.

use std::net::SocketAddr;
use std::time::Duration;

use crate::config::{self, DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_MAX_IDLE_TIMEOUT};
use crate::error::NetError;
use crate::tls::{IdentityCert, TrustCheck};

/// Binds an endpoint with the default idle-timeout/keep-alive settings
/// ([`DEFAULT_MAX_IDLE_TIMEOUT`], [`DEFAULT_KEEP_ALIVE_INTERVAL`]).
pub fn new_endpoint(
    bind_addr: SocketAddr,
    identity: &IdentityCert,
    trust_check: TrustCheck,
) -> Result<quinn::Endpoint, NetError> {
    new_endpoint_with_timeouts(
        bind_addr,
        identity,
        trust_check,
        DEFAULT_MAX_IDLE_TIMEOUT,
        DEFAULT_KEEP_ALIVE_INTERVAL,
    )
}

/// Binds an endpoint with explicit idle-timeout/keep-alive settings —
/// exposed mainly so tests can use short timeouts instead of waiting out
/// the production defaults.
pub fn new_endpoint_with_timeouts(
    bind_addr: SocketAddr,
    identity: &IdentityCert,
    trust_check: TrustCheck,
    max_idle_timeout: Duration,
    keep_alive_interval: Duration,
) -> Result<quinn::Endpoint, NetError> {
    let server_cfg = config::server_config(
        identity,
        trust_check.clone(),
        max_idle_timeout,
        keep_alive_interval,
    )?;
    let client_cfg =
        config::client_config(identity, trust_check, max_idle_timeout, keep_alive_interval)?;

    let mut endpoint = quinn::Endpoint::server(server_cfg, bind_addr)
        .map_err(|e| NetError::Transport(e.to_string()))?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}
