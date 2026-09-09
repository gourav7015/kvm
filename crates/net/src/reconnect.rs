//! The generic reconnect-with-backoff loop, decoupled from any specific
//! transport call so it can be tested with a fake `connect` closure and a
//! paused clock instead of real networking and real waiting.

use std::future::Future;

use tokio::time::sleep;

use crate::backoff::next_delay;
use crate::error::NetError;

/// Repeatedly calls `connect` until it succeeds, sleeping
/// [`next_delay`]`(attempt)` between failed attempts. Never gives up on
/// its own — callers stop it by dropping the future (e.g. via
/// `tokio::select!` against a shutdown signal, or aborting the task).
pub async fn connect_with_backoff<F, Fut, T>(mut connect: F) -> T
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, NetError>>,
{
    let mut attempt = 0u32;
    loop {
        match connect(attempt).await {
            Ok(value) => return value,
            Err(_) => {
                sleep(next_delay(attempt)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// Proves the retry loop actually waits `next_delay(attempt)` between
    /// attempts rather than busy-looping: with the clock paused, the
    /// background task must make *no* progress until time is advanced by
    /// exactly the expected delay, at each step.
    #[tokio::test(start_paused = true)]
    async fn retries_wait_the_expected_backoff_and_never_busy_loop() {
        let call_count = Arc::new(AtomicU32::new(0));
        let succeed_on_attempt = 4u32;

        let counting_count = call_count.clone();
        let task = tokio::spawn(async move {
            connect_with_backoff(move |attempt| {
                counting_count.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt == succeed_on_attempt {
                        Ok(attempt)
                    } else {
                        Err(NetError::ConnectionClosed)
                    }
                }
            })
            .await
        });

        // Yield so the first (attempt 0) call happens immediately.
        tokio::task::yield_now().await;
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "first attempt must happen immediately, no initial delay"
        );

        for attempt in 0..succeed_on_attempt {
            // Before advancing the expected delay, no further attempt
            // should have happened yet — this is the busy-loop check.
            assert_eq!(
                call_count.load(Ordering::SeqCst),
                attempt + 1,
                "attempt {attempt} ran ahead of its backoff delay"
            );
            tokio::time::advance(next_delay(attempt)).await;
            tokio::task::yield_now().await;
        }

        let result = task.await.unwrap();
        assert_eq!(result, succeed_on_attempt);
        assert_eq!(call_count.load(Ordering::SeqCst), succeed_on_attempt + 1);
    }
}
