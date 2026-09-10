//! Proves the ADR-0004 pairing model at the `net` layer, independent of
//! any pairing state machine: two devices that don't trust each other at
//! all can still complete a pairing-mode connection and exchange
//! [`PairingMessage`]s, while an ordinary [`kvm_net::connect`] between
//! the same two devices still fails until trust is actually granted.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use kvm_protocol::PairingMessage;

#[tokio::test]
async fn untrusted_devices_can_complete_a_pairing_mode_connection() {
    // Deliberately NOT `mutually_trusting_pair` — neither side trusts the
    // other at all, exactly the real first-contact scenario.
    let a = support::Device::new([50u8; 32]);
    let b = support::Device::new([51u8; 32]);

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

    let mut peer_a = connect_task
        .await
        .unwrap()
        .expect("pairing-mode connect must succeed even though neither side trusts the other yet");
    let mut peer_b = accept_task
        .await
        .unwrap()
        .expect("pairing-mode accept must succeed even though neither side trusts the other yet");

    assert_eq!(peer_a.remote_device_id, b.device_id);
    assert_eq!(peer_b.remote_device_id, a.device_id);

    peer_a
        .send_pairing(&PairingMessage::Request {
            label: Some("A's device".to_string()),
        })
        .await
        .unwrap();
    assert_eq!(
        peer_b.recv_pairing().await.unwrap(),
        PairingMessage::Request {
            label: Some("A's device".to_string())
        }
    );

    peer_b.send_pairing(&PairingMessage::Accept).await.unwrap();
    assert_eq!(peer_a.recv_pairing().await.unwrap(), PairingMessage::Accept);
}

#[tokio::test]
async fn ordinary_connect_still_rejects_devices_that_only_paired_at_the_transport_level() {
    let a = support::Device::new([52u8; 32]);
    let b = support::Device::new([53u8; 32]);

    // A pairing-mode connection succeeding must NOT itself grant trust —
    // that only happens if `core`'s pairing flow calls TrustStore::trust
    // afterward, which this test deliberately never does.
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
    connect_task.await.unwrap().unwrap();
    accept_task.await.unwrap().unwrap();

    // Now try an ordinary (non-pairing) connect between the same two
    // devices — still untrusted, so it must still fail.
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
        "a prior pairing-mode connection must not itself establish trust"
    );
    assert!(accept_task.await.unwrap().is_err());
}
