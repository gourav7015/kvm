//! Phase 3 DoD: "integration tests for capture -> normalize -> serialize
//! -> transport -> deserialize -> inject, where practical." CI has no
//! physical keyboard/mouse, so this exercises the full pipeline with
//! fake `Capture`/`Inject` doubles standing in for the OS backends, over
//! a *real* loopback `net::Peer` connection (real QUIC/TLS, real
//! identity-pinned trust, real wire framing) — everything except the OS
//! ends of the pipe is genuine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use kvm_core::{forward_capture_to_peer, inject_from_peer};
use kvm_identity::DeviceKeypair;
use kvm_input::{Capture, Inject, InputError};
use kvm_net::{IdentityCert, TrustCheck};
use kvm_protocol::{ButtonState, DeviceId, InputMessage, Key, Message, MouseButton, PlatformKind};

/// A minimal loopback device — same shape as `net`'s own test support,
/// duplicated here since integration-test helpers aren't shared across
/// crates.
struct Device {
    device_id: DeviceId,
    endpoint: quinn::Endpoint,
    addr: SocketAddr,
}

impl Device {
    fn new(seed: [u8; 32], trusted: Arc<Mutex<HashSet<DeviceId>>>) -> Self {
        let keypair = DeviceKeypair::from_secret_bytes(&seed);
        let device_id = keypair.device_id();
        let identity = IdentityCert::from_seed(&seed).unwrap();
        let trust_check: TrustCheck =
            Arc::new(move |id: &DeviceId| trusted.lock().unwrap().contains(id));
        let bind_addr = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        let endpoint = kvm_net::new_endpoint(bind_addr, &identity, trust_check).unwrap();
        let addr = endpoint.local_addr().unwrap();
        Self {
            device_id,
            endpoint,
            addr,
        }
    }
}

fn mutually_trusting_pair(seed_a: [u8; 32], seed_b: [u8; 32]) -> (Device, Device) {
    let trusted_by_a = Arc::new(Mutex::new(HashSet::new()));
    let trusted_by_b = Arc::new(Mutex::new(HashSet::new()));
    let a = Device::new(seed_a, trusted_by_a.clone());
    let b = Device::new(seed_b, trusted_by_b.clone());
    trusted_by_a.lock().unwrap().insert(b.device_id);
    trusted_by_b.lock().unwrap().insert(a.device_id);
    (a, b)
}

fn sample_events() -> Vec<InputMessage> {
    vec![
        InputMessage::Key {
            key: Key::A,
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::MacOs,
        },
        InputMessage::MouseMove { dx: 12, dy: -4 },
        InputMessage::MouseButton {
            button: MouseButton::Left,
            state: ButtonState::Released,
        },
    ]
}

/// Stands in for a real OS backend: sends a fixed sequence of events on
/// its own thread (mirroring the real backends' thread-based shape) and
/// then lets its sender drop, ending the channel — a clean, finite
/// stream of events instead of running forever.
struct FakeCapture {
    events: Vec<InputMessage>,
}

impl Capture for FakeCapture {
    fn start(&mut self, sink: std_mpsc::Sender<InputMessage>) -> Result<(), InputError> {
        let events = self.events.clone();
        thread::spawn(move || {
            for event in events {
                if sink.send(event).is_err() {
                    break;
                }
            }
        });
        Ok(())
    }

    fn stop(&mut self) {}
}

/// Stands in for a real OS backend: records every event it's asked to
/// inject instead of touching any OS API.
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
async fn captured_events_arrive_intact_on_the_peers_input_stream() {
    let (a, b) = mutually_trusting_pair([80u8; 32], [81u8; 32]);

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
    let peer_b = accept_task.await.unwrap().unwrap();

    let expected = sample_events();
    let count = expected.len();

    // `peer_b` (and its connection) is moved into a spawned task that
    // collects everything it receives; `peer_a` stays alive in this
    // task's own scope for the whole test, so the connection is never
    // dropped mid-flight — dropping either side's `Peer` closes the
    // connection immediately (quinn's close-on-drop), which would race
    // against not-yet-delivered bytes if done right after the last send.
    let receive_task = tokio::spawn(async move {
        let mut peer_b = peer_b;
        let mut received = Vec::new();
        for _ in 0..count {
            received.push(peer_b.streams.input.recv().await.unwrap());
        }
        received
    });

    let mut capture = FakeCapture {
        events: expected.clone(),
    };
    // The fake capture's thread finishes after this fixed sequence, its
    // sender drops, and `forward_capture_to_peer` sees the channel close
    // and returns cleanly — proving the bridge doesn't hang or error on
    // ordinary capture shutdown.
    forward_capture_to_peer(&mut capture, &mut peer_a.streams.input)
        .await
        .unwrap();

    let received = receive_task.await.unwrap();
    let expected_messages: Vec<Message> = expected.into_iter().map(Message::Input).collect();
    assert_eq!(received, expected_messages);
}

#[tokio::test]
async fn peer_input_messages_are_injected_in_order() {
    let (a, b) = mutually_trusting_pair([82u8; 32], [83u8; 32]);

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
    let peer_b = accept_task.await.unwrap().unwrap();

    let expected = sample_events();
    let received = Arc::new(Mutex::new(Vec::new()));
    let inject_task = {
        let received = received.clone();
        tokio::spawn(async move {
            let mut peer_b = peer_b;
            let mut inject = FakeInject {
                received: received.clone(),
            };
            inject_from_peer(&mut inject, &mut peer_b.streams.input).await
        })
    };

    for event in &expected {
        peer_a
            .streams
            .input
            .send(&Message::Input(event.clone()))
            .await
            .unwrap();
    }

    // Wait until every sent event has actually been injected before
    // tearing the connection down, instead of racing the close against
    // in-flight QUIC delivery.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if received.lock().unwrap().len() == expected.len() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for all events to be injected"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(*received.lock().unwrap(), expected);

    // Force-closing the connection ends `inject_from_peer`'s read loop
    // one way or another (a clean stream-closed or a transport error,
    // depending on how quinn surfaces a forced close) — either is a
    // correct, honest outcome; what matters for this DoD item is that
    // every event already in flight was injected first, asserted above.
    peer_a.connection.close(0u32.into(), b"test done");
    let _ = inject_task.await.unwrap();
}
