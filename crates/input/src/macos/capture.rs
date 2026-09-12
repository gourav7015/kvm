//! macOS capture via `CGEventTap`. This file is the "thin shim" ADR-0007
//! describes: it converts `CGEvent`s to [`InputMessage`]s and runs the
//! OS event loop — no cross-platform translation logic lives here.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use core_foundation::base::{CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
use core_foundation::string::{CFString, CFStringRef};
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
    // Cursor hiding for a background process -- see `set_cursor_hidden`
    // and ADR-0009 decision 20. `CGSMainConnectionID` and
    // `CGSSetConnectionProperty` are private WindowServer calls (the same
    // ones Synergy/Barrier/Deskflow use for exactly this); `CGCursorIsVisible`
    // is public but not wrapped by `core-graphics`.
    fn CGSMainConnectionID() -> i32;
    fn CGSSetConnectionProperty(
        connection: i32,
        target_connection: i32,
        key: CFStringRef,
        value: CFTypeRef,
    ) -> i32;
    fn CGCursorIsVisible() -> i32;
}

/// Hides (`true`) or shows (`false`) the cursor on behalf of this process,
/// which is never the frontmost app (it runs from a terminal). See
/// ADR-0009 decision 20 for why this exists and the measurements behind it.
///
/// Measured on real hardware (macOS 26.6.2): `CGDisplayHideCursor` from a
/// background process has **no effect at all** (`CGCursorIsVisible` never
/// drops to 0) unless the connection first sets the WindowServer property
/// `SetsCursorInBackground`; with it set, hiding and showing both work,
/// from the main thread and from a secondary thread alike. And if the
/// process dies while the cursor is hidden — including a hard `abort()`
/// with no cleanup — the WindowServer restores it on its own: a fresh
/// process immediately sees it visible again. So a crash can never leave
/// the user with an invisible cursor.
///
/// Every failure here is logged, never fatal: at worst the cursor stays
/// visible, which is the pre-existing behaviour.
fn set_cursor_hidden(hidden: bool) {
    let display = CGDisplay::main();
    let result = if hidden {
        let key = CFString::from_static_string("SetsCursorInBackground");
        // SAFETY: `CGSMainConnectionID` takes no arguments and returns this
        // process's own WindowServer connection. `key` and the true
        // `CFBoolean` are valid CF objects that outlive the call, which
        // only reads them.
        let err = unsafe {
            CGSSetConnectionProperty(
                CGSMainConnectionID(),
                CGSMainConnectionID(),
                key.as_concrete_TypeRef(),
                CFBoolean::true_value().as_CFTypeRef(),
            )
        };
        if err != 0 {
            tracing::warn!(
                err,
                "setting SetsCursorInBackground failed -- the cursor may stay visible"
            );
        }
        display.hide_cursor()
    } else {
        display.show_cursor()
    };
    if let Err(e) = result {
        tracing::warn!(error = ?e, hidden, "changing cursor visibility failed");
    }
    // SAFETY: `CGCursorIsVisible` takes no arguments and only reads state.
    let visible = unsafe { CGCursorIsVisible() } != 0;
    if visible == hidden {
        tracing::warn!(
            hidden,
            visible,
            "cursor visibility did not change as requested"
        );
    } else {
        tracing::info!(hidden, visible, "cursor visibility changed");
    }
}

/// The cursor-visibility change a `set_local_suppression(suppress)` call
/// must make, given whether this capture currently has the cursor hidden:
/// `Some(hide?)`, or `None` for no change. Keeps every
/// `CGDisplayHideCursor` matched by exactly one `CGDisplayShowCursor` —
/// the WindowServer counts hides, so one unmatched hide would keep the
/// cursor hidden even after a later show, and repeated calls with the same
/// value (e.g. `Session::remove_peer` restoring an already-local state)
/// must not hide or show twice.
fn cursor_visibility_change(currently_hidden: bool, suppress: bool) -> Option<bool> {
    (currently_hidden != suppress).then_some(suppress)
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
/// **Two "active re-warp" attempts were tried and reverted** (see ADR-0009
/// decisions 11 and 13's Updates) — moving the cursor back after the
/// fact, via two different APIs, produced two different runaway-loop
/// failures. That class of fix (compensate after the OS already moved
/// the cursor) is abandoned. This backend instead makes sure the
/// association call that's supposed to *prevent* the movement in the
/// first place is actually applied where it can reliably take effect.
///
/// **Thread affinity (decision 14): the association call now runs on
/// the capture thread itself, continuously, not once from whichever
/// thread calls `set_local_suppression`.** `CGAssociateMouseAndMouseCursorPosition`
/// is a WindowServer-connected Core Graphics call; like several other
/// APIs in this family, its effect is tied to the specific
/// thread/run-loop context it's issued from. The previous
/// implementation called it directly from `set_local_suppression`'s
/// caller — an async-runtime worker thread with no relationship to the
/// dedicated thread that owns this tap's `CGEventTap`/`CFRunLoop` — a
/// real, previously-untried candidate for why an independent
/// ground-truth measurement showed the position frozen in one real
/// session while later hardware reports showed the cursor moving in
/// the same shape of session. `set_local_suppression` now only flips
/// the shared `suppress` flag; the capture thread's own callback
/// re-applies the association to match that flag on every observed
/// mouse-motion event — not merely once when it changes, so it's also
/// a standing hardening against the OS silently resetting the
/// association under conditions this project hasn't identified (a
/// focus change, Mission Control, etc.), rather than a one-shot call
/// that could silently stop holding.
#[derive(Default)]
pub struct MacCapture {
    run_loop: Option<CFRunLoop>,
    thread: Option<thread::JoinHandle<()>>,
    suppress: Arc<AtomicBool>,
    /// Whether this capture currently has the cursor hidden — see
    /// `set_cursor_hidden` and ADR-0009 decision 20. Disassociation keeps
    /// the WindowServer's cursor position frozen while forwarding, but real
    /// hardware showed the user still seeing an arrow move; hiding the
    /// cursor for the duration of a `Forwarding` session is what
    /// established KVM tools do on macOS.
    cursor_hidden: bool,
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
                // state from a previous start()/stop() cycle never leaks
                // into this one. A `Cell` rather than a plain local:
                // `CGEventTap::new` requires an `Fn` callback, not
                // `FnMut`, so the closure only ever touches this through
                // a shared reference.
                let last_flags = Cell::new(CGEventFlags::empty());
                // Where this process's own most recent cursor warp
                // landed, held for exactly one following real event --
                // see `events::to_input_message` and ADR-0009 decision
                // 18. Fresh per session for the same reason as
                // `last_flags`.
                let post_warp_anchor: Cell<Option<core_graphics::geometry::CGPoint>> =
                    Cell::new(None);
                // `None` until the first mouse-motion event applies an
                // initial association state; used only to decide whether
                // a change is worth a log line (see decision 14) -- the
                // association call itself is unconditional every time.
                let last_applied_association: Cell<Option<bool>> = Cell::new(None);

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
                        let mut anchor = post_warp_anchor.get();
                        let message = to_input_message(event_type, event, &mut flags, &mut anchor);
                        last_flags.set(flags);
                        post_warp_anchor.set(anchor);
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
                        // See the struct doc (decision 14): applied here,
                        // on this tap's own thread, on *every*
                        // mouse-motion event -- not once from whichever
                        // thread calls `set_local_suppression`, and not
                        // only when `suppressed` changes. Reapplying
                        // continuously (rather than once per transition)
                        // is a deliberate hardening against the OS
                        // silently resetting the association for reasons
                        // this project hasn't identified -- there is no
                        // independent way to detect such a reset, so the
                        // only robust defense is to never stop asserting
                        // the intended state. The log line is still only
                        // emitted on an actual change, so a healthy
                        // session isn't flooded.
                        if is_mouse_motion {
                            match CGDisplay::associate_mouse_and_mouse_cursor_position(!suppressed)
                            {
                                Ok(()) => {
                                    if last_applied_association.replace(Some(!suppressed))
                                        != Some(!suppressed)
                                    {
                                        tracing::info!(
                                            suppressed,
                                            connected = !suppressed,
                                            "CGAssociateMouseAndMouseCursorPosition applied from capture thread"
                                        );
                                    }
                                }
                                Err(e) => tracing::warn!(
                                    error = ?e,
                                    suppressed,
                                    "CGAssociateMouseAndMouseCursorPosition failed"
                                ),
                            }
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
        // Same reasoning: never leave the cursor hidden behind a stopped
        // capture (the WindowServer also restores it if the process dies).
        if let Some(hide) = cursor_visibility_change(self.cursor_hidden, false) {
            set_cursor_hidden(hide);
            self.cursor_hidden = hide;
        }
    }

    fn set_local_suppression(&mut self, suppress: bool) {
        self.suppress.store(suppress, Ordering::Relaxed);
        tracing::info!(suppress, "set_local_suppression called");
        // Hidden for exactly the duration of local suppression -- see
        // ADR-0009 decision 20. Measured to work from any thread, so the
        // caller's (async worker) thread is fine here.
        if let Some(hide) = cursor_visibility_change(self.cursor_hidden, suppress) {
            set_cursor_hidden(hide);
            self.cursor_hidden = hide;
        }
        // Best-effort immediate attempt, in addition to (not instead
        // of) the capture thread's own continuous reapplication on
        // every mouse-motion event -- see decision 14 (struct doc). If
        // this call's thread-affinity concern is real, the capture
        // thread's reapplication is what actually matters and covers
        // the very next motion event regardless; if it isn't, this
        // proactive call covers the narrow window between the switch
        // and whatever motion event happens next, at zero cost either
        // way.
        if let Err(e) = CGDisplay::associate_mouse_and_mouse_cursor_position(!suppress) {
            tracing::warn!(
                error = ?e,
                suppress,
                "immediate CGAssociateMouseAndMouseCursorPosition attempt failed \
                 (non-fatal -- the capture thread reapplies this on the next motion event)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppression_hides_the_cursor_once_and_unsuppression_shows_it_once() {
        assert_eq!(cursor_visibility_change(false, true), Some(true));
        assert_eq!(cursor_visibility_change(true, false), Some(false));
    }

    /// A repeated call with the same value must never hide or show a
    /// second time: the WindowServer counts hides, so an extra hide would
    /// outlive the next show and leave the cursor invisible after
    /// forwarding ends.
    #[test]
    fn repeating_the_same_suppression_state_changes_nothing() {
        assert_eq!(cursor_visibility_change(true, true), None);
        assert_eq!(cursor_visibility_change(false, false), None);
    }
}
