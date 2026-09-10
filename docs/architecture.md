# Universal KVM — Architecture

Living summary of the system as actually built. Updated at the end of any
phase that changes crate responsibilities or data flow (see the build plan's
per-phase Definition of Done). Decisions and their rationale live in
[`docs/adr/`](adr/) — this file is the current-state index, not the decision
log.

## Workspace shape

```
apps/desktop (Tauri UI)
        |  commands/events
        v
   crates/core  -- device state, trust store, active layout, pairing/switching state machines
        |
   +----+---------+----------+
   v    v         v          v
 input clipboard transfer   net (quinn/QUIC)
   |      |         |          |
   +------+---------+----------+--> protocol (shared message types)
                                |
                       identity + discovery
```

`core` owns no sockets and no OS input APIs directly — it reacts to events
from `input`/`clipboard`/`net`/`discovery` and issues commands back through
channels. Platform-specific code is confined to backend modules inside
`input` and `clipboard`; every other crate is platform-agnostic.

## Crates

- **`protocol`** — Wire message types and versioned framing shared by every
  peer. Pure data definitions only; no I/O, no networking, no crypto.
- **`identity`** — Device cryptographic identity: keypair generation,
  OS-keychain-backed private key storage, and a pinned-public-key trust
  store. No networking or pairing UX.
- **`discovery`** — Local-network device discovery via mDNS, with a UDP
  broadcast fallback and manual IP entry when both are unavailable.
- **`net`** — QUIC transport: connection management, mutual authentication
  against the `identity` trust store, multiplexed streams per concern
  (control/input/clipboard/file), heartbeat, and reconnect-with-backoff.
- **`input`** — Cross-platform keyboard/mouse capture and injection. A
  platform-agnostic `Capture`/`Inject` trait plus pure, OS-independent
  modifier-translation and switching logic; per-OS backends stay thin shims
  behind the trait.
- **`clipboard`** — Cross-device clipboard read/write/watch, with per-OS
  change detection.
- **`transfer`** — File transfer: chunking, progress reporting, transfer
  queue, and pause/resume/cancel.
- **`core`** — Orchestrator: device state, active-device switching, screen
  layout, and pairing state machines. Wires `input`, `clipboard`, `transfer`,
  and `net` together via channels.
- **`apps/desktop`** — Tauri shell: dashboard UI (device list, layout editor,
  pairing, settings), tray icon, wraps `core` via Tauri commands/events.
  *(Not yet scaffolded — introduced when first needed, per the build plan.)*

## Status

- Phase 0 (project skeleton + CI) — done.
- Phase 1a (`protocol` crate: message types, versioned framing) — done.
  Serialization uses `postcard`, not the originally planned `bincode` — see
  [ADR-0002](adr/0002-postcard-not-bincode.md).
- Phase 1b (`identity` crate: keypairs, keychain storage, trust store) —
  code and automated tests green. Keychain integration manually verified
  on macOS (this session) and Windows (user-reported: real ignored
  integration test run against Windows Credential Manager, 2026-09-10).
  Linux Secret Service round-trip remains untested — no environment or
  report has covered it. **🟡 Phase 1b carries one open item (Linux
  keychain)**, explicitly not treated as blocking further phases per
  2026-09-10 direction — tracked below so it isn't lost, not because it
  stopped mattering.
- Phase 1c (`net` crate: QUIC transport, TLS mutual auth, multiplexed
  streams, heartbeat/reconnect) — code and automated tests green, CI green
  on all 3 OS runners. Real cross-machine LAN test completed 2026-09-10:
  Windows listener ↔ macOS connector, successful QUIC/TLS handshake and
  ping/pong exchange — genuinely cross-machine *and* cross-platform, the
  strongest form of evidence this DoD item asked for. **🟢 Phase 1c is
  CLOSED.**
  Identity uses self-signed X.509 certs (via `rcgen`) built from the
  existing Ed25519 keypair, verified by a custom rustls verifier that
  pins the embedded public key against the trust store rather than any CA
  chain — see [ADR-0003](adr/0003-net-tls-and-stream-design.md).

### Open manual QA (not blocking further phases, but tracked)

| Item | Status |
|---|---|
| macOS Keychain round-trip (`kvm-identity`, `cargo test -- --ignored`) | ✅ verified 2026-09-09 |
| Windows Credential Manager round-trip | ✅ verified 2026-09-10 (user-run, real ignored integration test) |
| Linux Secret Service round-trip | ⏳ pending — needs a Linux environment with a Secret Service daemon |
| Real cross-machine LAN test (`kvm-net`, `examples/lan_peer.rs`) | ✅ verified 2026-09-10 — Windows listener ↔ macOS connector |

See `/Users/gourav/.claude/plans/elegant-wishing-origami.md` for the full
phase breakdown, per-phase Definition of Done, risk register, and QA matrix.
