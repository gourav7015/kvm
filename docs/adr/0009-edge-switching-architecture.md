# ADR-0009: Edge-Based Switching Architecture

## Status

Accepted (2026-09-11)

## Context

Phases 0–3b closed a real, hardware-verified pipeline: authenticated
QUIC transport (1c), discovery+pairing (2), and bidirectional
keyboard/mouse capture/inject across macOS, Windows, and Linux/X11 (3,
3b) — but every real test so far has been a fixed, hardcoded two-device
pair driven entirely by hand (`input_relay listen`/`connect`, one
`Peer`, one direction at a time). Phase 4's job is to make switching
*automatic*: move the mouse to a configured screen edge and control
follows, across an arbitrary layout of paired devices, without a human
manually restarting anything. This is the first time `core` needs to
hold more than one `Peer` at once, and the first time an OS backend
needs to report *absolute* cursor position/screen size — both real,
load-bearing gaps this ADR closes deliberately.

## Decisions

### 1. `PointerGeometry` is a new, separate trait — not a change to `Capture`/`Inject`

`crates/input/src/traits.rs` gained `PointerGeometry: Send` (`cursor_position`,
`screen_size`, `set_cursor_position`), implemented as an addition to the
existing `MacInject`/`WindowsInject`/`X11Inject` structs. `Capture`/`Inject`
themselves are untouched — they only ever carry relative deltas, the
already hardware-verified Phase 3/3b path. Splitting the new capability
into its own trait means the entire edge-switching effort has zero risk
to that already-proven code; a caller that only wants relative
capture/inject never has to know `PointerGeometry` exists.

Implementations reuse existing per-OS mechanisms rather than adding
new ones: macOS reuses the same `CGEvent::new(source)?.location()` call
`MacInject::inject` already made for its relative-move approximation,
plus `CGDisplay::main().bounds()`. Windows uses `GetCursorPos`/
`SetCursorPos`/`GetSystemMetrics(SM_CXSCREEN/SM_CYSCREEN)` — three
Win32 calls, no new dependency. Linux/X11 uses `XQueryPointer`/
`GetGeometry` on the root window, and `XTestFakeInput` with `detail =
0` (absolute) — the other branch of the exact same `fake_input` call
the relative-move path already makes with `detail = 1`. No new
dependency was needed on any platform.

### 2. Ownership model: `Local` / `Forwarding`, tracked only by the capturing device — not a per-device `Active`/`Passive` pair

**This supersedes the originally-planned design.** The build plan
(`elegant-wishing-origami.md`'s Phase 4 section, written before
implementation) proposed a per-device `Active { forwarding_to:
Option<DeviceId> }` / `Passive { owned_by: DeviceId }` state, with every
device in the layout independently running its own edge-detection
against its own (possibly injected) cursor. That design was abandoned
during implementation of Milestone 1, before any networking code was
written, for a concrete correctness reason: with more than two devices,
independent per-device edge-detection creates a real distributed-
agreement problem — two devices could, even briefly, both believe
they're the active input source, or disagree about who currently owns
input after a race between a local edge-crossing and an incoming
`SwitchActive`. That class of bug is exactly what sank the
"catch bugs structurally" goal of this whole project if left unaddressed.

The actual model (`crates/core/src/ownership.rs`, `crates/core/src/router.rs`):
only the device that is *physically* capturing real local input —
whichever machine the person is actually sitting at — ever tracks
position or decides on edge crossings, for the *entire* session, even
across a multi-hop chain (A hands off to B, then directly to C, without
B deciding anything). It keeps:

```rust
enum OwnershipState {
    Local,
    Forwarding { target: DeviceId, virtual_cursor: (i32, i32) },
}
```

`virtual_cursor` is a *virtual* estimate of where the pointer would be
on `target`'s screen — computed once at each handoff via
`entry_position` (decision 3) and updated from then on purely by
accumulating the same relative deltas `Capture` produces (no polling
of the target's real cursor — there's no connection back for that, and
none is needed). A device that is merely the current forwarding target
runs no `Router`/`Session` participation of its own at all: it just
warps its cursor once per switch (`run_target`, decision 5) and injects
whatever `InputMessage`s arrive on the input stream. This is a single,
plain source of truth per session — no possibility of two devices
disagreeing, by construction — and matches how established KVM tools
(Synergy/Barrier/Deskflow, referenced in Phase 3b's Wayland research)
actually model this: exactly one machine's physical keyboard/mouse is
ever the source for a given session.

Pure/async split follows this codebase's established `pairing.rs`/
`pairing_session.rs` pattern exactly:
- `layout.rs` — the platform-independent device/edge map (`Layout::neighbor`),
  keyed entirely by `DeviceId`, never by IP address (IPs can change,
  `DeviceId` doesn't).
- `ownership.rs` — the pure `transition(state, event, ctx) -> state`
  function (no `Result`: every combination has a well-defined outcome,
  including "stay put," so there is no illegal-transition case at all).
- `router.rs` — wraps `ownership.rs` with position tracking and
  modifier bookkeeping (decision 4), pure, unit-tested, zero I/O,
  emitting `Effect` values (`Send`/`Switch`) rather than performing them.
- `session.rs` — the only async, I/O-performing layer: holds
  `HashMap<DeviceId, Peer>`, turns `Router`'s `Effect`s into real sends.

### 3. Pointer handoff: resolution-aware, one combined screen per device, no wrap-around

`entry_position(edge, source_screen, dest_screen, crossing_coordinate)`
rescales proportionally: `dest_y = (crossing_coordinate / source_height)
* dest_height` for a left/right transition (symmetric for top/bottom),
so differing resolutions/aspect ratios land at the equivalent relative
position rather than assuming a fragile 1:1 pixel mapping. The entry
edge is always the opposite of the exit edge. `crossing_coordinate` is
clamped into `[0, source_dimension]` first, so a stale/out-of-bounds
reading still produces an in-bounds result.

**Explicitly out of scope, not silently pretended to work:**
- **Multi-monitor.** Each device is treated as one combined screen
  boundary — whatever `PointerGeometry::screen_size` reports (on
  macOS, the primary display's `CGDisplayBounds`; analogous on
  Windows/X11). A machine with several monitors arranged in an
  unusual shape is not modeled precisely.
- **Wrap-around** (an edge with no configured neighbor looping back to
  the opposite side of the same screen). `Layout::neighbor` returning
  `None` for an unmapped edge already means "don't switch" — which is
  the correct default (no arbitrary behavior) — and a future
  wrap-around feature can be added as an explicit, off-by-default
  per-edge flag without changing anything built in this phase.

### 4. Modifier safety: flush held keys as ordinary `Released` input, not a dedicated reset message

`Router` tracks currently-held keys (`Pressed` inserts, `Released`
removes) for whichever device is the *current* forwarding target.
Whenever ownership leaves a `Forwarding` state — to a new target, or
back to `Local` — every key still believed held gets a synthesized
`Released` sent to the *old* target, before/alongside the switch. This
travels as a perfectly ordinary `InputMessage` on the same input
stream the target already reads (`run_target` needs no dedicated
"reset" handling at all — this was a deliberate design choice so the
receiving side stays as simple as Phase 3's `inject_from_peer`).
Leaving `Local` needs no flush: the local OS's own modifier state was
never touched by any of this (see decision 6), so it's already correct.

**Known, documented limitation:** if the target disconnects abruptly
while a modifier is held, there is no live connection left to flush a
`Released` onto — `Router::set_peer_disconnected` clears its own
held-key bookkeeping (nothing to send) and returns to `Local`
immediately, but cannot fix the dead target's own OS-level key state.
This mirrors Phase 3b's real hardware finding (`docs/manual-qa/phase-3b-linux-input.md`
§6): the *surviving* side was verified to never get stuck; the
*disconnecting* side's residual state is outside what any of this code
can control after the connection is gone.

### 5. Target-side handling: `run_target`, not a one-shot warp

The receiving/target side needs nothing beyond one function
(`crates/core/src/session.rs::run_target`): concurrently select over
the peer's control stream (warp the cursor via `PointerGeometry::set_cursor_position`
on every `SwitchActive`, not just the first) and its input stream
(inject every `InputMessage`, applying the same modifier-role
translation `inject_from_peer` already applies). This explicitly
handles rapid back-and-forth: a *later* return to an already-visited
target re-warps the cursor to the newly computed entry position,
rather than silently resuming wherever the cursor happened to be left
from the previous visit. A regression test
(`rapid_back_and_forth_re_warps_the_targets_cursor_on_every_return`)
deliberately perturbs the target's cursor between switches specifically
to catch a version of this function that only handled the first
`SwitchActive` (an earlier, one-shot `become_target` did exactly that,
and was replaced).

### 6. Security: routing reuses the existing trust boundary, invents no new one

`Session::add_peer` is the *only* way a `DeviceId` becomes a possible
switch target, and it is only ever called with a `Peer` that already
came out of `net::connect`/`accept` — which already enforce full
TLS/trust-store verification (Phase 1c/2). A `Router` cannot itself
name a device that wasn't registered this way: `Effect::Send`/`Effect::Switch`
only ever carry a `DeviceId` present in `Router`'s own connected-peers
set, itself only ever populated from `Session::add_peer`. A discovered-
but-unpaired or revoked device has no `Peer` and therefore cannot
appear in that set at all — this is a structural guarantee, not a
runtime check that could be forgotten on some code path, and is the
reason `router.rs`'s own unit tests (`an_untrusted_or_never_connected_device_can_never_become_a_target`)
and `session.rs`'s integration tests
(`an_unregistered_device_can_never_become_a_target_even_if_the_layout_names_it`)
can assert it directly against the real types rather than against a
separate, parallel trust check.

### 7. `ControlMessage` gained two variants; the wire's `SwitchActive` changed shape

`ControlMessage::SwitchActive` existed since Phase 1a but was never
consumed until now — its shape changed from `{ device_id }` to `{
device_id, cursor_position: (i32, i32) }`, carrying the resolution-
aware entry position (decision 3) so the receiver only has to warp its
cursor there, not compute anything itself. `ControlMessage::ScreenInfo
{ width, height }` is new — exchanged once per connection
(`exchange_screen_size`, right after `connect`/`accept` returns, before
the `Peer` is handed to `Session`/`run_target`) so the handoff math has
each side's *real* screen size instead of a guess. Both changes are
additive/pre-consumption (nothing depended on the old `SwitchActive`
shape) and are the only two protocol touch-points this phase made.

### 8. Known limitation: local input is not yet actually suppressed while forwarding

Every backend's `Capture` is listen-only by design (ADR-0007 §4,
ADR-0008 decision 1 — `CGEventTapOptions::ListenOnly` on macOS, the
non-exclusive XInput2 raw-event model on Linux): it observes and
reports input without ever blocking it, so the local OS *always*
applies a captured event natively regardless of `Router`'s decision.
`Router` correctly avoids re-injecting or double-routing that event
(`Local` routing produces zero effects — see `router.rs`'s module
docs), which prevents *duplicate injected input*, but it cannot
prevent the local machine's own screen from continuing to visibly show
the cursor/keystrokes while `Forwarding` is active elsewhere. Actually
suppressing local input during forwarding would need at least one
backend to grow a genuinely blocking capture mode (e.g. macOS's
`CGEventTapOptions::Default`, which can consume events by not
returning them) — real per-OS hardware work, deliberately deferred
past this phase rather than silently pretended to already work.
Tracked here as the concrete next architectural step if real hardware
QA (`docs/manual-qa/phase-4-edge-switching.md`) finds it disruptive in
practice.

### 9. Local input is now suppressed while `Forwarding` (macOS, Windows) — supersedes decision 8's "known limitation"

**Update (2026-09-11, following real Mac<->Windows manual QA):** decision
8 flagged that every backend was listen-only and deferred the question
of whether that would be disruptive in practice to real hardware QA.
It was: with the mouse forwarding to Windows, the Mac's own cursor kept
visibly moving and the Mac's own keyboard kept typing into whatever
local app had focus, at the same time input was correctly being
forwarded — both keyboard and pointer control need to belong to exactly
one machine at a time, not silently both.

Implemented, not left deferred further. `Capture` (`crates/input/src/traits.rs`)
gained one additive method, defaulted to a no-op so no existing backend
had to change on the same day:

```rust
fn set_local_suppression(&mut self, suppress: bool) {}
```

`Session::handle_captured` (`crates/core/src/session.rs`) calls it
exactly on a `Local`<->`Forwarding` transition (comparing
`Router::state()` before and after applying that call's effects) —
never on every captured event, and never redundantly on a multi-hop
switch that stays `Forwarding` (A hands off to B, then directly to C).
This mirrors decision 4's modifier-flush timing exactly: a state-
transition-triggered side effect, not a per-event one.

Implemented for the two backends real hardware QA actually exercised
so far:
- **macOS**: the tap is created with `CGEventTapOptions::Default`
  instead of `ListenOnly` (both already `core-graphics` constants, no
  new dependency), so it's *capable* of consuming an event
  (`CallbackResult::Drop`) rather than only ever `Keep`ing it. The
  event is still converted and sent to the capture sink exactly as
  before either way — suppression only changes what the callback
  returns to the OS, never whether the forwarding target hears about
  it. A shared `Arc<AtomicBool>` (reset to `false` on every fresh
  `start()`, same hygiene as the existing per-session `Cell`s) carries
  the toggle from `set_local_suppression` into the tap's callback.
- **Windows**: `WH_KEYBOARD_LL`/`WH_MOUSE_LL`'s documented contract
  already provides exactly this mechanism — returning a non-zero
  `LRESULT` instead of calling `CallNextHookEx` discards the event for
  every later hook/window in the chain. A process-wide `AtomicBool`
  (matching the existing `SINK`/`LAST_MOUSE_POS` static pattern, for
  the same "bare function pointer, no closure state" reason) gates
  that choice in both hook procs, after the event has already been
  converted and sent to the sink.

**Not yet implemented, an explicit rollout gap, not silently
overlooked:** X11 (`crates/input/src/x11/capture.rs`) keeps the
trait's default no-op. Its raw XI2 capture model has no per-event
"discard" return value the way macOS/Windows do (ADR-0008 decision 1
chose raw events specifically *because* the exclusive-grab APIs
that could block delivery, `XGrabKeyboard`/`XGrabPointer`, are
unsuitable for an always-on capture session) — a real implementation
would need to issue a temporary grab only for the duration of a
`Forwarding` session and release it on return to `Local`, communicated
into the capture thread's own X connection (which, like the mouse
position/keymap state already there, isn't reachable from outside that
thread without a similar dedicated-message mechanism to the existing
stop-signal `ClientMessage`). Deferred to Round B (Mac<->Linux) of
Phase 4's manual QA, where it can actually be verified against real
hardware rather than guessed at — tracked here, not discovered as a
surprise gap later.

## Consequences

- `crates/core` gains `layout.rs`, `ownership.rs`, `router.rs`,
  `session.rs` — the first time `core` holds more than one `net::Peer`
  at a time. `crates/input` gains `PointerGeometry`, implemented for
  all three existing backends. `crates/protocol` gains one changed and
  one new `ControlMessage` variant.
- Automated coverage: 53 unit tests in `kvm-core` (`layout`/`ownership`/
  `router`, all pure, no I/O) plus 10 real-loopback-`Peer` integration
  tests in `crates/core/tests/session_end_to_end.rs` covering edge
  crossing + cursor warp, screen-size exchange, an unregistered device
  never becoming a target, disconnect/reconnect without restarting the
  session, modifier flush landing as ordinary input on the old target,
  no leakage to an inactive-but-connected peer, rapid back-and-forth
  re-warping, `RecenterLocal` actually reaching `PointerGeometry`, a
  single transient injection failure not killing the session, and
  local-capture suppression toggling exactly on `Local`/`Forwarding`
  transitions (decision 9). None of this depends on physical hardware.
- `crates/core/examples/edge_switch_relay.rs` is the manual-QA tool for
  real hardware, driving `docs/manual-qa/phase-4-edge-switching.md`.
- Decision 8's limitation is resolved by decision 9 for macOS and
  Windows (the two backends real hardware QA has exercised so far);
  X11 still carries it, tracked for Round B of manual QA.
- Multi-device (3+) routing is implemented and unit-tested
  (`a_switch_chain_can_continue_onward_to_a_third_device`) but not yet
  exercised on real hardware — only a two-machine hub/join topology has
  been physically tested as of this ADR's acceptance; a third machine
  is needed to demonstrate the star/cross topology the Phase 4 kickoff
  described.
