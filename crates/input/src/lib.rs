//! Cross-platform keyboard/mouse capture and injection.
//!
//! A platform-agnostic `Capture`/`Inject` trait plus pure, OS-independent
//! modifier-translation and switching logic; per-OS backends stay thin
//! shims behind the trait. See ADR-0007 for the full design rationale.
//!
//! Linux has no backend yet — deferred to the Phase 3b X11/Wayland spike.
//! The crate still builds cleanly on Linux: everything below is
//! platform-independent except the `macos`/`windows` modules, which are
//! `cfg`-gated to their own OS.

mod error;
mod traits;
mod translate;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
mod windows;

pub use error::InputError;
pub use traits::{Capture, Inject};
pub use translate::translate_for_target;

#[cfg(target_os = "macos")]
pub use macos::{MacCapture, MacInject, has_accessibility_permission};

#[cfg(windows)]
pub use windows::{WindowsCapture, WindowsInject};
