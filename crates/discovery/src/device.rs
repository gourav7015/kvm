//! A device found (or manually entered) on the local network.
//!
//! `device_id` here is an *unauthenticated claim* — mDNS TXT records and
//! UDP broadcast packets can be forged by anyone on the LAN. Discovery
//! only narrows down "who to try pairing with"; the pairing-mode TLS
//! handshake (see `kvm-net` and ADR-0004) is what cryptographically
//! proves an identity. Never treat a `DiscoveredDevice`'s `device_id` as
//! trusted.

use std::net::SocketAddr;

use kvm_protocol::DeviceId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// The claimed device identity, if the discovery path carried one.
    /// `None` for a bare manual IP entry, where nothing is known until a
    /// pairing-mode connection actually completes a TLS handshake.
    pub device_id: Option<DeviceId>,
    pub addr: SocketAddr,
    pub label: Option<String>,
}

impl DiscoveredDevice {
    /// A device entered manually by IP address — the fallback when
    /// neither mDNS nor UDP broadcast discovery finds anything (e.g. a
    /// network that blocks both).
    pub fn manual(addr: SocketAddr) -> Self {
        Self {
            device_id: None,
            addr,
            label: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn manual_entry_has_no_claimed_identity() {
        let addr: SocketAddr = "192.168.1.50:51820".parse().unwrap();
        let device = DiscoveredDevice::manual(addr);
        assert_eq!(device.device_id, None);
        assert_eq!(device.addr, addr);
        assert_eq!(device.label, None);
    }
}
