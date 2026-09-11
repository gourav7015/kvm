//! macOS capture via `CGEventTap`. This file is the "thin shim" ADR-0007
//! describes: it converts `CGEvent`s to [`InputMessage`]s and runs the
//! OS event loop — no cross-platform translation logic lives here.

use std::cell::Cell;
use std::sync::mpsc;
use std::thread;

use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
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
/// listen-only `CGEventTap`. Requires Accessibility permission — see
/// [`has_accessibility_permission`] and ADR-0007.
#[derive(Default)]
pub struct MacCapture {
    run_loop: Option<CFRunLoop>,
    thread: Option<thread::JoinHandle<()>>,
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
                    CGEventTapOptions::ListenOnly,
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
                        CallbackResult::Keep
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
    }
}
