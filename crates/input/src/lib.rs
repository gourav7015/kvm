//! Cross-platform keyboard/mouse capture and injection.
//!
//! A platform-agnostic `Capture`/`Inject` trait plus pure, OS-independent
//! modifier-translation and switching logic; per-OS backends stay thin
//! shims behind the trait. See ADR-0007 for the full design rationale.
//!
//! Linux has an X11 backend (`x11`, this module is named for the
//! mechanism, not just the OS, since Wayland needs a different one
//! entirely — see ADR-0008). No Wayland backend exists: ADR-0008
//! records a formal NO-GO for general-purpose global capture under
//! Wayland's security model, not silently deferred scope.

mod error;
mod traits;
mod translate;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
mod x11;

pub use error::InputError;
pub use traits::{Capture, Inject};
pub use translate::translate_for_target;

#[cfg(target_os = "macos")]
pub use macos::{MacCapture, MacInject, has_accessibility_permission};

#[cfg(windows)]
pub use windows::{WindowsCapture, WindowsInject};

#[cfg(target_os = "linux")]
pub use x11::{X11Capture, X11Inject};
