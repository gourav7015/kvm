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

    /// While capture keeps running and reporting events to the sink,
    /// suppresses (`true`) or resumes (`false`) this OS's own delivery
    /// of that same input to itself. See ADR-0009 decision 8/9: every
    /// backend was originally listen-only (never blocking), which real
    /// hardware QA found disruptive — while `Forwarding` to another
    /// device, the capturing machine's own keyboard/mouse should not
    /// also keep acting locally.
    ///
    /// Default no-op: a backend that hasn't grown a blocking capture
    /// mode yet simply keeps behaving exactly as before (still
    /// listen-only) rather than failing to compile or panicking. Not
    /// every backend needs to implement this on the same day it's
    /// added elsewhere — see ADR-0009's per-platform rollout note.
    fn set_local_suppression(&mut self, _suppress: bool) {}

    /// Where the most recently captured pointer event says the pointer
    /// is, in this device's own screen coordinates — or `None` if the
    /// backend doesn't record it. See ADR-0009 decision 21: on macOS this
    /// is the only trustworthy position immediately after local
    /// suppression ends, because the OS's own position query still returns
    /// the point the cursor was frozen at while forwarding.
    fn last_pointer_location(&self) -> Option<(i32, i32)> {
        None
    }

    /// Whether this device's Caps Lock is currently on, or `None` if the
    /// backend doesn't report it. Sent to a target when it becomes the
    /// active one, so the two machines' independent Caps Lock states start
    /// out matched — see ADR-0007's Caps Lock update.
    fn caps_lock_state(&self) -> Option<bool> {
        None
    }
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
