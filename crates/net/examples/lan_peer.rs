//! Manual cross-machine LAN test for Phase 1c's DoD ("real cross-machine
//! LAN test — not just loopback"). Not run by `cargo test`; run by hand on
//! two separate machines on the same LAN.
//!
//! Uses two fixed, hardcoded device seeds so both sides trust each other
//! out of the box — fine for this manual check, never do this for a real
//! device identity.
//!
//! ## Usage
//!
//! On machine 1 (the listener):
//! ```text
//! cargo run -p kvm-net --example lan_peer -- listen
//! ```
//! Note the printed address (it prints the port; use machine 1's real LAN
//! IP, e.g. from `ifconfig`/`ip addr`, with that port).
//!
//! On machine 2 (the connector):
//! ```text
//! cargo run -p kvm-net --example lan_peer -- connect 192.168.1.23:51820
//! ```
//!
//! Success looks like both processes printing "ping/pong exchanged
//! successfully" and exiting 0. This exercises the full stack (TLS mutual
//! auth, stream setup, framed messaging) over a real network path instead
//! of loopback — real NICs, real OS network stacks, real routers/switches
//! in between, which is exactly what loopback cannot exercise.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::env;
use std::net::{Ipv4Addr, SocketAddr};

use kvm_protocol::{ControlMessage, Message};

const SEED_LISTENER: [u8; 32] = [0xAA; 32];
const SEED_CONNECTOR: [u8; 32] = [0xBB; 32];

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("listen") => run_listener().await,
        Some("connect") => {
            let addr: SocketAddr = args
                .get(2)
                .expect("usage: lan_peer connect <peer_addr>")
                .parse()
                .expect("invalid socket address");
            run_connector(addr).await;
        }
        _ => {
            eprintln!("usage: lan_peer listen | lan_peer connect <peer_addr>");
            std::process::exit(2);
        }
    }
}

fn identity_and_trust(
    our_seed: [u8; 32],
    peer_seed: [u8; 32],
) -> (
    kvm_net::IdentityCert,
    kvm_protocol::DeviceId,
    kvm_net::TrustCheck,
) {
    let our_keypair = kvm_identity::DeviceKeypair::from_secret_bytes(&our_seed);
    let peer_keypair = kvm_identity::DeviceKeypair::from_secret_bytes(&peer_seed);
    let peer_device_id = peer_keypair.device_id();

    let identity = kvm_net::IdentityCert::from_seed(&our_seed).expect("build identity cert");
    let trust_check: kvm_net::TrustCheck = std::sync::Arc::new(move |id| *id == peer_device_id);
    (identity, our_keypair.device_id(), trust_check)
}

async fn run_listener() {
    let (identity, our_device_id, trust_check) = identity_and_trust(SEED_LISTENER, SEED_CONNECTOR);

    // 0.0.0.0 so it's reachable from another machine on the LAN, not just
    // loopback.
    let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 51820));
    let endpoint = kvm_net::new_endpoint(bind_addr, &identity, trust_check).expect("bind endpoint");
    println!(
        "listening on {} — connect from the other machine with:\n  cargo run -p kvm-net --example lan_peer -- connect <this-machine-lan-ip>:{}",
        endpoint.local_addr().unwrap(),
        endpoint.local_addr().unwrap().port()
    );

    let incoming = endpoint.accept().await.expect("endpoint closed");
    let mut peer = kvm_net::accept(incoming, our_device_id)
        .await
        .expect("handshake failed");
    println!("accepted connection from {:?}", peer.remote_device_id);

    match peer.recv_control().await.expect("failed to receive") {
        ControlMessage::Ping { nonce } => {
            peer.streams
                .control
                .send(&Message::Control(ControlMessage::Pong { nonce }))
                .await
                .expect("failed to send pong");
            println!("ping/pong exchanged successfully");
        }
        other => panic!("expected Ping, got {other:?}"),
    }
}

async fn run_connector(peer_addr: SocketAddr) {
    let (identity, our_device_id, trust_check) = identity_and_trust(SEED_CONNECTOR, SEED_LISTENER);

    let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
    let endpoint = kvm_net::new_endpoint(bind_addr, &identity, trust_check).expect("bind endpoint");

    let mut peer = kvm_net::connect(&endpoint, peer_addr, our_device_id)
        .await
        .expect("handshake failed");
    println!("connected to {:?}", peer.remote_device_id);

    peer.streams
        .control
        .send(&Message::Control(ControlMessage::Ping { nonce: 42 }))
        .await
        .expect("failed to send ping");

    match peer.recv_control().await.expect("failed to receive") {
        ControlMessage::Pong { nonce: 42 } => println!("ping/pong exchanged successfully"),
        other => panic!("expected Pong{{42}}, got {other:?}"),
    }
}
