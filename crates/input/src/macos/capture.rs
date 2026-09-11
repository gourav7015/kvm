//! macOS capture via `CGEventTap`. This file is the "thin shim" ADR-0007
//! describes: it converts `CGEvent`s to [`InputMessage`]s and runs the
//! OS event loop — no cross-platform translation logic lives here.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult,
};

use crate::error::InputError;
use crate::macos::events::to_input_message;
use crate::macos::permission::has_accessibility_permission;
use crate::traits::Capture;

// `core-graphics` 0.25.0 keeps its own `CGEventTapEnable` FFI binding
// module-private and only exposes it via `CGEventTap::enable(&self)` --
// which needs the full, owned `CGEventTap`, unavailable from inside its
// own 'static callback (the classic bootstrapping problem: the callback
// is constructed *before* `CGEventTap::new` returns the tap it would
// need to borrow). `CGEventTapEnable` is a stable, public macOS API
// (ApplicationServices/CoreGraphics), already linked transitively by
// `core-graphics` itself, so it's declared directly here instead.
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapEnable(tap: CGEventTapProxy, enable: bool);
}

/// Global, low-level capture of this machine's keyboard and mouse via a
/// `CGEventTap`. Requires Accessibility permission — see
/// [`has_accessibility_permission`] and ADR-0007.
///
/// The tap is created with `CGEventTapOptions::Default` (not
/// `ListenOnly`) so it is *capable* of blocking events, but by default
/// behaves exactly like a listen-only tap: every captured event is
/// still passed through (`CallbackResult::Keep`) unless
/// [`set_local_suppression`](Capture::set_local_suppression) has been
/// called with `true`. See ADR-0009 decision 9 for why: real hardware
/// QA found that a machine forwarding input to another device still
/// visibly/audibly acted on it locally too (the cursor kept moving,
/// keystrokes kept landing in whatever Mac app had focus) — genuinely
/// disruptive, not merely cosmetic, so this backend now suppresses
/// local delivery for the duration of a `Forwarding` session.
///
/// **Suppression is two real, separate mechanisms, not one** — a
/// second real-hardware finding on top of the first: dropping the
/// `CGEvent` (via the tap callback returning `CallbackResult::Drop`)
/// stops *event delivery* to other apps, which is sufficient for
/// keyboard input, but does **not** stop the WindowServer's own cursor
/// position from tracking raw HID mouse motion — that tracking runs
/// independently of whatever any single event tap decides to do with
/// the resulting `CGEvent`. Actually freezing the visible cursor needs
/// [`CGDisplay::associate_mouse_and_mouse_cursor_position`] set to
/// `false`, the same API used for exactly this purpose by tools that
/// need raw relative mouse deltas without the OS cursor visibly moving
/// (e.g. games reading "mouse look" input) — HID motion (and therefore
/// this tap's own `MouseMoved` events) keeps flowing normally while
/// disassociated, so forwarding is unaffected; only the local cursor's
/// on-screen position stops updating.
///
/// **A third, "active re-warp" layer was tried and reverted** (see
/// ADR-0009 decision 11's Update): it used `CGWarpMouseCursorPosition`
/// to forcibly snap the cursor back to an anchor on every suppressed
/// move, on the theory that disassociation alone might not be honored
/// on every input path (trackpad vs. mouse). Real hardware QA
/// afterward reported the *target* machine's cursor behaving
/// "extremely out of control" — consistent with that warp not actually
/// being the zero-event operation its documentation claims on this
/// macOS version, feeding spurious corrective deltas into the exact
/// same capture pipeline and forwarding them as if they were real
/// input. Removed rather than patched further: the two mechanisms
/// below were already directly verified sufficient by an independent
/// ground-truth measurement (reading real OS cursor position on a
/// fixed cadence across three live `Forwarding` sessions, frozen every
/// time) before the third layer was ever added.
#[derive(Default)]
pub struct MacCapture {
    run_loop: Option<CFRunLoop>,
    thread: Option<thread::JoinHandle<()>>,
    suppress: Arc<AtomicBool>,
}

impl MacCapture {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Capture for MacCapture {
    fn start(&mut self, sink: mpsc::Sender<kvm_protocol::InputMessage>) -> Result<(), InputError> {
        if !has_accessibility_permission() {
            return Err(InputError::PermissionDenied(
                "Accessibility permission not granted for this process — grant it in \
                 System Settings > Privacy & Security > Accessibility, then restart"
                    .to_string(),
            ));
        }

        let (setup_tx, setup_rx) = mpsc::channel::<Result<CFRunLoop, String>>();
        let suppress = Arc::clone(&self.suppress);

        let join_handle = thread::Builder::new()
            .name("kvm-input-macos-capture".to_string())
            .spawn(move || {
                let events_of_interest = crate::macos::events::CAPTURED_EVENT_TYPES.to_vec();
                // Fresh per capture session, so a stale modifier-held
                // state (or a stale absolute cursor reading — see
                // ADR-0009) from a previous start()/stop() cycle never
                // leaks into this one. A `Cell` rather than a plain
                // local: `CGEventTap::new` requires an `Fn` callback, not
                // `FnMut`, so the closure only ever touches this through
                // a shared reference.
                let last_flags = Cell::new(CGEventFlags::empty());
                let last_position: Cell<Option<core_graphics::geometry::CGPoint>> = Cell::new(None);

                let tap = CGEventTap::new(
                    CGEventTapLocation::HID,
                    CGEventTapPlacement::HeadInsertEventTap,
                    // `Default`, not `ListenOnly` -- see the struct doc:
                    // this tap must be *capable* of dropping an event
                    // (ADR-0009 decision 9), even though by default (see
                    // the `suppress` check below) it keeps every event.
                    CGEventTapOptions::Default,
                    events_of_interest,
                    move |proxy, event_type, event| {
                        // Real hardware finding (see ADR-0009's Update
                        // note): macOS can silently disable an event tap
                        // under load (timeout) or on user request, and
                        // delivers this notification regardless of the
                        // tap's registered event mask. Undetected, the
                        // OS's own cursor keeps moving completely
                        // normally (this is a listen-only tap, nothing
                        // else depends on it) while this process simply
                        // stops receiving *any* further captured events
                        // -- exactly the "real cursor moves fine, but
                        // tracked position silently stops updating and
                        // never catches up" symptom real hardware QA
                        // hit. Must be re-enabled explicitly; the OS
                        // does not do this on its own.
                        if matches!(
                            event_type,
                            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                        ) {
                            tracing::warn!(
                                ?event_type,
                                "CGEventTap was disabled by the OS -- re-enabling immediately"
                            );
                            // SAFETY: `proxy` is the tap's own proxy for
                            // this exact callback invocation, handed to
                            // us by CGEventTapCreate/the run loop for
                            // this purpose.
                            unsafe { CGEventTapEnable(proxy, true) };
                            return CallbackResult::Keep;
                        }
                        let mut flags = last_flags.get();
                        let mut position = last_position.get();
                        let message =
                            to_input_message(event_type, event, &mut flags, &mut position);
                        last_flags.set(flags);
                        last_position.set(position);
                        if let Some(message) = message {
                            // A full channel or a dropped receiver just
                            // means "no one is listening anymore" — not a
                            // capture failure worth surfacing per-event.
                            let _ = sink.send(message);
                        }
                        // Reported to the sink either way (so the
                        // forwarding target always gets it) -- only
                        // whether this machine's *own* OS also acts on
                        // it depends on `suppress` (ADR-0009 decision 9).
                        let suppressed = suppress.load(Ordering::Relaxed);
                        // TEMPORARY diagnostic tracing (Bug 9 root-cause
                        // hunt): direct, per-event runtime proof of (a)
                        // whether the callback is actually observing this
                        // event at all while forwarding, and (b) what
                        // disposition it actually chose -- not merely
                        // that the suppression code path was reached.
                        // Restrict to the event kinds suppression cares
                        // about so a normal Local session isn't flooded.
                        if matches!(
                            event_type,
                            CGEventType::MouseMoved
                                | CGEventType::LeftMouseDragged
                                | CGEventType::RightMouseDragged
                                | CGEventType::OtherMouseDragged
                                | CGEventType::KeyDown
                                | CGEventType::KeyUp
                                | CGEventType::FlagsChanged
                        ) {
                            tracing::info!(
                                ?event_type,
                                suppressed,
                                disposition = if suppressed { "drop" } else { "keep" },
                                "tap callback observed event"
                            );
                        }
                        if suppressed {
                            CallbackResult::Drop
                        } else {
                            CallbackResult::Keep
                        }
                    },
                );

                let tap = match tap {
                    Ok(tap) => tap,
                    Err(()) => {
                        let _ = setup_tx.send(Err(
                            "failed to create CGEventTap (permission revoked mid-startup?)"
                                .to_string(),
                        ));
                        return;
                    }
                };

                let source = match tap.mach_port().create_runloop_source(0) {
                    Ok(source) => source,
                    Err(()) => {
                        let _ = setup_tx.send(Err("failed to create run loop source".to_string()));
                        return;
                    }
                };

                let run_loop = CFRunLoop::get_current();
                // SAFETY: `kCFRunLoopDefaultMode` is a static CF constant
                // provided by the framework; reading it is always sound.
                run_loop.add_source(&source, unsafe { kCFRunLoopDefaultMode });

                if setup_tx.send(Ok(run_loop)).is_err() {
                    // Caller already gave up waiting — nothing to run for.
                    return;
                }

                CFRunLoop::run_current();
            })
            .map_err(|e| InputError::CaptureFailed(e.to_string()))?;

        match setup_rx.recv() {
            Ok(Ok(run_loop)) => {
                self.run_loop = Some(run_loop);
                self.thread = Some(join_handle);
                Ok(())
            }
            Ok(Err(message)) => Err(InputError::CaptureFailed(message)),
            Err(_) => Err(InputError::CaptureFailed(
                "capture thread exited before completing setup".to_string(),
            )),
        }
    }

    fn stop(&mut self) {
        if let Some(run_loop) = self.run_loop.take() {
            run_loop.stop();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // A fresh start() must never inherit a suppressed state from a
        // previous session -- same reasoning as `last_flags`/`last_position`
        // above. Also make sure the cursor is never left frozen behind
        // (disassociated) if capture stops while a `Forwarding` session
        // was still active.
        self.suppress.store(false, Ordering::Relaxed);
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
    }

    fn set_local_suppression(&mut self, suppress: bool) {
        self.suppress.store(suppress, Ordering::Relaxed);
        // TEMPORARY diagnostic tracing (Bug 9 root-cause hunt): proves
        // this method was actually called (and with what value) --
        // "the code path executed" is not itself evidence the OS
        // behavior changed, but its absence would immediately rule out
        // "never called" as the cause.
        tracing::info!(suppress, "set_local_suppression called");
        // See the struct doc: dropping the CGEvent alone does not stop
        // the OS from moving the visible cursor in response to raw HID
        // motion -- that needs this separate association toggle. Logged
        // on both success and failure (not just failure) so a passing
        // Ok(()) here can be directly checked against whether the real
        // cursor position actually stopped moving (a separate,
        // independent measurement -- see edge_switch_relay.rs).
        match CGDisplay::associate_mouse_and_mouse_cursor_position(!suppress) {
            Ok(()) => tracing::info!(
                suppress,
                connected = !suppress,
                "CGAssociateMouseAndMouseCursorPosition succeeded"
            ),
            Err(e) => tracing::warn!(
                error = ?e,
                suppress,
                "CGAssociateMouseAndMouseCursorPosition failed"
            ),
        }
    }
}
