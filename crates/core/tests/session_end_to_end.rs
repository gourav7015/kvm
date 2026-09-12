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

/// Stands in for a real `Capture` in these fakes-only tests -- records
/// every `set_local_suppression` call so tests can assert `Session`
/// toggles it exactly on `Local`<->`Forwarding` transitions (ADR-0009
/// decision 9), never on every event.
#[derive(Default)]
struct FakeCapture {
    suppression_calls: Vec<bool>,
}

impl kvm_input::Capture for FakeCapture {
    fn start(&mut self, _sink: std::sync::mpsc::Sender<InputMessage>) -> Result<(), InputError> {
        Ok(())
    }

    fn stop(&mut self) {}

    fn set_local_suppression(&mut self, suppress: bool) {
        self.suppression_calls.push(suppress);
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
    let mut local_capture = FakeCapture::default();

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
            &mut local_capture,
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
            &mut local_capture,
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
    let mut local_capture = FakeCapture::default();

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
                &mut local_capture,
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
                &mut local_capture,
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
                &mut local_capture,
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
                &mut local_capture,
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
    let mut local_capture = FakeCapture::default();

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
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
    let mut local_capture = FakeCapture::default();

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
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
    session.remove_peer(b.device_id, &mut local_capture);
    assert_eq!(
        session.ownership_state(),
        OwnershipState::Local,
        "a disconnected target must fall back to local input immediately"
    );
    assert_eq!(
        local_capture.suppression_calls,
        vec![true, false],
        "remove_peer itself must restore local input suppression -- not left \
         to a caller that might never get another captured event to notice"
    );
    session.resync_local_position(500, 400);

    // Local input must keep working with no crash and no special
    // handling required from the caller.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 1, dy: 1 },
            &mut local_geometry,
            &mut local_capture,
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
            &mut local_capture,
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
    let mut local_capture = FakeCapture::default();

    // -> Forwarding(B); hold Shift on B.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    session
        .handle_captured(
            key_event(Key::ShiftLeft, ButtonState::Pressed),
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();

    // Cross onward from B's screen to C -- must flush Shift's Released
    // onto B before (or as part of) the switch to C.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 1100, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
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
    let mut local_capture = FakeCapture::default();

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    session
        .handle_captured(
            key_event(Key::A, ButtonState::Pressed),
            &mut local_geometry,
            &mut local_capture,
        )
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
    let mut local_capture = FakeCapture::default();

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
            &mut local_capture,
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
    let mut local_capture = FakeCapture::default();

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
            &mut local_capture,
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
                &mut local_capture,
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

#[tokio::test]
async fn local_capture_is_suppressed_exactly_on_local_forwarding_transitions() {
    // Regression test for real Mac->Windows hardware QA (see ADR-0009
    // decision 9): the capturing device's own OS must stop acting on
    // captured input for the duration of a `Forwarding` session --
    // toggled exactly on the `Local`<->`Forwarding` boundary, not on
    // every single captured event (which would be wasted, and would
    // make an eventual real-hardware unsuppress/suppress flicker if a
    // backend's toggle call is itself non-trivial).
    let a = Device::new([26u8; 32]);
    let b = Device::new([27u8; 32]);
    let c = Device::new([28u8; 32]);
    let (peer_a_b, _peer_b) = connect_pair(&a, &b).await;
    let (peer_a_c, _peer_c) = connect_pair(&a, &c).await;

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
    layout
        .set_neighbor(c.device_id, Edge::Left, b.device_id)
        .unwrap();
    layout
        .set_neighbor(b.device_id, Edge::Left, a.device_id)
        .unwrap();

    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a_b, (1000, 800));
    session.add_peer(c.device_id, peer_a_c, (1000, 800));
    let mut local_geometry = FakeGeometry::new((500, 400));
    let mut local_capture = FakeCapture::default();

    // Local -> Forwarding(B): must suppress.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(local_capture.suppression_calls, vec![true]);

    // An ordinary forwarded move while already Forwarding must not
    // toggle suppression again.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 1, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(local_capture.suppression_calls, vec![true]);

    // Forwarding(B) -> Forwarding(C), a switch that never passes
    // through Local: suppression is already on and must stay on,
    // without an extra redundant toggle.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 1100, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { target, .. } if target == c.device_id
    ));
    assert_eq!(local_capture.suppression_calls, vec![true]);

    // Forwarding(C) -> Forwarding(B): C's configured Left neighbor is
    // B, not A, so this one crossing lands back on B, not directly on
    // Local -- suppression must still not toggle.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: -1100, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { target, .. } if target == b.device_id
    ));
    assert_eq!(local_capture.suppression_calls, vec![true]);

    // Forwarding(B) -> Local: must resume (unsuppress).
    session
        .handle_captured(
            InputMessage::MouseMove { dx: -1100, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(session.ownership_state(), OwnershipState::Local);
    assert_eq!(local_capture.suppression_calls, vec![true, false]);
}

#[tokio::test]
async fn suppression_is_restored_even_when_the_disconnect_send_returns_a_generic_error() {
    // Regression test for a real Mac->Windows hardware safety finding:
    // "terminated the connection on Windows, still wasn't able to use
    // my Mac keyboard and trackpad." Root cause: MessageStream::send
    // (crates/net/src/framed.rs) always maps a broken connection to
    // NetError::Transport, never the more specific ConnectionClosed --
    // so a target disconnecting abruptly makes the *next* captured
    // event's Session::apply hit the generic-error arm, which correctly
    // calls remove_peer but then returns Err. The old handle_captured
    // applied every effect with `?`, so that Err propagated straight
    // out of the function before ever reaching the suppression-restore
    // check -- silently leaving local input suppressed forever, in
    // direct violation of "never leave local input permanently disabled
    // after disconnect, crash, or transition failure."
    let a = Device::new([29u8; 32]);
    let b = Device::new([30u8; 32]);
    let (peer_a, _peer_b) = connect_pair(&a, &b).await;
    // Cloning before registering peer_a with the session: quinn's
    // Connection is a cheap, Arc-backed handle, so closing this clone
    // closes the same underlying connection peer_a's streams use.
    let connection_handle = peer_a.connection.clone();

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));
    let mut local_geometry = FakeGeometry::new((500, 400));
    let mut local_capture = FakeCapture::default();

    // -> Forwarding(B): suppression engages.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(local_capture.suppression_calls, vec![true]);

    // Simulate B disconnecting abruptly (a real network drop, or the
    // target process exiting) -- closed from our own side so the very
    // next send on this connection fails deterministically and
    // immediately, with no dependency on real network timing.
    connection_handle.close(0u32.into(), b"simulated abrupt disconnect");

    // A follow-up move now tries to forward to B and fails. The error
    // must still reach the caller (unchanged behavior)...
    let result = session
        .handle_captured(
            InputMessage::MouseMove { dx: 1, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await;
    assert!(
        result.is_err(),
        "a genuine send failure must still be surfaced to the caller"
    );

    // ...but ownership must have fallen back to Local regardless...
    assert_eq!(
        session.ownership_state(),
        OwnershipState::Local,
        "a disconnected target must still force ownership back to Local"
    );
    // ...and, the actual point of this test, local input suppression
    // must have been lifted too -- not left stuck because apply()
    // returned an error before the old code ever reached this check.
    // Both `remove_peer` (directly) and `handle_captured`'s own
    // before/after check independently try to restore it -- a
    // deliberate belt-and-suspenders after the real-hardware finding
    // that a *single* restore path is one bug away from leaving a user
    // with no keyboard or trackpad and no way to even reach a terminal
    // to recover -- so the exact call count isn't asserted, only that
    // it was engaged once and is left lifted.
    assert_eq!(
        local_capture.suppression_calls.first(),
        Some(&true),
        "suppression must have engaged when forwarding began"
    );
    assert_eq!(
        local_capture.suppression_calls.last(),
        Some(&false),
        "suppression must end up lifted even though the disconnect surfaced as an error"
    );
}

#[tokio::test]
async fn active_target_connection_closed_detects_a_closed_connection_with_no_send_attempt() {
    // Regression test for the watchdog in edge_switch_relay.rs (see
    // ADR-0009's Update): this must be detectable *without* ever
    // attempting a send, since the whole point is proactively noticing
    // a dead connection even if the user's local input is stuck and no
    // further captured event will ever arrive to trigger the lazy
    // (send-failure-triggered) recovery path.
    let a = Device::new([31u8; 32]);
    let b = Device::new([32u8; 32]);
    let (peer_a, _peer_b) = connect_pair(&a, &b).await;
    let connection_handle = peer_a.connection.clone();

    let layout = two_device_layout(a.device_id, b.device_id);
    let mut session = Session::new(a.device_id, layout, (1000, 800), (500, 400));
    session.add_peer(b.device_id, peer_a, (1000, 800));
    let mut local_geometry = FakeGeometry::new((500, 400));
    let mut local_capture = FakeCapture::default();

    // Not yet Forwarding: nothing to detect.
    assert_eq!(session.active_target_connection_closed(), None);

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { target, .. } if target == b.device_id
    ));

    // Forwarding, but the connection is still alive: nothing to detect.
    assert_eq!(session.active_target_connection_closed(), None);

    // Close it without ever attempting a send -- no InputMessage
    // reaches handle_captured at all here.
    connection_handle.close(0u32.into(), b"simulated abrupt disconnect");
    // Give the connection driver a moment to observe the local close.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    assert_eq!(
        session.active_target_connection_closed(),
        Some(b.device_id),
        "a closed connection to the active target must be detectable with no send attempt"
    );
}

#[tokio::test]
async fn ownership_matrix_local_forwarding_keyboard_and_disconnect() {
    // Regression test walking the exact Phase 4 ownership contract:
    //   Local + mouse   -> local allowed, no suppression
    //   Forwarding + mouse    -> local suppressed, remote forwarded
    //   Forwarding + keyboard -> local (still) suppressed, remote forwarded
    //   Forwarding -> Local   -> local restored
    //   Forwarding -> disconnect -> local restored
    // in one place, as a single source of truth for the contract
    // rather than scattered across several narrower tests.
    let a = Device::new([33u8; 32]);
    let b = Device::new([34u8; 32]);
    let (peer_a, mut peer_b) = connect_pair(&a, &b).await;
    let connection_handle = peer_a.connection.clone();

    // A two-way edge (unlike `two_device_layout`, which only maps one
    // direction): this test needs to cross back via an ordinary edge
    // crossing, not just via a forced disconnect.
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
    let mut local_capture = FakeCapture::default();

    // Local + mouse -> local allowed: no effect at all, no suppression
    // call, since the OS already applied this event itself.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 1, dy: 1 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(session.ownership_state(), OwnershipState::Local);
    assert!(local_capture.suppression_calls.is_empty());

    // -> Forwarding(B): crossing the edge enables remote forwarding and
    // engages local suppression together, as one transition.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { target, .. } if target == b.device_id
    ));
    assert_eq!(local_capture.suppression_calls, vec![true]);

    // Forwarding + mouse -> local (still) suppressed, remote forwarded.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 5, dy: 5 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(
        local_capture.suppression_calls,
        vec![true],
        "an ordinary forwarded move must not re-toggle suppression"
    );

    // Forwarding + keyboard -> local (still) suppressed, remote forwarded.
    session
        .handle_captured(
            key_event(Key::A, ButtonState::Pressed),
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(
        local_capture.suppression_calls,
        vec![true],
        "a forwarded key event must not re-toggle suppression either"
    );

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout(
            std::time::Duration::from_millis(200),
            peer_b.streams.input.recv(),
        )
        .await
        {
            Ok(Ok(Message::Input(InputMessage::Key { key: Key::A, .. }))) => break,
            _ => assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for B to receive the forwarded key"
            ),
        }
    }

    // Forwarding -> Local: local input restored.
    session
        .handle_captured(
            InputMessage::MouseMove { dx: -1100, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert_eq!(session.ownership_state(), OwnershipState::Local);
    assert_eq!(local_capture.suppression_calls, vec![true, false]);

    // Forwarding -> disconnect -> local restored, via the same
    // centralized remove_peer path (decision 12).
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { .. }
    ));
    connection_handle.close(0u32.into(), b"simulated abrupt disconnect");
    let result = session
        .handle_captured(
            InputMessage::MouseMove { dx: 1, dy: 0 },
            &mut local_geometry,
            &mut local_capture,
        )
        .await;
    assert!(result.is_err());
    assert_eq!(session.ownership_state(), OwnershipState::Local);
    assert_eq!(local_capture.suppression_calls.last(), Some(&false));
}

/// Regression test for the real-hardware stalled-target incident (ADR-0009
/// decision 19): the target stayed *connected* but stopped processing input
/// (its console was paused mid-write), so the closed-connection watchdog
/// could never fire and local input stayed suppressed until the target
/// process was killed. Here the target end is held open but never read --
/// pings go unanswered -- and local input must come back once the liveness
/// timeout passes, not before.
#[tokio::test]
async fn a_connected_target_that_stops_answering_pings_hands_local_input_back() {
    let hub = Device::new([0x71; 32]);
    let target = Device::new([0x72; 32]);
    // Held (not dropped) so the connection stays open, but never read.
    let (hub_peer, _target_peer_never_read) = connect_pair(&hub, &target).await;

    let mut session = Session::new(
        hub.device_id,
        two_device_layout(hub.device_id, target.device_id),
        (1000, 800),
        (500, 400),
    );
    session.set_liveness_timeout(std::time::Duration::from_millis(500));
    session.add_peer(target.device_id, hub_peer, (1000, 800));
    let mut geometry = FakeGeometry::new((500, 400));
    let mut capture = FakeCapture::default();

    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut geometry,
            &mut capture,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { .. }
    ));
    assert_eq!(capture.suppression_calls, vec![true]);

    assert_eq!(session.ping_active_target(&mut capture).await, None);
    assert_eq!(
        session.check_target_liveness(&mut capture),
        None,
        "must not give up before the liveness timeout"
    );

    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(session.ping_active_target(&mut capture).await, None);
    assert_eq!(
        session.check_target_liveness(&mut capture),
        Some(target.device_id),
        "a connected-but-silent target must be given up on"
    );
    assert_eq!(session.ownership_state(), OwnershipState::Local);
    assert_eq!(
        capture.suppression_calls,
        vec![true, false],
        "local input must be un-suppressed"
    );
}

/// The other side of the same rule: a target running the real
/// `run_target` loop answers every ping, so forwarding continues well past
/// the liveness timeout.
#[tokio::test]
async fn a_responsive_target_answers_pings_and_forwarding_continues() {
    let hub = Device::new([0x73; 32]);
    let target = Device::new([0x74; 32]);
    let (hub_peer, mut target_peer) = connect_pair(&hub, &target).await;

    tokio::spawn(async move {
        let mut geometry = FakeGeometry::new((0, 0));
        let mut inject = FakeInject {
            received: Arc::new(Mutex::new(Vec::new())),
        };
        let _ = run_target(&mut target_peer, &mut geometry, &mut inject).await;
    });

    let mut session = Session::new(
        hub.device_id,
        two_device_layout(hub.device_id, target.device_id),
        (1000, 800),
        (500, 400),
    );
    session.set_liveness_timeout(std::time::Duration::from_millis(500));
    session.add_peer(target.device_id, hub_peer, (1000, 800));
    let mut geometry = FakeGeometry::new((500, 400));
    let mut capture = FakeCapture::default();
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut geometry,
            &mut capture,
        )
        .await
        .unwrap();

    // ~900 ms in total -- well past the 500 ms timeout -- answering every ping.
    for _ in 0..9 {
        assert_eq!(session.ping_active_target(&mut capture).await, None);
        let (from, result) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.recv_from_active_target(),
        )
        .await
        .expect("a responsive target must answer a ping");
        assert_eq!(from, target.device_id);
        assert!(
            matches!(
                result,
                Ok(Message::Control(kvm_protocol::ControlMessage::Pong { .. }))
            ),
            "expected a Pong, got {result:?}"
        );
        session.handle_target_control(from, result, &mut capture);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    assert_eq!(session.check_target_liveness(&mut capture), None);
    assert!(matches!(
        session.ownership_state(),
        OwnershipState::Forwarding { .. }
    ));
    assert_eq!(capture.suppression_calls, vec![true]);
}

/// The emergency chord (ADR-0009 decision 19) end to end: with the target
/// connected but unresponsive, Control+Option+Command+Escape on this side
/// alone must un-suppress local input immediately -- no timeout, no help
/// from the target.
#[tokio::test]
async fn the_emergency_chord_restores_local_input_with_the_target_unresponsive() {
    let hub = Device::new([0x75; 32]);
    let target = Device::new([0x76; 32]);
    let (hub_peer, _target_peer_never_read) = connect_pair(&hub, &target).await;

    let mut session = Session::new(
        hub.device_id,
        two_device_layout(hub.device_id, target.device_id),
        (1000, 800),
        (500, 400),
    );
    session.add_peer(target.device_id, hub_peer, (1000, 800));
    let mut geometry = FakeGeometry::new((500, 400));
    let mut capture = FakeCapture::default();
    session
        .handle_captured(
            InputMessage::MouseMove { dx: 600, dy: 0 },
            &mut geometry,
            &mut capture,
        )
        .await
        .unwrap();
    assert_eq!(capture.suppression_calls, vec![true]);

    for key in [Key::ControlLeft, Key::AltLeft, Key::MetaLeft, Key::Escape] {
        session
            .handle_captured(
                key_event(key, ButtonState::Pressed),
                &mut geometry,
                &mut capture,
            )
            .await
            .unwrap();
    }

    assert_eq!(session.ownership_state(), OwnershipState::Local);
    assert_eq!(capture.suppression_calls, vec![true, false]);
}

/// Blocks inside its first `inject` until released -- stands in for the
/// real-hardware target whose injection loop froze on a paused console
/// write (ADR-0009 decision 20).
struct FreezingInject {
    received: Arc<Mutex<Vec<InputMessage>>>,
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
    frozen_once: bool,
}

impl Inject for FreezingInject {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError> {
        self.received.lock().unwrap().push(event.clone());
        if !self.frozen_once {
            self.frozen_once = true;
            let _ = self.entered.send(());
            let _ = self.release.recv();
        }
        Ok(())
    }
}

/// Regression test for the stale-input replay seen on real hardware
/// (ADR-0009 decision 20): the target froze mid-injection, the hub gave up
/// and dropped the connection, and when the target unfroze it injected
/// moves it had already buffered locally. With the hub's connection gone,
/// not one more event may be injected. (Without the guard this fails
/// whenever the loop happens to poll the input stream before the control
/// stream after release.)
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_target_frozen_mid_injection_never_replays_input_after_the_hub_dropped_it() {
    let hub = Device::new([0x77; 32]);
    let target = Device::new([0x78; 32]);
    let (mut hub_peer, mut target_peer) = connect_pair(&hub, &target).await;
    let target_connection = target_peer.connection.clone();

    let received = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut inject = FreezingInject {
        received: received.clone(),
        entered: entered_tx,
        release: release_rx,
        frozen_once: false,
    };
    let target_task = tokio::spawn(async move {
        let mut geometry = FakeGeometry::new((0, 0));
        run_target(&mut target_peer, &mut geometry, &mut inject).await
    });

    for _ in 0..20 {
        hub_peer
            .streams
            .input
            .send(&Message::Input(InputMessage::MouseMove { dx: 1, dy: 0 }))
            .await
            .unwrap();
    }

    // The target is now stuck inside its first injection.
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(std::time::Duration::from_secs(5)))
        .await
        .unwrap()
        .expect("the target never started injecting");

    // The hub gives up on it, as `Session::check_target_liveness` does:
    // dropping the peer closes the connection.
    drop(hub_peer);
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while target_connection.close_reason().is_none() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the target never saw the connection close"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    release_tx.send(()).unwrap();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), target_task)
        .await
        .expect("the target loop must end once released");

    assert_eq!(
        received.lock().unwrap().len(),
        1,
        "only the event already mid-injection may land; the 19 still buffered must not be replayed"
    );
}
