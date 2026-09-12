//! **Diagnostic only — changes no production behavior.**
//!
//! Phase 4 gesture investigation (ADR-0009 decision 21): while forwarding,
//! a three-finger trackpad swipe still acts on the Mac. This Mac has
//! three-finger horizontal swipes set to switch Spaces and vertical swipes
//! to open Mission Control (`TrackpadThreeFinger{Horiz,Vert}SwipeGesture =
//! 2`), and `MacCapture`'s tap only registers ordinary keyboard/mouse
//! event types, so gesture events are never even seen — let alone dropped.
//!
//! Before changing the production tap, this measures on real hardware:
//!
//! 1. **Which event types** a three-finger swipe produces (their raw
//!    numbers — `core-graphics` has no names for gesture types), and **at
//!    which tap location** they are visible: `HID` (where `MacCapture`
//!    taps) or `Session`.
//! 2. With `--drop-gestures`, whether **dropping** those types at the HID
//!    tap actually stops Mission Control / the Space switch.
//!
//! Ordinary keyboard and mouse events are never dropped, in either mode,
//! and the probe exits by itself after `--seconds` (default 20), so it can
//! never take the keyboard or trackpad away.
//!
//! ## Usage
//!
//! ```text
//! cargo run -p kvm-input --example mac_gesture_probe
//! cargo run -p kvm-input --example mac_gesture_probe -- --drop-gestures
//! ```
//!
//! In each run, do a few three-finger swipes up and left/right, then read
//! the summary. Needs Accessibility permission, like `MacCapture`.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("mac_gesture_probe is a macOS-only diagnostic; nothing to do on this platform.");
}

#[cfg(target_os = "macos")]
fn main() {
    real::main();
}

#[cfg(target_os = "macos")]
#[allow(clippy::print_stdout)]
mod real {
    use std::collections::BTreeMap;
    use std::ffi::c_void;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    /// The event types `MacCapture` already registers (keyboard, mouse
    /// buttons/moves/drags, scroll) plus the two tap-disabled
    /// notifications. Anything else is what this probe is looking for.
    const ORDINARY_TYPES: &[u32] = &[
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        10,
        11,
        12,
        22,
        25,
        26,
        27,
        0xFFFF_FFFE,
        0xFFFF_FFFF,
    ];

    const TAP_HID: u32 = 0;
    const TAP_SESSION: u32 = 1;
    const HEAD_INSERT: u32 = 0;
    const OPTION_DEFAULT: u32 = 0;
    const OPTION_LISTEN_ONLY: u32 = 1;

    type TapCallback = extern "C" fn(*mut c_void, u32, *mut c_void, *mut c_void) -> *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: TapCallback,
            user_info: *mut c_void,
        ) -> *mut c_void;
        fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: *mut c_void,
            order: isize,
        ) -> *mut c_void;
        fn CFRunLoopGetCurrent() -> *mut c_void;
        fn CFRunLoopAddSource(run_loop: *mut c_void, source: *mut c_void, mode: *const c_void);
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source: u8) -> i32;
        static kCFRunLoopDefaultMode: *const c_void;
    }

    static DROP_GESTURES: AtomicBool = AtomicBool::new(false);
    static COUNTS: Mutex<BTreeMap<(&'static str, u32), u64>> = Mutex::new(BTreeMap::new());
    static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

    fn type_name(event_type: u32) -> &'static str {
        match event_type {
            18 => "Rotate",
            19 => "BeginGesture",
            20 => "EndGesture",
            29 => "Gesture",
            30 => "Magnify",
            31 => "Swipe",
            32 => "SmartMagnify",
            33 => "QuickLook",
            34 => "Pressure",
            37 => "DirectTouch",
            38 => "ChangeMode",
            _ => "?",
        }
    }

    fn record(location: &'static str, event_type: u32) -> bool {
        let first = COUNTS
            .lock()
            .map(|mut counts| {
                let n = counts.entry((location, event_type)).or_insert(0);
                *n += 1;
                *n == 1
            })
            .unwrap_or(false);
        let ordinary = ORDINARY_TYPES.contains(&event_type);
        if first && !ordinary {
            let ms = STARTED
                .lock()
                .ok()
                .and_then(|s| s.map(|t| t.elapsed().as_millis()))
                .unwrap_or(0);
            println!(
                "t={ms}ms  {location:<7} first non-ordinary event type {event_type} ({})",
                type_name(event_type)
            );
        }
        ordinary
    }

    extern "C" fn hid_callback(
        _proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        _user_info: *mut c_void,
    ) -> *mut c_void {
        let ordinary = record("HID", event_type);
        if !ordinary && DROP_GESTURES.load(Ordering::Relaxed) {
            return std::ptr::null_mut(); // dropped
        }
        event
    }

    extern "C" fn session_callback(
        _proxy: *mut c_void,
        event_type: u32,
        event: *mut c_void,
        _user_info: *mut c_void,
    ) -> *mut c_void {
        record("Session", event_type);
        event
    }

    fn install(location: u32, options: u32, callback: TapCallback, name: &str) {
        // SAFETY: plain FFI with valid arguments: a known tap location and
        // placement, an all-events mask, a callback with the documented
        // `CGEventTapCallBack` signature, and no user info. The run-loop
        // calls receive the objects just created by those same calls.
        unsafe {
            let tap = CGEventTapCreate(
                location,
                HEAD_INSERT,
                options,
                u64::MAX,
                callback,
                std::ptr::null_mut(),
            );
            if tap.is_null() {
                println!("could not create the {name} tap -- is Accessibility permission granted?");
                return;
            }
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopDefaultMode);
            CGEventTapEnable(tap, true);
        }
    }

    pub fn main() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let drop = args.iter().any(|a| a == "--drop-gestures");
        let seconds: f64 = args
            .iter()
            .position(|a| a == "--seconds")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(20.0);
        DROP_GESTURES.store(drop, Ordering::Relaxed);
        if let Ok(mut started) = STARTED.lock() {
            *started = Some(Instant::now());
        }

        println!("=== mac_gesture_probe ===");
        println!(
            "mode: {} -- ordinary keyboard/mouse events are never dropped",
            if drop {
                "DROP non-ordinary event types at the HID tap"
            } else {
                "observe only"
            }
        );
        println!(
            "Do a few three-finger swipes UP, then LEFT and RIGHT. Ends by itself after {seconds}s.\n"
        );

        install(
            TAP_HID,
            if drop {
                OPTION_DEFAULT
            } else {
                OPTION_LISTEN_ONLY
            },
            hid_callback,
            "HID",
        );
        install(TAP_SESSION, OPTION_LISTEN_ONLY, session_callback, "Session");

        // SAFETY: runs this thread's run loop, where both taps were added.
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
        }

        println!("\n=== SUMMARY: event types seen (location, type, name, count) ===");
        if let Ok(counts) = COUNTS.lock() {
            for (&(location, event_type), &n) in counts.iter() {
                let kind = if ORDINARY_TYPES.contains(&event_type) {
                    "ordinary"
                } else {
                    "NOT captured by MacCapture today"
                };
                println!(
                    "  {location:<7} type {event_type:<10} {:<12} x{n:<6} {kind}",
                    type_name(event_type)
                );
            }
        }
        if drop {
            println!(
                "\nDid Mission Control / the Space switch still happen during this run? Please report yes/no."
            );
        }
    }
}
