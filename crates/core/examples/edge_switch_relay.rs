//! Manual cross-machine test for Phase 4's automatic edge-based
//! switching — see `docs/manual-qa/phase-4-edge-switching.md` and
//! ADR-0009. Not run by `cargo test` — run by hand across two real
//! machines with a real physical desk between them.
//!
//! Unlike `input_relay` (one fixed, manually-chosen direction), this
//! exercises the real `Session`/`Router` state machine: move the
//! mouse to the configured screen edge on the *hub* machine and
//! control should switch automatically, then switch back when the
//! mouse crosses back the other way — no restart needed either time.
//!
//! Only one machine (the "hub") ever runs a `Session` — see ADR-0009
//! for why only the device that's actually capturing real local input
//! ever tracks position or makes switching decisions. The other
//! machine ("join") is a pure target for the whole run: it never
//! captures its own input for switching purposes, matching how
//! established KVM tools (Synergy/Barrier/Deskflow) model a single
//! physical input source. This example only wires up a two-device
//! (hub + one peer) layout, with the peer mapped to `--edge` from the
//! hub and the hub mapped back on the opposite edge from the peer, so
//! ownership can cross both ways. A real star/cross topology (the
//! kickoff's Linux/Mac/Windows/Laptop diagram) extends this the same
//! way `Layout` already supports — one more `add_device`/`set_neighbor`
//! pair and one more `Session::add_peer` per additional machine — just
//! not concretely exercisable with only two physical machines on hand.
//!
//! ## Usage
//!
//! On the machine whose keyboard/mouse should stay physically in use
//! (the hub):
//! ```text
//! cargo run -p kvm-core --example edge_switch_relay -- hub --edge right
//! ```
//! On the other machine:
//! ```text
//! cargo run -p kvm-core --example edge_switch_relay -- join <hub-lan-ip>:51823
//! ```
//! `--edge` accepts `left`/`right`/`top`/`bottom` (default `right`) —
//! which edge of the hub's screen the peer sits across. On macOS,
//! capturing (the hub role) requires Accessibility permission for the
//! terminal/binary running this example.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
mod real {
    use std::env;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use std::sync::mpsc as std_mpsc;

    use kvm_core::{
        Edge, Layout, LayoutDevice, OwnershipState, Session, exchange_screen_size, run_target,
    };
    use kvm_identity::DeviceKeypair;
    use kvm_input::{Capture, PointerGeometry};
    use kvm_net::{IdentityCert, TrustCheck};
    use kvm_protocol::InputMessage;

    #[cfg(target_os = "macos")]
    use kvm_input::{MacCapture as LocalCapture, MacInject as LocalInject};
    #[cfg(windows)]
    use kvm_input::{WindowsCapture as LocalCapture, WindowsInject as LocalInject};
    #[cfg(target_os = "linux")]
    use kvm_input::{X11Capture as LocalCapture, X11Inject as LocalInject};

    /// Unlike `MacInject`/`WindowsInject` (infallible `Default`),
    /// `X11Inject::new()` connects to the X server immediately and can
    /// fail — this wrapper keeps that one difference from leaking into
    /// the otherwise-identical bodies below. See `input_relay.rs`'s
    /// identical helper.
    #[cfg(any(target_os = "macos", windows))]
    fn new_local_inject() -> LocalInject {
        LocalInject::new()
    }
    #[cfg(target_os = "linux")]
    fn new_local_inject() -> LocalInject {
        LocalInject::new().expect("failed to connect to the X server")
    }

    const SEED_HUB: [u8; 32] = [0xEE; 32];
    const SEED_JOIN: [u8; 32] = [0xFF; 32];
    const LISTEN_PORT: u16 = 51823;

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

    fn parse_edge(s: &str) -> Edge {
        match s {
            "left" => Edge::Left,
            "right" => Edge::Right,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            other => panic!("invalid --edge {other:?}: expected left|right|top|bottom"),
        }
    }

    fn opposite(edge: Edge) -> Edge {
        match edge {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }

    fn parse_edge_flag(args: &[String]) -> Edge {
        args.iter()
            .position(|a| a == "--edge")
            .and_then(|i| args.get(i + 1))
            .map(|s| parse_edge(s))
            .unwrap_or(Edge::Right)
    }

    pub async fn run() {
        let args: Vec<String> = env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("hub") => run_hub(parse_edge_flag(&args)).await,
            Some("join") => {
                let addr: SocketAddr = args
                    .get(2)
                    .expect("usage: edge_switch_relay join <hub_addr>")
                    .parse()
                    .expect("invalid socket address");
                run_join(addr).await;
            }
            _ => {
                eprintln!(
                    "usage: edge_switch_relay hub [--edge left|right|top|bottom] | edge_switch_relay join <hub_addr>"
                );
                std::process::exit(2);
            }
        }
    }

    async fn run_hub(edge_to_peer: Edge) {
        let (identity, our_device_id, trust_check) = identity_and_trust(SEED_HUB, SEED_JOIN);
        let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, LISTEN_PORT));
        let endpoint =
            kvm_net::new_endpoint(bind_addr, &identity, trust_check).expect("bind endpoint");
        println!(
            "hub listening on {} — the peer is configured across the {edge_to_peer:?} edge",
            endpoint.local_addr().unwrap()
        );

        let incoming = endpoint.accept().await.expect("endpoint closed");
        let mut peer = kvm_net::accept(incoming, our_device_id)
            .await
            .expect("handshake failed");
        let peer_device_id = peer.remote_device_id;
        println!("accepted connection from {peer_device_id:?}");

        let mut geometry = new_local_inject();
        let our_screen_size = geometry.screen_size().expect("read local screen size");
        let initial_position = geometry
            .cursor_position()
            .expect("read local cursor position");
        println!("our screen size: {our_screen_size:?}, starting cursor: {initial_position:?}");

        let peer_screen_size = exchange_screen_size(&mut peer, our_screen_size)
            .await
            .expect("screen-size exchange failed");
        println!("peer screen size: {peer_screen_size:?}");

        let mut layout = Layout::new();
        layout.add_device(LayoutDevice {
            device_id: our_device_id,
            label: "hub".to_string(),
            enabled: true,
        });
        layout.add_device(LayoutDevice {
            device_id: peer_device_id,
            label: "peer".to_string(),
            enabled: true,
        });
        layout
            .set_neighbor(our_device_id, edge_to_peer, peer_device_id)
            .unwrap();
        layout
            .set_neighbor(peer_device_id, opposite(edge_to_peer), our_device_id)
            .unwrap();

        let mut session = Session::new(our_device_id, layout, our_screen_size, initial_position);
        session.add_peer(peer_device_id, peer, peer_screen_size);

        let (sink, source) = std_mpsc::channel();
        let mut capture = LocalCapture::new();
        capture.start(sink).expect("failed to start capture");
        println!(
            "capturing local input (Ctrl+C to stop) — move the mouse to the {edge_to_peer:?} \
             edge to switch, cross back the other way to return"
        );

        // Bridge the blocking std::sync::mpsc capture channel onto a
        // tokio channel (same pattern as `input_bridge::forward_capture_to_peer`)
        // so this loop can `select!` between captured events and new
        // incoming connections. Without this, `endpoint.accept()` is
        // never polled again after the first connection, and a real
        // hardware disconnect (network blip, sleep/wake, anything not
        // caused by a code bug -- see ADR-0009's Update note) leaves
        // no way back in short of killing and restarting this whole
        // process, contradicting the Phase 4 DoD's explicit
        // "reconnection must work without app restart."
        let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            while let Ok(event) = source.recv() {
                if async_tx.send(event).is_err() {
                    break;
                }
            }
        });

        let mut last_state = session.ownership_state();
        loop {
            tokio::select! {
                event = async_rx.recv() => {
                    let Some(event) = event else {
                        println!("capture ended");
                        break;
                    };
                    let is_move = matches!(event, InputMessage::MouseMove { .. });
                    if let Err(e) = session.handle_captured(event, &mut geometry).await {
                        eprintln!("session error: {e}");
                    }
                    let state = session.ownership_state();
                    if state != last_state {
                        println!("ownership changed: {last_state:?} -> {state:?}");
                        if state == OwnershipState::Local
                            && let Ok(pos) = geometry.cursor_position()
                        {
                            session.resync_local_position(pos.0, pos.1);
                            println!("resynced local cursor position to {pos:?}");
                        }
                        last_state = state;
                    } else if is_move {
                        // Only mouse moves can change ownership; everything
                        // else (keys, buttons, scroll) never does, so this
                        // branch is the "nothing changed" common case for
                        // moves specifically — deliberately not logged, to
                        // avoid the excessive per-event noise the kickoff
                        // warns against.
                    }
                }
                incoming = endpoint.accept() => {
                    let Some(incoming) = incoming else {
                        println!("endpoint closed");
                        break;
                    };
                    match kvm_net::accept(incoming, our_device_id).await {
                        Ok(mut new_peer) => {
                            if new_peer.remote_device_id != peer_device_id {
                                eprintln!(
                                    "rejecting connection from unexpected device {:?} \
                                     (only {peer_device_id:?} is configured in this layout)",
                                    new_peer.remote_device_id
                                );
                                continue;
                            }
                            match exchange_screen_size(&mut new_peer, our_screen_size).await {
                                Ok(new_screen_size) => {
                                    println!(
                                        "peer reconnected, screen size: {new_screen_size:?} \
                                         -- no restart needed"
                                    );
                                    session.add_peer(peer_device_id, new_peer, new_screen_size);
                                }
                                Err(e) => {
                                    eprintln!("screen-size exchange failed on reconnect: {e}");
                                }
                            }
                        }
                        Err(e) => eprintln!("failed to accept a reconnect attempt: {e}"),
                    }
                }
            }
        }
    }

    async fn run_join(hub_addr: SocketAddr) {
        let (identity, our_device_id, trust_check) = identity_and_trust(SEED_JOIN, SEED_HUB);
        let bind_addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0));
        let endpoint =
            kvm_net::new_endpoint(bind_addr, &identity, trust_check).expect("bind endpoint");

        let mut peer = kvm_net::connect(&endpoint, hub_addr, our_device_id)
            .await
            .expect("handshake failed");
        println!("connected to hub {:?}", peer.remote_device_id);

        let mut geometry = new_local_inject();
        let our_screen_size = geometry.screen_size().expect("read local screen size");
        exchange_screen_size(&mut peer, our_screen_size)
            .await
            .expect("screen-size exchange failed");

        let mut inject = new_local_inject();
        println!(
            "waiting to become the active input target (Ctrl+C to stop) — \
             this machine's own input is never captured for switching"
        );
        match run_target(&mut peer, &mut geometry, &mut inject).await {
            Ok(()) => println!("hub disconnected"),
            Err(e) => eprintln!("session error: {e}"),
        }
    }
}

#[cfg(any(target_os = "macos", windows, target_os = "linux"))]
#[tokio::main]
async fn main() {
    // Off by default -- set RUST_LOG=kvm_core=trace (or =debug/=info) to
    // watch the router's position/edge/ownership decisions live while
    // diagnosing a real-hardware edge-switch issue. See ADR-0009.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    real::run().await;
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
fn main() {
    eprintln!(
        "edge_switch_relay: no Capture/Inject backend on this OS yet \
         (see ADR-0007 and ADR-0008 for what's supported). Nothing to run."
    );
}
