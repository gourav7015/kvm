//! A short, human-comparable code derived from two device identities —
//! used during pairing so a person can confirm both sides derived the
//! same value from the same (cryptographically real, TLS-verified) peer
//! identity, catching a substituted/MITM peer. See ADR-0004.
//!
//! This is an integrity check, not a secret: both device IDs are already
//! known to anyone on the pairing connection, and the code is never
//! transmitted — each side computes it locally and a human compares.

use sha2::{Digest, Sha256};

use crate::keypair::DeviceId;

/// Derives a 6-digit code (formatted `"123-456"`) from two device
/// identities. Order-independent: `pairing_code(a, b) == pairing_code(b,
/// a)`, since either side may compute it first.
pub fn pairing_code(a: &DeviceId, b: &DeviceId) -> String {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };

    let mut hasher = Sha256::new();
    hasher.update(first);
    hasher.update(second);
    let digest = hasher.finalize();

    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&digest[0..4]);
    let value = u32::from_be_bytes(bytes) % 1_000_000;

    format!("{:03}-{:03}", value / 1000, value % 1000)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn is_order_independent() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert_eq!(pairing_code(&a, &b), pairing_code(&b, &a));
    }

    #[test]
    fn is_deterministic() {
        let a = [7u8; 32];
        let b = [9u8; 32];
        assert_eq!(pairing_code(&a, &b), pairing_code(&a, &b));
    }

    #[test]
    fn distinct_device_id_pairs_rarely_collide() {
        // Not a formal collision-rate proof, just a sanity check that
        // this isn't accidentally constant or trivially colliding across
        // a batch of distinct inputs — the property that gives the code
        // any security value at all.
        let mut codes = std::collections::HashSet::new();
        for i in 0u8..50 {
            let a = [i; 32];
            let b = [i.wrapping_add(100); 32];
            codes.insert(pairing_code(&a, &b));
        }
        assert!(
            codes.len() > 45,
            "too many collisions among distinct device-id pairs: {} unique out of 50",
            codes.len()
        );
    }

    #[test]
    fn a_substituted_peer_produces_a_different_code() {
        // Simulates the actual MITM-detection scenario from ADR-0004: the
        // victim thinks it's talking to `real_peer` but TLS actually
        // verified `attacker`'s identity (a different keypair, since the
        // attacker cannot forge the real peer's signature). The code the
        // victim sees must differ from the one the real peer would show.
        let me = [3u8; 32];
        let real_peer = [4u8; 32];
        let attacker = [5u8; 32];

        let expected_code = pairing_code(&me, &real_peer);
        let code_seen_by_victim = pairing_code(&me, &attacker);
        assert_ne!(expected_code, code_seen_by_victim);
    }

    #[test]
    fn format_is_three_digits_dash_three_digits() {
        let code = pairing_code(&[0u8; 32], &[255u8; 32]);
        assert_eq!(code.len(), 7);
        assert_eq!(code.as_bytes()[3], b'-');
        assert!(
            code.chars()
                .enumerate()
                .all(|(i, c)| i == 3 || c.is_ascii_digit())
        );
    }
}
