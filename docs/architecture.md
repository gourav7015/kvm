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
  behind the trait. macOS, Windows, and Linux/X11 backends exist; no
  Wayland backend (see [ADR-0008](adr/0008-linux-input-architecture.md) —
  a formal NO-GO for general-purpose capture under Wayland's current
  security model, not a silently-dropped gap).
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

- Phase 3 (input engine, macOS↔Windows target pair) — **🟢 CLOSED.** Real
  Mac↔Windows hardware QA completed 2026-09-10/11
  (`docs/manual-qa/phase-3-input.md`): keyboard, mouse, Accessibility
  permission handling, modifier translation in both directions
  (including real shortcut combinations — Cmd+A/X/V on the Mac landing
  as Ctrl+A/X/V on Windows, each key captured and injected as its own
  independently-ordered event with no protocol change needed), latency,
  and Windows UAC/elevated-window behavior are all confirmed PASS. Two
  real bugs were found and fixed during this QA pass, not glossed over:
  (1) `commit 8266c24` — `translate_for_target` was correct and
  unit-tested but never wired into the live inject path; (2) `commit
  0c75c78` — macOS never captured a bare modifier key press at all
  (reported as `FlagsChanged`, dropped unconditionally by the capture
  backend), blocking Mac→Windows Command→Control translation until
  fixed by diffing modifier flags across events. The UAC test was
  initially blocked by the test machine's account being the Windows
  built-in Administrator (exempt from UAC's split-token model); resolved
  by creating a genuine standard user account and retesting for real.
  `protocol` gained a normalized `Key`/`PlatformKind`
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

- Phase 3b (Linux input spike, X11 GO / Wayland NO-GO) — **🟢 CLOSED.**
  Real Ubuntu 24.04.4 LTS/GNOME/X11 hardware QA completed 2026-09-11
  (`docs/manual-qa/phase-3b-linux-input.md`), paired against the same
  Mac used throughout Phase 3: both directions confirmed PASS —
  keyboard (letters, digits, Enter/Tab/Backspace/Delete/Escape/Space,
  arrows, F1–F5), mouse (move, left/right/middle click, scroll),
  modifier translation both directions (Control↔Command, live evidence
  both ways, including a real Shift+A producing a real capital letter
  and a real injected Control+C genuinely interrupting a running
  process on the target machine), local-input preservation (the Linux
  machine's own keyboard/mouse kept working normally throughout — no
  exclusive grab), and a stuck-modifier/disconnect scenario (held Shift
  through an abrupt kill mid-connection — did not get stuck). `input`
  gained an `x11` backend (`crates/input/src/x11`): XInput2 raw events
  for capture, the XTEST extension's `FakeInput` for injection, via
  `x11rb` (isolated to `cfg(target_os = "linux")`, `xinput`+`xtest`
  features only, no system X11 dev packages needed — confirmed on real
  hardware). A dynamic keycode↔keysym table (`x11::keymap`, queried
  per-session via `GetKeyboardMapping`) bridges X11's non-portable
  keycodes to the portable keysym layer (`x11::keysym`, unit-tested
  exactly like the macOS/Windows keycode tables — 32 real `cargo test`
  passes on the Ubuntu machine itself, not just cross-compiled).
  One real bug was found and fixed during hardware QA (commit
  `0e550fd`): every event was captured and injected twice, a documented
  XInput2 pitfall (`XIAllDevices` matches slave devices too, not just
  the master) — fixed by selecting `XIAllMasterDevices` instead,
  reverified as fixed on the same hardware. Wayland: no code — a
  formal, evidenced NO-GO for general-purpose global *capture* under
  Wayland's current security model (the `org.freedesktop.portal.InputCapture`
  mechanism that would enable it isn't supported by KWin even in KDE 6,
  and Mutter's support is limited; injection alone has better portal
  support via `RemoteDesktop`+`libei` but requires a per-session
  consent dialog, incompatible with a silent background service, and
  wasn't pursued for being half of what a bidirectional KVM needs).
  Full rationale: [ADR-0008](adr/0008-linux-input-architecture.md).
  Not exhaustively tested (noted, not blocking, same standard as
  Phase 3's own minor gaps): function keys F6–F12, the key-repeat flag
  under real held-key auto-repeat, horizontal scroll, a mid-session
  keyboard layout change, and X11's permissive cross-privilege
  injection behavior with an actual privilege-separated target.
  `protocol::Key` gained `#[derive(Hash)]` (purely additive, needed for
  `x11::keymap`'s reverse lookup) — the only change to any pre-existing
  Phase 1–3 code this phase made.

- Phase 4 (edge-based switching, multi-device layout) — **🟡 automated
  work CLOSED, real hardware QA OPEN.** `core` gained `layout.rs` (a
  platform-independent `DeviceId`-keyed edge map, never IP-keyed since
  IPs can change), `ownership.rs` (a pure `Local`/`Forwarding` state
  machine — deliberately *not* the per-device `Active`/`Passive` model
  originally sketched in the build plan; see
  [ADR-0009](adr/0009-edge-switching-architecture.md) decision 2 for
  why that changed during implementation), `router.rs` (pure
  edge-detection + modifier-flush-on-switch, wrapping `ownership.rs`),
  and `session.rs` (the first async layer in this codebase to hold more
  than one `net::Peer` at once, plus `run_target` for the receiving
  side and `exchange_screen_size` for real resolution-aware handoff).
  `input` gained a new, additive `PointerGeometry` trait (absolute
  cursor position, screen size, cursor warp) implemented for all three
  existing backends with no new dependencies. `protocol` gained one
  changed (`SwitchActive` now carries a computed cursor position) and
  one new (`ScreenInfo`) `ControlMessage` variant — the only two wire
  touch-points this phase made.
  Security reuses the existing Phase 1c/2 trust boundary rather than
  inventing a new one: a device only ever becomes a switch target via
  `Session::add_peer`, which is only ever called with a `Peer` that
  already passed `net::connect`/`accept`'s TLS/trust-store
  verification — a discovered-but-unpaired or revoked device
  structurally cannot appear as a target, confirmed by tests at both
  the pure-router and real-`Peer`-integration level.
  Automated coverage: 53 unit tests (`layout`/`ownership`/`router`, all
  pure/zero-I/O) plus 10 real-loopback-`Peer` integration tests in
  `crates/core/tests/session_end_to_end.rs` (edge crossing + cursor
  warp, screen-size exchange, an unregistered device never becoming a
  target, disconnect/reconnect without restarting the session,
  modifier flush landing as ordinary input on the old target, no
  leakage to an inactive-but-connected peer, rapid back-and-forth
  re-warping, `RecenterLocal` reaching `PointerGeometry`, a transient
  injection failure not killing the session, and local-capture
  suppression toggling correctly) — none depend on physical hardware.
  Real Mac<->Windows hardware QA is underway (see
  `docs/manual-qa/phase-4-edge-switching.md`) and has already driven
  several real-hardware-only fixes not visible from unit tests alone:
  raw-HID-delta vs. screen-point mouse tracking, an unreachable
  OS-clamped edge threshold, self-triggering handoff bounce-back, a
  pinned local cursor after the first switch, a self-captured synthetic
  warp event, undetected `CGEventTap` disablement, a single transient
  injection failure killing the whole target session, a hub that could
  only ever accept one connection, and — the latest, ADR-0009 decision
  9 — local input not being suppressed while `Forwarding`, now fixed
  for macOS and Windows (X11 still carries the older listen-only
  behavior, tracked for Round B). **Phase 4 closed 2026-09-12** on the
  real Mac (hub) -> Windows (join) acceptance at `891a80c` (tag
  `phase-4-done`), by the project owner's decision that this direction
  is the Phase 4 acceptance. Rows that acceptance did not cover — the
  Linux/X11 and Windows-as-hub directions, multi-device, and
  reconnection without a hub restart on hardware — are recorded in the
  manual QA doc as not tested and carried forward, never marked done
  from unit tests alone.

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
| Windows UAC/elevated-window behavior | ✅ PASS — tested under a genuine standard user account (the original account was the exempt built-in Administrator); an unelevated relay correctly failed to inject into an elevated Notepad window, with no crash and no bypass attempted |
| **Control (Windows) → Command (Mac) translation** | ✅ PASS — confirmed with reproducible log evidence (real Ctrl+A on Windows executed as Cmd+A on the Mac) |
| **Command (Mac) → Control (Windows) translation** | ✅ PASS (after the `FlagsChanged` fix) — confirmed with reproducible log evidence (real bare Command press on the Mac captured, translated to `ControlLeft`, and injected on Windows) |
| Option↔Alt never translates | ✅ PASS, both directions |
| End-to-end input latency measured and logged (`examples/input_relay.rs`) | ✅ PASS — network RTT 7.9–15.0ms, injection calls ~70–700µs typical (Mac CGEvent and Windows SendInput both), felt latency acceptable for interactive use |
| Real shortcut combinations (Cmd+A/X/V on Mac → Ctrl+A/X/V on Windows) | ✅ PASS — confirmed on real hardware; each key's own independently-ordered press/release event is sufficient, no "concurrent modifiers" field needed |

### Manual QA record — Phase 4 (real Mac→Windows run 2026-09-12)

Full detail, configuration and run history:
`docs/manual-qa/phase-4-edge-switching.md`. Hub: MacBook Air
(`Mac16,12`), macOS 26.6.2, 1470×956 points. Join: Lenovo laptop,
Windows 10 build 10.0.19045.6466, 1366×768 at 96 DPI. Accepted at
`891a80c`.

| Item | Status |
|---|---|
| Edge switching Mac→Windows and back, at the real edges | ✅ PASS |
| Windows cursor reaches the whole screen (all four corners) | ✅ PASS |
| Proportional crossing between different resolutions | ✅ PASS (3 of 4 exact in the final run; first-crossing anomaly recorded as a follow-up) |
| Mac cursor hidden and still while forwarding; lands at the edge on return | ✅ PASS (one first-switch miss recorded as a follow-up) |
| Keyboard: letters, digits, punctuation, number pad, Caps Lock, Num Lock | ✅ PASS |
| Two-finger scroll speed; three-finger gestures kept off the Mac | ✅ PASS |
| Emergency return chord; stalled-target liveness recovery; disconnect fallback | ✅ PASS |
| Reconnect without hub restart | ⏭ not verified on hardware (automated only) |
| Linux/X11 and Windows-as-hub directions | ⏭ not tested |

See `/Users/gourav/.claude/plans/elegant-wishing-origami.md` for the full
phase breakdown, per-phase Definition of Done, risk register, and QA matrix.
