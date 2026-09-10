//! macOS Accessibility permission check — a named DoD item, not an
//! afterthought (ADR-0007). `CGEventTap` installs "successfully" even
//! without this permission; it simply never delivers events, which would
//! otherwise look like a silent, confusing no-op.

// SAFETY: `AXIsProcessTrusted` is a plain, argument-free C function from
// ApplicationServices.framework (via its HIServices sub-framework, which
// ApplicationServices re-exports) that returns a boolean; it has no
// preconditions beyond the framework being linked.
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Whether this process currently has Accessibility permission — required
/// for [`crate::macos::MacCapture`] to actually receive events and for
/// [`crate::macos::MacInject`] to successfully inject them.
pub fn has_accessibility_permission() -> bool {
    // SAFETY: see the `extern` block above.
    unsafe { AXIsProcessTrusted() }
}
