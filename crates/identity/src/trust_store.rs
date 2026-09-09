//! A pinned-public-key trust store, persisted as a JSON file on disk.
//!
//! There is no certificate authority: a device is trusted because a human
//! pinned its public key during pairing, and stays trusted until explicitly
//! revoked. This module only manages that local set of pinned keys — it
//! knows nothing about pairing UX or the network.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use kvm_protocol::DeviceId;
use serde::{Deserialize, Serialize};

use crate::error::IdentityError;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustStoreFile {
    /// Hex-encoded [`DeviceId`]s, for a human-readable file on disk.
    trusted_device_ids: Vec<String>,
}

/// The set of devices this device trusts, backed by a JSON file.
#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    trusted: BTreeSet<DeviceId>,
}

impl TrustStore {
    /// Loads the trust store from `path`, or starts empty if no file exists
    /// there yet.
    ///
    /// Returns [`IdentityError::CorruptTrustStore`] if the file exists but
    /// its contents can't be parsed, or contain a device ID that isn't
    /// valid hex / isn't 32 bytes — this is never silently treated as an
    /// empty trust store, since that would mean silently un-trusting every
    /// paired device.
    pub fn load_or_create(path: impl Into<PathBuf>) -> Result<Self, IdentityError> {
        let path = path.into();

        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    trusted: BTreeSet::new(),
                });
            }
            Err(e) => return Err(IdentityError::TrustStoreIo(e.to_string())),
        };

        let file: TrustStoreFile = serde_json::from_str(&contents)
            .map_err(|e| IdentityError::CorruptTrustStore(e.to_string()))?;

        let mut trusted = BTreeSet::new();
        for hex_id in file.trusted_device_ids {
            let bytes = hex::decode(&hex_id).map_err(|e| {
                IdentityError::CorruptTrustStore(format!("invalid hex device id: {e}"))
            })?;
            let device_id: DeviceId = bytes.try_into().map_err(|_| {
                IdentityError::CorruptTrustStore("device id is not 32 bytes".to_string())
            })?;
            trusted.insert(device_id);
        }

        Ok(Self { path, trusted })
    }

    /// Whether `device_id` is currently trusted.
    pub fn is_trusted(&self, device_id: &DeviceId) -> bool {
        self.trusted.contains(device_id)
    }

    /// Pins `device_id` as trusted and persists the change. Trusting an
    /// already-trusted device is a no-op (idempotent).
    pub fn trust(&mut self, device_id: DeviceId) -> Result<(), IdentityError> {
        self.trusted.insert(device_id);
        self.save()
    }

    /// Revokes trust in `device_id` and persists the change. Revoking a
    /// device that isn't trusted is a no-op (idempotent).
    pub fn revoke(&mut self, device_id: &DeviceId) -> Result<(), IdentityError> {
        self.trusted.remove(device_id);
        self.save()
    }

    /// All currently-trusted device IDs, in a stable (sorted) order.
    pub fn trusted_devices(&self) -> impl Iterator<Item = &DeviceId> {
        self.trusted.iter()
    }

    fn save(&self) -> Result<(), IdentityError> {
        let file = TrustStoreFile {
            trusted_device_ids: self.trusted.iter().map(hex::encode).collect(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| IdentityError::TrustStoreIo(e.to_string()))?;

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| IdentityError::TrustStoreIo(e.to_string()))?;
        }
        fs::write(&self.path, json).map_err(|e| IdentityError::TrustStoreIo(e.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn temp_store_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("trust_store.json")
    }

    #[test]
    fn missing_file_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = TrustStore::load_or_create(temp_store_path(&dir)).unwrap();
        assert_eq!(store.trusted_devices().count(), 0);
    }

    #[test]
    fn trust_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_store_path(&dir);
        let device_id: DeviceId = [7u8; 32];

        let mut store = TrustStore::load_or_create(&path).unwrap();
        store.trust(device_id).unwrap();
        assert!(store.is_trusted(&device_id));

        let reloaded = TrustStore::load_or_create(&path).unwrap();
        assert!(reloaded.is_trusted(&device_id));
    }

    #[test]
    fn revoke_persists_across_reload() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_store_path(&dir);
        let device_id: DeviceId = [8u8; 32];

        let mut store = TrustStore::load_or_create(&path).unwrap();
        store.trust(device_id).unwrap();
        store.revoke(&device_id).unwrap();
        assert!(!store.is_trusted(&device_id));

        let reloaded = TrustStore::load_or_create(&path).unwrap();
        assert!(!reloaded.is_trusted(&device_id));
    }

    #[test]
    fn trusting_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let device_id: DeviceId = [9u8; 32];

        let mut store = TrustStore::load_or_create(temp_store_path(&dir)).unwrap();
        store.trust(device_id).unwrap();
        store.trust(device_id).unwrap();
        assert_eq!(store.trusted_devices().count(), 1);
    }

    #[test]
    fn revoking_untrusted_device_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let device_id: DeviceId = [10u8; 32];

        let mut store = TrustStore::load_or_create(temp_store_path(&dir)).unwrap();
        store.revoke(&device_id).unwrap();
        assert!(!store.is_trusted(&device_id));
    }

    #[test]
    fn revoked_device_reconnecting_is_not_trusted() {
        // Adversarial case named explicitly in the build plan's Phase 1b
        // DoD / risk register: a device that *was* trusted and got
        // revoked must not be re-accepted just because it still has the
        // same keypair and tries again.
        let dir = tempfile::tempdir().unwrap();
        let path = temp_store_path(&dir);
        let device_id: DeviceId = [11u8; 32];

        let mut store = TrustStore::load_or_create(&path).unwrap();
        store.trust(device_id).unwrap();
        store.revoke(&device_id).unwrap();

        // Simulate the revoked device "reconnecting": nothing about
        // checking is_trusted again re-establishes trust.
        assert!(!store.is_trusted(&device_id));
        let reloaded = TrustStore::load_or_create(&path).unwrap();
        assert!(!reloaded.is_trusted(&device_id));
    }

    #[test]
    fn corrupted_json_fails_gracefully_not_silently_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_store_path(&dir);
        fs::write(&path, b"{ this is not valid json").unwrap();

        let err = TrustStore::load_or_create(&path).unwrap_err();
        assert!(matches!(err, IdentityError::CorruptTrustStore(_)));
    }

    #[test]
    fn trust_store_with_invalid_hex_device_id_fails_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_store_path(&dir);
        fs::write(&path, br#"{"trusted_device_ids":["not-hex!!"]}"#).unwrap();

        let err = TrustStore::load_or_create(&path).unwrap_err();
        assert!(matches!(err, IdentityError::CorruptTrustStore(_)));
    }

    #[test]
    fn trust_store_with_wrong_length_device_id_fails_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_store_path(&dir);
        // Valid hex, but only 4 bytes instead of 32.
        fs::write(&path, br#"{"trusted_device_ids":["deadbeef"]}"#).unwrap();

        let err = TrustStore::load_or_create(&path).unwrap_err();
        assert!(matches!(err, IdentityError::CorruptTrustStore(_)));
    }

    #[test]
    fn multiple_devices_list_in_stable_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        let a: DeviceId = [1u8; 32];
        let b: DeviceId = [2u8; 32];
        let c: DeviceId = [3u8; 32];

        let mut store = TrustStore::load_or_create(temp_store_path(&dir)).unwrap();
        store.trust(c).unwrap();
        store.trust(a).unwrap();
        store.trust(b).unwrap();

        let listed: Vec<DeviceId> = store.trusted_devices().copied().collect();
        assert_eq!(listed, vec![a, b, c]);
    }
}
