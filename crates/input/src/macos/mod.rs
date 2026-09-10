//! macOS backend: `CGEventTap` for capture, synthetic `CGEvent`s for
//! injection. See ADR-0007 §4. Everything macOS-specific is confined to
//! this module and its submodules — the rest of the crate never sees a
//! `CGEventType` or a Carbon virtual keycode.

mod capture;
mod events;
mod inject;
mod keycode;
mod permission;

pub use capture::MacCapture;
pub use inject::MacInject;
pub use permission::has_accessibility_permission;
