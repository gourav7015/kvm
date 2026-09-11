//! Windows backend: global low-level hooks for capture, `SendInput` for
//! injection. See ADR-0007 for the design rationale, including the
//! documented no-elevation/UAC limitation.

mod capture;
mod dpi;
mod inject;
mod keycode;

pub use capture::WindowsCapture;
pub use inject::WindowsInject;
