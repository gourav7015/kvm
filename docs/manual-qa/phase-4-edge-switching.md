# Phase 4 Manual QA — Edge-Based Switching

**Status: Mac (hub) → Windows (join) accepted on real hardware,
2026-09-12, at commit `891a80c`.** Sections 2–4 (Linux, and the reversed
roles with Windows as the hub) were not part of this acceptance and are
recorded as not tested; see §8 for the follow-ups this run left.

Automated coverage at `891a80c`: `cargo test --workspace` — 227 passed,
1 ignored; clippy `-D warnings` on macOS plus the Windows and Linux
targets for `kvm-input`/`kvm-protocol`; `fmt --check`; `cargo deny
check`. Those tests cover the routing/security/modifier-safety state
machine with fakes standing in for `Capture`/`Inject`/`PointerGeometry`
and real loopback QUIC peers. They cannot cover real cursor warping,
edge-crossing feel, or what appears on two physical screens — that's
what this procedure is for. **A row is marked PASS only if it was
actually run on the named hardware.**

Uses `crates/core/examples/edge_switch_relay.rs`. Build once per
machine:

```
cargo build -p kvm-core --example edge_switch_relay
```

On the machine whose keyboard/mouse should stay physically in use (the
**hub**):

```
cargo run -p kvm-core --example edge_switch_relay -- hub --edge right
```

On the other machine (the **join**):

```
cargo run -p kvm-core --example edge_switch_relay -- join <hub-lan-ip>:51823
```

`--edge` is which edge of the hub's screen the join machine sits
across (`left`/`right`/`top`/`bottom`, default `right`); the hub also
configures the reverse edge automatically so ownership can cross back.
On macOS, the hub role needs Accessibility permission granted to the
terminal/binary running the example (same requirement as Phase 3). On
Windows, run the join from PowerShell; clicking inside its window
starts a QuickEdit selection that pauses it (see §5 — the hub now
recovers by itself).

## Tested configuration

| | Hub | Join |
|---|---|---|
| Machine | MacBook Air (model `Mac16,12`), built-in trackpad + external PC keyboard with a number pad | Lenovo laptop |
| OS | macOS 26.6.2 (build 25G83) | Windows 10, build 10.0.19045.6466 |
| Display | 2560×1664 Retina, reported as **1470×956** points | **1366×768**, 96 DPI (100% scaling) |
| Toolchain | Rust 1.98.1 | Rust stable (version not recorded) |
| Role | `hub --edge right` at 192.168.1.2:51823 | `join 192.168.1.2:51823` |

Same LAN, QUIC transport, protocol 1.3 at acceptance. Logs: hub
`~/kvm-accept-mac-*.log` (UTC timestamps; local time is UTC+5:30), join
`kvm-accept-win-*.log` on the Windows machine.

## Acceptance history (2026-09-12)

Each hardware run exposed at most a few defects; each was traced to a
stage with log evidence before being fixed (ADR-0007 / ADR-0009 hold the
detail).

| Build | Result on hardware | Fixed in |
|---|---|---|
| `ca59d93` | Pointer range fixed, but every switch bounced straight back (~8 ms): the recentre warp leaked into the next real event's delta | `35c8495` (ADR-0009 d.18) |
| `35c8495` | **Windows cursor reaches all four corners** (left x=5, top y=11, bottom y=767 of 1366×768); 7/7 switches, each with exactly one re-anchored correction. Punctuation and number-pad keys sent as raw Mac codes (e.g. `` ` `` typed "2", keypad 9 pressed the Windows key) | `c605a80` (ADR-0007) |
| `c605a80` | Keys correct. Clicking into the Windows PowerShell window (QuickEdit) stalled the target while the connection stayed up; the hub kept its own input suppressed | `0040eea` (d.19) |
| `0040eea` | Liveness check gave local input back by itself (`silent_for=2.09s`). The Mac arrow visibly moved while forwarding although both position readings stayed frozen | `dfdcf03` (d.20) |
| `dfdcf03` | Mac cursor hidden while forwarding; emergency chord works; stalled-target replay removed. Found: early edge switching, Caps/Num Lock not sent, gestures acting on the Mac | `76217ff` (d.21) |
| `76217ff` | Early switching fixed, Caps Lock sent. Found: Mac cursor reappeared at screen centre on return; Num Lock inert | `c3cb1d4` (d.22) |
| `c3cb1d4` | Return landing works. Found: gesture re-showed the cursor (`visible=true` while forwarding); Caps Lock reversed; Num Lock inert on the number pad | `e18265a` (d.23, ADR-0007) |
| `e18265a` | Gestures, Caps Lock, Num Lock, and gestures on the Mac itself all PASS. Found: two-finger scroll ~60× too slow (Mac sent lines, Windows read 1/120 notches) | `891a80c` (ADR-0007) |
| **`891a80c`** | **Scroll PASS** (402 scroll events, mean \|dy\| 115 in the new unit) and **full round trip PASS** — final acceptance | — |

## 0. Prerequisites

- [x] Real Mac available, LAN-reachable
- [x] Real Windows machine available, LAN-reachable
- [ ] Real Linux/X11 machine — **not available** for this acceptance
- [x] `cargo build -p kvm-core --example edge_switch_relay` succeeds on
      the Mac and the Windows machine

## 1. Mac (hub) → Windows (join)

| Check | Result |
|---|---|
| Hub prints its own screen size and starting cursor position | ✅ PASS — 1470×956 |
| Hub prints the peer's (Windows) screen size after connecting | ✅ PASS — 1366×768 |
| Moving the mouse to the configured edge on the Mac switches ownership | ✅ PASS — final run: 4/4 switches, 4/4 returns; switching happens at the real right edge (early switching fixed in `76217ff`) |
| Cursor appears on the Windows screen at the proportionally-correct position along the shared edge | ✅ PASS, with one exception — final run: 3 of 4 crossings within ±0.002 of the expected 768/956 = 0.803 ratio; the first crossing after the hub started landed 63 px low (follow-up F2) |
| Mouse movement after the switch moves the Windows cursor across the **whole** screen | ✅ PASS — all four corners reached; 1,471 injected moves in a Windows log sample, 0 mismatched |
| The Mac's own cursor stays hidden and still while forwarding | ✅ PASS — hidden at switch, shown on return; one first-switch miss in the final run (follow-up F1) |
| Keyboard input lands on Windows, with modifier translation correct (accepted under the Phase 3 Cmd↔Ctrl rule; replaced 2026-09-13 by the positional rule — Option↔Windows key, Command↔Alt, Control unchanged — not yet re-verified, ADR-0007 decision 3) | ✅ PASS — letters, digits, punctuation (`` ` - = [ ] \ ; ' , . / `` and shifted forms), number pad and numpad Enter, verified in Notepad |
| Caps Lock matches between the machines | ✅ PASS — synced as state (`e18265a`): capitals when on, lowercase when off, including after toggling it on the Mac first |
| Num Lock switches the Windows number pad between digits and navigation | ✅ PASS (`e18265a`); the external keyboard's LED is driven by the Mac and never shows Windows' state — expected |
| Two-finger scroll on Windows at a natural speed | ✅ PASS (`891a80c`) |
| Three-finger gestures do not act on the Mac while forwarding, and still work on the Mac otherwise | ✅ PASS (`e18265a`) |
| Moving back across the opposite edge on Windows returns ownership to the Mac, cursor landing at the Mac's right edge at the matching height | ✅ PASS (landing since `c3cb1d4`) |
| A modifier held at the moment of switching away is not stuck on Windows afterward | ◐ Edge-switch path **not run** on hardware (automated: `switching_to_a_new_target_flushes_held_modifiers_on_the_old_one_as_ordinary_input`). The same release mechanism ran on hardware via the emergency chord: Control, Option and Command were released on Windows |
| Rapid repeated crossing back and forth does not desync, freeze, or crash either side | ✅ PASS — 22 switches in one session (`dfdcf03` run) without desync or crash |
| Different screen resolutions: the crossing point lands proportionally, not pixel-for-pixel | ✅ PASS — see the ratio row above |
| Emergency return (Control+Option+Command+Esc) gives local input back instantly | ✅ PASS |

## 2. Mac (hub) ↔ Linux/X11 (join)

Linux code for both roles is complete as of ADR-0009 decision 24. Run
on the Ubuntu 24.04 machine **in an X11 session** (at the login screen,
pick "Ubuntu on Xorg"; `echo $XDG_SESSION_TYPE` must print `x11`):

```
git pull origin main
RUST_LOG=kvm_core=debug,kvm_input=info cargo run -p kvm-core --example edge_switch_relay -- join <mac-lan-ip>:51823 2>&1 | tee ~/kvm-accept-linux-join.log
```

Check the `stage="4-x11-inject"` lines: `matched=true` away from the
screen edges confirms moves land exactly where the Mac expects.

| Check | Result |
|---|---|
| Same checks as section 1, repeated against the Linux machine | ⏭ NOT TESTED yet — code complete (decision 24) |

## 3. Windows (hub) ↔ Mac (join)

| Check | Result |
|---|---|
| Same checks as section 1, with roles reversed | ⏭ NOT TESTED — outside this acceptance's scope (follow-up F4) |

## 4. Linux/X11 (hub) ↔ Mac (join)

Linux hub (X11 session, as above):

```
RUST_LOG=kvm_core=debug,kvm_input=debug cargo run -p kvm-core --example edge_switch_relay -- hub --edge right 2>&1 | tee ~/kvm-accept-linux-hub.log
```

Mac join: `cargo run -p kvm-core --example edge_switch_relay -- join <linux-lan-ip>:51823`.
While forwarding, the hub logs `local input grabbed`, its own arrow
disappears and its apps receive no input; on return it logs `local
input grabs released`. If the Linux screen is ever stuck grabbed:
**Control+Option(Alt)+Command(Super)+Esc** on the Linux keyboard, or
stop the relay (the X server drops the grab when the process exits).

| Check | Result |
|---|---|
| Same checks as section 1, with roles reversed | ⏭ NOT TESTED yet — code complete (decision 24) |
| While forwarding, the Linux screen's own apps receive no keyboard/mouse input and its arrow is hidden | ⏭ NOT TESTED yet |
| The hub's own cursor warps never register as motion (no bounce-back on switching) | ⏭ NOT TESTED yet |

## 5. Disconnected / unresponsive target

| Check | Result |
|---|---|
| While forwarding, stop the join process — the hub falls back to local input immediately, logs it, and does not freeze the pointer or keyboard | ✅ PASS — `falling back to local input` in three separate runs; local input restored each time |
| Target connected but no longer processing input (QuickEdit click in its console) — the hub gives local input back without user action | ✅ PASS — `stopped answering pings ... silent_for=2.09s`; restored 0.85 s after Windows froze; nothing replayed when Windows resumed (`dfdcf03`) |
| Restart the join process and reconnect without restarting the hub — a fresh edge crossing switches ownership again | ⏭ NOT VERIFIED on hardware — every retained hub log shows exactly one accepted connection (automated: `disconnect_then_reconnect_recovers_ownership_without_restarting_the_session`; follow-up F3) |

## 6. Known, expected gaps — not bugs

- **Local input suppression** is implemented on macOS (events dropped,
  cursor disassociated and hidden, gestures dropped) and Windows; X11
  capture is still listen-only (ADR-0009 decision 9).
- **Multi-monitor** is not modeled per-monitor — only each machine's
  combined screen boundary. Both test machines had one display.
- **No wrap-around**: crossing an edge with no configured neighbor does
  nothing; the cursor stops at the edge.
- **Three-or-more devices** are unit-tested
  (`a_switch_chain_can_continue_onward_to_a_third_device`) but not run on
  hardware — `edge_switch_relay` wires a two-device pair.
- Number-pad Enter injects on Windows as plain Return; only the two
  measured gesture types (29, 30) are dropped; setting Caps Lock on a
  macOS *target* is not implemented.

## 7. Security verification

- [x] A device never added via `Session::add_peer` cannot become a switch
      target whatever the layout names — structural (ADR-0009 decision
      6), covered by `an_unregistered_device_can_never_become_a_target_even_if_the_layout_names_it`;
      `edge_switch_relay.rs` only ever calls `add_peer` with a `Peer`
      from a real `net::connect`/`accept`.

## 8. Follow-ups found in the acceptance logs (not blocking)

Neither the user nor the run reported a visible problem from F1 or F2;
both were found by reading the final run's hub log and are recorded
rather than fixed, so the accepted build stays exactly what was tested.

- **F1 — first switch after hub start occasionally leaves the cursor
  visible.** Final run, first switch: `cursor visibility did not change
  as requested hidden=true visible=true`, and the cursor stayed visible
  for that whole 1.5 s forwarding period; the other three switches (and
  the first switch of earlier runs) hid it correctly.
- **F2 — first crossing after hub start can land off-proportion.** Final
  run, first switch: the router's tracked y was 534 while the real
  cursor was at 471, so Windows was entered 63 px low; the next three
  crossings matched within 1–2 px. Same stale-position mechanism as
  ADR-0009 decision 21, which re-anchors after every return but not at
  session start.
- **F3** — reconnect without hub restart: not hardware-verified (§5).
- **F4** — Linux and reversed-role sections (§2–4) not tested.
- **F5** — X11 and macOS-*target* paths changed during this acceptance
  (Caps Lock state, scroll unit) are compiled, linted and unit-tested
  but not hardware-verified.
