//! mDNS/DNS-SD discovery via `mdns-sd` — the primary discovery path.
//! Falls back to [`crate::udp_broadcast`] on networks that block mDNS,
//! and to manual IP entry ([`crate::DiscoveredDevice::manual`]) when
//! both are unavailable.

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::device::DiscoveredDevice;
use crate::error::DiscoveryError;

/// Service type advertised/browsed. The label before `._udp.local.` must
/// be <= 15 bytes per mDNS-SD's constraint; `_universal-kvm` is 14.
const SERVICE_TYPE: &str = "_universal-kvm._udp.local.";
const DEVICE_ID_TXT_KEY: &str = "device_id";

pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
}

impl MdnsDiscovery {
    pub fn new() -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new().map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        Ok(Self { daemon })
    }

    /// Advertises this device on the LAN. The device's identity travels
    /// in a TXT record (`device_id`, hex-encoded) — like every discovery
    /// path, this is an unauthenticated claim, not proof.
    pub fn advertise(
        &self,
        device_id: &kvm_protocol::DeviceId,
        port: u16,
        label: Option<&str>,
    ) -> Result<(), DiscoveryError> {
        let instance_name = label.unwrap_or("Universal KVM Device");
        let device_id_hex = hex::encode(device_id);
        let host_name = host_name_for(device_id);
        let properties = [(DEVICE_ID_TXT_KEY, device_id_hex.as_str())];

        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            instance_name,
            &host_name,
            "",
            port,
            &properties[..],
        )
        .map_err(|e| DiscoveryError::Mdns(e.to_string()))?
        .enable_addr_auto();

        self.daemon
            .register(service_info)
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))
    }

    /// Starts browsing for other devices. Returns a receiver of
    /// discovered devices as they resolve — malformed or missing
    /// `device_id` TXT records are skipped (treated as noise, same
    /// policy as [`crate::udp_broadcast`]) rather than surfaced as
    /// errors, since mDNS traffic on a real LAN includes services this
    /// crate has no reason to understand.
    pub fn browse(&self) -> Result<flume::Receiver<DiscoveredDevice>, DiscoveryError> {
        let events = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))?;

        let (tx, rx) = flume::unbounded();
        tokio::spawn(async move {
            while let Ok(event) = events.recv_async().await {
                if let ServiceEvent::ServiceResolved(resolved) = event {
                    let Some(device_id_hex) = resolved.get_property_val_str(DEVICE_ID_TXT_KEY)
                    else {
                        continue;
                    };
                    let Ok(device_id_bytes) = hex::decode(device_id_hex) else {
                        continue;
                    };
                    let Ok(device_id): Result<kvm_protocol::DeviceId, _> =
                        device_id_bytes.try_into()
                    else {
                        continue;
                    };

                    let port = resolved.get_port();
                    for addr in resolved.get_addresses_v4() {
                        let device = DiscoveredDevice {
                            device_id: Some(device_id),
                            addr: std::net::SocketAddr::new(std::net::IpAddr::V4(addr), port),
                            label: Some(resolved.get_fullname().to_string()),
                        };
                        if tx.send(device).is_err() {
                            return; // receiver dropped
                        }
                    }
                }
            }
        });

        Ok(rx)
    }

    pub fn shutdown(&self) -> Result<(), DiscoveryError> {
        self.daemon
            .shutdown()
            .map_err(|e| DiscoveryError::Mdns(e.to_string()))?;
        Ok(())
    }
}

/// Builds the mDNS host name for a device.
///
/// Regression note: this used to be the full 64-hex-char device_id, which
/// silently broke address-record publishing — DNS labels are capped at
/// 63 bytes (RFC 1035), and a too-long label doesn't error, it just never
/// resolves. Caught via `tests/mdns_loopback.rs` timing out, root-caused
/// with macOS's own `dns-sd -L` tool showing the same non-resolution
/// independent of this crate's code. An 8-byte (16-hex-char) prefix is
/// still effectively unique for this purpose; the full device_id travels
/// intact in the TXT record regardless.
fn host_name_for(device_id: &kvm_protocol::DeviceId) -> String {
    format!("{}.local.", &hex::encode(device_id)[..16])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression guard: every label in the constructed host name
    /// must be <= 63 bytes, for every possible device_id, not just the
    /// ones exercised by the (slow, real-network) loopback test.
    #[test]
    fn host_name_labels_never_exceed_the_dns_limit() {
        for device_id in [[0u8; 32], [0xFFu8; 32], [77u8; 32]] {
            let host_name = host_name_for(&device_id);
            for label in host_name.trim_end_matches('.').split('.') {
                assert!(
                    label.len() <= 63,
                    "label {label:?} in host name {host_name:?} exceeds the 63-byte DNS limit"
                );
            }
        }
    }
}
