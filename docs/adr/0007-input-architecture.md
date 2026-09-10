# ADR-0007: Input Architecture — Normalized Events, Capture/Inject Boundary, Modifier Translation

## Status

Accepted (2026-09-10)

## Context

Phase 3 needed a platform-independent input representation plus per-OS
capture/injection, with the explicit "killer feature" of translating
modifier roles across platforms (macOS Command ↔ Windows Control, etc.)
so shortcuts feel native on whichever machine is currently receiving
input. Several concrete design points needed settling before writing any
OS-specific code.

## Decisions

### 1. `protocol::InputMessage::Key` carries a normalized `Key`, not a raw platform keycode

Phase 1a's original `InputMessage::Key` carried `keycode: u32` — a
deliberately raw, OS-specific value, on the theory that translation was
entirely `input`/`core`'s job. Revisited here: a raw keycode is
meaningless without knowing which OS produced it, and the DoD's own
framing ("do not put OS-specific key codes directly into the portable
protocol unless there is a strong documented reason") argues for
normalizing *before* the wire, not after.

`protocol` now defines `Key` — a physical/logical key enum (letters,
digits, named modifiers with explicit `Left`/`Right` variants, function
keys, navigation, common editing keys) and `PlatformKind` (`MacOs` /
`Windows` / `Linux`). Every `InputMessage::Key` carries both the
normalized `key` and a `source_os: PlatformKind` tag. An unrecognized
platform key becomes `Key::Unknown(raw_code)` — preserved, not dropped or
silently misrepresented as some other key (the DoD explicitly asks that
unsupported keys be documented, not silently wrong).

Each device's `input` crate owns two independent translation tables, both
per-OS and both "thin shim" territory (event-shape conversion, zero
business logic):
- **Capture side:** raw platform key code → `Key` (and back, for inject).
- Nothing about *cross-platform* modifier meaning lives here — that's
  decision 3 below.

### 2. `Capture`/`Inject` are plain synchronous traits, not async

OS input APis are inherently callback/event-loop shaped (macOS
`CGEventTap` delivers events to a `CFRunLoop`; Windows low-level hooks
deliver to a `GetMessage` pump), not `Future`-shaped. Forcing them into
`async fn` would mean fighting the OS's own concurrency model for no
benefit. Instead:

```rust
pub trait Capture: Send {
    fn start(&mut self, sink: mpsc::Sender<InputMessage>) -> Result<(), InputError>;
    fn stop(&mut self);
}
pub trait Inject: Send {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError>;
}
```

Each backend runs its OS event loop on a dedicated `std::thread`,
forwarding normalized events into a `std::sync::mpsc` channel. Bridging
that into the async world (reading the channel, writing to
`Peer::streams.input`) is a small amount of glue, not a reason to make
the OS-facing trait itself async — matching the "thin shim, provably no
business logic" requirement, since an async trait would have pulled
tokio-specific concerns into what should be a pure OS boundary.

### 3. Modifier-role translation happens on the *injecting* side, using the message's `source_os` and the local (compile-time-known) target OS

This is the actual killer-feature logic, and it's pure: given
`(key, source_os, target_os)`, decide what to inject. Concretely,
`input::translate::translate_for_target` is a free function with no OS
dependency at all — fully unit-testable with table-driven tests and no
backend required, which is exactly what the DoD's "modifier-translation
table-driven unit tests (no OS backend needed)" asks for.

Doing this on the *receiving* side (rather than pre-translating before
sending) means the wire always carries what was actually pressed,
unmodified — useful for diagnostics, and avoids needing the sender to
know the receiver's OS at capture time. Since every message already
carries `source_os`, and the injecting device always knows its own OS via
`cfg!(target_os = ...)`, no additional handshake/capability-exchange
message is needed to make this work — deliberately avoiding scope creep
into protocol negotiation machinery for Phase 3.

### 4. macOS: `CGEventTap` (capture) + `CGEvent::post` (inject), permission checked via `AXIsProcessTrusted`

Verified against the actual `core-graphics`/`core-foundation` crate
source (not just docs, which were incomplete) before writing any FFI:
`CGEventTap::new` registers a callback on a `CFMachPort`; its run-loop
source is added to a `CFRunLoop` running on a dedicated thread. Injection
uses `CGEventSource` + `CGEvent::new_keyboard_event`/`new_mouse_event`/
`new_scroll_event`, posted via `CGEvent::post`.

Accessibility permission (`AXIsProcessTrusted`, bound directly via a
minimal `extern "C"` declaration against `ApplicationServices.framework`
— no dedicated crate needed for one function) is checked *before*
attempting to install the tap. A missing grant surfaces as
`InputError::PermissionDenied`, never a panic, never a silent no-op that
looks like it worked — this is a named DoD item, not an afterthought.

### 5. Windows: low-level hooks (`WH_KEYBOARD_LL`/`WH_MOUSE_LL`) for capture, `SendInput` for injection

Verified against the actual generated `windows` crate 0.62.2 source
(same discipline as macOS, and more important here since this environment
has no Windows machine to compile-test against directly — verified by
extracting the crate and reading the real bindings, then cross-checked
with `cargo check --target x86_64-pc-windows-msvc`, which type-checks
without needing a Windows linker). `SetWindowsHookExW` installs a global
hook whose callback is a bare `unsafe extern "system" fn` — it cannot
capture closure state, so the capture channel's sender is smuggled
through via a process-wide `Mutex<Option<Sender<..>>>` (a plain,
const-initialized `Mutex` rather than `OnceLock`, so capture can be
stopped and started again within one process — `OnceLock` would only
ever accept the first sender). All `unsafe` is confined to
`input::windows`, each block carries a comment naming the specific FFI
contract it's upholding (valid pointer from the OS, correct struct
layout, etc.).

No UAC elevation is requested by this code. Consequence (documented, not
silently discovered later): `SendInput` cannot inject into a window
running at a higher integrity level than the injecting process (e.g. a
UAC-elevated app). If Universal KVM runs unelevated and the focused
window on the target machine is elevated, injected input will be
silently ignored by Windows itself — a known Windows platform limitation,
not a bug in this code. Documented in the manual QA procedure so it's a
known, expected condition rather than a confusing report later.

### 6. Linux input is deferred to Phase 3b, not stubbed here

No Linux backend module exists yet. The crate still compiles cleanly on
Linux (`ubuntu-latest` CI) because the platform-independent trait
definitions, `Key`/`PlatformKind`, and the translation logic have zero
platform dependency — only the `macos`/`windows` backend modules are
`cfg`-gated to their respective OSes. Phase 3b's own DoD already commits
to a dedicated X11-vs-Wayland go/no-go ADR before any Linux capture code
is written; duplicating that investigation here would be premature.

### 7. Input only ever flows over an already-authenticated `Peer`

No new networking or trust mechanism was introduced. Input messages ride
the existing `input` stream on a `net::Peer` (from Phase 1c's stream
multiplexing) exactly like `Control`/`Clipboard`/`Transfer` do — a `Peer`
only exists after a successful (non-pairing-mode) `connect`/`accept`,
which already enforces the full trust-store/TLS model from ADR-0003/
ADR-0004/ADR-0005. There is no separate "send input to anyone discovered"
path; discovery (Phase 2) still only produces addresses to *attempt*
pairing with, never a channel that bypasses trust.

## Consequences

- `protocol::InputMessage::Key`'s shape changed (this is a pre-1.0,
  nothing-deployed wire format — no compatibility concern, unlike a
  golden-fixture-protected message like `Handshake::Hello`).
- `core` gains a small `input_bridge` module (sender/receiver async loops
  bridging a `Capture`/`Inject` to a `Peer`) rather than a full device-list/
  switching system — that orchestration is explicitly Phase 4's job
  (edge-based switching + layout), not built prematurely here.
- Manual QA (real keyboard/mouse capture and injection, Accessibility
  permission prompts, UAC behavior) cannot run in CI and is tracked
  exactly like every other real-hardware gate this project has closed so
  far — open until actually performed on the real Mac and Windows
  machines, not assumed from a green build.
