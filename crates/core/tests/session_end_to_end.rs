//! Phase 4 DoD: automated tests covering the multi-device routing/state
//! machine end to end, over *real* loopback `net::Peer` connections
//! (real QUIC/TLS, real identity-pinned trust) — everything except the
//! OS ends of the pipe (`Capture`/`Inject`/`PointerGeometry`, stood in
//! for by fakes/`Session`'s own bookkeeping) is genuine. See ADR-0009.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use kvm_core::{Edge, Layout, LayoutDevice, OwnershipState, Session, become_target};
use kvm_identity::DeviceKeypair;
use kvm_input::{Inject, InputError, PointerGeometry};
use kvm_net::{IdentityCert, Peer, TrustCheck};
use kvm_protocol::{ButtonState, DeviceId, InputMessage, Key, Message, PlatformKind};

struct Device {
    device_id: DeviceId,
    endpoint: quinn::Endpoint,
    addr: SocketAddr,
    trusted: Arc<Mutex<HashSet<DeviceId>>>,
}

impl Device {
    fn new(seed: [u8; 32]) -> Self {
        let keypair = DeviceKeypair::from_secret_bytes(&seed);
        let device_id = keypair.device_id();
        let identity = IdentityCert::from_seed(&seed).unwrap();
        let trusted = Arc::new(Mutex::new(HashSet::new()));
        let trusted_for_check = trusted.clone();
        let trust_check: TrustCheck =
            Arc::new(move |id: &DeviceId| trusted_for_check.lock().unwrap().contains(id));
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let endpoint = kvm_net::new_endpoint(bind_addr, &identity, trust_check).unwrap();
        let addr = endpoint.local_addr().unwrap();
        Self {
            device_id,
            endpoint,
            addr,
            trusted,
        }
    }

    fn trust(&self, other: DeviceId) {
        self.trusted.lock().unwrap().insert(other);
    }
}

/// Establishes a real, mutually-authenticated loopback connection and
/// returns each side's `Peer` from that side's own perspective.
async fn connect_pair(a: &Device, b: &Device) -> (Peer, Peer) {
    a.trust(b.device_id);
    b.trust(a.device_id);

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
    let peer_a = connect_task.await.unwrap().unwrap();
    let peer_b = accept_task.await.unwrap().unwrap();
    (peer_a, peer_b)
}

fn key_event(key: Key, state: ButtonState) -> InputMessage {
    InputMessage::Key {
        key,
        state,
        repeat: false,
        source_os: PlatformKind::MacOs,
    }
}

/// A two-device layout: `US` on the left, `NEIGHBOR` to its right.
fn two_device_layout(us: DeviceId, neighbor: DeviceId) -> Layout {
    let mut layout = Layout::new();
    layout.add_device(LayoutDevice {
        device_id: us,
        label: "us".to_string(),
        enabled: true,
    });
    layout.add_device(LayoutDevice {
        device_id: neighbor,
        label: "neighbor".to_string(),
        enabled: true,
    });
    layout.set_neighbor(us, Edge::Right, neighbor).unwrap();
    layout
}

struct FakeGeometry {
    position: Arc<Mutex<(i32, i32)>>,
}

impl PointerGeometry for FakeGeometry {
    fn cursor_position(&self) -> Result<(i32, i32), InputError> {
        Ok(*self.position.lock().unwrap())
    }

    fn screen_size(&self) -> Result<(u32, u32), InputError> {
        Ok((1000, 800))
    }

    fn set_cursor_position(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        *self.position.lock().unwrap() = (x, y);
        Ok(())
    }
}

struct FakeInject {
    received: Arc<Mutex<Vec<InputMessage>>>,
}

impl Inject for FakeInject {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError> {
        self.received.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn edge_crossing_switches_active_target_and_warps_the_targets_cursor() {
    let a = Device::new([1u8; 32]);
    let b = Device::new([2u8; 32]);
    let (peer_a, mut peer_b) = connect_pair(&a, &b).await;

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));

    let target_position = Arc::new(Mutex::new((999, 999)));
    let mut target_geometry = FakeGeometry {
        position: target_position.clone(),
    };
    let target_received = Arc::new(Mutex::new(Vec::new()));
    let target_task = {
        let received = target_received.clone();
        tokio::spawn(async move {
            let mut inject = FakeInject { received };
            become_target(&mut peer_b, &mut target_geometry, &mut inject).await
        })
    };

    // Push far enough right to cross the configured edge.
    session
        .handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 })
        .await
        .unwrap();
    assert_eq!(
        session.ownership_state(),
        OwnershipState::Forwarding {
            target: b.device_id,
            virtual_cursor: (0, 400),
        }
    );

    // A follow-up move should now be forwarded to B, proving the
    // control-then-input sequence lands correctly on the real wire.
    session
        .handle_captured(InputMessage::MouseMove { dx: 10, dy: 5 })
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if !target_received.lock().unwrap().is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the forwarded move to be injected"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert_eq!(*target_position.lock().unwrap(), (0, 400));
    assert_eq!(
        *target_received.lock().unwrap(),
        vec![InputMessage::MouseMove { dx: 10, dy: 5 }]
    );

    drop(session);
    let _ = target_task.await;
}

#[tokio::test]
async fn an_unregistered_device_can_never_become_a_target_even_if_the_layout_names_it() {
    let a = Device::new([3u8; 32]);
    let untrusted_id = DeviceKeypair::from_secret_bytes(&[4u8; 32]).device_id();

    // The layout names a neighbor across the right edge, but that
    // device was never passed to `Session::add_peer` -- no `net::Peer`
    // exists for it, so it structurally cannot receive anything,
    // matching the kickoff's "an untrusted/unpaired device must never
    // become an input target" requirement.
    let layout = two_device_layout(a.device_id, untrusted_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));

    session
        .handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 })
        .await
        .unwrap();

    assert_eq!(session.ownership_state(), OwnershipState::Local);
}

#[tokio::test]
async fn disconnect_then_reconnect_recovers_ownership_without_restarting_the_session() {
    let a = Device::new([5u8; 32]);
    let b = Device::new([6u8; 32]);
    let (peer_a, peer_b_first) = connect_pair(&a, &b).await;

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));

    session
        .handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 })
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { .. }
    ));

    // Simulate a connection-health monitor noticing B is gone (dropping
    // its Peer ends the connection).
    drop(peer_b_first);
    session.remove_peer(b.device_id);
    assert_eq!(
        session.ownership_state(),
        OwnershipState::Local,
        "a disconnected target must fall back to local input immediately"
    );
    session.resync_local_position(500, 400);

    // Local input must keep working with no crash and no special
    // handling required from the caller.
    session
        .handle_captured(InputMessage::MouseMove { dx: 1, dy: 1 })
        .await
        .unwrap();

    // Reconnect: a brand new real Peer connection, no session restart.
    let (peer_a_second, _peer_b_second) = connect_pair(&a, &b).await;
    session.add_peer(b.device_id, peer_a_second, (1000, 800));

    session
        .handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 })
        .await
        .unwrap();
    assert!(
        matches!(
            session.ownership_state(),
            OwnershipState::Forwarding { target, .. } if target == b.device_id
        ),
        "ownership must recover through the freshly reconnected peer"
    );
}

#[tokio::test]
async fn switching_to_a_new_target_flushes_held_modifiers_on_the_old_one_as_ordinary_input() {
    let a = Device::new([7u8; 32]);
    let b = Device::new([8u8; 32]);
    let c = Device::new([9u8; 32]);
    let (peer_a_b, mut peer_b) = connect_pair(&a, &b).await;
    let (peer_a_c, mut peer_c) = connect_pair(&a, &c).await;

    let mut layout = Layout::new();
    for (id, label) in [(a.device_id, "a"), (b.device_id, "b"), (c.device_id, "c")] {
        layout.add_device(LayoutDevice {
            device_id: id,
            label: label.to_string(),
            enabled: true,
        });
    }
    layout
        .set_neighbor(a.device_id, Edge::Right, b.device_id)
        .unwrap();
    layout
        .set_neighbor(b.device_id, Edge::Right, c.device_id)
        .unwrap();

    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a_b, (1000, 800));
    session.add_peer(c.device_id, peer_a_c, (1000, 800));

    // -> Forwarding(B); hold Shift on B.
    session
        .handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 })
        .await
        .unwrap();
    session
        .handle_captured(key_event(Key::ShiftLeft, ButtonState::Pressed))
        .await
        .unwrap();

    // Cross onward from B's screen to C -- must flush Shift's Released
    // onto B before (or as part of) the switch to C.
    session
        .handle_captured(InputMessage::MouseMove { dx: 1100, dy: 0 })
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { target, .. } if target == c.device_id
    ));

    // B must have received the held key's Pressed (from before the
    // onward switch) followed by its flushed Released, as plain Input
    // messages on its ordinary input stream -- no dedicated "reset"
    // mechanism needed on the receiving side.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let released = loop {
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            peer_b.streams.input.recv(),
        )
        .await
        {
            Ok(Ok(Message::Input(
                event @ InputMessage::Key {
                    state: ButtonState::Released,
                    ..
                },
            ))) => {
                break event;
            }
            Ok(Ok(_)) => {} // the earlier Pressed -- keep waiting for the Released
            _ => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for B to receive the flushed Released"
                );
            }
        }
    };
    assert_eq!(released, key_event(Key::ShiftLeft, ButtonState::Released));

    // C must never see that key at all -- no leakage to the new
    // target of state that belonged to the old one.
    let leaked = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        peer_c.streams.input.recv(),
    )
    .await;
    assert!(
        leaked.is_err(),
        "C must not receive the old target's held-key release"
    );

    drop(peer_b);
}

#[tokio::test]
async fn only_the_active_target_receives_input_never_an_inactive_connected_peer() {
    let a = Device::new([10u8; 32]);
    let b = Device::new([11u8; 32]);
    let c = Device::new([12u8; 32]);
    let (peer_a_b, mut peer_b) = connect_pair(&a, &b).await;
    let (peer_a_c, mut peer_c) = connect_pair(&a, &c).await;

    // Only a Right neighbor is configured (to B); C is connected and
    // trusted but not reachable via any edge from A right now.
    let mut layout = Layout::new();
    for (id, label) in [(a.device_id, "a"), (b.device_id, "b"), (c.device_id, "c")] {
        layout.add_device(LayoutDevice {
            device_id: id,
            label: label.to_string(),
            enabled: true,
        });
    }
    layout
        .set_neighbor(a.device_id, Edge::Right, b.device_id)
        .unwrap();

    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a_b, (1000, 800));
    session.add_peer(c.device_id, peer_a_c, (1000, 800));

    session
        .handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 })
        .await
        .unwrap();
    session
        .handle_captured(key_event(Key::A, ButtonState::Pressed))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            peer_b.streams.input.recv(),
        )
        .await
        {
            Ok(Ok(Message::Input(InputMessage::Key { .. }))) => break,
            _ => {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timed out waiting for B to receive the key"
                );
            }
        }
    }

    let leaked = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        peer_c.streams.input.recv(),
    )
    .await;
    assert!(
        leaked.is_err(),
        "C is connected but not the active target -- it must receive nothing"
    );
}
