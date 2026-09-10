//! QUIC transport: connection management, mutual authentication against
//! the `identity` trust store, multiplexed streams per concern
//! (control/input/clipboard/file), heartbeat, and reconnect-with-backoff.

mod backoff;
mod config;
mod endpoint;
mod error;
mod framed;
mod peer;
mod reconnect;
mod streams;
mod tls;

pub use config::{DEFAULT_KEEP_ALIVE_INTERVAL, DEFAULT_MAX_IDLE_TIMEOUT, IGNORED_SERVER_NAME};
pub use endpoint::{new_endpoint, new_endpoint_with_timeouts};
pub use error::NetError;
pub use framed::MessageStream;
pub use peer::{Peer, accept, accept_for_pairing, connect, connect_for_pairing};
pub use reconnect::connect_with_backoff;
pub use streams::Streams;
pub use tls::{IdentityCert, TrustCheck};
