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
  on all 3 OSes: macOS (this session), Windows (user-reported, 2026-09-10),
  and Linux (user-reported, 2026-09-10 — Ubuntu 24.04.4 LTS/GNOME,
  `gnome-keyring-daemon` confirmed as the `org.freedesktop.secrets`
  provider via `busctl`; `keyring` 4.2.0's Linux path has no fallback
  store — verified against its actual source — so a passing test there
  can only mean the real Secret Service was used). **🟢 Phase 1b is
  CLOSED.**
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
- Phase 2 (discovery + pairing) — **🟢 CLOSED.** Pairing: `protocol` gained a
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
  Real cross-machine discovery (`crates/discovery/examples/discover.rs`)
  completed 2026-09-10, both directions, Mac ↔ Windows: mDNS and UDP
  broadcast each confirmed working both ways, with exact `device_id`
  match, real routable LAN addresses, and correct ports. Discovery ≠
  trust confirmed both structurally (`kvm-discovery` has no production
  dependency on `kvm-identity`; the only `TrustStore::trust()` call site
  in the workspace is `core::pairing_session::commit_if_completed`, gated
  behind `PairingState::Completed`) and empirically (no trust-store file
  touched during either discovery round). One non-blocking gap noted, not
  a DoD violation: UDP broadcast events aren't deduplicated by `device_id`
  at the crate level — a future `core` device list will need to do that.

- Phase 3 (input engine, macOS↔Windows target pair) — **🟡 real
  Mac↔Windows hardware QA completed 2026-09-10/11; both translation
  directions now confirmed working, but this phase is not closed** —
  Windows UAC/elevated-window behavior remains untestable on this
  session's Windows machine specifically (its logged-in account is the
  built-in Administrator account, which is exempt from UAC's
  split-token model entirely — there's no unelevated state available on
  it to test against; needs a standard user account elsewhere). Two real
  issues were found and fixed during this QA pass: (1) `commit 8266c24`
  — `translate_for_target` was correct and unit-tested but never wired
  into the live inject path; (2) `commit 0c75c78` — macOS never captured
  a bare modifier key press at all (reported as `FlagsChanged`, which
  the capture backend dropped unconditionally), blocking Mac→Windows
  Command→Control translation; fixed by diffing modifier flags across
  events, confirmed on real hardware (`docs/manual-qa/phase-3-input.md`).
  Still open beyond the UAC item: `InputMessage::Key` carries no
  concurrently-held-modifiers field, so a real shortcut *combination*
  like Cmd+C still sends only the plain letter — the standalone tap fix
  doesn't cover that, and it's separate, not-yet-scoped work. `protocol`
  gained a normalized `Key`/`PlatformKind`
  instead of a raw OS keycode; `input` gained the `Capture`/`Inject`
  trait boundary, a pure OS-independent modifier-translation function
  (the killer feature: Command↔Control swap only when exactly one side
  is macOS, everything else passes through), a macOS backend
  (`CGEventTap`/`CGEvent::post`, `AXIsProcessTrusted`-gated), and a
  Windows backend (`WH_KEYBOARD_LL`/`WH_MOUSE_LL` hooks, `SendInput`,
  compile-verified via `cargo check --target x86_64-pc-windows-msvc`
  since this environment has no Windows machine to build on directly).
  `core` gained a small `input_bridge` module bridging `Capture`/`Inject`
  to an existing `Peer`'s `input` stream — no new networking or trust
  path; input only ever flows over an already-authenticated connection.
  A loopback integration test proves the full
  capture→normalize→serialize→transport→deserialize→inject pipeline with
  fake `Capture`/`Inject` doubles standing in for the OS ends (CI has no
  physical keyboard/mouse). See
  [ADR-0007](adr/0007-input-architecture.md) for the full design
  rationale, including why Linux capture is deferred to Phase 3b (a
  dedicated X11-vs-Wayland go/no-go decision, not silently skipped).
  Manual QA procedure: `docs/manual-qa/phase-3-input.md`.

### Manual QA record (all closed)

| Item | Status |
|---|---|
| macOS Keychain round-trip (`kvm-identity`, `cargo test -- --ignored`) | ✅ verified 2026-09-09 |
| Windows Credential Manager round-trip | ✅ verified 2026-09-10 (user-run, real ignored integration test) |
| Linux Secret Service round-trip | ✅ verified 2026-09-10 — Ubuntu 24.04.4 LTS, GNOME, `gnome-keyring-daemon` (PID confirmed via `busctl --user list`) |
| Real cross-machine LAN test (`kvm-net`, `examples/lan_peer.rs`) | ✅ verified 2026-09-10 — Windows listener ↔ macOS connector |
| mDNS discovery across real separate machines (`kvm-discovery`, `examples/discover.rs`) | ✅ verified 2026-09-10 — both directions, Mac↔Windows |
| UDP broadcast discovery across real separate machines | ✅ verified 2026-09-10 — both directions, Mac↔Windows |
| Manual IP entry path | ✅ covered by the Phase 1c LAN test (`lan_peer.rs` uses a directly-supplied address, no discovery involved) plus `DiscoveredDevice::manual`'s unit test |

### Manual QA record — Phase 3 (real Mac↔Windows run 2026-09-10/11)

Full detail and evidence: `docs/manual-qa/phase-3-input.md`. Two real
issues were found and fixed during this run (see
[ADR-0007](adr/0007-input-architecture.md)'s Update notes):
`translate_for_target` (commit `8266c24`) was correct and unit-tested
but never actually wired into the live inject path; and macOS never
captured a bare modifier key press at all (commit `0c75c78`), blocking
Mac→Windows translation until fixed.

| Item | Status |
|---|---|
| macOS Accessibility-permission-missing path (clear error, no panic, no silent no-op) | ✅ PASS — verified 2026-09-10/11, real `PermissionDenied` before grant, real success after |
| macOS keyboard capture/injection (letters, digits, Enter/Tab/Backspace/Escape/Space, arrows, F1–F5, bare modifiers after the fix) | ✅ PASS |
| macOS mouse capture/injection (move, left/right/middle click, scroll) | ✅ PASS |
| Windows keyboard capture/injection (letters, digits, Enter/Tab/Backspace/Escape/Space, arrows, F1–F5, all modifiers) | ✅ PASS |
| Windows mouse capture/injection (move, left/right/middle click, scroll) | ✅ PASS |
| Windows UAC/elevated-window behavior | ⬜ OPEN — this session's Windows machine is logged in as the built-in Administrator account (exempt from UAC's split-token model), so there is no unelevated state to test against on it at all; needs a standard user account with UAC active |
| **Control (Windows) → Command (Mac) translation** | ✅ PASS — confirmed with reproducible log evidence (real Ctrl+A on Windows executed as Cmd+A on the Mac) |
| **Command (Mac) → Control (Windows) translation** | ✅ PASS (after the `FlagsChanged` fix) — confirmed with reproducible log evidence (real bare Command press on the Mac captured, translated to `ControlLeft`, and injected on Windows) |
| Option↔Alt never translates | ✅ PASS, both directions |
| End-to-end input latency measured and logged (`examples/input_relay.rs`) | ✅ PASS — network RTT 7.9–15.0ms, injection calls ~70–700µs typical (Mac CGEvent and Windows SendInput both), felt latency acceptable for interactive use |
| Modifier state on a shortcut *combination* (e.g. does Cmd+C carry "Command held" alongside `C`) | ⬜ OPEN — not part of this fix; `InputMessage::Key` still has no concurrent-modifiers field, separate not-yet-scoped work |

See `/Users/gourav/.claude/plans/elegant-wishing-origami.md` for the full
phase breakdown, per-phase Definition of Done, risk register, and QA matrix.
