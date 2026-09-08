//! Cross-platform keyboard/mouse capture and injection.
//!
//! A platform-agnostic `Capture`/`Inject` trait plus pure, OS-independent
//! modifier-translation and switching logic; per-OS backends stay thin
//! shims behind the trait.
