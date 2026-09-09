//! The reconnect backoff schedule, kept as a pure function so it's
//! exactly testable without any real networking or real waiting —
//! catching a busy-loop bug here doesn't require a flaky timing-based
//! integration test.

use std::time::Duration;

const BASE: Duration = Duration::from_millis(100);
const MAX: Duration = Duration::from_secs(5);

/// The delay to wait before reconnect attempt number `attempt` (0-based:
/// `attempt = 0` is the delay before the *first* retry, after the initial
/// attempt already failed). Doubles each attempt, capped at [`MAX`].
pub fn next_delay(attempt: u32) -> Duration {
    let factor = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    let base_millis = BASE.as_millis() as u64;
    let scaled = base_millis.saturating_mul(factor);
    Duration::from_millis(scaled).min(MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_each_attempt_until_the_cap() {
        assert_eq!(next_delay(0), Duration::from_millis(100));
        assert_eq!(next_delay(1), Duration::from_millis(200));
        assert_eq!(next_delay(2), Duration::from_millis(400));
        assert_eq!(next_delay(3), Duration::from_millis(800));
        assert_eq!(next_delay(4), Duration::from_millis(1600));
        assert_eq!(next_delay(5), Duration::from_millis(3200));
    }

    #[test]
    fn never_exceeds_the_cap() {
        assert_eq!(next_delay(6), MAX);
        assert_eq!(next_delay(100), MAX);
        assert_eq!(next_delay(u32::MAX), MAX);
    }

    #[test]
    fn never_zero_never_busy_loops() {
        for attempt in 0..200 {
            assert!(
                next_delay(attempt) >= BASE,
                "attempt {attempt} produced a delay below the base — would busy-loop"
            );
        }
    }

    #[test]
    fn is_non_decreasing() {
        let mut previous = Duration::ZERO;
        for attempt in 0..200 {
            let delay = next_delay(attempt);
            assert!(
                delay >= previous,
                "attempt {attempt} decreased from the previous delay"
            );
            previous = delay;
        }
    }
}
