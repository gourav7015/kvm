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
    CGEvent, CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
    CGEventTapProxy, CGEventType, CGMouseButton, CallbackResult, EventField,
};
use core_graphics::geometry::CGPoint;

use crate::error::InputError;
use crate::macos::events::{SYNTHETIC_EVENT_MARKER, is_our_own_synthetic_event, to_input_message};
use crate::macos::inject::event_source;
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

/// Posts a synthetic, absolute mouse-move `CGEvent` to `point`, tagged
/// with [`SYNTHETIC_EVENT_MARKER`] so it's recognized as our own warp
/// rather than real motion — the exact same posting technique
/// `MacInject::set_cursor_position` already uses for `RecenterLocal`
/// and the target-side `SwitchActive` warp, reused here instead of
/// `CGWarpMouseCursorPosition` (see the struct doc for why).
fn post_synthetic_warp(point: CGPoint) -> Result<(), InputError> {
    let source = event_source()?;
    let cg_event =
        CGEvent::new_mouse_event(source, CGEventType::MouseMoved, point, CGMouseButton::Left)
            .map_err(|()| {
                InputError::InjectFailed("failed to create mouse-move CGEvent".to_string())
            })?;
    cg_event.set_integer_value_field(EventField::EVENT_SOURCE_USER_DATA, SYNTHETIC_EVENT_MARKER);
    cg_event.post(CGEventTapLocation::HID);
    Ok(())
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
/// **Suppression is three real, layered mechanisms** — real hardware
/// QA repeatedly disagreed with what a single mechanism should have
/// produced, so each layer is independently real, not decorative:
///
/// 1. Dropping the `CGEvent` (`CallbackResult::Drop`) stops *event
///    delivery* to other apps — sufficient for keyboard input.
/// 2. [`CGDisplay::associate_mouse_and_mouse_cursor_position`] set to
///    `false` is the documented way to stop the WindowServer's cursor
///    position from tracking raw HID motion while still letting this
///    tap's own `MouseMoved` events flow normally. Directly measured
///    sufficient on its own in one real hardware round (135 samples of
///    the OS's own reported cursor position, frozen throughout three
///    live `Forwarding` sessions) — but a *later* round reported the
///    cursor visibly moving again in the same shape of session, an
///    unresolved discrepancy between measurement and observation this
///    ADR does not fully explain (candidate: trackpad-driven cursor
///    rendering not fully honoring the association toggle on every
///    macOS version — see ADR-0009 decision 13's Update).
/// 3. **Active correction, using the codebase's already-proven warp
///    technique.** An earlier attempt at this (decision 11) used
///    `CGWarpMouseCursorPosition`, untested elsewhere in this codebase,
///    and real hardware QA found it made things *worse* (the target
///    machine's cursor went "extremely out of control") — consistent
///    with that API's "generates no event" documentation not holding
///    on this macOS version, feeding spurious corrective deltas into
///    the same capture pipeline and forwarding them as real input.
///    This version instead reuses the exact `CGEventPost` +
///    `SYNTHETIC_EVENT_MARKER` pattern already proven correct for
///    `RecenterLocal` and the target-side `SwitchActive` warp: post an
///    absolute move to `anchor` (the position when suppression began),
///    tagged as synthetic. The resulting event *does* arrive back at
///    this same tap (posting always does), but is recognized via the
///    marker and explicitly excluded from re-triggering a further
///    correction (breaking any feedback loop at the source) and from
///    ever becoming a captured [`kvm_protocol::InputMessage`]
///    (`to_input_message` already filters synthetic events for any
///    caller) — so, unlike the reverted attempt, this mechanism cannot
///    leak a spurious delta to whatever device is being forwarded to,
///    even if it fires on every single suppressed move.
#[derive(Default)]
pub struct MacCapture {
    run_loop: Option<CFRunLoop>,
    thread: Option<thread::JoinHandle<()>>,
    suppress: Arc<AtomicBool>,
    /// The most recently observed real cursor position. An
    /// `Arc<Mutex<..>>` rather than the thread-local `Cell` a purely
    /// listen-only tap would only ever need, because
    /// `set_local_suppression` (called from a different thread) reads
    /// it to seed `anchor`.
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
                // state from a previous start()/stop() cycle never
                // leaks into this one. A `Cell` rather than a plain
                // local: `CGEventTap::new` requires an `Fn` callback,
                // not `FnMut`, so the closure only ever touches this
                // through a shared reference.
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
                        // rates; a poisoned lock (a panic while held,
                        // which nothing here ever does) falls back to
                        // the poisoned guard's data rather than losing
                        // capture entirely.
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
                        let is_mouse_motion = matches!(
                            event_type,
                            CGEventType::MouseMoved
                                | CGEventType::LeftMouseDragged
                                | CGEventType::RightMouseDragged
                                | CGEventType::OtherMouseDragged
                        );
                        // Our own corrective warp's echo, arriving back
                        // at this same tap (posting always delivers to
                        // every tap watching, including the one that
                        // posted it) -- must not trigger yet another
                        // correction, or this would feed back on itself
                        // forever. See the struct doc.
                        let is_synthetic = is_our_own_synthetic_event(event);
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
                                is_synthetic,
                                disposition = if suppressed { "drop" } else { "keep" },
                                "tap callback observed event"
                            );
                        }
                        if suppressed {
                            if is_mouse_motion
                                && !is_synthetic
                                && let Some(anchor_point) =
                                    *anchor.lock().unwrap_or_else(|e| e.into_inner())
                            {
                                match post_synthetic_warp(anchor_point) {
                                    Ok(()) => {
                                        *last_position.lock().unwrap_or_else(|e| e.into_inner()) =
                                            Some(anchor_point);
                                    }
                                    Err(e) => tracing::warn!(
                                        error = ?e,
                                        "failed to actively re-anchor the cursor while suppressed"
                                    ),
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
        // (disassociated, or pinned by the active correction) if
        // capture stops while a `Forwarding` session was still active.
        self.suppress.store(false, Ordering::Relaxed);
        *self.anchor.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(true);
    }

    fn set_local_suppression(&mut self, suppress: bool) {
        self.suppress.store(suppress, Ordering::Relaxed);
        tracing::info!(suppress, "set_local_suppression called");
        if suppress {
            // Snapshot where the cursor is *right now* as the anchor
            // the active correction (see the struct doc) will hold it
            // at for the rest of this `Forwarding` session. Read after
            // `RecenterLocal`'s own warp has already landed (that
            // effect is applied before this call -- see
            // `Session::handle_captured`), so this is the recentered
            // position, not wherever the triggering edge crossing just
            // left it.
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
