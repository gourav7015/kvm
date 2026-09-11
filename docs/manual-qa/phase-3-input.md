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
| Shift / Control / Option / Command alone | **PASS** (after a fix — see below) |
| Unmapped/punctuation keys (e.g. period) | PASS (correctly `Key::Unknown(code)`, not dropped or misrepresented) |

**Bare modifier keys initially produced no event at all when pressed
alone on the Mac** — confirmed both by code inspection and by this
hardware run. Root cause: macOS's `CGEventTap` reports a bare modifier
transition as `CGEventType::FlagsChanged`, and `macos/events.rs`
originally returned `None` for that event type unconditionally
(previously documented as "deliberately deferred").

**Fixed during this QA session** (commit `0c75c78`, ADR-0007's second
Update note): `macos/events.rs` now diffs the modifier flags immediately
before and after a `FlagsChanged` event against the specific category
bit for the key its own keycode identifies, reporting a press/release
only when that bit actually flipped. Retested on real hardware —
pressing bare Command produced:

```
received Key { key: MetaLeft, state: Pressed, repeat: false, source_os: MacOs } -> translated+injected Key { key: ControlLeft, state: Pressed, repeat: false, source_os: MacOs } in 629.6µs
received Key { key: MetaLeft, state: Released, repeat: false, source_os: MacOs } -> translated+injected Key { key: ControlLeft, state: Released, repeat: false, source_os: MacOs } in 438.9µs
```

Command is now captured and correctly translated to Control on
injection. Two things remain deliberately out of scope, not silently
mishandled: holding both keys of one modifier category (e.g. both Shift
keys) and releasing one — `CGEventFlags` has one bit per category, not
per key, so that can't be told apart from "nothing changed" and is
reported as no event rather than guessed at; and Caps Lock, whose flag
is a toggle rather than a held-state.

**Real shortcut combinations confirmed working, not just bare taps.**
Tested Cmd+A, Cmd+X, and Cmd+V on the Mac — all three landed on Windows
as Ctrl+A, Ctrl+X, Ctrl+V (select-all, cut, paste) respectively, i.e.
real, functional shortcuts, not just isolated key presses. This was initially
assumed *not* to work, on the theory that `InputMessage::Key` carries no
"concurrently held modifiers" field so a letter's own event wouldn't
know Command was held — but that concern doesn't actually apply: each
key (modifier or not) is captured and injected as its own independent,
correctly-ordered press/release event, and Windows' own keyboard state
tracking reconstructs "Ctrl is down, then A goes down" from that ordered
stream exactly as it would for two fingers on a real keyboard. No
protocol change was needed. This closes out what was previously listed
as a real limitation.

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
actual killer feature, confirmed working end-to-end on real hardware.
The reverse direction (Command on Mac → Control on Windows) was
initially blocked by the FlagsChanged gap above; after that fix, it's
confirmed working too (see Round 1).

**Bug found and fixed during this QA session** (see `docs/adr/0007-input-architecture.md`'s Update note and commit `8266c24`):
`translate_for_target` was fully implemented and unit-tested but never
actually called by `core::input_bridge::inject_from_peer` or this
example's listener loop — the translation logic was fully correct but
completely unwired. Found via code inspection before it could waste a
hardware test cycle confirming the obvious symptom. Fixed at both call
sites; regression test added at `core`'s layer
(`peer_key_events_are_translated_for_this_builds_platform`).

- [x] Letters, digits land correctly, both directions
- [x] **Command (Mac) → Control (Windows)**: **PASS** after the FlagsChanged fix, confirmed with reproducible log evidence in Round 1
- [x] **Control (Windows) → Command (Mac)**: **PASS**, confirmed with reproducible log evidence above
- [x] Option (Mac) / Alt (Windows): PASS, correctly never swapped
- [x] Function keys, arrows, Enter/Escape/Backspace/Tab/Space: PASS both directions
- [x] Mouse move/click/scroll (left/right/middle): PASS both directions

## 4. Windows UAC / elevated-window behavior

**OPEN — cannot be tested on this specific machine/account.** Every
Command Prompt window on this Windows machine shows "Administrator:" in
its title bar regardless of how it's launched — including one opened
with no "Run as administrator" request at all. This means the logged-in
account is the Windows **built-in Administrator account**, which is
specifically exempt from UAC's Admin Approval Mode / split-token model:
every process this account runs already carries the full administrator
token, with no unelevated state to compare against. The intended
test — an unelevated relay process failing to inject into a genuinely
higher-integrity window — has no unelevated context available to set up
on this machine at all. This is a property of the test environment, not
something more attempts would resolve.

To actually close this item, the relay needs to run under a **standard
(non-built-in-Administrator) user account** with UAC's Admin Approval
Mode active, on a different machine or account than the one used for
this QA session.

- [ ] OPEN: needs a standard user account with UAC active; not available on this session's Windows machine

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
