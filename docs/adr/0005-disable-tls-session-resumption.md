# ADR-0005: Disable TLS 1.3 Session Ticket Issuance

## Status

Accepted (2026-09-10)

## Context

While writing Phase 2's end-to-end pairing test (`core/tests/
pairing_end_to_end.rs` — pair two devices, revoke one, confirm ordinary
reconnection then fails), the revoke-then-reconnect assertion failed: a
device revoked *after* a prior successful connection could still connect
again. This is a real defect in the pinned-identity trust model from
ADR-0003, not a test bug — trust must be re-checked on every connection,
since our trust store can change at any time (that's the entire point of
revocation).

Root cause: rustls's `ServerConfig` issues TLS 1.3 session tickets after
every successful handshake by default (`send_tls13_tickets = 2`).
Session resumption via a stored ticket is a PSK-based abbreviated
handshake that deliberately skips full certificate re-verification —
that's its whole performance purpose in the use cases rustls is designed
for (web servers, where a slowly-changing CA hierarchy makes re-verifying
identity on every connection wasteful). Our model is the opposite: a
locally-mutable, frequently-changing pinned-identity trust store where
"is this peer still trusted" must be answered fresh, every time.

Confirmed via direct experiment: temporarily reverting the fix made a
purpose-built regression test (`net/tests/untrusted_peer.rs`,
`revocation_is_not_bypassed_by_a_resumed_tls_session`) fail deterministically;
restoring it made the same test pass reliably across repeated clean
rebuilds. (A stale incrementally-cached test binary briefly produced a
false failure signal mid-investigation — resolved by `cargo clean -p
kvm-net` and confirmed stable across 5 repeated clean-rebuild runs.)

## Decision

Set `rustls_config.send_tls13_tickets = 0` in `net::config::server_config`.
No session tickets are ever issued, so a client has nothing to resume
with — every connection, without exception, runs a full TLS 1.3 handshake
and therefore a full run of [`PinnedServerVerifier`]/
[`PinnedClientVerifier`] against the live trust store.

## Consequences

- Every connection pays the cost of a full asymmetric handshake; there is
  no abbreviated-handshake fast path. Given this is a LAN, peer-to-peer
  application (not a high-throughput public server), that cost is
  negligible and correctness matters far more here.
- This closes the gap for *all* callers of `net`, not just pairing —
  the bug would have equally affected an ordinary reconnect after
  revocation outside of any pairing flow. `net/tests/untrusted_peer.rs`
  carries the regression test rather than `core`, since this is a `net`
  crate property independent of the pairing feature that happened to
  surface it.
- Lesson for the rest of the build: a security property ("revocation is
  immediate") can look correct in a test that only ever exercises the
  *first* connection attempt. The regression test that caught this
  deliberately completes one full successful connection *before*
  revoking, specifically because that's the scenario a resumption-based
  bypass requires and a naive "revoke immediately, then try to connect"
  test cannot exercise.
