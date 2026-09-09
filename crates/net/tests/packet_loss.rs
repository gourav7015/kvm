//! Phase 1c DoD: "simulated packet-loss/latency test."
//!
//! quinn has no public API for injecting loss/latency into a connection,
//! so this drives both endpoints through a small UDP relay that sits in
//! the middle and deliberately drops a fraction of packets and delays the
//! rest — a real, if crude, degraded network, not a mock of one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use kvm_protocol::{ControlMessage, Message};
use tokio::net::UdpSocket;

/// Starts a relay that forwards packets between whoever first talks to it
/// and `target_addr`, dropping every `drop_every_nth` packet (in each
/// direction, independently) and delaying every forwarded packet by
/// `added_delay`. Returns the address to connect to instead of
/// `target_addr` directly, plus the task driving the relay.
async fn start_lossy_relay(
    target_addr: SocketAddr,
    drop_every_nth: u32,
    added_delay: Duration,
) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let socket = Arc::new(
        UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap(),
    );
    let relay_addr = socket.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let mut client_addr: Option<SocketAddr> = None;
        let mut buf = vec![0u8; 2048];
        let seq_a_to_b = AtomicU32::new(0);
        let seq_b_to_a = AtomicU32::new(0);

        loop {
            let (n, from) = match socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(_) => break,
            };
            let data = buf[..n].to_vec();

            let (dest, seq) = if from == target_addr {
                match client_addr {
                    Some(addr) => (addr, seq_b_to_a.fetch_add(1, Ordering::SeqCst)),
                    None => continue,
                }
            } else {
                client_addr = Some(from);
                (target_addr, seq_a_to_b.fetch_add(1, Ordering::SeqCst))
            };

            if drop_every_nth != 0 && seq % drop_every_nth == drop_every_nth - 1 {
                continue; // simulated loss
            }

            let socket = socket.clone();
            tokio::spawn(async move {
                if !added_delay.is_zero() {
                    tokio::time::sleep(added_delay).await;
                }
                let _ = socket.send_to(&data, dest).await;
            });
        }
    });

    (relay_addr, handle)
}

#[tokio::test]
async fn connection_survives_packet_loss_and_added_latency() {
    let (a, b) = support::mutually_trusting_pair([40u8; 32], [41u8; 32]);

    // Drop roughly 1 in 5 packets each direction, add 15ms of latency to
    // every packet that does get through.
    let (relay_addr, _relay_task) = start_lossy_relay(b.addr, 5, Duration::from_millis(15)).await;

    let connect_task = {
        let our_id = a.device_id;
        let endpoint = a.endpoint.clone();
        tokio::spawn(async move { kvm_net::connect(&endpoint, relay_addr, our_id).await })
    };
    let accept_task = {
        let our_id = b.device_id;
        let endpoint = b.endpoint.clone();
        tokio::spawn(async move {
            let incoming = endpoint.accept().await.expect("endpoint closed");
            kvm_net::accept(incoming, our_id).await
        })
    };

    let connect_result = tokio::time::timeout(Duration::from_secs(10), connect_task)
        .await
        .expect("handshake did not complete within 10s despite loss/latency")
        .unwrap();
    let mut peer_a =
        connect_result.expect("handshake must still succeed under degraded network conditions");
    let mut peer_b = accept_task
        .await
        .unwrap()
        .expect("accept must still succeed under degraded network conditions");

    // Exchange several messages — enough that, with real loss in the
    // mix, at least some individual packets are actually dropped and
    // must be recovered by QUIC's own retransmission rather than the
    // test getting lucky on a single round trip.
    for i in 0..20u64 {
        let message = Message::Control(ControlMessage::Ping { nonce: i });
        peer_a.streams.control.send(&message).await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(5), peer_b.streams.control.recv())
            .await
            .expect("message did not arrive within 5s")
            .unwrap();
        assert_eq!(received, message);
    }
}
