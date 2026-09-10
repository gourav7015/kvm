# Phase 3 Manual QA — Input Engine (macOS ↔ Windows)

Automated tests cover the platform-independent pieces (modifier
translation, keycode round-trips, the capture→inject pipeline via fake
OS backends over a real loopback connection). They cannot cover real OS
permission prompts, real hooks/taps, or how translated modifiers
actually feel — that's what this procedure is for. Do not mark any row
in `docs/architecture.md`'s Phase 3 manual QA table done until it was
actually run on the real machine named.

Uses `crates/core/examples/input_relay.rs`. Build once per machine:

```
cargo build -p kvm-core --example input_relay
```

**Executed 2026-09-10/11** on a real Mac (this session's development
machine, LAN IP `192.168.1.2`) and a real Windows laptop (Lenovo, LAN IP
`192.168.1.7`), both on the same LAN already proven in Phase 1c/2.
Results below are from that run.

## 1. macOS Accessibility-permission-missing path

**PASS.** Before Accessibility was granted to the terminal running the
example, `connect` completed a real QUIC/TLS handshake (`network
round-trip: 14.994541ms`) but capture failed cleanly:

```
capture forwarding failed: Input(PermissionDenied("Accessibility permission not granted for this process — grant it in System Settings > Privacy & Security > Accessibility, then restart"))
```

No panic in the library itself, no false success — `kvm_input`/
`kvm_core::forward_capture_to_peer` returned a proper `Result::Err` all
the way up. (The example script's own `.expect()` on that `Result` then
exits via panic, matching the existing `lan_peer.rs` convention for
these manual scripts — that's the example's top-level handling, not a
defect in the input engine's contract.)

After granting Accessibility (System Settings > Privacy & Security >
Accessibility), a fresh `connect` attempt started capturing successfully
with no error.

- [x] PASS: permission-missing surfaced as a clear error, not a panic or silent no-op in the library
- [x] PASS: after granting permission, injection succeeds

## 2. macOS keyboard/mouse capture and injection (Mac → Mac loopback sanity check)

**Skipped** — went straight to the real Mac↔Windows pair (section 3),
which is a strictly stronger test than Mac↔Mac loopback and is what
Phase 3 actually targets.

## 3. macOS ↔ Windows, both directions

### Round 1 — Mac capture → Windows inject

Verified via `injected {event} in {duration}` lines in the Windows
listener's log, matching real physical key presses on the Mac.

| Category | Result |
|---|---|
| Letters (A–Z sampled: X, C, H) | PASS |
| Digits 0–9 | PASS — all ten captured in order, both edges |
| Enter, Backspace, Escape, Space | PASS |
| Tab | PASS — flagged ambiguous mid-session (unclear during a busy, log-flooded multi-category test, same readability issue the F-key retest below ran into), then confirmed cleanly with a dedicated isolated retest (Tab alone, nothing else) |
| Arrow keys (Up/Down/Left/Right) | PASS |
| Function keys F1–F5 | PASS — isolated retest confirmed `Key::F1`..`Key::F5` exactly; one stray `Unknown(176)` between F4/F5 confirmed to be an incidental press of the `fn`/Globe key, not an F-key mapping error |
| `fn`/Globe key | PASS (correctly `Key::Unknown(code)`) — not in Phase 3's supported key set by design (not one of the keys the DoD lists: letters, digits, modifiers, F1–F12, arrows, common editing keys); an unmapped key becoming `Unknown` rather than being dropped or misrepresented is the intended, documented behavior |
| Delete (forward-delete) | NOT TESTED — this MacBook's keyboard has one physical key (Backspace); forward-delete needs Fn+Delete, not tried |
| Mouse move | PASS |
| Mouse scroll | PASS |
| Left click | PASS |
| Right click | PASS |
| Middle click | PASS (trackpad three-finger/middle-click gesture registered) |
| Shift / Control / Option / Command alone | **FAIL** — see below |
| Unmapped/punctuation keys (e.g. period) | PASS (correctly `Key::Unknown(code)`, not dropped or misrepresented) |

**Bare modifier keys (Shift, Control, Option, Command) produce no event
at all when pressed alone on the Mac.** Root cause confirmed both by
code inspection and by this hardware run: macOS's `CGEventTap` reports a
bare modifier transition as `CGEventType::FlagsChanged`, and
`macos/events.rs` explicitly returns `None` for that event type (a
decision already documented in ADR-0007/the code comment as
"deliberately deferred", not an oversight). Practical impact confirmed
here is broader than just bare taps: since `InputMessage::Key` carries
no "concurrently held modifiers" field, a real Mac-side shortcut like
Cmd+C only ever sends a plain `Key::C` — the Command press itself is
silently absent from the wire. **This is a real, hardware-confirmed
limitation of the current Phase 3 scope**, not fixed during this QA
session per the "no feature expansion during QA" instruction — a proper
fix needs stateful flags-diffing and likely a protocol change, which is
real design/implementation work for a follow-up, not a QA-session patch.

### Round 2 — Windows capture → Mac inject

| Category | Result |
|---|---|
| Letters A–Z (all 26 confirmed) | PASS |
| Digits 0–9 | PASS |
| Enter, Tab, Backspace, Escape, Space | PASS |
| Arrow keys | PASS |
| Function keys F1–F5 | PASS |
| CapsLock | PASS (captured; correctly not translated) |
| Shift (left/right) | PASS (captured; correctly not translated) |
| Alt | PASS (captured; correctly not translated) |
| Windows key (Meta) | PASS (captured; correctly **not** translated — no Mac equivalent slot for the physical Windows key, by design) |
| **Control → Command translation** | **PASS** — see evidence below |
| Mouse move, scroll, left/right/middle click | PASS |

**Modifier translation, real hardware, unambiguous evidence** (after
fixing `input_relay.rs` to log the raw received event alongside the
translated one — the original version only logged post-translation,
making a genuine translated Ctrl and an untranslated Meta print
identically):

```
received Key { key: ControlLeft, state: Pressed, repeat: false, source_os: Windows } -> translated+injected Key { key: MetaLeft, state: Pressed, repeat: false, source_os: Windows } in 72.167333ms
injected Key { key: A, state: Pressed, repeat: false, source_os: Windows } in 33.637125ms
injected Key { key: A, state: Released, repeat: false, source_os: Windows } in 111.5µs
received Key { key: ControlLeft, state: Released, repeat: false, source_os: Windows } -> translated+injected Key { key: MetaLeft, state: Released, repeat: false, source_os: Windows } in 111.625µs
```

A physical Ctrl+A press on the Windows keyboard was captured as
`ControlLeft`, sent over the real QUIC connection, translated to
`MetaLeft`, and injected — executing as Cmd+A on the Mac. This is the
actual killer feature, confirmed working end-to-end on real hardware in
the direction that's actually testable (see Round 1's FlagsChanged note
for why the reverse direction can't currently exercise a bare-modifier
press at all).

**Bug found and fixed during this QA session** (see `docs/adr/0007-input-architecture.md`'s Update note and commit `8266c24`):
`translate_for_target` was fully implemented and unit-tested but never
actually called by `core::input_bridge::inject_from_peer` or this
example's listener loop — the translation logic was fully correct but
completely unwired. Found via code inspection before it could waste a
hardware test cycle confirming the obvious symptom. Fixed at both call
sites; regression test added at `core`'s layer
(`peer_key_events_are_translated_for_this_builds_platform`).

- [x] Letters, digits land correctly, both directions
- [x] **Command (Mac) → Control (Windows)**: not exercisable — Mac never captures a bare Command press (FlagsChanged gap, see above). Confirmed **FAIL** for this specific direction, root cause identified.
- [x] **Control (Windows) → Command (Mac)**: **PASS**, confirmed with reproducible log evidence above
- [x] Option (Mac) / Alt (Windows): PASS, correctly never swapped
- [x] Function keys, arrows, Enter/Escape/Backspace/Tab/Space: PASS both directions
- [x] Mouse move/click/scroll (left/right/middle): PASS both directions

## 4. Windows UAC / elevated-window behavior

**NOT TESTED under the intended conditions.** The Windows terminal used
throughout this session's testing ran elevated ("Administrator: ..." in
the title bar) — an elevated process is not subject to the UIPI
restriction ADR-0007 §5 documents (that restriction only blocks a
*lower*-integrity process from injecting into a *higher*-integrity
window; an elevated injector can target anything). So this test's real
precondition — the relay running unelevated, target window elevated —
was never actually in effect. Needs re-running with a standard
(non-administrator) terminal running the relay, targeting a
"Run as administrator" window, to genuinely exercise this path.

- [ ] OPEN: needs a real run with an unelevated relay process

## 5. End-to-end latency

Real measurements from this session, same Mac↔Windows LAN pair used in
Phase 1c/2:

- Network RTT: **7.9–15.0 ms** across four separate connections (7.9405ms, 9.323083ms, 10.372417ms, 14.994541ms)
- Typical injection-call duration: **macOS injection (CGEvent) ~90–700µs** per event (occasional outliers up to a few ms, and one cold-start outlier at 34.2ms for the very first injected event after a fresh connection); **Windows injection (SendInput) ~70–500µs** per event (one cold-start outlier at 72ms for the first event after a fresh connection)

These two numbers do not sum to a true "time from keypress to visible
effect" measurement — the two machines' clocks aren't synchronized, so
no single cross-machine timestamp delta is computed (see the doc comment
in `input_relay.rs` for why).

- [x] Network RTT and injection-call duration recorded above
- [x] PASS: felt end-to-end latency is acceptable for interactive use — RTT under 15ms plus sub-millisecond injection calls (aside from one-off cold-start outliers) is well within what feels instantaneous for keyboard/mouse control; visually confirmed via the Mac→Windows and Windows→Mac typing/clicking tests above landing with no perceptible lag
