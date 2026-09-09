# ADR-0001: Core Tech Stack

## Status

Accepted (2026-09-09)

## Context

Universal KVM is a cross-platform (macOS/Windows/Linux) software KVM: one
keyboard/mouse controlling multiple computers, plus clipboard sync and file
transfer over the local network. It needs a native desktop app, encrypted
peer-to-peer networking, local device discovery, and low-level OS input
capture/injection on three different platforms. The stack needed to satisfy:
a fast path to a polished management UI (device list, pairing, layout editor,
settings) without hand-building three native UIs; a Rust core for the
performance- and OS-integration-sensitive engines (input, clipboard,
transfer, networking); and security by construction rather than bolted on
later (every connection encrypted and mutually authenticated).

## Decision

- **UI shell: Tauri.** Rust backend + a web-based frontend in a native
  webview, over a pure-Rust GUI (egui/iced) or fully-native-per-OS UI
  (SwiftUI/WinUI/GTK). Tauri lets the Rust core stay pure/OS-integration-
  focused while the dashboard is fast to build in a web frontend; low-level
  OS input hooks still need native Rust bindings regardless of UI choice, so
  Tauri doesn't cost us anything there. Fully-native-per-OS UI would give the
  best native feel but triples UI implementation effort — deferred past MVP.
- **Transport: QUIC via `quinn` + `rustls`.** Multiplexed streams per concern
  (control/input/clipboard/file) over one connection per device pair;
  built-in encryption and stream multiplexing fit the concurrent,
  latency-sensitive nature of input+clipboard+transfer running at once.
- **Device identity: `ed25519-dalek` keypairs**, private key stored via the
  OS keychain (`keyring` crate). Trust model is "exchange + pin public keys"
  on pairing — no certificate authority, since this is a peer-to-peer LAN
  tool with no central server.
- **Discovery: `mdns-sd`** for zero-config LAN discovery, **UDP broadcast**
  as a fallback for networks that block mDNS, and **manual IP entry** as the
  last-resort fallback.
- **Input capture/inject: custom trait-based engine**, not an existing crate.
  No crate on the ecosystem cleanly covers capture *and* inject *and* the
  cross-platform modifier-translation logic (Cmd<->Ctrl, Option<->Alt) that
  is a deliberate differentiator for this product. Per-OS backends: macOS via
  `CGEventTap`/`CGEvent`, Windows via raw input + `SendInput`, Linux via
  `evdev`/X11 initially (Wayland is a known, separately-tracked risk — see
  the Phase 3b spike in the build plan).
- **Clipboard: `arboard`** for cross-platform read/write, plus a per-OS
  change-watcher/poll loop for detecting copy events.
- **Serialization: `postcard`** *(originally `bincode`; superseded by
  [ADR-0002](0002-postcard-not-bincode.md) after `bincode` was flagged
  unmaintained — RUSTSEC-2025-0141)* over a small versioned message framing,
  defined in a dependency-free `protocol` crate (no I/O), so the wire format
  can evolve without breaking already-paired peers.
- **Async runtime: `tokio`**, required by `quinn` in any case.
- **License: `MIT OR Apache-2.0`**, the Rust ecosystem's default dual license.

## Consequences

- The `input` and `clipboard` crates carry the highest platform-specific risk
  and effort; they are structured with a strict trait boundary so the
  OS-specific code stays a thin, manually-verified shim while translation and
  switching logic remains pure, unit-testable Rust (enforced via code
  review, see the build plan).
- No CA/central server means trust-store correctness (revocation, key
  rotation) is security-critical and gets adversarial test coverage from the
  crate that introduces it (`identity`, then `discovery`/pairing).
- Wayland input injection is a real open risk, not yet resolved — tracked
  explicitly rather than assumed away; see the Phase 3b ADR when it lands.
- Building a Tauri app requires a Node/JS toolchain alongside Rust for the
  frontend; this is deferred until the UI is actually needed (see
  `docs/architecture.md` status) rather than scaffolded in Phase 0.
