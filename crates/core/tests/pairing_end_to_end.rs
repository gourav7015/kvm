//! Phase 2 DoD: "end-to-end loopback pairing test converging trust
//! stores." Wires the real pieces together — `identity::TrustStore`
//! (file-backed, not a test double), `net`'s pairing-mode connect/accept,
//! and `core`'s pairing state machine — over loopback, and proves the
//! whole chain: two devices that start completely untrusted end up with
//! each other pinned in their *real* trust stores, agree on the same
//! human-comparable code, and a subsequent ordinary (non-pairing)
//! connection then succeeds without any further pairing step. Also
//! covers the "revoked device re-pairing" adversarial case: revoking
//! after a successful pairing must make ordinary reconnection fail again.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use kvm_core::{PairingState, commit_if_completed, run_acceptor, run_initiator};
use kvm_identity::{DeviceKeypair, TrustStore};
use kvm_net::{IdentityCert, TrustCheck};
use kvm_protocol::DeviceId;

/// A device backed by its real identity and a real, file-persisted
/// `TrustStore` — the endpoint's trust check reads that store live, so
/// updating it (as pairing does) immediately changes what the endpoint
/// will accept on future connections.
struct Device {
    device_id: DeviceId,
    seed: [u8; 32],
    endpoint: quinn::Endpoint,
    addr: SocketAddr,
    trust_store: Arc<Mutex<TrustStore>>,
}

impl Device {
    fn new(seed: [u8; 32], trust_store_path: std::path::PathBuf) -> Self {
        let keypair = DeviceKeypair::from_secret_bytes(&seed);
        let device_id = keypair.device_id();
        let identity = IdentityCert::from_seed(&seed).unwrap();

        let trust_store = Arc::new(Mutex::new(
            TrustStore::load_or_create(trust_store_path).unwrap(),
        ));
        let trust_check: TrustCheck = {
            let trust_store = trust_store.clone();
            Arc::new(move |id: &DeviceId| trust_store.lock().unwrap().is_trusted(id))
        };

        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let endpoint = kvm_net::new_endpoint(bind_addr, &identity, trust_check).unwrap();
        let addr = endpoint.local_addr().unwrap();

        Self {
            device_id,
            seed,
            endpoint,
            addr,
            trust_store,
        }
    }

    fn identity_cert(&self) -> IdentityCert {
        IdentityCert::from_seed(&self.seed).unwrap()
    }

    fn is_trusted(&self, id: &DeviceId) -> bool {
        self.trust_store.lock().unwrap().is_trusted(id)
    }
}

#[tokio::test]
async fn pairing_converges_real_trust_stores_and_unlocks_ordinary_reconnection() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = Device::new([60u8; 32], dir_a.path().join("trust.json"));
    let b = Device::new([61u8; 32], dir_b.path().join("trust.json"));

    // Neither trusts the other yet — the real starting point.
    assert!(!a.is_trusted(&b.device_id));
    assert!(!b.is_trusted(&a.device_id));

    let accept_task = {
        let our_id = b.device_id;
        let identity = b.identity_cert();
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept_for_pairing(incoming, our_id, &identity).await
        })
    };
    let connect_task = {
        let addr = b.addr;
        let our_id = a.device_id;
        let identity = a.identity_cert();
        let endpoint = a.endpoint.clone();
        tokio::spawn(async move {
            kvm_net::connect_for_pairing(&endpoint, addr, our_id, &identity).await
        })
    };

    let peer_a = connect_task.await.unwrap().unwrap();
    let peer_b = accept_task.await.unwrap().unwrap();

    let a_code = Arc::new(Mutex::new(String::new()));
    let b_code = Arc::new(Mutex::new(String::new()));

    let initiator_task = {
        let a_code = a_code.clone();
        let our_id = a.device_id;
        tokio::spawn(async move {
            run_initiator(peer_a, our_id, Some("A's device".to_string()), |code| {
                *a_code.lock().unwrap() = code.to_string();
            })
            .await
        })
    };
    let acceptor_task = {
        let b_code = b_code.clone();
        let our_id = b.device_id;
        tokio::spawn(async move {
            run_acceptor(peer_b, our_id, |code, label| {
                *b_code.lock().unwrap() = code.to_string();
                assert_eq!(label, Some("A's device"));
                true // human clicks Accept
            })
            .await
        })
    };

    let (initiator_state, _peer_a) = initiator_task.await.unwrap().unwrap();
    let (acceptor_state, _peer_b) = acceptor_task.await.unwrap().unwrap();

    // Both sides independently derived the same code — the actual
    // integrity property from ADR-0004.
    let a_code = a_code.lock().unwrap().clone();
    let b_code = b_code.lock().unwrap().clone();
    assert!(!a_code.is_empty());
    assert_eq!(a_code, b_code);

    assert_eq!(
        initiator_state,
        PairingState::Completed {
            peer_device_id: b.device_id
        }
    );
    assert_eq!(
        acceptor_state,
        PairingState::Completed {
            peer_device_id: a.device_id
        }
    );

    // Commit trust on both sides, into the *real* file-backed store.
    assert!(commit_if_completed(&initiator_state, &mut a.trust_store.lock().unwrap()).unwrap());
    assert!(commit_if_completed(&acceptor_state, &mut b.trust_store.lock().unwrap()).unwrap());

    assert!(a.is_trusted(&b.device_id));
    assert!(b.is_trusted(&a.device_id));

    // Trust persists across a reload of the trust store file, not just
    // in memory.
    let reloaded_a = TrustStore::load_or_create(dir_a.path().join("trust.json")).unwrap();
    assert!(reloaded_a.is_trusted(&b.device_id));

    // An ORDINARY (non-pairing) connection now succeeds with no further
    // pairing step.
    let accept_task = {
        let our_id = b.device_id;
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept(incoming, our_id).await
        })
    };
    let addr = b.addr;
    let our_id = a.device_id;
    let endpoint = a.endpoint.clone();
    let connect_result =
        tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await })
            .await
            .unwrap();
    assert!(
        connect_result.is_ok(),
        "paired devices must connect normally without re-pairing"
    );
    assert!(accept_task.await.unwrap().is_ok());

    // Revoke on A's side (adversarial case: "revoked device re-pairing"
    // from the build plan's risk register) — ordinary reconnection must
    // fail again immediately, using the real trust store's revoke path.
    a.trust_store.lock().unwrap().revoke(&b.device_id).unwrap();
    assert!(!a.is_trusted(&b.device_id));

    let accept_task = {
        let our_id = b.device_id;
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept(incoming, our_id).await
        })
    };
    let addr = b.addr;
    let our_id = a.device_id;
    let endpoint = a.endpoint.clone();
    let connect_result =
        tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await })
            .await
            .unwrap();
    assert!(
        connect_result.is_err(),
        "a revoked device must be rejected, not silently allowed back in"
    );
    assert!(accept_task.await.unwrap().is_err());
}

#[tokio::test]
async fn a_rejected_pairing_grants_no_trust() {
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let a = Device::new([62u8; 32], dir_a.path().join("trust.json"));
    let b = Device::new([63u8; 32], dir_b.path().join("trust.json"));

    let accept_task = {
        let our_id = b.device_id;
        let identity = b.identity_cert();
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept_for_pairing(incoming, our_id, &identity).await
        })
    };
    let connect_task = {
        let addr = b.addr;
        let our_id = a.device_id;
        let identity = a.identity_cert();
        let endpoint = a.endpoint.clone();
        tokio::spawn(async move {
            kvm_net::connect_for_pairing(&endpoint, addr, our_id, &identity).await
        })
    };
    let peer_a = connect_task.await.unwrap().unwrap();
    let peer_b = accept_task.await.unwrap().unwrap();

    let initiator_task = {
        let our_id = a.device_id;
        tokio::spawn(async move { run_initiator(peer_a, our_id, None, |_code| {}).await })
    };
    let acceptor_task = {
        let our_id = b.device_id;
        // The human on B's side clicks Reject.
        tokio::spawn(async move { run_acceptor(peer_b, our_id, |_code, _label| false).await })
    };

    let (initiator_state, _) = initiator_task.await.unwrap().unwrap();
    let (acceptor_state, _) = acceptor_task.await.unwrap().unwrap();

    assert_eq!(
        initiator_state,
        PairingState::Rejected {
            peer_device_id: b.device_id
        }
    );
    assert_eq!(
        acceptor_state,
        PairingState::Rejected {
            peer_device_id: a.device_id
        }
    );

    assert!(!commit_if_completed(&initiator_state, &mut a.trust_store.lock().unwrap()).unwrap());
    assert!(!commit_if_completed(&acceptor_state, &mut b.trust_store.lock().unwrap()).unwrap());
    assert!(!a.is_trusted(&b.device_id));
    assert!(!b.is_trusted(&a.device_id));
}
