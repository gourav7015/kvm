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
- Phase 2 (discovery + pairing) — **code and automated tests done; real
  multi-machine manual QA still open.** Pairing: `protocol` gained a
  `Pairing` message concern, `identity` gained `pairing_code` (a pure,
  human-comparable code derived from two device IDs, no wire
  transmission), `net` gained `connect_for_pairing`/`accept_for_pairing`
  (a deliberately permissive TLS mode used only for first contact — see
  [ADR-0004](adr/0004-pairing-model.md)), and `core` now has its first
  real content: a pure pairing state machine plus the async glue driving
  it over a real `Peer`. An end-to-end loopback test proves the full
  chain: two devices starting completely untrusted converge on matching
  pairing codes, commit trust into real (file-backed) `TrustStore`s on
  both sides, and an ordinary post-pairing connection then succeeds with
  no further pairing step.
  Discovery: the `discovery` crate now exists — mDNS advertise/browse
  (`mdns-sd`), a UDP broadcast fallback with its own tiny wire format
  (`announcement.rs`, extensively fuzz-style unit tested), and manual IP
  entry (`DiscoveredDevice::manual`). A loopback integration test proves
  mDNS advertise+browse genuinely round-trips through the real OS
  multicast stack, not just that the code compiles.
  Two real bugs were found and fixed while building this, both by actual
  end-to-end verification rather than code review: (1) rustls issues TLS
  1.3 session tickets by default, which let a revoked device keep
  reconnecting via a resumed session that skipped full certificate
  re-verification — [ADR-0005](adr/0005-disable-tls-session-resumption.md);
  (2) the mDNS host name was built from the full 64-char hex device_id,
  silently exceeding DNS's 63-byte label limit and breaking address
  record resolution with no visible error — [ADR-0006](adr/0006-mdns-hostname-dns-label-limit.md).
  **What's still open:** the DoD's "mDNS-only, UDP-fallback-only, and
  manual-IP-only paths each manually verified" calls for real separate
  machines on a real LAN (real routers/Wi-Fi/multicast behavior that
  loopback cannot exercise) — this environment has one machine. A
  ready-to-run manual tool exists at `crates/discovery/examples/discover.rs`
  and has been smoke-tested over loopback (mDNS path confirmed working
  end-to-end; UDP broadcast's core send/recv/encode/decode is covered by
  dedicated automated tests, since two example processes on one machine
  can't share the broadcast port the way two real machines would).

### Open manual QA (not blocking further phases, but tracked)

| Item | Status |
|---|---|
| macOS Keychain round-trip (`kvm-identity`, `cargo test -- --ignored`) | ✅ verified 2026-09-09 |
| Windows Credential Manager round-trip | ✅ verified 2026-09-10 (user-run, real ignored integration test) |
| Linux Secret Service round-trip | ⏳ pending — needs a Linux environment with a Secret Service daemon |
| Real cross-machine LAN test (`kvm-net`, `examples/lan_peer.rs`) | ✅ verified 2026-09-10 — Windows listener ↔ macOS connector |
| mDNS discovery across real separate machines (`kvm-discovery`, `examples/discover.rs`) | ⏳ pending — needs a second machine on the same LAN |
| UDP broadcast discovery across real separate machines | ⏳ pending — needs a second machine on the same LAN |
| Manual IP entry path | ⏳ pending — trivial in isolation, but worth confirming alongside the above two on real hardware |

See `/Users/gourav/.claude/plans/elegant-wishing-origami.md` for the full
phase breakdown, per-phase Definition of Done, risk register, and QA matrix.
