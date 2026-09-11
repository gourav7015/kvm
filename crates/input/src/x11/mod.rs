//! X11 backend: XInput2 raw events for capture, the XTEST extension's
//! `FakeInput` request for injection. See ADR-0008 for the design
//! rationale and the separate, non-GO Wayland decision — this module
//! is X11-specific (not "Linux" generically), since Wayland needs a
//! fundamentally different mechanism, not a variant of this one.

mod capture;
mod events;
mod inject;
mod keymap;
mod keysym;

pub use capture::X11Capture;
pub use inject::X11Inject;
