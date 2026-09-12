# ADR-0007: Input Architecture — Normalized Events, Capture/Inject Boundary, Modifier Translation

## Status

Accepted (2026-09-10)

**Update (2026-09-10, during Phase 3 manual QA):** decision 3 below
describes translation happening "on the injecting side," but the first
implementation only ever *defined* `translate_for_target` — nothing
actually called it. `core::input_bridge::inject_from_peer` and
`examples/input_relay.rs`'s listener loop both injected every `Key`
event exactly as received. Found via code inspection while preparing
the real-hardware modifier-translation test (caught before it wasted a
hardware test cycle, not caught by unit tests, since the pure function
itself was correct and fully covered — the gap was purely in wiring it
into the live path). Fixed by calling `translate_for_target` in both
call sites, using a small per-target `LOCAL_PLATFORM` constant
(`cfg(target_os = "macos")` / `cfg(windows)` / `cfg(unix, not(macos))`);
regression test added at `core`'s layer
(`peer_key_events_are_translated_for_this_builds_platform` in
`crates/core/tests/input_bridge_end_to_end.rs`), since that's the
lowest layer that can actually exercise the live wiring end-to-end.
`MacInject`/`WindowsInject` themselves were correctly left untouched —
translation stays out of the per-OS shims, preserving decision 2's
"provably thin, zero business logic" property.

**Update (2026-09-11, following real Mac<->Windows manual QA):** the
real hardware run confirmed decision 4's `FlagsChanged => None` gap in
practice — a bare Shift/Control/Option/Command press on the Mac
produced nothing at all, which meant Mac->Windows modifier translation
couldn't be exercised in that direction. Implemented rather than left
deferred: `macos/events.rs` now diffs the modifier flags immediately
before and after a `FlagsChanged` event (state carried in a `Cell`
owned by the capture session, reset on every `start()`) against the
category bit for the specific key the event's own keycode identifies,
and reports a press or release only when that bit actually flipped. Two
things are deliberately still out of scope, not silently mishandled:
holding both keys in one modifier category (e.g. both Shift keys) and
releasing one — `CGEventFlags` has one bit per *category*, not per key,
so that specific transition can't be told apart from "nothing changed"
from this event alone, and is reported as no event rather than guessed
at; and Caps Lock, whose flag is a toggle rather than a held-state, a
different enough semantic that it isn't folded into this inference.
Fully unit-tested as a pure function (`modifier_transition`, given
already-extracted flags values, no real `CGEvent` needed) — the actual
`CGEvent` field extraction around it stays the untested "thin shim",
per this ADR's own HAL-split rationale. This closes the "Mac
Command press is never captured" gap.

Initially assumed (incorrectly) that real shortcut *combinations* would
still be broken, on the theory that `InputMessage::Key` carries no
"concurrently held modifiers" field so a letter's own event wouldn't
know Command was held alongside it. Real-hardware retest disproved
this: Cmd+A, Cmd+X, and Cmd+V on the Mac all landed on Windows as
Ctrl+A/X/V — real, functional shortcuts. No protocol change was
needed, because Command and the letter are each captured and injected
as their own independently-ordered press/release events, and the
injecting OS's own keyboard state tracking reconstructs "Ctrl down,
then A down" from that ordered stream exactly as it would from two
fingers on a real keyboard — the same mechanism that already made
mouse-drag-while-a-key-is-held work without any special-casing.

**Update (2026-09-12, Phase 4 real Mac->Windows acceptance run):**
punctuation and numpad keys were broken on the receiving side, and two
separate defects combined to cause it — both measured from the hub
log's `stage="3-session-send"` lines, which record the exact `Key` sent
for every press.

*`Key` had no variants for punctuation or numpad keys at all*, so every
one of them left the Mac as `Key::Unknown(<raw macOS keycode>)`.

*Every injector then injected a foreign `Unknown` code as if it were
its own.* `key_to_vk`'s `Key::Unknown(code) => VIRTUAL_KEY(code)`
reinterprets a macOS keycode as a Windows VK — two unrelated numbering
schemes. From the log: `` ` `` (`Unknown(50)`, pressed 14 times) typed
the digit `2` (VK 0x32); keypad Enter (`Unknown(76)`, 32 times) typed
`L`; keypad 0–7 typed `R`–`Y`; `[` pressed Page Up; `-` pressed Escape;
`'` pressed Right arrow; `/` pressed Print Screen; and **keypad 9
(`Unknown(92)` = VK 0x5C) pressed the Windows key**. Comma, period,
semicolon, `]`, `=` and `\` landed on unassigned VKs and did nothing.
`key_to_keycode` and `key_to_keysym` had the identical flaw in the
other directions. Decision 1 above already said raw codes weren't
portable; nothing enforced it.

**Fix, two parts.** `Key` gains 27 variants — the 11 US-layout
punctuation positions and 16 numpad keys (protocol 1.1, `PROTOCOL_MINOR`
bumped). They are **appended after `Unknown`**: postcard encodes an enum
variant as its declaration index, so inserting them earlier would have
silently renumbered `Unknown` on the wire; `key_wire_indices_are_stable`
now pins those indices. All three tables map them (macOS `kVK_*`,
Windows `VK_OEM_*`/`VK_NUMPAD*`, X11 `XK_*`). And a new pure rule,
`translate::is_injectable`, refuses to inject a `Key::Unknown` on any
platform other than the message's `source_os`; each injector checks it
and returns `InputError::Unsupported` instead of guessing. The decision
itself lives in the OS-independent `translate` module, consistent with
the first update above — the per-OS shims only enforce it. A
same-platform `Unknown` still round-trips exactly, so decision 1's
"never silently dropped" property holds wherever the code actually
means something.

This amends decision 1: a raw code is preserved on the wire as before,
but is only ever injected on the platform that produced it.

Known, documented limits of this change: keys that remain unnamed
(macOS keypad Clear and keypad `=`, the ISO `§` key, F13+, media keys)
are now refused cross-platform — logged, not typed as some unrelated
key. Windows has no distinct VK for numpad Enter (it is `VK_RETURN`
plus the extended-key flag), so it injects as a plain Return and a
Windows-captured numpad Enter reads back as `Key::Enter`. X11 keypad
injection depends on the running server's keymap exposing the `KP_*`
keysyms and is not yet hardware-verified. Regression tests use the
codes from the real log verbatim, at the capture table
(`punctuation_and_numpad_codes_from_the_acceptance_log_are_named_not_unknown`),
the Windows table
(`keys_from_the_acceptance_log_inject_their_own_vk_not_the_mac_numbers`),
and the portability rule
(`a_foreign_raw_key_code_from_the_acceptance_log_is_never_injectable`).

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
