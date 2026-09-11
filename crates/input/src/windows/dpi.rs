//! Real root cause of the Mac<->Windows pointer-range hardware bug (see
//! ADR-0009 decision 9): `GetSystemMetrics`/`GetCursorPos`/`SetCursorPos`
//! all report and accept coordinates in *this process's own DPI-aware
//! coordinate space*, not necessarily the display's real native pixel
//! grid. A process with no declared DPI awareness (the default for a
//! plain `cargo run` binary, no manifest) is "DPI-unaware," and Windows
//! silently virtualizes every one of those calls: the screen size, the
//! read cursor position, and the position a warp/move actually lands at
//! are all scaled by the system's DPI factor and then remapped back to
//! real pixels internally, with its own rounding. That rounding is not
//! guaranteed reversible at the extremes of the coordinate range --
//! this is a well-documented, real Windows behavior, not a guess -- so
//! a DPI-unaware process can genuinely fail to reach some physical
//! pixels at the edges of the screen, and can see its reported/typed
//! positions drift by a pixel or few depending on the scale factor's
//! rounding at that particular coordinate. This is the exact "movement
//! compressed/inconsistent, cannot reliably reach all four edges"
//! symptom real hardware QA hit.
//!
//! The correct fix -- not a magic scale factor, not a hard-coded
//! correction -- is to make the process declare Per-Monitor-V2 DPI
//! awareness, which switches every one of those Win32 calls onto the
//! display's real, physical pixel grid. `PointerGeometry::screen_size`
//! (exchanged with the Mac's `Router` to drive the resolution-aware
//! handoff rescale) and every `GetCursorPos`/`SetCursorPos` call in
//! [`crate::windows::inject`] then all agree with each other and with
//! the panel's actual resolution, with no virtualization layer in the
//! way at all.

use std::sync::Once;

use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

static ENSURE_ONCE: Once = Once::new();

/// Declares this process Per-Monitor-V2 DPI aware, exactly once no
/// matter how many times it's called. Must run before the first
/// `GetSystemMetrics`/`GetCursorPos`/`SetCursorPos` call -- both
/// [`crate::windows::WindowsInject::new`] and
/// [`crate::windows::WindowsCapture::start`] call this as their first
/// action, since either can be constructed first depending on whether
/// this machine is acting as a hub or a join target.
pub(crate) fn ensure_process_dpi_awareness() {
    ENSURE_ONCE.call_once(|| {
        // SAFETY: `SetProcessDpiAwarenessContext` takes a plain enum-like
        // constant with no buffer/pointer contract; calling it more than
        // once per process is documented as safe (later calls simply
        // fail), which is why this is additionally guarded by `Once`
        // rather than relying on that alone.
        match unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        } {
            Ok(()) => tracing::info!(
                "process declared Per-Monitor-V2 DPI aware -- GetSystemMetrics/GetCursorPos/SetCursorPos now operate in real physical pixels"
            ),
            Err(e) => tracing::warn!(
                error = ?e,
                "failed to declare Per-Monitor-V2 DPI awareness -- pointer coordinates may be \
                 DPI-virtualized (a real, known cause of restricted/inconsistent cursor range)"
            ),
        }
    });
}
