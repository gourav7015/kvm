//! Local-network device discovery via mDNS, with a UDP broadcast fallback
//! and support for manual IP entry when both are unavailable.
//!
//! Every path here produces, at most, an *unauthenticated claim* of a
//! device's identity — see [`DiscoveredDevice`]'s docs. Discovery only
//! narrows down who to try pairing with; `kvm-net`'s pairing-mode TLS
//! handshake (ADR-0004) is what actually proves identity.

mod announcement;
mod device;
mod error;
mod mdns;
mod udp_broadcast;

pub use device::DiscoveredDevice;
pub use error::DiscoveryError;
pub use mdns::MdnsDiscovery;
pub use udp_broadcast::{DEFAULT_ANNOUNCE_PORT, UdpBroadcastDiscovery};
