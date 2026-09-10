//! Manual cross-machine input test for Phase 3's macOS<->Windows target
//! pair. Not run by `cargo test` — run by hand on two real machines, one
//! on each side of a real desk, per `docs/manual-qa/phase-3-input.md`.
//!
//! Not built at all on Linux (no `Capture`/`Inject` backend yet — see
//! ADR-0007 §6); `cargo build --workspace --all-targets` still succeeds
//! there via the stub `main` below.
//!
//! ## Usage
//!
//! On the machine whose keyboard/mouse should be *controlled* (the
//! injection target):
//! ```text
//! cargo run -p kvm-core --example input_relay -- listen
//! ```
//!
//! On the machine whose keyboard/mouse should be *captured* (the
//! source):
//! ```text
//! cargo run -p kvm-core --example input_relay -- connect <listener-lan-ip>:51821
//! ```
//! On macOS, capture requires Accessibility permission for the terminal
//! (or the built binary) running this example — grant it in System
//! Settings > Privacy & Security > Accessibility, then re-run; a missing
//! grant is reported as a clear error, not a silent no-op.
//!
//! ## What gets measured, and what doesn't
//!
//! Both sides log a network round-trip time over the control stream
//! (existing `ControlMessage::Ping`/`Pong`, the same mechanism
//! `lan_peer.rs` uses) as a proxy for one-way transport latency. The
//! listener additionally logs how long each *local* `inject()` call
//! takes. Neither machine's clock is assumed to be synchronized with
//! the other's, so no single "capture-to-injection" timestamp delta is
//! computed or logged — that would silently imply a precision this
//! setup can't actually provide. The manual QA doc asks the human
//! tester to also judge felt end-to-end latency directly (typing/moving
//! the mouse and watching the target machine).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(any(target_os = "macos", windows))]
mod real {
    use std::env;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::time::Instant;

    use kvm_core::forward_capture_to_peer;
    use kvm_identity::DeviceKeypair;
    use kvm_input::Inject;
    use kvm_net::{IdentityCert, TrustCheck};
    use kvm_protocol::{ControlMessage, Message};

    #[cfg(target_os = "macos")]
    use kvm_input::{MacCapture as LocalCapture, MacInject as LocalInject};
    #[cfg(windows)]
    use kvm_input::{WindowsCapture as LocalCapture, WindowsInject as LocalInject};

    const SEED_LISTENER: [u8; 32] = [0xCC; 32];
    const SEED_CONNECTOR: [u8; 32] = [0xDD; 32];
    const LISTEN_PORT: u16 = 51821;

    pub async fn run() {
        let args: Vec<String> = env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("listen") => run_listener().await,
            Some("connect") => {
                let addr: SocketAddr = args
                    .get(2)
                    .expect("usage: input_relay connect <peer_addr>")
                    .parse()
                    .expect("invalid socket address");
                run_connector(addr).await;
            }
            _ => {
                eprintln!("usage: input_relay listen | input_relay connect <peer_addr>");
                std::process::exit(2);
            }
        }
    }

    fn identity_and_trust(
        our_seed: [u8; 32],
        peer_seed: [u8; 32],
    ) -> (IdentityCert, kvm_protocol::DeviceId, TrustCheck) {
        let our_keypair = DeviceKeypair::from_secret_bytes(&our_seed);
        let peer_keypair = DeviceKeypair::from_secret_bytes(&peer_seed);
        let peer_device_id = peer_keypair.device_id();

        let identity = IdentityCert::from_seed(&our_seed).expect("build identity cert");
        let trust_check: TrustCheck = Arc::new(move |id| *id == peer_device_id);
        (identity, our_keypair.device_id(), trust_check)
    }

    /// Round-trips a Ping and logs the elapsed time — a proxy for
    /// one-way transport latency (halve it), independent of either
    /// machine's wall-clock.
    async fn measure_round_trip(peer: &mut kvm_net::Peer, send_first: bool) {
        if send_first {
            let started = Instant::now();
            peer.streams
                .control
                .send(&Message::Control(ControlMessage::Ping { nonce: 1 }))
                .await
                .expect("failed to send ping");
            match peer.recv_control().await.expect("failed to receive") {
                ControlMessage::Pong { .. } => {
                    println!("network round-trip: {:?}", started.elapsed());
                }
                other => panic!("expected Pong, got {other:?}"),
            }
        } else {
            match peer.recv_control().await.expect("failed to receive") {
                ControlMessage::Ping { nonce } => {
                    peer.streams
                        .control
                        .send(&Message::Control(ControlMessage::Pong { nonce }))
                        .await
                        .expect("failed to send pong");
                }
                other => panic!("expected Ping, got {other:?}"),
            }
        }
    }

    async fn run_listener() {
        let (identity, our_device_id, trust_check) =
            identity_and_trust(SEED_LISTENER, SEED_CONNECTOR);
        let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, LISTEN_PORT));
        let endpoint =
            kvm_net::new_endpoint(bind_addr, &identity, trust_check).expect("bind endpoint");
        println!(
            "listening on {} — this machine's input will be controlled by whatever the connecting side captures",
            endpoint.local_addr().unwrap()
        );

        let incoming = endpoint.accept().await.expect("endpoint closed");
        let mut peer = kvm_net::accept(incoming, our_device_id)
            .await
            .expect("handshake failed");
        println!("accepted connection from {:?}", peer.remote_device_id);

        measure_round_trip(&mut peer, false).await;

        let mut inject = LocalInject::new();
        println!("injecting incoming input events (Ctrl+C here to stop)...");
        loop {
            match peer.streams.input.recv().await {
                Ok(Message::Input(event)) => {
                    let started = Instant::now();
                    match inject.inject(&event) {
                        Ok(()) => println!("injected {event:?} in {:?}", started.elapsed()),
                        Err(e) => eprintln!("injection failed for {event:?}: {e}"),
                    }
                }
                Ok(other) => eprintln!("unexpected message on input stream: {other:?}"),
                Err(e) => {
                    println!("input stream ended: {e}");
                    break;
                }
            }
        }
    }

    async fn run_connector(peer_addr: SocketAddr) {
        let (identity, our_device_id, trust_check) =
            identity_and_trust(SEED_CONNECTOR, SEED_LISTENER);
        let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        let endpoint =
            kvm_net::new_endpoint(bind_addr, &identity, trust_check).expect("bind endpoint");

        let mut peer = kvm_net::connect(&endpoint, peer_addr, our_device_id)
            .await
            .expect("handshake failed");
        println!("connected to {:?}", peer.remote_device_id);

        measure_round_trip(&mut peer, true).await;

        println!("capturing local input and forwarding it (Ctrl+C here to stop)...");
        let mut capture = LocalCapture::new();
        forward_capture_to_peer(&mut capture, &mut peer.streams.input)
            .await
            .expect("capture forwarding failed");
    }
}

#[cfg(any(target_os = "macos", windows))]
#[tokio::main]
async fn main() {
    real::run().await;
}

#[cfg(not(any(target_os = "macos", windows)))]
fn main() {
    eprintln!(
        "input_relay: no Capture/Inject backend on this OS yet \
         (Linux is deferred to the Phase 3b X11/Wayland spike — see ADR-0007 §6). Nothing to run."
    );
}
