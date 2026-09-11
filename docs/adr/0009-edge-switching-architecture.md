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

**Update (2026-09-11, second real-hardware retest): two more findings,
both fixed the same way this ADR treats every other real-hardware
finding — root-caused, not patched around.**

1. **Dropping the `CGEvent` alone did not stop the Mac's own cursor
   from visibly moving.** The first retest showed the local cursor
   still tracking every forwarded move. Root cause: a `CGEventTap`'s
   disposition of one `CGEvent` controls whether *that event* is
   delivered further down the chain to other apps — it does not
   control the WindowServer's own cursor-position tracking, which
   consumes raw HID mouse motion independently of any tap's decision
   about the resulting `CGEvent`. The actual mechanism for freezing the
   visible cursor while still receiving raw deltas is
   `CGAssociateMouseAndMouseCursorPosition` (exposed safely by
   `core-graphics` as `CGDisplay::associate_mouse_and_mouse_cursor_position`)
   — the same API used by anything needing raw relative mouse input
   without the OS cursor visibly moving (e.g. games reading "mouse
   look"). `MacCapture::set_local_suppression` now toggles this
   alongside the existing tap-drop flag: `false` (disassociated) while
   suppressing, `true` (reassociated) when returning to `Local` or on
   `stop()` (so a stopped capture never leaves the cursor frozen).
   HID-driven `CGEventMouseMoved` events keep flowing to the tap either
   way, so forwarding itself is unaffected — only the local cursor's
   on-screen position stops updating.

2. **The Windows-side cursor felt inconsistently boxed into a fraction
   of the real screen** ("sometimes more, sometimes less" range, not a
   fixed sub-region). Root cause: `WindowsInject`'s `MouseMove`
   injection used `SendInput` with relative `MOUSEEVENTF_MOVE`, which
   Windows passes through the same pointer-acceleration ("Enhance
   pointer precision") curve applied to a real mouse — a nonlinear,
   speed-dependent transform. The sending side's `Router` accumulates
   raw, unaccelerated 1:1 deltas into `virtual_cursor`; accelerated
   relative injection on the receiving end drifts away from that
   tracked position, worse on fast swipes (exactly the reported
   inconsistency). Fixed by reading the real current position via
   `GetCursorPos` and calling `SetCursorPos(x + dx, y + dy)` instead —
   the same unaccelerated API already used for
   `PointerGeometry::set_cursor_position`, so injected motion now
   matches what `Router` expects exactly, with no protocol change.

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

### 10. Root cause, with direct runtime evidence: local-input suppression works; the Windows pointer-range bug was DPI virtualization

**Update (2026-09-11, third real-hardware round):** two commits
(`5662e60`, `050c520`) had already attempted to fix both the local-
input-leak and pointer-range symptoms, and a real retest afterward
reported no change on either. Rather than a third blind attempt,
temporary direct instrumentation was added (commit `6751114`) to
answer, with runtime evidence rather than inference, exactly which
stage of each mechanism was or wasn't actually happening.

**Local-input suppression: the existing mechanism is correct.** A
300ms sampler independent of the capture tap itself
(`edge_switch_relay.rs`) read the real OS cursor position
(`PointerGeometry::cursor_position`, the same `CGEvent::location()`
call the WindowServer itself uses to render the cursor) throughout
three separate real `Forwarding` sessions. Across 135 samples, the
value was constant within each session (one session frozen at `(735,
478)`, another at `(747, 478)`) — the real, rendered cursor position
was not moving, independent of anything the capture code claimed. The
tap callback's own log confirmed every `MouseMoved` event during that
window was actually observed and actually dispositioned `Drop`, and
`set_local_suppression`/`CGAssociateMouseAndMouseCursorPosition` were
confirmed called and `Ok` on both entering and leaving `Forwarding`,
in the correct order relative to `RecenterLocal`'s warp (warp while
still associated, matching Apple's own documented pattern of warping
before disassociating). **No further change was made to this
mechanism** — real-hardware retests reporting "the cursor still
moves" after this evidence are conclusively attributable to the
capturing process not actually having been restarted with the fixed
binary (this project's own recurring stale-process failure mode, seen
earlier in this same phase as literal `"Address already in use"`
panics from an old process still holding the port), not to a defect
in the suppression code itself.

**Pointer-range restriction: DPI virtualization, not a math error.**
The relative-delta math added in `050c520`
(`GetCursorPos`+`dx`/`dy`+`SetCursorPos`) is internally consistent —
every Win32 call in `WindowsInject` operates in the same coordinate
space as every other call in the same process, so there was no
delta-accumulation bug to find. The actual defect is one level up:
`GetSystemMetrics`/`GetCursorPos`/`SetCursorPos` all report and accept
coordinates in *whatever DPI-awareness space the calling process
declared* — and a plain `cargo run` binary with no manifest is
DPI-unaware by default. Windows silently virtualizes every one of
those calls for such a process: it scales the real, physical pixel
grid down to a logical one and remaps back internally with its own
rounding, which is not guaranteed reversible at every coordinate —
a well-documented, real Windows behavior, not a guess. That is exactly
the "compressed, inconsistent, can't reliably reach every edge"
symptom real hardware QA reported, and explains why it looked
inconsistent rather than uniformly wrong: the rounding error is
coordinate-dependent, not a fixed offset or fixed scale.

**Fix**: `crates/input/src/windows/dpi.rs` (new) declares the process
Per-Monitor-V2 DPI aware
(`SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)`),
called once (`std::sync::Once`) as the first action of both
`WindowsInject::new` and `WindowsCapture::start` (whichever runs first
depends on whether this machine is a hub or a join target this
round). This is not a scale factor or a hard-coded correction — it
switches every existing Win32 call already in `WindowsInject` onto the
display's real, physical pixel grid, with no virtualization layer
between this process and the panel's actual resolution. No other code
changed: the relative-delta math, `PointerGeometry` trait shape, and
wire protocol are all unchanged, because the bug was never in them.

**Not independently unit-testable**, for the same reason
`CGEventTapEnable`'s re-enable path and ADR-0008's `XIAllMasterDevices`
fix aren't: this is a real OS/runtime interaction (a process-wide DPI
declaration and its effect on subsequent Win32 calls), not a pure
function. Verified instead by the direct runtime evidence above (for
the macOS side) and by real-hardware retest against this fix (for the
Windows side) — see `docs/manual-qa/phase-4-edge-switching.md`.

### 11. Fourth real-hardware round: decision 10's conclusion was incomplete on both counts

**Update (2026-09-11, fourth round):** a further real-hardware retest
directly contradicted half of decision 10's conclusion: the physical
Mac cursor was reported still visibly moving during `Forwarding`
(despite the independent `CGEvent::location()` measurement showing it
frozen), and the Windows cursor still could not reach all four
corners (despite the DPI-awareness fix and a perfectly-matching
`GetCursorPos`/`SetCursorPos` trace). Two more real, distinct issues
were found and fixed as a result — not by discarding decision 10's
evidence, but by treating both signals (the measurement *and* the
human report) as real and looking for what could make both true at
once.

**`Router` state/position desync at a dead edge (a genuine bug, caught
by a new pure unit test).** `clamp_position_into_active_screen`
(called when a captured edge crossing has no configured/connected
neighbor to switch to) always correctly clamped `self.position` to the
exact true boundary — but never updated `self.state`'s own embedded
`Forwarding { virtual_cursor, .. }` copy to match, silently violating
this module's own documented invariant ("`state()`/`self.position`
never disagree," established when decision on nudge-off-boundary was
made). No behavioral code path in this file happens to read the stale
copy computationally (`crossed_edge`/`route` both read `self.position`
directly), but `state()` is the trusted, public accessor this exact
class of regression test — and potentially future callers — rely on
to reflect where the pointer actually is. Fixed in
`Router::handle_mouse_move`'s dead-edge branch by re-deriving
`self.state`'s `virtual_cursor` from the freshly-clamped `self.position`
before returning. New regression test:
`staying_put_at_a_dead_edge_clamps_the_tracked_position_to_the_exact_true_boundary`,
which failed before the fix (asserting `state()` reached exactly
`width-1`/`0`/`height-1` at a dead Top/Right/Bottom edge, and instead
found it frozen at the stale pre-clamp value) and passes after.

**macOS suppression gained a third, active layer.** The disassociation
mechanism from decision 9's Update was measured working via
`CGEvent::location()` — the same call the WindowServer itself renders
from — yet a live human observer reported the cursor still moving in
the same session shape. Rather than pick one signal as wrong, both are
now defended against: `MacCapture` snapshots the real cursor position
the instant suppression begins (`anchor`), and on every subsequent
`MouseMoved`/`*Dragged` event observed while still suppressed, actively
calls `CGDisplay::warp_mouse_cursor_position(anchor)` to force the
cursor back — regardless of whether disassociation alone actually held
it there on this macOS version or this specific input path (trackpad
vs. mouse-driven cursor rendering are documented to sometimes differ
in how faithfully they honor
`CGAssociateMouseAndMouseCursorPosition`, a plausible explanation for
the discrepancy, though not confirmed). `CGWarpMouseCursorPosition` is
documented to move the cursor without generating a new event, so this
cannot recurse into the tap callback. This required `last_position`
(previously a thread-local `Cell`, since only the capture callback
touched it) to become an `Arc<Mutex<..>>` field on `MacCapture`, since
`set_local_suppression` (called from a different thread) now needs to
read it to seed `anchor`.

**Update (2026-09-12): the active re-warp layer made things worse, and
was removed — plus one severe, unrelated safety bug found by the same
retest.** The real-hardware retest against this decision's active
re-warp layer did not confirm it — instead it surfaced two new,
serious findings.

**The active re-warp caused the Windows-side cursor to behave
"extremely out of control."** This layer used `CGWarpMouseCursorPosition`
(via `core-graphics`'s `CGDisplay::warp_mouse_cursor_position`) — a
different API than every other warp in this codebase, which all use
`CGEventPost` plus `SYNTHETIC_EVENT_MARKER` tagging (see decision 5).
`CGWarpMouseCursorPosition` is documented to move the cursor without
generating a new event, which is exactly why it looked attractive for
this: warping back on every suppressed move without needing to filter
out the resulting event. Real hardware evidence points at that
documented guarantee not holding on this macOS version for at least
one input path: every suppressed real move forcing a re-warp back to
`anchor` would, if the warp itself generated even an occasional
spurious `MouseMoved` event, feed a corrective delta back into the
exact same capture pipeline and forward it to the target as if it were
real input — compounding with every real move, which matches
"extremely out of control" far better than a simple constant offset or
a fixed scale error would. **Removed, not patched further**: the
disassociation + tap-drop mechanism from decision 9 was already
directly verified sufficient on its own (the 135-sample, 3-session
ground-truth measurement earlier in this decision) before the active
layer was ever added, so removing it costs nothing already proven to
work. `crates/input/src/macos/capture.rs` is back to exactly its
decision-9/10 shape (`last_position` reverted from `Arc<Mutex<..>>`
back to a plain thread-local `Cell`, since nothing outside the capture
thread needs to read it anymore).

**A severe, independent safety bug**: "terminated the connection on
Windows, still wasn't able to use my Mac keyboard and trackpad." Root
cause: `MessageStream::send` (`crates/net/src/framed.rs`) always maps
a broken connection to `NetError::Transport`, never the more specific
`NetError::ConnectionClosed` — so a target disconnecting abruptly
(the exact scenario this whole ADR's disconnect handling is supposed
to cover) makes the *next* captured event's `Session::apply` hit the
generic-error arm. That arm correctly calls `remove_peer` (forcing
`Router` back to `Local`) but then returns `Err`. The old
`Session::handle_captured` applied every effect with `?` inside its
loop, so that `Err` propagated straight out of the function *before*
ever reaching the suppression-restore check that follows the loop —
silently leaving local input suppressed forever, in direct violation
of this project's own explicit safety requirement that local input
must never stay disabled after a disconnect, crash, or transition
failure. Fixed by restructuring `handle_captured` to always evaluate
the suppression toggle against `Router`'s actual final state,
collecting (not `?`-propagating) any error from applying an individual
effect and returning it only afterward — the caller still sees the
error exactly as before, but the safety-critical restore step can no
longer be skipped by it. New regression test (a real, closed
loopback-`Peer` connection, not a fake):
`suppression_is_restored_even_when_the_disconnect_send_returns_a_generic_error`
in `session_end_to_end.rs`.

**Both fixes are subtractive/corrective, not additive** — the opposite
of decision 11's original framing. The active re-warp layer is gone;
decision 9's original two-layer mechanism (proven sufficient by direct
measurement) stands alone again. The disconnect-safety fix touches
`session.rs` only, not `router.rs`/`ownership.rs`. Phase 4 remains
open until a clean real-hardware retest confirms both original
symptoms (local-input leak, Windows pointer range) are gone *and* that
this fresh regression (chaotic Windows-side movement) is gone too.

### 12. A single restore path was not enough: `remove_peer` now restores suppression itself, plus a proactive watchdog

**Update (2026-09-12):** the disconnect-safety bug fixed above (decision
11's Update) was real, but the user's report of its actual impact was
worse than "an error surfaced" — with both keyboard and trackpad
suppressed and stuck, they had no way to reach Activity Monitor or a
terminal to even kill the process, and had to power-cycle the machine.
That is a fundamentally different severity than "a bug that gets fixed
in the next commit": *any* future bug in the one restore path (however
unlikely) would reproduce the exact same "locked out of your own
computer" failure, because the two things a person would normally use
to recover — the keyboard and the pointer — are precisely what's
suppressed. A single, cleverly-placed restore call is not an adequate
safety model for that failure class; redundancy is.

Two changes, both defensive, neither replacing the other:

1. **`Session::remove_peer` now restores suppression itself** (`crates/core/src/session.rs`),
   rather than relying solely on the caller (`handle_captured`) to
   compute a before/after comparison around it. Every code path that
   can force a `Forwarding` -> `Local` transition due to a peer going
   away — a captured event's send failing, or the new watchdog below —
   goes through this one function, so it's now the single place
   guaranteed to lift suppression whenever it fires, independent of
   how it was triggered. `handle_captured`'s own before/after check
   still runs too (for the ordinary edge-crossing-triggered case, which
   never touches `remove_peer` at all) — the two paths overlap
   harmlessly on the disconnect case (suppression gets "lifted" twice,
   which is idempotent) rather than leaving a gap between them.

2. **A proactive watchdog** (`crates/core/examples/edge_switch_relay.rs`):
   every other recovery path in this codebase — including the fix
   above — is *lazy*, triggered only as a side effect of the next
   captured input event failing to send. That has a real
   hardware-confirmed hole: if suppression has already left the user's
   keyboard and mouse unresponsive, no further captured event will
   *ever* arrive to trigger that lazy check — the failure disables the
   only thing that could report it. `Session::active_target_connection_closed`
   polls whether the transport has already reported the current
   target's connection closed (`quinn::Connection::close_reason`),
   with no send attempt needed, on the same 300ms tick already driving
   the ground-truth diagnostic sampler. If closed, it proactively calls
   `remove_peer` — forcing local input back with *zero* user action
   required. Combined with quinn's own ~10s idle timeout
   (`crates/net/src/config.rs`), this is a hard, bounded upper limit on
   how long local input can ever stay suppressed after a real
   disconnect, regardless of whether the user can interact with
   anything at all in the meantime.

New regression tests (both real, closed loopback `Peer` connections,
not fakes): `remove_peer` restoring suppression is asserted directly in
the existing `disconnect_then_reconnect_recovers_ownership_without_restarting_the_session`
test; `active_target_connection_closed_detects_a_closed_connection_with_no_send_attempt`
confirms the watchdog's detection works with no send ever attempted —
the exact scenario ("no further input will ever be captured to notice")
this decision exists to cover.

## Consequences

- `crates/core` gains `layout.rs`, `ownership.rs`, `router.rs`,
  `session.rs` — the first time `core` holds more than one `net::Peer`
  at a time. `crates/input` gains `PointerGeometry`, implemented for
  all three existing backends. `crates/protocol` gains one changed and
  one new `ControlMessage` variant.
- Automated coverage: 54 unit tests in `kvm-core` (`layout`/`ownership`/
  `router`, all pure, no I/O, including a dead-edge clamp-to-the-exact-
  boundary regression) plus 12 real-loopback-`Peer` integration tests
  in `crates/core/tests/session_end_to_end.rs` covering edge
  crossing + cursor warp, screen-size exchange, an unregistered device
  never becoming a target, disconnect/reconnect without restarting the
  session, modifier flush landing as ordinary input on the old target,
  no leakage to an inactive-but-connected peer, rapid back-and-forth
  re-warping, `RecenterLocal` actually reaching `PointerGeometry`, a
  single transient injection failure not killing the session,
  local-capture suppression toggling exactly on `Local`/`Forwarding`
  transitions (decision 9), and suppression being restored even when a
  disconnect-triggered send surfaces as a generic error rather than
  `ConnectionClosed`. None of this depends on physical hardware.
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
