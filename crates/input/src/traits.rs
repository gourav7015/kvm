//! The `Capture`/`Inject` boundary — see ADR-0007 for why these are
//! plain synchronous traits rather than async, and why each backend
//! module (`macos`, `windows`) must stay a thin, mechanical implementation
//! of them with no cross-platform logic inside.

use std::sync::mpsc;

use kvm_protocol::InputMessage;

use crate::error::InputError;

/// Captures local keyboard/mouse input, normalizing it to
/// [`InputMessage`] and delivering it through `sink`.
///
/// Implementations run their OS event loop on a dedicated thread —
/// `start` returns once that thread is up and delivering events (or
/// returns an error immediately, e.g. [`InputError::PermissionDenied`]),
/// it does not block for the lifetime of capture.
pub trait Capture: Send {
    fn start(&mut self, sink: mpsc::Sender<InputMessage>) -> Result<(), InputError>;

    /// Stops capturing. Safe to call even if `start` was never called or
    /// already stopped.
    fn stop(&mut self);
}

/// Injects a normalized [`InputMessage`] as local keyboard/mouse input.
pub trait Inject: Send {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError>;
}
