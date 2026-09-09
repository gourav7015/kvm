# ADR-0003: `net` Crate — TLS Identity Model, Stream Tagging, and Liveness

## Status

Accepted (2026-09-09)

## Context

Phase 1c needed QUIC connections between peers authenticated by the
`identity` crate's Ed25519 keypairs, with no certificate authority, plus
multiplexed streams per concern and reconnect-with-backoff. Several
concrete design points came up while implementing this that ADR-0001
didn't (and couldn't) settle in advance.

## Decisions

### 1. Self-signed X.509 certs, not raw public keys (RFC 7250)

`quinn`/`rustls` need a certificate to run TLS. Two options exist:
self-signed X.509 certs whose embedded public key *is* the device's
Ed25519 identity, or rustls 0.23's newer raw-public-key (RFC 7250) support.
We use self-signed X.509 (via `rcgen`, generated fresh at process startup
from the existing `ed25519_dalek::SigningKey` seed — the cert itself is
disposable, the seed is the durable identity). RPK is real and simpler in
principle, but isn't what quinn's own examples or other Rust P2P stacks
(iroh, rust-libp2p) use, requires extra TLS extension negotiation, and the
verifier code ends up nearly identical anyway. Self-signed X.509 is the
proven path; RPK is a plausible later optimization, not a blocker.

Both directions are verified: a custom `rustls::client::danger::
ServerCertVerifier` (when dialing out) and `rustls::server::danger::
ClientCertVerifier` (when accepting, with `client_auth_mandatory() ->
true`) each extract the peer's SubjectPublicKeyInfo, decode it as an
Ed25519 `VerifyingKey`, and accept only if a caller-supplied `TrustCheck`
closure (`Arc<dyn Fn(&DeviceId) -> bool>`) returns `true`. This is the
entire trust boundary — a bug here is a full auth bypass, which is why
both directions get dedicated adversarial tests
(`tests/untrusted_peer.rs`).

### 2. Stream identification by tag byte, not acceptance order

QUIC guarantees in-order bytes *within* a stream, not arrival order
*across* streams. Four concern streams (control/input/clipboard/transfer)
are opened by whichever side initiates the connection; the accepting side
calls `accept_bi()` in a loop and cannot assume the Nth accepted stream is
the Nth one opened once packet loss/reordering is in play. Each stream
writes a single leading tag byte identifying its concern before any
message frames; the acceptor reads that byte first and routes the stream
by tag rather than by position. Verified directly by
`tests/packet_loss.rs`, which forces real reordering-prone conditions
through a UDP relay.

### 3. Liveness via QUIC idle-timeout/keep-alive, not an app-level Ping/Pong loop

`protocol::ControlMessage` already defines `Ping`/`Pong`, implying an
app-level heartbeat. In practice, quinn's built-in `max_idle_timeout` +
`keep_alive_interval` (`TransportConfig`) already solve "is this
connection alive" robustly at the transport layer — `Connection::closed()`
resolves when the peer goes silent, no hand-rolled reader/writer
concurrency needed. We rely on that for dead-connection detection
(`tests/forced_kill.rs`) rather than building a redundant app-level
heartbeat. `Ping`/`Pong` stay defined in `protocol` for possible future use
(e.g. app-visible RTT), just unused by `net`'s current liveness mechanism.

Defaults: `DEFAULT_MAX_IDLE_TIMEOUT` = 10s, `DEFAULT_KEEP_ALIVE_INTERVAL` =
3s. Both are exposed as explicit parameters
(`new_endpoint_with_timeouts`) so tests can use short timeouts instead of
waiting out the production defaults.

### 4. Reconnect backoff is a pure function, tested with a paused clock

`backoff::next_delay(attempt) -> Duration` (exponential, 100ms base, 5s
cap) has no I/O and is exhaustively unit-tested. The retry loop
(`reconnect::connect_with_backoff`) is generic over an injected `connect`
closure and is proven not to busy-loop using `#[tokio::test(start_paused =
true)]` plus `tokio::time::advance` — deterministic, no flaky real-time
waiting. `tests/forced_kill.rs` adds a live (short-real-time) complement
proving the same property against an actually-dead address.

### 5. A lightweight post-TLS handshake, not a redundant identity proof

Since mutual TLS already cryptographically proves both sides' identities,
the post-connect `Handshake::Hello` exchange is *not* a second identity
proof — it's a cross-check (does the peer's claimed `device_id` match what
TLS actually verified?) plus the natural place to reject a
replayed/duplicate handshake message on an already-set-up connection
(`Peer::recv_control` only accepts `ControlMessage`s once setup is done —
see `tests/replayed_handshake.rs`). `HandshakeMessage::HelloAck` stays
defined in `protocol` but unused here, since by the time streams exist
both sides are already mutually authenticated.

## Consequences

- `net` depends on `identity` only for `DeviceKeypair`-shaped seed bytes
  (via `IdentityCert::from_seed`) — it does not depend on `identity::
  TrustStore` directly, so callers can back `TrustCheck` with whatever
  storage/locking strategy fits `core`'s eventual design.
- The Phase 1c DoD's "real cross-machine LAN test" is **not yet
  satisfied** — this environment has one machine. A ready-to-run manual
  test (`examples/lan_peer.rs`) exists and has been smoke-tested over
  loopback; running it across two real machines is tracked as open manual
  QA in `docs/architecture.md`, not silently skipped.
- `cargo-deny` caught two real issues while adding dependencies for this
  phase: several newly-pulled licenses (ISC, Zlib, CDLA-Permissive-2.0)
  needed allowlisting (all standard, permissive, fine), and — same pattern
  as ADR-0002 — nothing unmaintained turned up this time, which is itself
  worth noting as the gate working as intended on every phase, not just
  the one where it caught something.
