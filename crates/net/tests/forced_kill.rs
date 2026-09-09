//! Phase 1c DoD: "forced-kill reconnect test with backoff-doesn't-busy-
//! loop assertion."
//!
//! Two claims, tested separately: (1) a peer that vanishes with no
//! graceful close (a crashed process, a severed link) is still detected
//! as dead within a bounded time, via QUIC's idle timeout rather than any
//! explicit notification; (2) reconnecting against the now-unreachable
//! address doesn't busy-loop — real attempts are sparse, consistent with
//! the backoff schedule, not thousands of instant retries. The exact
//! backoff *timing* is proven precisely and deterministically in
//! `reconnect.rs`'s paused-clock unit test; this test is the live,
//! real-network-facing complement to it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::time::{Duration, Instant};

#[tokio::test]
async fn hard_killed_peer_is_detected_and_reconnect_does_not_busy_loop() {
    let max_idle_timeout = Duration::from_millis(300);
    let keep_alive_interval = Duration::from_millis(100);
    let (a, b) = support::mutually_trusting_pair_with_timeouts(
        [30u8; 32],
        [31u8; 32],
        max_idle_timeout,
        keep_alive_interval,
    );
    let dead_addr = b.addr;

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

    // Hard-kill B: drop everything on its side without any graceful
    // close call. No CONNECTION_CLOSE frame is ever sent — A must notice
    // via silence (idle timeout), not a notification.
    drop(peer_b);
    drop(b);

    let detected = tokio::time::timeout(Duration::from_secs(3), peer_a.connection.closed()).await;
    assert!(
        detected.is_ok(),
        "a hard-killed peer must be detected via idle timeout within a bounded time, not hang indefinitely"
    );

    // Reconnecting against the now-dead address: run it for a short,
    // fixed wall-clock window and count attempts. A busy loop would
    // produce thousands of attempts in that window; a correct backoff
    // produces only a handful (~100ms, 200ms, 400ms, ... between them).
    let attempt_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let our_id = a.device_id;
    let endpoint = a.endpoint.clone();
    let counting = attempt_count.clone();

    let window = Duration::from_millis(900);
    let start = Instant::now();
    let _ = tokio::time::timeout(
        window,
        kvm_net::connect_with_backoff(move |_attempt| {
            counting.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let endpoint = endpoint.clone();
            async move { kvm_net::connect(&endpoint, dead_addr, our_id).await }
        }),
    )
    .await;
    let elapsed = start.elapsed();

    let attempts = attempt_count.load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        attempts >= 1,
        "expected at least the initial attempt within {elapsed:?}"
    );
    assert!(
        attempts <= 10,
        "busy-loop suspected: {attempts} connect attempts in {elapsed:?} against a dead address"
    );
}
