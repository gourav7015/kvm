# Phase 3b Manual QA — Linux Input Engine (X11)

**Executed 2026-09-11** on a real Ubuntu 24.04.4 LTS machine (GNOME,
X11 session, `XDG_SESSION_TYPE=x11`, `XDG_CURRENT_DESKTOP=ubuntu-xorg`,
x86_64), paired with the same Mac used throughout Phase 3, over the LAN
already proven in Phase 1c/2/3. Results below are from that run.

No Wayland backend exists — ADR-0008 records a formal NO-GO for
general-purpose capture under Wayland's current security model, so
there is nothing to manually test there.

## 0. Prerequisites — dependency/environment verification

No extra system packages were needed. `x11rb`'s `RustConnection`
implements Xauthority parsing in pure Rust (`x11rb_protocol::xauth`),
so there's no linkage against `libX11`/`libXtst`/`libXi` at all — just
Rust/cargo (already present) and the already-running X11 desktop
session.

```
git pull
cargo build -p kvm-input
cargo test -p kvm-input
cargo build -p kvm-core --example input_relay
```

- Real Linux hardware: Ubuntu 24.04.4 LTS, GNOME, X11 (`ubuntu-xorg`
  session), x86_64
- Required apt packages: **none** beyond the stock desktop install
- Build command: `cargo build -p kvm-core --example input_relay`
- Run command: `cargo run -p kvm-core --example input_relay -- listen`
  / `-- connect <ip>:51821`

## 1. Automated tests, run for real

```
cargo test -p kvm-input
```
**PASS.** `test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured;
0 filtered out` — `translate` (11), `x11::events` (12), `x11::keymap`
(3), `x11::keysym` (6). Matches the cross-compiled count exactly (the
`macos` module is absent on Linux, as expected).

## 2/3/4/5. Real cross-machine QA: Linux X11 ↔ macOS

### Round A — Linux captures, Mac injects

| Category | Result |
|---|---|
| Letters, digits | PASS |
| Enter, Tab, Backspace, Escape, Space | PASS |
| Arrow keys | PASS |
| Function keys F1–F5 | PASS |
| Bare Shift, Alt, Caps Lock, Super/Meta | PASS — all captured correctly (X11 has no `FlagsChanged`-style gap; modifier keys arrive as ordinary `RawKeyPress`/`RawKeyRelease`), and correctly **not** translated |
| **Bare Control → Command translation** | **PASS** — confirmed live: `received ControlLeft -> translated+injected MetaLeft` |
| Function keys F6–F12 | NOT TESTED |
| Key-repeat flag on real auto-repeat | NOT TESTED (no key was held long enough to trigger OS auto-repeat during this session) |
| Mouse move | PASS |
| Left/right/middle click | PASS |
| Scroll up/down | PASS |
| Scroll left/right (horizontal) | NOT TESTED |

**Real bug found and fixed during this round** (see ADR-0008's second
Update note, commit `0e550fd`): every captured event was initially
delivered and injected **twice**, consistently, across every key and
mouse button tested. Root cause: `xi_select_events` selected raw events
on `XIAllDevices`, a documented XInput2 pitfall (it additionally
matches each underlying slave device a master is paired with). Fixed
by switching to `XIAllMasterDevices`. Retested after the fix on the
same hardware: zero duplicates across a full retest of the same
category list above.

### Round B — Mac captures, Linux injects

| Category | Result |
|---|---|
| Letters, digits (`hello123`) | PASS — confirmed via real terminal echo on Ubuntu |
| Enter, Tab, Backspace | PASS |
| Escape | PASS — real `ESC` (`^[`) landed |
| Space | PASS |
| Arrow keys | PASS — real cursor-movement ANSI codes (`^[[A`/`^[[B`/`^[[C`/`^[[D`) landed |
| Function keys F1–F5 | PASS — real xterm function-key codes (`^[OP`, `^[OQ`, `^[OR`, `^[OS`, `^[[15~`) landed |
| Mouse move | PASS |
| Left/right/middle click | PASS |
| Scroll up/down | PASS |
| Bare Shift | PASS — captured, correctly not translated |
| Bare Control (Mac's own) | PASS — captured, correctly **not** translated (matches `mac_control_key_itself_is_not_remapped`) |
| Bare Option/Alt | PASS — captured, correctly not translated |
| **Bare Command → Control translation** | **PASS** — confirmed live: `received MetaLeft -> translated+injected ControlLeft` |
| Shift+letter combo (real capital output) | **PASS** — Shift+A on the Mac produced a real capital `A` on Ubuntu, confirming combo shortcuts work the same way already proven for Mac↔Windows (each key injected as its own correctly-ordered event; no protocol change needed) |
| **Real interrupt signal propagation** | **PASS** — Control+C (via Command+C→Control+C translation) injected from the Mac genuinely interrupted a running `sleep 100` process in a focused Ubuntu terminal. Real, functional, strongest form of evidence for this capability. |
| Bare Caps Lock | **Not captured — expected, not a new bug.** Same pre-existing, documented Phase 3 limitation (ADR-0007): Caps Lock's flag is a toggle, not a held-state, and was deliberately excluded from the `FlagsChanged`-diffing fix. This is the macOS *capture* side's known gap, independent of the Linux backend. |

## 6. Local-input preservation and disconnect/reconnect (Matrix C/D)

- **Local input preservation**: **PASS** — confirmed the Ubuntu
  machine's own keyboard/mouse kept working normally throughout every
  round of testing; capture never blocked or stole local input (no
  exclusive grab is used, per ADR-0008 decision 1's design).
- **Listener termination / peer reconnect**: **PASS** — exercised many
  times across this session (killed and restarted both the listener
  and connector repeatedly); each fresh `listen`/`connect` cycle
  reconnected cleanly.
- **Modifier held during an abrupt disconnect**: **PASS** — held Shift
  down on the Mac, force-killed the Mac-side connector process
  mid-hold (simulating an abrupt disconnect while a modifier was
  logically "down" on the injecting side), then typed a lowercase
  letter directly on the Ubuntu keyboard with **no** further Shift
  interaction. A real lowercase `a` appeared — Shift did not get stuck
  in a held state on the X server after the connection died.

## 7. Known, documented gaps — confirmed as expected, not bugs

- Keyboard layout switched mid-session: **NOT TESTED** this round.
- X11's permissive security model (injected input reaching a
  different-privilege window, unlike Windows' UAC restriction): **NOT
  TESTED** with an actual privilege-separated target this round — this
  is an inherent, well-understood X11 property (ADR-0008 decision 1),
  not something this code could change either way, so it wasn't
  treated as blocking.

## 8. Security verification

- Input still only flows over an already-authenticated `net::Peer` —
  unchanged by the Linux backend; `X11Capture`/`X11Inject` sit behind
  the same `Capture`/`Inject` trait boundary every other backend uses,
  with no new networking or trust path.
- No new unauthenticated input path was introduced — confirmed by code
  review, not just this session's live testing (the X11 backend has no
  code path that bypasses `core::input_bridge`/`net::Peer`).
- X11's own permissiveness (any X client can observe/synthesize input
  for any other client) is a documented, inherent OS-level property
  (ADR-0008), not something this code opts into or could opt out of.
