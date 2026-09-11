//! **Diagnostic only — changes no production behavior.**
//!
//! Phase 4 pointer-range experiment: distinguish HYPOTHESIS A from
//! HYPOTHESIS B for the "Windows cursor is boxed into the middle"
//! real-hardware failure.
//!
//! - **A**: the Mac's captured event *locations* are bounded by the Mac
//!   screen, so the point deltas this project derives from them stop
//!   representing continued physical movement.
//! - **B**: the Mac keeps reporting real movement, and it's lost
//!   somewhere later (normalization → router → session → QUIC →
//!   Windows injection).
//!
//! This probe measures the Mac stage in isolation, with nothing else in
//! the pipeline involved at all — no network, no router, no Windows. It
//! taps exactly the same events `MacCapture` taps, and for every single
//! mouse-motion event records **both** independent readings of "how far
//! did the pointer just move":
//!
//! 1. `CGEvent::location()` diffed against the previous reading — the
//!    absolute-position-derived delta [`kvm_input`]'s
//!    `macos::events::point_delta` actually computes and ships as
//!    `InputMessage::MouseMove`.
//! 2. `kCGMouseEventDeltaX`/`kCGMouseEventDeltaY` (`EventField::MOUSE_EVENT_DELTA_X/Y`)
//!    — the event's own relative motion fields, which come from the HID
//!    layer and are *not* a function of any screen position.
//!
//! If the two agree for the whole swipe, the Mac is faithfully
//! reporting movement and the loss is downstream → **B**. If reading 1
//! goes to zero (or the observed location pins at a screen boundary)
//! while reading 2 keeps reporting real motion, the Mac's absolute
//! position tracking is the bound → **A**, and reading 2 is the
//! evidence of what the physical mouse was actually still doing.
//!
//! ## Usage
//!
//! Baseline, no suppression — the cursor moves normally, just observed:
//! ```text
//! cargo run -p kvm-input --example mac_pointer_probe
//! ```
//!
//! The decisive run — reproduces the exact state a real `Forwarding`
//! session puts the Mac in (cursor recentered, then disassociated via
//! `CGAssociateMouseAndMouseCursorPosition(false)`, motion events
//! dropped):
//! ```text
//! cargo run -p kvm-input --example mac_pointer_probe -- --suppress
//! ```
//!
//! `--seconds N` sets the run length (default 30). Needs Accessibility
//! permission, same as `MacCapture`.
//!
//! **Safety**: unlike production, keyboard events are always passed
//! through even under `--suppress`, so Ctrl-C in the terminal always
//! works. The cursor/mouse association is restored on the normal exit
//! path, and macOS restores it on its own when the process dies, so a
//! Ctrl-C cannot leave the cursor frozen.
//!
//! ## What to do with it
//!
//! Run `--suppress`, then physically swipe right continuously for
//! several seconds, **keep going after the cursor would have reached
//! the Mac's right edge**, then repeat left, up and down. Read the
//! summary block at the end: `samples_with_zero_point_delta_but_real_hid_delta`
//! and `observed_location_x` vs `screen_size` answer the question
//! directly.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mac_pointer_probe is a macOS-only diagnostic; nothing to do on this platform.");
}

#[cfg(target_os = "macos")]
#[allow(clippy::unwrap_used, clippy::expect_used)]
fn main() {
    real::main();
}

#[cfg(target_os = "macos")]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]
mod real {
    use std::cell::{Cell, RefCell};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use core_foundation::runloop::{CFRunLoop, kCFRunLoopDefaultMode};
    use core_graphics::display::CGDisplay;
    use core_graphics::event::{
        CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        CallbackResult, EventField,
    };
    use core_graphics::geometry::CGPoint;

    /// One captured mouse-motion event, with both independent readings
    /// of the motion it represents.
    #[derive(Debug, Clone, Copy)]
    struct Sample {
        seq: u64,
        t_ms: u128,
        /// `CGEvent::location()` — the absolute reading.
        loc: (f64, f64),
        /// Delta derived by diffing `loc` against the previous sample's
        /// — exactly what `macos::events::point_delta` computes and what
        /// ships as `InputMessage::MouseMove`.
        point_delta: (i32, i32),
        /// `kCGMouseEventDeltaX/Y` — the event's own HID-derived
        /// relative motion, independent of any screen position.
        hid_delta: (i64, i64),
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let suppress = args.iter().any(|a| a == "--suppress");
        let seconds: u64 = args
            .iter()
            .position(|a| a == "--seconds")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(30);

        let display = CGDisplay::main();
        let bounds = display.bounds();
        let screen = (bounds.size.width, bounds.size.height);
        let origin = (bounds.origin.x, bounds.origin.y);

        println!("=== mac_pointer_probe ===");
        println!(
            "mac screen_size (CGDisplay::main().bounds(), logical points) = {:.0} x {:.0}",
            screen.0, screen.1
        );
        println!("mac display origin = ({:.0}, {:.0})", origin.0, origin.1);
        println!(
            "mode = {}",
            if suppress {
                "SUPPRESSED (mimics a real Forwarding session)"
            } else {
                "observe only (baseline)"
            }
        );
        println!("run length = {seconds}s");

        if suppress {
            // Mimic `Effect::RecenterLocal` exactly: production warps the
            // local cursor to the screen centre the instant ownership
            // leaves `Local`, so the swipe under test starts from the
            // same place a real session starts it from.
            let centre = CGPoint {
                x: origin.0 + screen.0 / 2.0,
                y: origin.1 + screen.1 / 2.0,
            };
            let _ = CGDisplay::warp_mouse_cursor_position(centre);
            println!("recentred cursor to ({:.0}, {:.0})", centre.x, centre.y);
            match CGDisplay::associate_mouse_and_mouse_cursor_position(false) {
                Ok(()) => println!("cursor/mouse DISASSOCIATED (cursor should now be frozen)"),
                Err(e) => println!("WARNING: disassociation failed: {e:?}"),
            }
        }

        println!();
        println!("Now swipe: RIGHT continuously for several seconds (keep going well past");
        println!("the point where the cursor would have hit the Mac's right edge), then");
        println!("LEFT, then UP, then DOWN. Ctrl-C or wait {seconds}s to finish.");
        println!();
        println!("seq,t_ms,loc_x,loc_y,point_dx,point_dy,hid_dx,hid_dy");

        let samples = run_tap(seconds, suppress);

        if suppress {
            match CGDisplay::associate_mouse_and_mouse_cursor_position(true) {
                Ok(()) => println!("\ncursor/mouse re-associated (cursor is live again)"),
                Err(e) => println!("\nWARNING: re-association failed: {e:?}"),
            }
        }

        report(&samples, screen, origin);
    }

    /// Runs a `CGEventTap` for `seconds`, collecting one [`Sample`] per
    /// mouse-motion event. Mirrors `MacCapture::start`'s threading
    /// (tap + `CFRunLoop` on a dedicated thread) so nothing about the
    /// measurement context differs from production.
    fn run_tap(seconds: u64, suppress: bool) -> Vec<Sample> {
        let (setup_tx, setup_rx) = mpsc::channel::<CFRunLoop>();
        let (sample_tx, sample_rx) = mpsc::channel::<Sample>();

        let handle = thread::Builder::new()
            .name("mac-pointer-probe".to_string())
            .spawn(move || {
                let started = Instant::now();
                let seq = Cell::new(0u64);
                let last_loc: RefCell<Option<CGPoint>> = RefCell::new(None);

                let tap = CGEventTap::new(
                    CGEventTapLocation::HID,
                    CGEventTapPlacement::HeadInsertEventTap,
                    CGEventTapOptions::Default,
                    crate::real::motion_event_types(),
                    move |proxy, event_type, event| {
                        if matches!(
                            event_type,
                            CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                        ) {
                            // Same OS behaviour `MacCapture` defends
                            // against — an un-re-enabled tap would look
                            // exactly like "movement stopped", which is
                            // the very thing under investigation, so it
                            // must be ruled out here too.
                            println!("!! tap disabled by the OS ({event_type:?}) -- re-enabling");
                            unsafe { CGEventTapEnable(proxy, true) };
                            return CallbackResult::Keep;
                        }

                        let is_motion = matches!(
                            event_type,
                            CGEventType::MouseMoved
                                | CGEventType::LeftMouseDragged
                                | CGEventType::RightMouseDragged
                                | CGEventType::OtherMouseDragged
                        );
                        if !is_motion {
                            // Keyboard always passes through, even under
                            // --suppress: the Ctrl-C escape hatch.
                            return CallbackResult::Keep;
                        }

                        // Re-assert the association from this thread on
                        // every motion event, exactly as production does
                        // (ADR-0009 decision 14) -- so the measured state
                        // is production's state, not an approximation.
                        if suppress {
                            let _ = CGDisplay::associate_mouse_and_mouse_cursor_position(false);
                        }

                        let loc = event.location();
                        let previous = last_loc.borrow_mut().replace(loc);
                        let point_delta = match previous {
                            Some(p) => ((loc.x - p.x).round() as i32, (loc.y - p.y).round() as i32),
                            None => (0, 0),
                        };
                        let hid_delta = (
                            event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X),
                            event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y),
                        );

                        seq.set(seq.get() + 1);
                        let sample = Sample {
                            seq: seq.get(),
                            t_ms: started.elapsed().as_millis(),
                            loc: (loc.x, loc.y),
                            point_delta,
                            hid_delta,
                        };
                        println!(
                            "{},{},{:.1},{:.1},{},{},{},{}",
                            sample.seq,
                            sample.t_ms,
                            sample.loc.0,
                            sample.loc.1,
                            sample.point_delta.0,
                            sample.point_delta.1,
                            sample.hid_delta.0,
                            sample.hid_delta.1,
                        );
                        let _ = sample_tx.send(sample);

                        if suppress {
                            CallbackResult::Drop
                        } else {
                            CallbackResult::Keep
                        }
                    },
                )
                .expect(
                    "failed to create CGEventTap -- grant Accessibility permission to this \
                     terminal/binary in System Settings > Privacy & Security > Accessibility",
                );

                let source = tap
                    .mach_port()
                    .create_runloop_source(0)
                    .expect("failed to create run loop source");
                let run_loop = CFRunLoop::get_current();
                run_loop.add_source(&source, unsafe { kCFRunLoopDefaultMode });
                if setup_tx.send(run_loop).is_err() {
                    return;
                }
                CFRunLoop::run_current();
            })
            .expect("failed to spawn probe thread");

        let run_loop = setup_rx.recv().expect("probe thread died during setup");
        thread::sleep(Duration::from_secs(seconds));
        run_loop.stop();
        let _ = handle.join();

        sample_rx.try_iter().collect()
    }

    fn motion_event_types() -> Vec<CGEventType> {
        vec![
            CGEventType::MouseMoved,
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
        ]
    }

    /// The block that actually answers A vs B.
    fn report(samples: &[Sample], screen: (f64, f64), origin: (f64, f64)) {
        println!("\n=== SUMMARY ===");
        if samples.is_empty() {
            println!("no mouse-motion events captured -- was Accessibility permission granted?");
            return;
        }

        let (mut min_x, mut max_x) = (f64::MAX, f64::MIN);
        let (mut min_y, mut max_y) = (f64::MAX, f64::MIN);
        let (mut point_travel_x, mut point_travel_y) = (0i64, 0i64);
        let (mut hid_travel_x, mut hid_travel_y) = (0i64, 0i64);
        let mut zero_point_but_real_hid = 0usize;
        let mut both_zero = 0usize;

        for s in samples {
            min_x = min_x.min(s.loc.0);
            max_x = max_x.max(s.loc.0);
            min_y = min_y.min(s.loc.1);
            max_y = max_y.max(s.loc.1);
            point_travel_x += i64::from(s.point_delta.0.abs());
            point_travel_y += i64::from(s.point_delta.1.abs());
            hid_travel_x += s.hid_delta.0.abs();
            hid_travel_y += s.hid_delta.1.abs();
            let point_zero = s.point_delta == (0, 0);
            let hid_zero = s.hid_delta == (0, 0);
            if point_zero && !hid_zero {
                zero_point_but_real_hid += 1;
            }
            if point_zero && hid_zero {
                both_zero += 1;
            }
        }

        println!("samples = {}", samples.len());
        println!(
            "screen_size = {:.0} x {:.0}  (valid location range x: {:.0}..={:.0}, y: {:.0}..={:.0})",
            screen.0,
            screen.1,
            origin.0,
            origin.0 + screen.0 - 1.0,
            origin.1,
            origin.1 + screen.1 - 1.0,
        );
        println!("observed_location_x = {min_x:.0} .. {max_x:.0}");
        println!("observed_location_y = {min_y:.0} .. {max_y:.0}");
        println!(
            "cumulative |point_delta| = ({point_travel_x}, {point_travel_y})   \
             <- what this project ships as InputMessage::MouseMove"
        );
        println!(
            "cumulative |hid_delta|   = ({hid_travel_x}, {hid_travel_y})   \
             <- what the mouse physically reported"
        );
        println!("samples_with_zero_point_delta_but_real_hid_delta = {zero_point_but_real_hid}");
        println!("samples_with_both_deltas_zero (genuinely still) = {both_zero}");

        println!("\n--- VERDICT ---");
        let pinned_x = max_x >= origin.0 + screen.0 - 4.0 || min_x <= origin.0 + 4.0;
        let pinned_y = max_y >= origin.1 + screen.1 - 4.0 || min_y <= origin.1 + 4.0;
        if zero_point_but_real_hid > 0 {
            println!(
                "HYPOTHESIS A supported: {zero_point_but_real_hid} motion events reported real \
                 physical movement via the HID delta fields while the location-derived delta \
                 this project uses reported exactly zero."
            );
            if pinned_x || pinned_y {
                println!(
                    "  ...and the observed location was pinned at a screen boundary \
                     (x pinned: {pinned_x}, y pinned: {pinned_y}) -- i.e. CGEvent::location() is \
                     bounded by the Mac display, so the derived delta cannot represent movement \
                     past it."
                );
            }
            println!(
                "  Consequence: InputMessage::MouseMove carries 0 for those events, so nothing \
                 downstream (router, QUIC, Windows) can possibly move the remote cursor -- the \
                 movement is lost at the very first stage."
            );
        } else if point_travel_x == 0 && point_travel_y == 0 {
            println!("inconclusive: no movement was recorded at all.");
        } else {
            println!(
                "HYPOTHESIS A NOT supported at this stage: every event with real HID motion also \
                 produced a nonzero location-derived delta. The Mac is reporting movement \
                 faithfully -- look downstream (HYPOTHESIS B)."
            );
        }
    }

    // Same rationale as `crates/input/src/macos/capture.rs`: this FFI
    // binding is module-private in `core-graphics` 0.25 and only
    // reachable through the owned `CGEventTap`, which the tap's own
    // 'static callback cannot borrow.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapEnable(tap: core_graphics::event::CGEventTapProxy, enable: bool);
    }
}
