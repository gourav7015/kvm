# Phase 3b Manual QA — Linux Input Engine (X11)

Status when this was written: the X11 backend (`crates/input/src/x11`)
compiles and passes `clippy -D warnings` when cross-checked from macOS
via `cargo check --target x86_64-unknown-linux-gnu` /
`cargo clippy --target x86_64-unknown-linux-gnu`, and its pure-logic
unit tests (`x11::keysym`, `x11::keymap`, `x11::events`) compile as
test binaries for that target. **None of this has been run against a
real X server yet** — this development machine has no Linux display
server to test against directly (see ADR-0008). Every row below is
OPEN until performed on real Linux hardware.

No Wayland backend exists — ADR-0008 records a formal NO-GO for
general-purpose capture under Wayland's current security model, so
there is nothing to manually test there for now.

## 0. Prerequisites

On the Linux test machine:
```
git pull
cargo build -p kvm-input
cargo test -p kvm-input
```
Confirm `$XDG_SESSION_TYPE` is `x11` (not `wayland`) — `echo
$XDG_SESSION_TYPE`. This backend will not work under a Wayland session
even if X11 libraries are technically present, since it needs a real
X11 *display server* to connect to, not just the client libraries.

The XTEST extension must be enabled on the X server (it is by default
on essentially every distribution's X.org; `xdpyinfo | grep XTEST`
confirms).

## 1. Automated tests, run for real (not just cross-compiled)

```
cargo test -p kvm-input
```
This exercises `x11::keysym`'s round-trip tests, `x11::keymap`'s
synthetic-table tests, and `x11::events`' pure event-conversion tests —
all of which run identically to the macOS/Windows suites since none of
them need a live X connection. This is the one thing on this list that
*is* meaningfully verified by cross-compilation already (the code
compiles and type-checks identically); running it for real just
confirms no target-specific runtime surprise (e.g. an integer-width
assumption that happens to hold on `x86_64-unknown-linux-gnu` but
wasn't actually exercised).

- [ ] PASS/FAIL: `cargo test -p kvm-input` passes with the same results as the macOS run

## 2. X11 capture — keyboard

Using `examples/input_relay.rs` (once it gains an X11 `LocalCapture`/
`LocalInject` selection — not yet wired up; until then, a small
throwaway test binary calling `X11Capture`/`X11Inject` directly is the
way to exercise this by hand) or a dedicated test binary:

- [ ] Letters, digits land correctly
- [ ] Enter, Tab, Backspace, Escape, Space
- [ ] Arrow keys
- [ ] Function keys F1–F12
- [ ] Bare Shift/Control/Alt/Super pressed alone — X11 should **not**
      have the macOS `FlagsChanged`-style gap (XI2 reports modifier
      keys as ordinary `RawKeyPress`/`RawKeyRelease` like any other
      key), so this should work out of the box; confirm it actually
      does
- [ ] Left/right modifier variants distinguished correctly (`ShiftLeft`
      vs `ShiftRight`, etc.)
- [ ] Key-repeat flag (`KeyEventFlags::KEY_REPEAT`) correctly reflects
      held-key auto-repeat

## 3. X11 capture — mouse

- [ ] Mouse move (relative deltas land correctly, not inverted or scaled wrong)
- [ ] Left/middle/right click
- [ ] Scroll up/down/left/right (button 4/5/6/7 convention)

## 4. X11 injection — keyboard and mouse

Same categories as above, this time verifying `X11Inject` actually
lands the event in a focused window (a text editor for keyboard, any
window for mouse).

- [ ] Keyboard categories from §2, injected correctly
- [ ] Mouse categories from §3, injected correctly

## 5. Cross-machine: X11 ↔ macOS/Windows

Once §2–4 pass locally, repeat the real cross-machine test Phase 3 did
for macOS↔Windows, this time Linux↔macOS and/or Linux↔Windows:

- [ ] Linux capture → macOS/Windows inject, keyboard + mouse
- [ ] macOS/Windows capture → Linux inject, keyboard + mouse
- [ ] Modifier translation: Linux's Control is treated identically to
      Windows' Control by `translate_for_target` (Linux↔macOS should
      translate exactly like Windows↔macOS already does; Linux↔Windows
      should never translate, both already agree Control is primary)

## 6. Known, documented gaps — not bugs to chase, just confirm they behave as expected

- [ ] Keyboard layout switched mid-session: confirm the keymap goes
      stale as documented (ADR-0008 decision 1) rather than crashing or
      silently corrupting input — restart should pick up the new layout
- [ ] X11's permissive security model: unlike Windows' UAC test,
      confirm injected input *does* reach a window owned by a different
      user-privilege process if you have one to test against (e.g. a
      `sudo`'d application) — this is *expected*, not a bug, per
      ADR-0008 decision 1; X11 has no equivalent restriction to Windows'
      UIPI, and this code doesn't add one

## 7. Environment actually used for this QA

Fill in once run:
- Distribution + version: ______
- Desktop environment: ______
- `$XDG_SESSION_TYPE`: ______
- X server: ______
- Date: ______
