# Phase 4 Manual QA — Edge-Based Switching

Automated tests (48 unit tests in `layout.rs`/`ownership.rs`/`router.rs`,
7 real-loopback-`Peer` integration tests in
`crates/core/tests/session_end_to_end.rs`) cover the routing/security/
modifier-safety state machine with deterministic fakes standing in for
`Capture`/`Inject`/`PointerGeometry`. They cannot cover real OS cursor
warping, real edge-crossing feel, or how switching actually looks across
two physical screens — that's what this procedure is for. **Do not mark
any row below done until it was actually run on the real machine(s)
named.** As of this document's creation, no row in this file has been
executed — see ADR-0009 for the architecture these tests exercise.

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
terminal/binary running the example (same requirement as Phase 3).

## 0. Prerequisites

- [ ] Real Mac available, LAN-reachable
- [ ] Real Windows machine available, LAN-reachable
- [ ] Real Linux/X11 machine available, LAN-reachable (per ADR-0008,
      no Wayland target exists — nothing to test there)
- [ ] `cargo build -p kvm-core --example edge_switch_relay` succeeds on
      each machine used

## 1. Mac (hub) ↔ Windows (join)

| Check | Result |
|---|---|
| Hub prints its own screen size and starting cursor position | NOT TESTED |
| Hub prints the peer's (Windows) screen size after connecting | NOT TESTED |
| Moving the mouse to the configured edge on the Mac switches ownership (`ownership changed: Local -> Forwarding { .. }` printed) | NOT TESTED |
| Cursor visibly appears on the Windows screen at the proportionally-correct position along the shared edge | NOT TESTED |
| Mouse movement after the switch visibly moves the Windows cursor | NOT TESTED |
| Keyboard input after the switch lands on Windows, with Cmd/Ctrl translation still correct (per ADR-0007) | NOT TESTED |
| Moving the mouse back across the opposite edge on the Windows side returns ownership to the Mac (`ownership changed: Forwarding { .. } -> Local` printed on the hub) | NOT TESTED |
| A modifier held at the moment of switching away is not stuck on Windows afterward (hold Shift, cross the edge mid-hold, then release Shift and type a letter directly on Windows — must be lowercase) | NOT TESTED |
| Rapid repeated crossing back and forth (several times in a row) does not desync, freeze, or crash either side | NOT TESTED |
| Different screen resolutions between the two machines: the crossing point along the shared edge lands proportionally, not identically-pixel-mapped | NOT TESTED |

## 2. Mac (hub) ↔ Linux/X11 (join)

| Check | Result |
|---|---|
| Same checks as section 1, repeated against the Linux machine | NOT TESTED |

## 3. Windows (hub) ↔ Mac (join)

| Check | Result |
|---|---|
| Same checks as section 1, with roles reversed (Windows is the machine whose input stays physically in use) | NOT TESTED |

## 4. Linux/X11 (hub) ↔ Mac (join)

| Check | Result |
|---|---|
| Same checks as section 1, with roles reversed | NOT TESTED |

## 5. Disconnected-target behavior

| Check | Result |
|---|---|
| While `Forwarding` to the join machine, kill the join process (Ctrl+C) — the hub must fall back to local input immediately, log the disconnect, and must not freeze the pointer or drop local keyboard input | NOT TESTED |
| Restart the join process and reconnect (no hub restart) — a fresh edge crossing must switch ownership again | NOT TESTED |

## 6. Known, expected gaps — not bugs

- **Local input is not suppressed while forwarding** (ADR-0009
  decision 8): every backend's `Capture` is listen-only, so the hub's
  own screen keeps visibly showing captured input even while it's
  being forwarded elsewhere. Confirm this is what actually happens
  (expected, not a regression) and judge whether it's disruptive
  enough in practice to prioritize a blocking-capture follow-up.
- **Multi-monitor** is not modeled per-monitor — only each machine's
  combined screen boundary. If either test machine has more than one
  monitor, note which one `PointerGeometry::screen_size` actually
  reported.
- **No wrap-around**: crossing an edge with no configured neighbor
  simply does nothing (by design) — confirm this is what happens (no
  crash, cursor just stops at the edge) rather than testing it as a
  failure.
- **Three-or-more-device star/cross topology** (the original kickoff's
  Linux/Mac/Windows/Laptop diagram) is unit-tested
  (`a_switch_chain_can_continue_onward_to_a_third_device`) but not yet
  exercised on real hardware — `edge_switch_relay` only wires up a
  two-device hub/join pair. Revisit once a third physical machine is
  available.

## 7. Security verification

- [ ] Confirm (by code review, matching `router.rs`/`session.rs`'s
      tests) that a device never added via `Session::add_peer` cannot
      become a switch target no matter what the layout names — this is
      structurally guaranteed (see ADR-0009 decision 6) and already
      covered by automated tests; no additional real-hardware action
      needed beyond confirming the architecture wasn't bypassed by any
      manual-QA shortcut in `edge_switch_relay.rs` itself (it isn't:
      the example only ever calls `add_peer` with a `Peer` from a real
      `net::connect`/`accept`).
