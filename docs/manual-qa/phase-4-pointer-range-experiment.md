# Phase 4 — pointer-range root-cause experiment (A vs B)

**Status: awaiting a real-hardware run. No production fix has been made yet.**

This document defines the one experiment that decides the open Phase 4
blocker: the Windows cursor cannot reach the right/top/bottom edges and
corners — "boxed into the middle."

## The two hypotheses

**HYPOTHESIS A** — the Mac's captured event *locations* are bounded by
the Mac display, so the deltas this project derives from them stop
representing continued physical movement. Movement is lost at stage 1,
before anything else in the pipeline could affect it.

**HYPOTHESIS B** — the Mac keeps reporting real movement, and it is lost
or transformed somewhere later: normalization → router → session →
QUIC → Windows injection.

## Why this is decidable at the Mac alone

`crates/input/src/macos/events.rs` derives `InputMessage::MouseMove`'s
`dx`/`dy` by diffing successive `CGEvent::location()` readings
(`point_delta`). `location()` is an *absolute screen coordinate*. Every
event's own independent, HID-derived relative motion is available on the
same event as `kCGMouseEventDeltaX`/`kCGMouseEventDeltaY`, and is not a
function of any screen position.

Logging both readings for the same event separates the hypotheses with
no ambiguity and no other machine involved:

| `point_delta` (shipped) | HID delta (physical truth) | Conclusion |
| --- | --- | --- |
| tracks HID delta all the way through the swipe | nonzero | Mac is faithful → **B**, look downstream |
| goes to 0 while the pointer is still physically moving | nonzero | Movement already gone at stage 1 → **A** |
| 0 | 0 | The hand genuinely stopped; not evidence either way |

## Quantitative prediction under Hypothesis A

This Mac reports `CGDisplay::main().bounds()` = **1470 × 956** logical
points, origin (0, 0) — measured, not assumed. So:

- `Effect::RecenterLocal` warps the local cursor to the centre,
  **(735, 478)**, the instant ownership leaves `Local`.
- `entry_position` + `nudge_off_boundary` land the Windows cursor at
  **x = 5** after a Right-edge crossing (`EDGE_MARGIN + 1`).
- If `location()` is bounded by the Mac display, the largest rightward
  advance one uninterrupted swipe can express is
  `1469 − 735 = 734` points (a little less in practice — ADR-0009 notes
  the real OS clamp lands a few points short of the nominal maximum).
- **Predicted maximum reachable Windows x ≈ 5 + 734 = 739**, regardless
  of how far the hand physically travels.

The reported real-hardware figure is **744, held for 114 of ~1263
samples, against ~4537 px of physical travel** — agreement to within
about five points, or 0.7%, on a swipe more than three times the width
of the Windows screen. Vertically the same bound predicts a reach of at
most ±478 out of 768, which is the "cannot reach top or bottom" half of
the report.

This is a prediction the experiment can falsify. If the probe shows
`point_delta` tracking the HID delta faithfully past the Mac's edge,
Hypothesis A is wrong and the search moves downstream.

## Experiment 1 — the Mac stage in isolation (decisive, run this first)

Needs only the Mac. No network, no Windows, no router.

```sh
# Baseline: cursor moves normally, just observed.
cargo run -p kvm-input --example mac_pointer_probe -- --seconds 30

# Decisive: reproduces the exact state a real Forwarding session puts
# the Mac in (recentre, disassociate, drop motion events).
cargo run -p kvm-input --example mac_pointer_probe -- --suppress --seconds 30
```

Needs Accessibility permission for the terminal, same as `MacCapture`.
Keyboard events are always passed through even under `--suppress`, so
Ctrl-C always works; the association is restored on exit, and macOS
restores it anyway when the process dies, so the cursor cannot be left
frozen.

During the run, deliberately:

1. Swipe **right** continuously for several seconds.
2. **Keep going** well past the point where the Mac cursor would have
   hit its own right edge — this is the part that matters.
3. Repeat **left**, then **up**, then **down**.

Then read the summary block. The three lines that answer the question:

- `samples_with_zero_point_delta_but_real_hid_delta` — nonzero means
  movement is being lost at stage 1. **This is the A/B discriminator.**
- `observed_location_x` / `observed_location_y` vs `screen_size` —
  whether `location()` pinned at a display boundary.
- `cumulative |point_delta|` vs `cumulative |hid_delta|` — how much
  physical movement was shipped versus how much actually happened.

The probe prints its own verdict line at the end.

## Experiment 2 — the full pipeline, one movement followed end to end

Run the ordinary Phase 4 manual-QA relay with stage tracing on. Every
stage now carries a `stage=` field, so one filter follows a single
physical movement through the whole chain.

On the Mac (hub):

```sh
RUST_LOG=kvm_core=debug,kvm_input=debug \
  cargo run -p kvm-core --example edge_switch_relay -- hub --edge right \
  2>&1 | tee /tmp/kvm-mac.log
```

On Windows (join):

```powershell
$env:RUST_LOG="kvm_core=debug,kvm_input=info"
cargo run -p kvm-core --example edge_switch_relay -- join <mac-lan-ip>:51823 `
  2>&1 | Tee-Object -FilePath kvm-win.log
```

Stages emitted:

| Stage | Where | What it proves |
| --- | --- | --- |
| `1-capture` | `macos/events.rs` | `CGEvent` location, the delta derived from it, **and** the HID delta — i.e. whether the Mac still sees the movement |
| `2-router` | `core/router.rs` | the normalized delta as routing receives it, and the virtual position it accumulates into |
| `3-session-send` | `core/session.rs` | exactly what goes onto the wire before QUIC |
| `4-windows-inject` | `windows/inject.rs` | received `dx`/`dy`, pre-injection `GetCursorPos`, intended position, actual post-write position, and whether they matched |

Perform the same movement script as Experiment 1, starting from a known
position and entering `Forwarding` first.

Read the logs for where the movement stops changing:

- `1-capture` `hid_dx` nonzero while `dx` is 0 → lost at capture (**A**).
- `dx` nonzero at stage 1 but 0 at stage 2 → lost in normalization.
- stage 2 nonzero, stage 3 missing or different → lost in routing/session.
- stage 3 nonzero, stage 4 `dx` different or absent → lost in transport.
- stage 4 `matched=false` away from a true screen boundary → lost in
  Windows injection.

## Recording the result

Paste into this section, for both experiments:

- The probe's `=== SUMMARY ===` and `--- VERDICT ---` blocks verbatim.
- For each of right / left / up / down: did `CGEvent::location()` keep
  changing? Did `point_delta`? Did the router's virtual cursor? Did
  Windows receive the deltas? Did its calculated position change? Did
  its actual cursor change?
- The Mac's reported screen size (printed by the probe).

Only after that does a production fix get written, at whichever layer
the evidence names.

## Results

_(to be filled in from the real-hardware run)_
