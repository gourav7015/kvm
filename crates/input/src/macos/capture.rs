//! macOS capture via `CGEventTap`. This file is the "thin shim" ADR-0007
//! describes: it converts `CGEvent`s to [`InputMessage`]s and runs the
//! OS event loop — no cross-platform translation logic lives here.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CallbackResult,
};
use core_graphics::geometry::CGPoint;

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
/// **Third, active layer**: real hardware QA reported the cursor still
/// visibly moving even with the above two mechanisms in place, despite
/// direct, independent measurement (reading the same `CGEvent::location()`
/// the WindowServer itself renders from) showing the position genuinely
/// frozen for the exact same session. Rather than trust either signal
/// alone, this backend also actively fights back: while suppressed,
/// every observed `MouseMoved`/`*Dragged` event triggers an immediate
/// [`CGDisplay::warp_mouse_cursor_position`] back to `anchor` (the
/// position captured the instant suppression began). `CGWarpMouseCursorPosition`
/// is documented to move the cursor *without* generating a new event,
/// so this cannot recurse into the tap callback again. This makes
/// suppression correct even if some future macOS release (or some
/// input path this project hasn't found yet, e.g. trackpad-specific
/// smoothing) turns out not to fully honor the association toggle on
/// its own.
#[derive(Default)]
pub struct MacCapture {
    run_loop: Option<CFRunLoop>,
    thread: Option<thread::JoinHandle<()>>,
    suppress: Arc<AtomicBool>,
    /// The most recently observed real cursor position, shared with
    /// `set_local_suppression` (called from a different thread than the
    /// capture callback) so it can snapshot the position suppression
    /// began at.
    last_position: Arc<Mutex<Option<CGPoint>>>,
    /// Set to `Some` the instant suppression begins (a snapshot of
    /// `last_position` at that moment), cleared when it ends. `None`
    /// while not suppressed.
    anchor: Arc<Mutex<Option<CGPoint>>>,
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
        let last_position = Arc::clone(&self.last_position);
        let anchor = Arc::clone(&self.anchor);
        // A fresh start() must never inherit a stale reading from a
        // previous start()/stop() cycle.
        *last_position.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;

        let join_handle = thread::Builder::new()
            .name("kvm-input-macos-capture".to_string())
            .spawn(move || {
                let events_of_interest = crate::macos::events::CAPTURED_EVENT_TYPES.to_vec();
                // Fresh per capture session, so a stale modifier-held
                // state from a previous start()/stop() cycle never leaks
                // into this one. A `Cell` rather than a plain local:
                // `CGEventTap::new` requires an `Fn` callback, not
                // `FnMut`, so the closure only ever touches this through
                // a shared reference.
                let last_flags = Cell::new(CGEventFlags::empty());

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
                        // Locking on every event is fine at real input
                        // rates (this is the same cadence `suppress`'s
                        // atomic load already runs at); a poisoned lock
                        // (a panic while held, which nothing here ever
                        // does) falls back to the poisoned guard's data
                        // rather than losing capture entirely.
                        let mut position_guard =
                            last_position.lock().unwrap_or_else(|e| e.into_inner());
                        let mut position = *position_guard;
                        let message =
                            to_input_message(event_type, event, &mut flags, &mut position);
                        last_flags.set(flags);
                        *position_guard = position;
                        drop(position_guard);
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
                        let is_mouse_motion = matches!(
                            event_type,
                            CGEventType::MouseMoved
                                | CGEventType::LeftMouseDragged
                                | CGEventType::RightMouseDragged
                                | CGEventType::OtherMouseDragged
                        );
                        if is_mouse_motion
                            || matches!(
                                event_type,
                                CGEventType::KeyDown
                                    | CGEventType::KeyUp
                                    | CGEventType::FlagsChanged
                            )
                        {
                            tracing::info!(
                                ?event_type,
                                suppressed,
                                disposition = if suppressed { "drop" } else { "keep" },
                                "tap callback observed event"
                            );
                        }
                        if suppressed {
                            if is_mouse_motion {
                                // Active third layer -- see the struct
                                // doc. Force the cursor straight back to
                                // where it was when suppression began,
                                // regardless of whether disassociation
                                // alone actually held it there. Warping
                                // (not `CGEventPost`) generates no new
                                // event, so this cannot re-enter this
                                // callback.
                                if let Some(anchor_point) =
                                    *anchor.lock().unwrap_or_else(|e| e.into_inner())
                                {
                                    match CGDisplay::warp_mouse_cursor_position(anchor_point) {
                                        Ok(()) => {
                                            *last_position
                                                .lock()
                                                .unwrap_or_else(|e| e.into_inner()) =
                                                Some(anchor_point);
                                        }
                                        Err(e) => tracing::warn!(
                                            error = ?e,
                                            "failed to actively re-anchor the cursor while suppressed"
                                        ),
                                    }
                                }
                            }
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
        // (disassociated, or pinned by the active re-anchor) if capture
        // stops while a `Forwarding` session was still active.
        self.suppress.store(false, Ordering::Relaxed);
        *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
    }

    fn set_local_suppression(&mut self, suppress: bool) {
        self.suppress.store(suppress, Ordering::Relaxed);
        tracing::info!(suppress, "set_local_suppression called");
        if suppress {
            // Snapshot where the cursor is *right now* as the anchor the
            // active re-warp (see the struct doc) will hold it at for
            // the rest of this Forwarding session. Read after
            // RecenterLocal's own warp has already landed (that effect
            // is applied before this call -- see
            // `Session::handle_captured`), so this is the recentered
            // position, not wherever the triggering edge crossing left
            // it.
            let current = *self.last_position.lock().unwrap_or_else(|e| e.into_inner());
            *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) = current;
        } else {
            *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
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
