//! Phase 1c DoD: "replayed-handshake test."
//!
//! Once the initial Hello exchange completes, the control stream is for
//! [`kvm_protocol::ControlMessage`]s only — a second Handshake message on
//! an already-established connection is a protocol violation, not
//! something that gets silently accepted or re-processed as if it were a
//! fresh session.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use kvm_protocol::{HandshakeMessage, Message};

#[tokio::test]
async fn replaying_a_handshake_message_after_setup_is_a_protocol_violation() {
    let (a, b) = support::mutually_trusting_pair([20u8; 32], [21u8; 32]);

    let connect_task = {
        let addr = b.addr;
        let our_id = a.device_id;
        let endpoint = a.endpoint.clone();
        tokio::spawn(async move { kvm_net::connect(&endpoint, addr, our_id).await })
    };
    let accept_task = {
        let our_id = b.device_id;
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept(incoming, our_id).await
        })
    };

    let mut peer_a = connect_task.await.unwrap().unwrap();
    let mut peer_b = accept_task.await.unwrap().unwrap();

    // The one-time handshake already completed inside `connect`/`accept`.
    // Replay it: A sends *another* Hello on the same control stream,
    // exactly as a captured-and-resent handshake packet would look from
    // B's point of view.
    peer_a
        .streams
        .control
        .send(&Message::Handshake(HandshakeMessage::Hello {
            device_id: a.device_id,
        }))
        .await
        .unwrap();

    let result = peer_b.recv_control().await;
    assert!(
        result.is_err(),
        "a replayed Handshake message on the control stream must be rejected, got {result:?}"
    );
}
