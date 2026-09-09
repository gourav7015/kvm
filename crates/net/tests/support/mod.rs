//! Shared helpers for `net`'s integration tests: spin up a loopback
//! device with a real Ed25519 identity and a configurable trust set.

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kvm_identity::DeviceKeypair;
use kvm_net::{IdentityCert, TrustCheck};
use kvm_protocol::DeviceId;

/// A loopback test device: a real identity plus a QUIC endpoint bound to
/// an OS-assigned port, backed by a mutable trust set the test can edit.
pub struct Device {
    pub device_id: DeviceId,
    pub endpoint: quinn::Endpoint,
    pub addr: SocketAddr,
    trusted: Arc<Mutex<HashSet<DeviceId>>>,
}

impl Device {
    pub fn new(seed: [u8; 32]) -> Self {
        Self::with_timeouts(
            seed,
            kvm_net::DEFAULT_MAX_IDLE_TIMEOUT,
            kvm_net::DEFAULT_KEEP_ALIVE_INTERVAL,
        )
    }

    /// Same as [`Device::new`], but with explicit idle-timeout/keep-alive
    /// settings — for tests that need a dead connection to be detected
    /// quickly instead of waiting out the production defaults.
    pub fn with_timeouts(
        seed: [u8; 32],
        max_idle_timeout: Duration,
        keep_alive_interval: Duration,
    ) -> Self {
        let keypair = DeviceKeypair::from_secret_bytes(&seed);
        let device_id = keypair.device_id();
        let identity = IdentityCert::from_seed(&seed).unwrap();

        let trusted = Arc::new(Mutex::new(HashSet::new()));
        let trust_check: TrustCheck = {
            let trusted = trusted.clone();
            Arc::new(move |id: &DeviceId| trusted.lock().unwrap().contains(id))
        };

        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let endpoint = kvm_net::new_endpoint_with_timeouts(
            bind_addr,
            &identity,
            trust_check,
            max_idle_timeout,
            keep_alive_interval,
        )
        .unwrap();
        let addr = endpoint.local_addr().unwrap();

        Self {
            device_id,
            endpoint,
            addr,
            trusted,
        }
    }

    pub fn trust(&self, device_id: DeviceId) {
        self.trusted.lock().unwrap().insert(device_id);
    }

    pub fn untrust(&self, device_id: &DeviceId) {
        self.trusted.lock().unwrap().remove(device_id);
    }
}

/// Two devices that already trust each other, ready to connect.
pub fn mutually_trusting_pair(seed_a: [u8; 32], seed_b: [u8; 32]) -> (Device, Device) {
    let a = Device::new(seed_a);
    let b = Device::new(seed_b);
    a.trust(b.device_id);
    b.trust(a.device_id);
    (a, b)
}

/// Same as [`mutually_trusting_pair`], with explicit short idle-timeout/
/// keep-alive settings for tests that need fast dead-connection detection.
pub fn mutually_trusting_pair_with_timeouts(
    seed_a: [u8; 32],
    seed_b: [u8; 32],
    max_idle_timeout: Duration,
    keep_alive_interval: Duration,
) -> (Device, Device) {
    let a = Device::with_timeouts(seed_a, max_idle_timeout, keep_alive_interval);
    let b = Device::with_timeouts(seed_b, max_idle_timeout, keep_alive_interval);
    a.trust(b.device_id);
    b.trust(a.device_id);
    (a, b)
}
