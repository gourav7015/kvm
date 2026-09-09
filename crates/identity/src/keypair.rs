//! Device keypairs: generation, signing, and verification.
//!
//! A device's [`DeviceId`] *is* its Ed25519 public key bytes — there's no
//! separate identifier to keep in sync.

use ed25519_dalek::{Signer, Verifier, VerifyingKey};
use getrandom::SysRng;
use getrandom::rand_core::UnwrapErr;

use crate::error::IdentityError;

pub use ed25519_dalek::SigningKey;
pub use kvm_protocol::DeviceId;

/// A device's Ed25519 keypair.
pub struct DeviceKeypair {
    signing_key: SigningKey,
}

impl DeviceKeypair {
    /// Generates a new random keypair using the OS CSPRNG.
    pub fn generate() -> Self {
        let mut csprng = UnwrapErr(SysRng);
        Self {
            signing_key: SigningKey::generate(&mut csprng),
        }
    }

    /// Reconstructs a keypair from a previously-generated 32-byte seed
    /// (as returned by [`DeviceKeypair::secret_bytes`]).
    pub fn from_secret_bytes(seed: &[u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(seed),
        }
    }

    /// The 32-byte seed to persist in the OS keychain. Never sent over the
    /// network — only [`DeviceKeypair::device_id`] is.
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// This device's stable identifier: its public key bytes.
    pub fn device_id(&self) -> DeviceId {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Signs a message with this device's private key.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }
}

/// Verifies a signature against a claimed device identity (public key) and
/// message.
///
/// Returns [`IdentityError::InvalidPublicKey`] if `device_id` isn't a valid
/// Ed25519 point, or [`IdentityError::VerificationFailed`] if the signature
/// doesn't check out — the two are kept distinct so a caller can tell
/// "malformed peer data" apart from "this peer isn't who it claims".
pub fn verify(
    device_id: &DeviceId,
    message: &[u8],
    signature: &[u8; 64],
) -> Result<(), IdentityError> {
    let verifying_key = VerifyingKey::from_bytes(device_id)
        .map_err(|e| IdentityError::InvalidPublicKey(e.to_string()))?;
    let signature = ed25519_dalek::Signature::from_bytes(signature);
    verifying_key
        .verify(message, &signature)
        .map_err(|_| IdentityError::VerificationFailed)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_succeeds() {
        let keypair = DeviceKeypair::generate();
        let message = b"hello device";
        let signature = keypair.sign(message);
        verify(&keypair.device_id(), message, &signature).unwrap();
    }

    #[test]
    fn round_trip_through_secret_bytes_preserves_identity() {
        let original = DeviceKeypair::generate();
        let restored = DeviceKeypair::from_secret_bytes(&original.secret_bytes());
        assert_eq!(original.device_id(), restored.device_id());

        let message = b"round trip";
        let signature = restored.sign(message);
        verify(&original.device_id(), message, &signature).unwrap();
    }

    #[test]
    fn verify_with_wrong_public_key_fails() {
        let signer = DeviceKeypair::generate();
        let impostor = DeviceKeypair::generate();
        let message = b"who am I talking to";
        let signature = signer.sign(message);

        let err = verify(&impostor.device_id(), message, &signature).unwrap_err();
        assert!(matches!(err, IdentityError::VerificationFailed));
    }

    #[test]
    fn verify_with_tampered_message_fails() {
        let keypair = DeviceKeypair::generate();
        let signature = keypair.sign(b"original message");

        let err = verify(&keypair.device_id(), b"tampered message", &signature).unwrap_err();
        assert!(matches!(err, IdentityError::VerificationFailed));
    }

    #[test]
    fn verify_with_tampered_signature_fails() {
        let keypair = DeviceKeypair::generate();
        let message = b"a message";
        let mut signature = keypair.sign(message);
        signature[0] ^= 0xFF;

        let err = verify(&keypair.device_id(), message, &signature).unwrap_err();
        assert!(matches!(err, IdentityError::VerificationFailed));
    }

    #[test]
    fn different_keypairs_have_different_device_ids() {
        let a = DeviceKeypair::generate();
        let b = DeviceKeypair::generate();
        assert_ne!(a.device_id(), b.device_id());
    }

    /// RFC 8032 §7.1 TEST 1: a known-correct (seed, public key, empty
    /// message, signature) tuple, external to our implementation. This
    /// catches a wiring bug (e.g. swapped byte order, or seed vs. expanded
    /// key confusion) that a self-consistent round-trip test cannot: a
    /// round trip can be internally consistent while still disagreeing
    /// with every other Ed25519 implementation on the wire.
    #[test]
    fn matches_rfc8032_test_vector_1() {
        fn hex32(s: &str) -> [u8; 32] {
            let bytes = hex::decode(s).unwrap();
            bytes.try_into().unwrap()
        }
        fn hex64(s: &str) -> [u8; 64] {
            let bytes = hex::decode(s).unwrap();
            bytes.try_into().unwrap()
        }

        let seed = hex32("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
        let expected_public_key =
            hex32("d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a");
        let expected_signature = hex64(
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        );

        let keypair = DeviceKeypair::from_secret_bytes(&seed);
        assert_eq!(keypair.device_id(), expected_public_key);

        let signature = keypair.sign(b"");
        assert_eq!(signature, expected_signature);

        verify(&expected_public_key, b"", &expected_signature).unwrap();
    }
}
