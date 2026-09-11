//! Phase 4 DoD: automated tests covering the multi-device routing/state
//! machine end to end, over *real* loopback `net::Peer` connections
//! (real QUIC/TLS, real identity-pinned trust) — everything except the
//! OS ends of the pipe (`Capture`/`Inject`/`PointerGeometry`, stood in
//! for by fakes/`Session`'s own bookkeeping) is genuine. See ADR-0009.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use kvm_core::{
    Edge, Layout, LayoutDevice, OwnershipState, Session, exchange_screen_size, run_target,
};
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

impl FakeGeometry {
    fn new(initial: (i32, i32)) -> Self {
        Self {
            position: Arc::new(Mutex::new(initial)),
        }
    }
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

/// Fails exactly once (on the Nth call), then succeeds for every call
/// after that -- stands in for a real, transient `SendInput`/`CGEvent`
/// rejection (a focus change, a UIPI restriction, a momentary OS
/// hiccup) that must not be fatal to the whole target session.
struct FlakyInject {
    received: Arc<Mutex<Vec<InputMessage>>>,
    fail_on_call: usize,
    calls: usize,
}

impl Inject for FlakyInject {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError> {
        self.calls += 1;
        if self.calls == self.fail_on_call {
            return Err(InputError::InjectFailed(
                "simulated transient injection failure".to_string(),
            ));
        }
        self.received.lock().unwrap().push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn screen_size_exchange_is_symmetric_and_carries_each_sides_real_size() {
    let a = Device::new([18u8; 32]);
    let b = Device::new([19u8; 32]);
    let (mut peer_a, mut peer_b) = connect_pair(&a, &b).await;

    let (a_learned, b_learned) = tokio::join!(
        exchange_screen_size(&mut peer_a, (1920, 1080)),
        exchange_screen_size(&mut peer_b, (1280, 720)),
    );

    assert_eq!(a_learned.unwrap(), (1280, 720));
    assert_eq!(b_learned.unwrap(), (1920, 1080));
}

#[tokio::test]
async fn edge_crossing_switches_active_target_and_warps_the_targets_cursor() {
    let a = Device::new([1u8; 32]);
    let b = Device::new([2u8; 32]);
    let (peer_a, mut peer_b) = connect_pair(&a, &b).await;

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));
    let mut local_geometry = FakeGeometry::new((500, 400));

    let target_position = Arc::new(Mutex::new((999, 999)));
    let mut target_geometry = FakeGeometry {
        position: target_position.clone(),
    };
    let target_received = Arc::new(Mutex::new(Vec::new()));
    let target_task = {
        let received = target_received.clone();
        tokio::spawn(async move {
            let mut inject = FakeInject { received };
            run_target(&mut peer_b, &mut target_geometry, &mut inject).await
        })
    };

    // Push far enough right to cross the configured edge.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
        .await
        .unwrap();
    assert_eq!(
        session.ownership_state(),
        OwnershipState::Forwarding {
            target: b.device_id,
            // Nudged in from the exact boundary (x=0) by
            // EDGE_MARGIN+1 -- landing exactly on the boundary is
            // itself a real-hardware bug (see ADR-0009's Update note).
            virtual_cursor: (5, 400),
        }
    );

    // A follow-up move should now be forwarded to B, proving the
    // control-then-input sequence lands correctly on the real wire.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 10, dy: 5 },
            &mut local_geometry,
        )
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

    assert_eq!(*target_position.lock().unwrap(), (5, 400));
    assert_eq!(
        *target_received.lock().unwrap(),
        vec![InputMessage::MouseMove { dx: 10, dy: 5 }]
    );

    drop(session);
    let _ = target_task.await;
}

#[tokio::test]
async fn rapid_back_and_forth_re_warps_the_targets_cursor_on_every_return() {
    let a = Device::new([20u8; 32]);
    let b = Device::new([21u8; 32]);
    let (peer_a, mut peer_b) = connect_pair(&a, &b).await;

    // A two-way edge: A's right neighbor is B, and B's left neighbor is
    // A, so ownership can bounce back and forth repeatedly.
    let mut layout = Layout::new();
    layout.add_device(LayoutDevice {
        device_id: a.device_id,
        label: "a".to_string(),
        enabled: true,
    });
    layout.add_device(LayoutDevice {
        device_id: b.device_id,
        label: "b".to_string(),
        enabled: true,
    });
    layout
        .set_neighbor(a.device_id, Edge::Right, b.device_id)
        .unwrap();
    layout
        .set_neighbor(b.device_id, Edge::Left, a.device_id)
        .unwrap();

    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));
    let mut local_geometry = FakeGeometry::new((500, 400));

    let target_position = Arc::new(Mutex::new((999, 999)));
    let mut target_geometry = FakeGeometry {
        position: target_position.clone(),
    };
    let target_task = tokio::spawn(async move {
        let mut inject = FakeInject {
            received: Arc::new(Mutex::new(Vec::new())),
        };
        run_target(&mut peer_b, &mut target_geometry, &mut inject).await
    });

    for round in 0..3 {
        // Reset to the same starting position every round, so each
        // round's expected warp target is identical and independent of
        // the last -- isolating "does every return re-warp" from any
        // accumulated-position arithmetic.
        session.resync_local_position(500, 400);

        // -> Forwarding(B): warps B's cursor to the entry position.
        session
            .handle_captured(
                InputMessage::MouseMove { dx: 600, dy: 0 },
                &mut local_geometry,
            )
            .await
            .unwrap();
        assert!(matches!(
            session.ownership_state(),
            OwnershipState::Forwarding { target, .. } if target == b.device_id
        ));

        // Perturb B's cursor away from the expected warp target, so a
        // later round that *didn't* re-warp would be caught (rather
        // than passing by coincidence because it was already there
        // from the previous round).
        *target_position.lock().unwrap() = (777, 777);

        // -> Local: crossing back through B's Left edge.
        session
            .handle_captured(
                InputMessage::MouseMove { dx: -1100, dy: 0 },
                &mut local_geometry,
            )
            .await
            .unwrap();
        assert_eq!(session.ownership_state(), OwnershipState::Local);
        session.resync_local_position(500, 400);

        // -> Forwarding(B) again: must re-warp to (5, 400) (nudged in
        // from the exact boundary -- see ADR-0009's Update note), not
        // silently leave B's cursor at the perturbed (777, 777).
        session
            .handle_captured(
                InputMessage::MouseMove { dx: 600, dy: 0 },
                &mut local_geometry,
            )
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if *target_position.lock().unwrap() == (5, 400) {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "round {round}: timed out waiting for B's cursor to be re-warped to (5, 400)"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Cross back once more so the next round starts from Local.
        session
            .handle_captured(
                InputMessage::MouseMove { dx: -1100, dy: 0 },
                &mut local_geometry,
            )
            .await
            .unwrap();
        assert_eq!(session.ownership_state(), OwnershipState::Local);
    }

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
    let mut local_geometry = FakeGeometry::new((500, 400));

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
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
    let mut local_geometry = FakeGeometry::new((500, 400));

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
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
        .handle_captured(
            InputMessage::MouseMove { dx: 1, dy: 1 },
            &mut local_geometry,
        )
        .await
        .unwrap();

    // Reconnect: a brand new real Peer connection, no session restart.
    let (peer_a_second, _peer_b_second) = connect_pair(&a, &b).await;
    session.add_peer(b.device_id, peer_a_second, (1000, 800));

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
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
    let mut local_geometry = FakeGeometry::new((500, 400));

    // -> Forwarding(B); hold Shift on B.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
        .await
        .unwrap();
    session
        .handle_captured(
            key_event(Key::ShiftLeft, ButtonState::Pressed),
            &mut local_geometry,
        )
        .await
        .unwrap();

    // Cross onward from B's screen to C -- must flush Shift's Released
    // onto B before (or as part of) the switch to C.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 1100, dy: 0 },
            &mut local_geometry,
        )
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
    let mut local_geometry = FakeGeometry::new((500, 400));

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
        .await
        .unwrap();
    session
        .handle_captured(key_event(Key::A, ButtonState::Pressed), &mut local_geometry)
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

#[tokio::test]
async fn recentering_the_local_cursor_is_actually_applied_via_pointer_geometry() {
    // Regression test for a real Mac->Windows hardware QA finding (see
    // ADR-0009's Update note): Effect::RecenterLocal must actually
    // reach a real PointerGeometry::set_cursor_position call, not just
    // exist as a value nobody applies.
    let a = Device::new([22u8; 32]);
    let b = Device::new([23u8; 32]);
    let (peer_a, mut peer_b) = connect_pair(&a, &b).await;

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (990, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));
    // Start pinned near our own right edge -- exactly the scenario
    // that triggered the real bug.
    let mut local_geometry = FakeGeometry::new((990, 400));

    let target_task = tokio::spawn(async move {
        let mut geometry = FakeGeometry::new((0, 0));
        let mut inject = FakeInject {
            received: Arc::new(Mutex::new(Vec::new())),
        };
        run_target(&mut peer_b, &mut geometry, &mut inject).await
    });

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
        .await
        .unwrap();

    // Our own local cursor must now be away from the edge (the middle
    // of our 1000x800 screen), not still pinned at (990, 400).
    assert_eq!(local_geometry.cursor_position().unwrap(), (500, 400));

    drop(session);
    let _ = target_task.await;
}

#[tokio::test]
async fn a_single_transient_injection_failure_does_not_kill_the_whole_session() {
    // Regression test for a real Mac->Windows hardware QA finding (see
    // ADR-0009's Update note): run_target used to propagate any single
    // injection failure with `?`, ending the entire target session (and
    // therefore the whole connection) on one transient SendInput
    // rejection -- exactly the "worked for a while, then the whole
    // thing silently died, with no way back in short of a full
    // restart" real hardware symptom. A single failure must be
    // surfaced (logged) but not fatal: later events on the same
    // connection must still be injected normally.
    let a = Device::new([24u8; 32]);
    let b = Device::new([25u8; 32]);
    let (peer_a, mut peer_b) = connect_pair(&a, &b).await;

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));
    let mut local_geometry = FakeGeometry::new((500, 400));

    let received = Arc::new(Mutex::new(Vec::new()));
    let target_task = {
        let received = received.clone();
        tokio::spawn(async move {
            let mut geometry = FakeGeometry::new((0, 0));
            // Fails on the 2nd injected event (the 1st being the
            // control-stream cursor warp doesn't count against this --
            // FlakyInject only counts Inject::inject calls, i.e. Input
            // stream events).
            let mut inject = FlakyInject {
                received,
                fail_on_call: 2,
                calls: 0,
            };
            run_target(&mut peer_b, &mut geometry, &mut inject).await
        })
    };

    // -> Forwarding(B).
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
        )
        .await
        .unwrap();

    // Three follow-up moves: the 2nd's injection fails on B, but B's
    // session must keep running and inject the 1st and 3rd normally.
    for delta in [(1, 0), (2, 0), (3, 0)] {
        session
            .handle_captured(
                InputMessage::MouseMove {
                    dx: delta.0,
                    dy: delta.1,
                },
                &mut local_geometry,
            )
            .await
            .unwrap();
    }

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if received.lock().unwrap().len() >= 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for events after the failed one to still be injected"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // The 2nd move (dx=2) was dropped by the simulated failure; the 1st
    // (dx=1) and 3rd (dx=3) must both have landed.
    let got = received.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            InputMessage::MouseMove { dx: 1, dy: 0 },
            InputMessage::MouseMove { dx: 3, dy: 0 },
        ]
    );

    drop(session);
    let _ = target_task.await;
}
