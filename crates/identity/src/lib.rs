//! Device cryptographic identity: keypair generation, OS-keychain-backed
//! private key storage, and a pinned-public-key trust store.
//!
//! No networking or pairing UX here — just local crypto/storage primitives
//! that `net` and `core` build on.

mod error;
mod keypair;
mod keystore;
mod trust_store;

pub use error::IdentityError;
pub use keypair::{DeviceId, DeviceKeypair, verify};
pub use keystore::{delete_secret, load_or_create_keypair, load_secret, save_secret};
pub use trust_store::TrustStore;
