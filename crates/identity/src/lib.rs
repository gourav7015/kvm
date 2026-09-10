//! Device cryptographic identity: keypair generation, OS-keychain-backed
//! private key storage, and a pinned-public-key trust store.
//!
//! No networking here — the pairing *code* derivation lives here since
//! it's pure identity math, but the pairing *state machine* and UX live
//! in `core` (see ADR-0004).

mod error;
mod keypair;
mod keystore;
mod pairing_code;
mod trust_store;

pub use error::IdentityError;
pub use keypair::{DeviceId, DeviceKeypair, verify};
pub use keystore::{delete_secret, load_or_create_keypair, load_secret, save_secret};
pub use pairing_code::pairing_code;
pub use trust_store::TrustStore;
