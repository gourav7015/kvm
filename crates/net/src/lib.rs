//! QUIC transport: connection management, mutual authentication against the
//! `identity` trust store, multiplexed streams per concern (control/input/
//! clipboard/file), heartbeat, and reconnect-with-backoff.
