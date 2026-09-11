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

/// Absolute pointer position/screen-bounds queries — see ADR-0009.
///
/// Deliberately a separate trait from [`Capture`]/[`Inject`], not an
/// addition to either: `Capture`/`Inject` only ever carry *relative*
/// deltas (the hardware-verified Phase 3/3b path), and this trait's
/// only consumer is `core`'s edge-detection polling, which has no
/// business influencing the event-shape of ordinary input relay.
///
/// Coordinates are in each device's own OS-reported screen space —
/// origin and units are whatever the OS uses (macOS reports top-left
/// origin for `CGDisplayBounds`; Windows/X11 report top-left too, but
/// this trait makes no cross-OS normalization claim beyond that). One
/// combined screen boundary per device: multi-monitor layouts are out
/// of scope for Phase 4 (see ADR-0009).
pub trait PointerGeometry: Send {
    /// The current absolute cursor position, in this device's screen
    /// coordinate space.
    fn cursor_position(&self) -> Result<(i32, i32), InputError>;

    /// This device's combined screen size (width, height).
    fn screen_size(&self) -> Result<(u32, u32), InputError>;

    /// Moves the cursor to an absolute position in this device's
    /// screen coordinate space.
    fn set_cursor_position(&mut self, x: i32, y: i32) -> Result<(), InputError>;
}
