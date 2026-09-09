//! Errors produced by keypair operations, OS keychain access, and the
//! trust store.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentityError {
    /// A public key's raw bytes don't form a valid Ed25519 point.
    #[error("invalid public key bytes: {0}")]
    InvalidPublicKey(String),

    /// A signature failed to verify against the given public key and
    /// message. Deliberately carries no detail about *why* — that would
    /// leak information useful to an attacker probing signature checks.
    #[error("signature verification failed")]
    VerificationFailed,

    /// The OS keychain could not be reached at all (locked, unavailable,
    /// permission denied, etc.) — distinct from [`IdentityError::PrivateKeyNotFound`],
    /// which means the keychain was reachable but had no entry.
    #[error("failed to access OS keychain: {0}")]
    Keychain(String),

    /// The OS keychain was reachable, but no private key entry exists yet
    /// for this device (e.g. first run, before a keypair is generated).
    #[error("no private key found in the OS keychain for this device")]
    PrivateKeyNotFound,

    /// The trust store file exists but its contents are not valid —
    /// corrupted, truncated, or containing malformed entries. Callers
    /// should treat this as unrecoverable without user intervention; it is
    /// never silently treated as an empty trust store.
    #[error("trust store file is corrupted: {0}")]
    CorruptTrustStore(String),

    /// Reading or writing the trust store file failed for reasons other
    /// than corrupt content (permissions, disk full, missing parent
    /// directory, etc.).
    #[error("failed to read or write trust store file: {0}")]
    TrustStoreIo(String),
}
