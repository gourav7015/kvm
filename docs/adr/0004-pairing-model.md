# ADR-0004: Pairing Model — Permissive TLS + Local Code Confirmation

## Status

Accepted (2026-09-10)

## Context

`net`'s TLS verifiers (ADR-0003) only complete a handshake with a peer
already present in the trust store — that's the entire point of the
pinned-identity model. But pairing is, by definition, the process of
establishing trust with a device that *isn't* trusted yet. If the normal
verifiers are the only way to connect, two devices can never have their
first conversation at all. This needed a real answer before writing any
Phase 2 code, not a bolt-on later.

## Decision

**Pairing uses a separate, deliberately permissive connection mode, and
moves the actual trust decision out of TLS and into a human-confirmed
code comparison — the same shape as Bluetooth Numeric Comparison or
Signal/WhatsApp safety-number verification.**

Concretely:

1. Discovery (mDNS/UDP broadcast/manual IP) gives an address and an
   *unauthenticated claim* of a device ID — anyone can broadcast a fake
   one, so it is never trusted on its own.
2. To pair, a device connects using `net`'s pairing-mode entry points
   (`connect_for_pairing`/`accept_for_pairing`), which use an
   unconditionally-permissive `TrustCheck` (`|_| true`) *for that one
   connection attempt only*. TLS still runs for real — the peer must
   still prove possession of the private key behind whatever device ID it
   presents — pairing mode only skips the trust-store lookup, not the
   cryptography.
3. Once connected, each side independently computes a short numeric code
   from the two now-cryptographically-known device IDs (order-independent,
   no wire transmission — see `identity::pairing_code`). Because both
   sides derive it from the same TLS-verified identities, an attacker
   sitting in the middle (who would need a different keypair than the
   real peer) produces a *different* code on the victim's screen — this
   is the actual security property, not the TLS handshake itself.
4. A human compares the codes (or, in an automated/headless flow, the
   pairing state machine's caller supplies the accept/reject decision)
   and accepts or rejects.
5. **Only on mutual accept does either side call `identity::TrustStore::
   trust(peer_device_id)`.** From that point on, ordinary (non-pairing)
   connections between these two devices use the normal strict verifier
   and succeed automatically, no pairing step required.

The pairing state machine itself (`core`) is pure logic over events
(`IncomingRequest`, `LocalAccept`, `LocalReject`, `RemoteAccept`,
`RemoteReject`, `Cancel`, `Timeout`) — it has no idea `net`, mDNS, or a
socket exist. This is what the Phase 2 DoD's "pairing state machine
unit-tested as pure logic" is asking for, and it's the same
discipline we used for `net`'s backoff schedule (ADR-0003 §4): keep the
part that's hard to get right pure and exhaustively testable, keep the
I/O-touching glue thin around it.

## Consequences

- `net` grows a second, explicitly-named connection path
  (`connect_for_pairing`/`accept_for_pairing`) alongside the normal
  `connect`/`accept`. Anyone reading a call site immediately sees which
  trust model is in effect — there is no single "connect" that silently
  behaves differently depending on hidden state.
- The pairing code is a *usability/integrity* signal (did a MITM
  substitute a different peer?), not a secrecy mechanism — it's derived
  from public information (both device IDs) and is deliberately never
  treated as a shared secret. This matches how Signal/WhatsApp/Bluetooth
  numeric comparison actually works, and avoids needing any secure
  code-exchange channel that wouldn't exist yet anyway.
- A live pairing-mode connection is inherently a larger attack surface
  than a normal one (anyone can complete the TLS handshake). This is
  scoped down deliberately: pairing mode should only be *entered*
  in response to explicit user action ("add a device"), should time out
  quickly, and should never coexist with normal peer connections on the
  same endpoint without care — revisited if/when `core` wires multiple
  concurrent connections together.
- Revocation already covered by Phase 1b/1c's adversarial tests continues
  to matter: a device removed from the trust store after pairing must
  fail the *normal* (non-pairing) verifier on its next connection attempt,
  exactly as already tested.
