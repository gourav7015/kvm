//! OS-keychain-backed storage for a device's private key.
//!
//! This talks to real OS secure storage (macOS Keychain, Windows Credential
//! Manager, Linux Secret Service) and cannot be meaningfully exercised in
//! CI — Linux CI runners have no Secret Service daemon, and headless
//! macOS/Windows runners can't be relied on to have an unlocked keychain.
//! Correctness here is a manual-QA item (see the build plan's Phase 1b
//! entry and cross-platform QA matrix), not something `cargo test` alone
//! can verify.

use keyring::Entry;

use crate::error::IdentityError;

const SERVICE_NAME: &str = "dev.universal-kvm";
const ACCOUNT_NAME: &str = "device-identity";

fn entry() -> Result<Entry, IdentityError> {
    Entry::new(SERVICE_NAME, ACCOUNT_NAME).map_err(|e| IdentityError::Keychain(e.to_string()))
}

/// Persists a device's private key seed to the OS keychain, overwriting any
/// existing entry.
pub fn save_secret(seed: &[u8; 32]) -> Result<(), IdentityError> {
    let entry = entry()?;
    entry
        .set_secret(seed)
        .map_err(|e| IdentityError::Keychain(e.to_string()))
}

/// Loads this device's private key seed from the OS keychain.
///
/// Returns [`IdentityError::PrivateKeyNotFound`] specifically when the
/// keychain has no entry yet (first run), distinct from any other keychain
/// access failure.
pub fn load_secret() -> Result<[u8; 32], IdentityError> {
    let entry = entry()?;
    let bytes = entry.get_secret().map_err(|e| match e {
        keyring::Error::NoEntry => IdentityError::PrivateKeyNotFound,
        other => IdentityError::Keychain(other.to_string()),
    })?;
    bytes.try_into().map_err(|_| {
        IdentityError::Keychain("stored private key has unexpected length".to_string())
    })
}

/// Removes this device's private key from the OS keychain, if present.
pub fn delete_secret() -> Result<(), IdentityError> {
    let entry = entry()?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(IdentityError::Keychain(e.to_string())),
    }
}

/// Loads the device's existing keypair from the OS keychain, or generates
/// and persists a new one if none exists yet.
pub fn load_or_create_keypair() -> Result<crate::keypair::DeviceKeypair, IdentityError> {
    match load_secret() {
        Ok(seed) => Ok(crate::keypair::DeviceKeypair::from_secret_bytes(&seed)),
        Err(IdentityError::PrivateKeyNotFound) => {
            let keypair = crate::keypair::DeviceKeypair::generate();
            save_secret(&keypair.secret_bytes())?;
            Ok(keypair)
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These touch the real OS keychain and are excluded from normal CI runs
    // (see the module doc comment) — run explicitly with
    // `cargo test -- --ignored` on a machine with an unlocked keychain, as
    // part of the manual QA pass for this phase.
    //
    // Deliberately one test, not several: every case here shares the same
    // (SERVICE_NAME, ACCOUNT_NAME) global keychain entry, and cargo's test
    // harness runs tests concurrently by default — splitting this into
    // separate #[test] functions caused exactly the race it looks like it
    // would (one test's save observed by another test's "missing entry"
    // check). Sequencing everything in one test sidesteps that without
    // complicating the real API just to make it test-isolable.
    #[test]
    #[ignore = "touches the real OS keychain; see manual QA matrix"]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    fn keychain_round_trip_and_load_or_create() {
        // Start from a clean slate regardless of what a previous run left
        // behind.
        let _ = delete_secret();

        let err = load_secret().unwrap_err();
        assert!(matches!(err, IdentityError::PrivateKeyNotFound));

        let seed = [42u8; 32];
        save_secret(&seed).unwrap();
        let loaded = load_secret().unwrap();
        assert_eq!(loaded, seed);

        delete_secret().unwrap();
        let err = load_secret().unwrap_err();
        assert!(matches!(err, IdentityError::PrivateKeyNotFound));

        let first = load_or_create_keypair().unwrap();
        let second = load_or_create_keypair().unwrap();
        assert_eq!(first.device_id(), second.device_id());

        delete_secret().unwrap();
    }
}
