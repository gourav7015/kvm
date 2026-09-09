//! Phase 1c DoD: "adversarial untrusted-peer-rejected test."
//!
//! Every case here spawns an accept task on the far side even when the
//! test only cares about one direction's result — without it, nothing
//! ever drives the incoming handshake forward, and the connecting side
//! just sees total silence until its idle timeout fires instead of a
//! prompt rejection. (Found by this suite hanging for 10s per test.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

async fn spawn_accept(
    device: &support::Device,
) -> tokio::task::JoinHandle<Result<kvm_net::Peer, kvm_net::NetError>> {
    let our_id = device.device_id;
    let endpoint = device.endpoint.clone();
    tokio::spawn(async move {
        let incoming = endpoint.accept().await.expect("endpoint closed");
        kvm_net::accept(incoming, our_id).await
    })
}

#[tokio::test]
async fn connecting_to_a_device_that_does_not_trust_us_is_rejected() {
    let a = support::Device::new([10u8; 32]);
    let b = support::Device::new([11u8; 32]);
    // Deliberately one-sided: A trusts B, but B does NOT trust A.
    a.trust(b.device_id);

    let accept_task = spawn_accept(&b).await;

    let addr = b.addr;
    let our_id = a.device_id;
    let endpoint = a.endpoint.clone();
    let connect_result =
        tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await })
            .await
            .unwrap();

    assert!(
        connect_result.is_err(),
        "connecting to a peer that doesn't trust us must fail, not silently succeed"
    );
    assert!(
        accept_task.await.unwrap().is_err(),
        "the rejecting side must not treat this as a connected peer either"
    );
}

#[tokio::test]
async fn accepting_a_connection_from_an_untrusted_device_is_rejected() {
    let a = support::Device::new([12u8; 32]);
    let b = support::Device::new([13u8; 32]);
    // The other direction: B trusts A, but A does NOT trust B.
    b.trust(a.device_id);

    let accept_task = spawn_accept(&b).await;

    let addr = b.addr;
    let our_id = a.device_id;
    let endpoint = a.endpoint.clone();
    let connect_task = tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await });

    let connect_result = connect_task.await.unwrap();
    let accept_result = accept_task.await.unwrap();

    assert!(
        connect_result.is_err(),
        "the connecting side must see the handshake fail"
    );
    assert!(
        accept_result.is_err(),
        "the accepting side must not treat an untrusted client as connected"
    );
}

#[tokio::test]
async fn revoked_device_is_rejected_even_though_it_was_previously_trusted() {
    // Named explicitly in the build plan's risk register: a device that
    // was trusted and got revoked must not be re-accepted just because it
    // still holds the same keypair and tries again.
    let (a, b) = support::mutually_trusting_pair([14u8; 32], [15u8; 32]);
    a.untrust(&b.device_id);

    let accept_task = spawn_accept(&b).await;

    let addr = b.addr;
    let our_id = a.device_id;
    let endpoint = a.endpoint.clone();
    let connect_result =
        tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await })
            .await
            .unwrap();

    assert!(
        connect_result.is_err(),
        "a revoked device must be rejected, not silently re-trusted"
    );
    assert!(accept_task.await.unwrap().is_err());
}
