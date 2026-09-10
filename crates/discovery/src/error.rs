//! Errors produced while discovering or advertising over the LAN.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("mDNS error: {0}")]
    Mdns(String),

    #[error("UDP broadcast I/O error: {0}")]
    UdpIo(String),
}
