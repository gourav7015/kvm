//! Device cryptographic identity: keypair generation, OS-keychain-backed
//! private key storage, and a pinned-public-key trust store.
//!
//! No networking or pairing UX here — just local crypto/storage primitives
//! that `net` and `core` build on.
