//! Errors produced by input capture/injection.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum InputError {
    /// The OS has not granted this process permission to capture or
    /// inject input (macOS Accessibility, primarily). Surfaced
    /// explicitly rather than silently failing or pretending to succeed
    /// — see ADR-0007.
    #[error("input permission not granted: {0}")]
    PermissionDenied(String),

    /// Starting or running the capture loop failed for a reason other
    /// than a missing permission.
    #[error("failed to capture input: {0}")]
    CaptureFailed(String),

    /// Injecting an event failed. The OS may have silently ignored the
    /// injection (e.g. targeting an elevated window on Windows — see
    /// ADR-0007) without reporting an error at all; this variant is for
    /// cases the OS *does* report.
    #[error("failed to inject input: {0}")]
    InjectFailed(String),

    /// This platform has no backend yet (e.g. Linux — deferred to Phase
    /// 3b) or the specific event is one this backend doesn't support.
    #[error("unsupported on this platform: {0}")]
    Unsupported(String),
}
