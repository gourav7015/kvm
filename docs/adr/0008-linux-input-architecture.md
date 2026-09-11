# ADR-0008: Linux Input Architecture — X11 GO, Wayland NO-GO for General Capture

## Status

Accepted (2026-09-11)

## Context

Phase 3 (ADR-0007) built and hardware-verified the macOS↔Windows input
engine, deferring Linux entirely — X11 and Wayland are different enough
display-server architectures, with different security models, that
neither should be assumed to work like the other or like macOS/Windows.
Phase 3b's job is a spike: investigate both, prototype the one that's
actually feasible, and make a formal, evidenced GO/NO-GO call for each
rather than silently discovering a gap later or shipping a fragile
workaround.

`input`'s existing `Capture`/`Inject` trait boundary (ADR-0007 §2) and
the normalized `Key`/`PlatformKind` wire representation (ADR-0007 §1)
were designed to be platform-agnostic already — the question for this
ADR is purely "what goes behind the trait on Linux," not whether the
trait itself needs to change. `PlatformKind::Linux` already existed in
`protocol` since Phase 1a; this ADR is the first time anything actually
produces or consumes it from a real Linux backend.

## Decisions

### 1. X11: GO — XInput2 raw events for capture, XTEST for injection

Verified against the actual `x11rb` 0.13.2 crate source (protocol
struct field layouts, extension `ConnectionExt` method names, event
opcodes) and the XTEST 2.2 protocol spec, not just docs — same
discipline as ADR-0007's macOS/Windows research.

**Capture** uses XInput2's *raw* events (`XI_RawKeyPress`,
`XI_RawKeyRelease`, `XI_RawButtonPress`, `XI_RawButtonRelease`,
`XI_RawMotion`), selected on the root window for `XIAllDevices` via
`XISelectEvents`. This is the correct modern choice, not
`XGrabKeyboard`/`XGrabPointer` (the older API): grabs are *exclusive* —
taking one steals input from every other application on the machine
for as long as the grab holds, which is catastrophic for a background
listen-only capture tool (the user couldn't use their own Linux machine
normally while it ran). Raw XI2 events are explicitly designed for
passive, non-exclusive, system-wide observation — the same shape as
macOS's `CGEventTap` in `ListenOnly` mode and Windows' `WH_KEYBOARD_LL`/
`WH_MOUSE_LL` hooks. (The older XRecord extension is a legacy
alternative some tools still use for the same passive-observation goal;
XInput2 raw events are the currently-recommended mechanism and were
chosen over it.)

**Injection** uses the XTEST extension's `FakeInput` request
(`XTestFakeInput`) — the standard, decades-stable mechanism every
X11 automation tool (`xdotool` included) uses. Motion is injected as
*relative* (`detail = 1`), matching the relative-delta shape
`InputMessage::MouseMove` already carries from the macOS/Windows
backends — no protocol change needed.

**Keycodes are not portable; keysyms are.** Unlike macOS's Carbon
keycodes or Windows' `VK_*` constants — both fixed, documented values
independent of the specific machine — an X11 *keycode* is an arbitrary
number assigned by that particular X server's keyboard driver, with no
fixed meaning across machines or even across sessions on the same
machine. Only the *keysym* layer (`XK_a`, `XK_Shift_L`, etc., from
`X11/keysymdef.h`, stable for decades) is portable. So X11's "thin
shim" splits into two pieces where macOS/Windows only needed one:
- `x11::keysym` — pure, fully unit-tested, keysym↔`Key` (exactly
  parallel to `macos::keycode`/`windows::keycode`).
- `x11::keymap` — a keycode↔keysym table, queried once per
  capture/inject session via the core protocol's
  `GetKeyboardMapping` request. This is the actual "thin shim" (not
  unit-tested against a real server, consistent with ADR-0007's
  HAL-split rationale) — it exists purely to bridge "the wire protocol
  gives us raw keycodes" to "normalization needs keysyms."

**Known limitation, documented not silently accepted:** the keymap is
a snapshot taken at `start()`/construction time. If the keyboard layout
changes *while* a capture or inject session is running (switching
layouts mid-session), the table goes stale until the next restart. This
is a real gap for the "thinnest possible" spike implementation, not a
correctness bug in what it does handle — recorded here rather than
quietly left unmentioned.

**Dependency:** `x11rb` 0.13.2 (`MIT OR Apache-2.0`, actively
maintained, the de facto standard safe X11 binding for Rust), with only
the `xinput` and `xtest` extension features enabled — not
`all-extensions`. Uses `RustConnection` (the pure-Rust, socket-based
connection), so none of the optional `libxcb`-interop features (which
would pull in unsafe FFI and `libc`) are needed at all. X11-only —
gated behind `cfg(target_os = "linux")` in `crates/input/Cargo.toml`,
isolated from every other platform's dependencies, matching the
existing macOS/Windows pattern.

**Security:** no elevated privileges beyond ordinary X client access —
same permissive model X11 has always had (any client with a connection
to the display can, by design, observe and synthesize input for any
other client; this is a property of X11 itself, not something this
code opts into or could opt out of while still functioning). This is
markedly more permissive than Wayland's model — see decision 2. Input
still only ever flows over an already-authenticated `net::Peer`
(ADR-0007 §7, unchanged) — X11's own permissiveness doesn't touch that
boundary at all.

### 2. Wayland: NO-GO for general-purpose global capture

Researched current (2026) compositor and portal support before writing
any code, rather than assuming Wayland "just needs a different API" the
way X11 vs. the others did.

Wayland's security model deliberately does **not** allow an ordinary
application to passively observe system-wide keyboard/mouse input the
way X11, macOS, and Windows all permit — this is intentional, specifically
to prevent arbitrary applications from acting as keyloggers. The
sanctioned mechanism for *capture* is `org.freedesktop.portal.InputCapture`,
working with `libei` as the actual event transport (both landed together
in `xdg-desktop-portal` 1.17, mid-2023). As of this research (2026):
KWin (KDE's compositor) does **not** implement `InputCapture` even in
the KDE 6 release line — only the separate `RemoteDesktop` portal
shipped there. Mutter (GNOME's compositor) has some of the underlying
plumbing but `InputCapture` support is not broadly available there
either — the infrastructure exists mainly in service of `RemoteDesktop`,
not general input capture. Notably, the COSMIC desktop's portal
backend has an open, unresolved feature request specifically asking for
`InputCapture` "for Software KVM Support" — i.e., the exact use case
this project needs is a known, currently-unmet gap across the
ecosystem, not a solved problem this project failed to find.

**Injection** is in meaningfully better shape: `org.freedesktop.portal.RemoteDesktop`
(also using `libei`/`libeis` as transport) is shipped by both GNOME
(`xdg-desktop-portal-gnome`) and KDE (`xdg-desktop-portal-kde`), and is
what tools like GNOME's own built-in remote-desktop server and
RustDesk use for unprivileged Wayland input injection. But it comes
with real constraints incompatible with this project's "background
input engine" shape: it requires an explicit, per-session **user
consent dialog** — by design, not a bug or an oversight — so it cannot
be a silent, always-on background service the way the X11/macOS/
Windows backends are. `libei`'s own documentation is explicit that
input which must bypass that consent prompt belongs at the kernel
layer (`/dev/uinput`) instead — i.e., the portal's designers
deliberately did not build a bypass, and this project will not build
one either (per this phase's explicit instruction not to introduce
privileged mechanisms to force a GO).

**Decision:** since a general-purpose Universal KVM fundamentally needs
the *capture* side (a Linux machine acting as the keyboard/mouse
*source*, not just a target), and `InputCapture` is not reliably
available across the compositors that matter (GNOME, KDE), Wayland is
a **NO-GO for the capture half** of this project's requirements under
normal, non-privileged constraints, today. Injection alone is closer to
CONDITIONAL-GO territory (portal support exists on the two major
desktops) but isn't pursued in this phase, since an inject-only Linux
backend doesn't satisfy the bidirectional KVM use case Phase 3
established as the actual target, and would be new, non-trivial
dependency surface (`ashpd` + D-Bus portal session negotiation +
`libei`) for half a feature. This is recorded as a deliberate,
evidenced architectural limitation, not a silently-dropped feature —
revisit if/when `InputCapture` gains real compositor coverage (the
COSMIC feature request above is one signal to watch).

No Wayland code was written this phase — `crates/input/src/x11` is
named for the mechanism, not "linux" generically, specifically so a
future Wayland backend (if the ecosystem gets there) is a genuinely
separate module behind the same `Capture`/`Inject` traits, not a
variant of the X11 one.

### 3. Linux requires (at least) two backends, selected how

Confirmed explicitly, matching the instruction not to assume otherwise:
X11 and Wayland need entirely different mechanisms (raw XI2 events vs.
a portal+`libei` session), different dependencies, and have different
GO statuses. Right now there is exactly one Linux backend (`x11`),
`cfg`-gated to `target_os = "linux"` the same simple way macOS/Windows
are gated to their own OS — no runtime X11-vs-Wayland detection exists
yet because there is only one implementation to select. If a Wayland
backend is ever built, `input` will need a runtime check (e.g.
inspecting `XDG_SESSION_TYPE` / whether a Wayland display is reachable)
to choose between them, since both could be compiled into the same
Linux binary — that selection logic doesn't exist yet and is explicitly
out of scope until there's a second backend to select between.

### 4. `Key` gained `Hash`

A small, purely-additive protocol change: `#[derive(Hash)]` added to
`protocol::Key` (alongside its existing `Debug, Clone, Copy, PartialEq,
Eq, Serialize, Deserialize`), needed so `x11::keymap`'s reverse
(`Key` → keycode) lookup can use a `HashMap` instead of a linear scan.
Costs nothing for macOS/Windows — it's an additive derive, not a
behavior or wire-format change — and is the only modification this
phase made to any pre-existing Phase 1–3 code.

## Consequences

- `crates/input` gains an `x11` module (capture, inject, keysym table,
  runtime keymap, event conversion), `cfg`-gated to `target_os =
  "linux"`, following the exact "thin shim + pure translation logic"
  split ADR-0007 established. `x11rb` is the only new dependency,
  isolated to Linux via `[target.'cfg(target_os = "linux")'.dependencies]`.
- No Wayland code exists. `docs/manual-qa/phase-3b-linux-input.md`
  documents exactly what was and wasn't demonstrated, and what a real
  Linux machine still needs to verify by hand (this spike's code has
  been compiled and clippy-checked via `cargo check`/`clippy --target
  x86_64-unknown-linux-gnu`, cross-compiled from macOS — it has not yet
  run against a real X server, since this development machine has no
  Linux display server to test against directly).
- `translate_for_target` needed no changes: it already treated
  `PlatformKind::Linux` identically to `PlatformKind::Windows`
  (Windows/Linux both use Control as the primary shortcut modifier,
  only macOS differs) since Phase 3's original design — X11 input
  flowing through the same translation path Windows already proved
  requires nothing new there.
- Keyboard-layout changes mid-session on X11 are a known, documented
  gap (see decision 1) — not silently mishandled, but not solved either;
  a real fix would re-query `KeyMap` on an `XkbNewKeyboardNotify`/
  `MappingNotify` event, deferred as future work.
- The Windows UAC-style "what happens with elevated windows" question
  has an X11 analogue worth flagging for future manual QA, not
  investigated in this spike: X11's permissive model means there's no
  equivalent restriction to test — any X client can talk to any window
  regardless of the owning process's privilege level. This is a
  property of X11 the manual QA doc should note as expected behavior,
  not something Phase 3b needed to prove.
