//! Wire message types and versioned framing shared by every peer.
//!
//! Pure data definitions only — no I/O, no networking, no crypto. This is the
//! contract that `net`, `core`, and every other crate serialize against.
