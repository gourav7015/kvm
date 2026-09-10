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

## 1. macOS Accessibility-permission-missing path

Run on the Mac, **before** granting Accessibility to the terminal/binary
running this example (or after revoking it in System Settings > Privacy
& Security > Accessibility):

```
cargo run -p kvm-core --example input_relay -- listen
```
From another already-permitted machine or a second process, connect and
send at least one input event (or just run `connect` from the Windows
side per step 3 below). Expected: the listener prints an
`InputError::PermissionDenied` message with actionable text — never a
panic, never a silent no-op that looks like it worked.

Then grant Accessibility to the terminal (or the built binary) in
System Settings, re-run, and confirm injection now succeeds.

- [ ] PASS/FAIL: permission-missing surfaced as a clear error, not a panic or silent no-op
- [ ] PASS/FAIL: after granting permission, injection succeeds

## 2. macOS keyboard/mouse capture and injection (Mac → Mac loopback sanity check)

Before testing across machines, sanity-check both roles on one Mac using
two terminals and `listen`/`connect 127.0.0.1:51821`. Type letters,
numbers, held modifiers (Shift/Control/Option/Command, both left and
right where the keyboard has them), function keys, arrows, Enter, Tab,
Backspace, Space in the capturing terminal; move/click/drag/scroll with
the mouse. Watch the listener's log lines (`injected ... in ...`) and
confirm the injected events land in the frontmost app on the listening
side.

- [ ] PASS/FAIL: keyboard events captured and injected correctly
- [ ] PASS/FAIL: mouse events (move/click/drag/scroll) captured and injected correctly

## 3. macOS ↔ Windows, both directions

On the Windows laptop:
```
cargo run -p kvm-core --example input_relay -- listen
```
Note the printed address; find the machine's real LAN IP (`ipconfig`).

On the Mac:
```
cargo run -p kvm-core --example input_relay -- connect <windows-lan-ip>:51821
```
Type/click/scroll on the Mac; watch it land on Windows. Then swap roles
(Windows `connect`s to a Mac `listen`er) and repeat.

For each direction, specifically verify:
- [ ] Letters, digits, and punctuation keys land correctly
- [ ] **Command (Mac) feels like Control (Windows) and vice versa** — the
      actual killer feature. E.g. Cmd+C on the Mac side should inject as
      Ctrl+C on Windows, and Ctrl+C typed on Windows should inject as
      Cmd+C on the Mac. (`translate_for_target`'s unit tests already
      prove the logic table is correct; this step confirms it wasn't
      wired backwards and actually feels native.)
- [ ] Option (Mac) / Alt (Windows) do **not** get swapped (by design —
      they already correspond in role)
- [ ] Function keys (F1–F12), arrows, Home/End/PageUp/PageDown, Enter,
      Escape, Backspace, Delete, Tab, Space all land correctly
- [ ] Mouse move, left/right/middle click, drag, and scroll all land
      correctly
- [ ] Repeat in the opposite direction

## 4. Windows UAC / elevated-window behavior

On the Windows listener, bring an elevated window to the foreground
(e.g. an admin Command Prompt via right-click > "Run as administrator").
With the example running unelevated (the normal case), attempt to inject
keystrokes targeting that window from the Mac side.

Expected (per ADR-0007 §5, a documented Windows platform limitation, not
a bug in this code): the elevated window does not receive the injected
input — `SendInput` cannot cross integrity levels upward. Confirm this
is what actually happens (not, e.g., a crash or an error that looks like
something else), and that the listener process itself keeps running.

- [ ] PASS/FAIL: elevated foreground window silently doesn't receive input, as expected; the relay process itself doesn't crash or hang

## 5. End-to-end latency

`input_relay` logs a network round-trip time (Ping/Pong over the control
stream) at connection start, and the listener logs each injection call's
own duration. Record both from a real Mac↔Windows run:

- Network RTT: ______ ms
- Typical injection-call duration: ______ ms

These two numbers do not sum to a true "time from keypress to visible
effect" measurement — the two machines' clocks aren't synchronized, so
no single cross-machine timestamp delta is computed (see the doc comment
in `input_relay.rs` for why). Separately, judge felt latency directly:
type/move the mouse on the source machine while watching the target
machine's screen.

- [ ] Network RTT and injection-call duration recorded above
- [ ] PASS/FAIL: felt end-to-end latency is acceptable for interactive use (no obvious lag)
