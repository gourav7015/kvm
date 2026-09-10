//! UDP broadcast discovery: the fallback for networks that block mDNS.
//! Devices periodically broadcast a small announcement (see
//! [`crate::announcement`]) and listen for others' announcements on the
//! same port.

use std::net::{Ipv4Addr, SocketAddr};

use kvm_protocol::DeviceId;
use tokio::net::UdpSocket;

use crate::announcement::{decode, encode};
use crate::device::DiscoveredDevice;
use crate::error::DiscoveryError;

/// Default port used for broadcast announcements. Distinct from any
/// device's QUIC listening port, which is carried *inside* the
/// announcement payload instead.
pub const DEFAULT_ANNOUNCE_PORT: u16 = 51821;

/// A bound broadcast socket, ready to announce this device and listen
/// for others.
pub struct UdpBroadcastDiscovery {
    socket: UdpSocket,
    announce_port: u16,
}

impl UdpBroadcastDiscovery {
    /// Binds to `0.0.0.0:announce_port` with broadcast enabled.
    pub async fn bind(announce_port: u16) -> Result<Self, DiscoveryError> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, announce_port))
            .await
            .map_err(|e| DiscoveryError::UdpIo(e.to_string()))?;
        socket
            .set_broadcast(true)
            .map_err(|e| DiscoveryError::UdpIo(e.to_string()))?;
        Ok(Self {
            socket,
            announce_port,
        })
    }

    /// Broadcasts one announcement of this device to the local subnet.
    /// Callers typically call this on a repeating interval.
    pub async fn announce(
        &self,
        device_id: &DeviceId,
        our_quic_port: u16,
        label: Option<&str>,
    ) -> Result<(), DiscoveryError> {
        let payload = encode(device_id, our_quic_port, label);
        let dest = SocketAddr::from((Ipv4Addr::BROADCAST, self.announce_port));
        self.socket
            .send_to(&payload, dest)
            .await
            .map_err(|e| DiscoveryError::UdpIo(e.to_string()))?;
        Ok(())
    }

    /// Waits for and returns the next valid announcement received.
    /// Malformed datagrams (including ones from something other than
    /// this protocol entirely) are silently skipped rather than
    /// returned as errors — this is an unauthenticated broadcast
    /// channel, noise is expected.
    ///
    /// The returned device's `addr` uses `our_quic_port` from the
    /// announcement and the sender's actual source IP (not any IP the
    /// announcement claims) — the source IP is the one piece of this
    /// exchange the OS itself vouches for.
    pub async fn recv_announcement(&self) -> Result<DiscoveredDevice, DiscoveryError> {
        let mut buf = [0u8; 256];
        loop {
            let (n, from) = self
                .socket
                .recv_from(&mut buf)
                .await
                .map_err(|e| DiscoveryError::UdpIo(e.to_string()))?;

            let Some(announcement) = decode(&buf[..n]) else {
                continue;
            };

            return Ok(DiscoveredDevice {
                device_id: Some(announcement.device_id),
                addr: SocketAddr::new(from.ip(), announcement.port),
                label: announcement.label,
            });
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn announce_and_receive_round_trip_on_loopback() {
        // Two sockets on distinct ports so they don't collide with each
        // other's own broadcast (and to avoid interference from any
        // other discovery test/process on the machine running this).
        let announcer = UdpBroadcastDiscovery::bind(0).await.unwrap();
        let listener_port = 51999;
        let listener = UdpBroadcastDiscovery::bind(listener_port).await.unwrap();

        let device_id = [42u8; 32];
        // Send directly to the listener's port rather than a real subnet
        // broadcast address, since CI sandboxes commonly disallow
        // broadcasting out; unicast to a known port exercises the same
        // encode/send/recv/decode path.
        let payload = encode(&device_id, 51820, Some("Test Device"));
        announcer
            .socket
            .send_to(&payload, (Ipv4Addr::LOCALHOST, listener_port))
            .await
            .unwrap();

        let discovered = listener.recv_announcement().await.unwrap();
        assert_eq!(discovered.device_id, Some(device_id));
        assert_eq!(discovered.addr.port(), 51820);
        assert_eq!(discovered.addr.ip(), Ipv4Addr::LOCALHOST);
        assert_eq!(discovered.label.as_deref(), Some("Test Device"));
    }

    #[tokio::test]
    async fn malformed_datagrams_are_skipped_not_returned_as_errors() {
        let sender = UdpBroadcastDiscovery::bind(0).await.unwrap();
        let listener_port = 52000;
        let listener = UdpBroadcastDiscovery::bind(listener_port).await.unwrap();

        // Junk datagram first, then a real announcement.
        sender
            .socket
            .send_to(
                b"not a real announcement",
                (Ipv4Addr::LOCALHOST, listener_port),
            )
            .await
            .unwrap();
        let device_id = [9u8; 32];
        let payload = encode(&device_id, 51820, None);
        sender
            .socket
            .send_to(&payload, (Ipv4Addr::LOCALHOST, listener_port))
            .await
            .unwrap();

        let discovered = listener.recv_announcement().await.unwrap();
        assert_eq!(discovered.device_id, Some(device_id));
    }
}
