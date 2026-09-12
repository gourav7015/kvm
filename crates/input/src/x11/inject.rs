//! X11 injection via the XTEST extension's `FakeInput` request. The
//! other half of the "thin shim" alongside [`crate::x11::capture`] —
//! pure event-shape conversion, no cross-platform translation logic.

use kvm_protocol::{ButtonState, InputMessage, Key, MouseButton, PlatformKind};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT,
    KEY_RELEASE_EVENT, KeyButMask, MOTION_NOTIFY_EVENT, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::error::InputError;
use crate::scroll::{UNITS_PER_NOTCH, take_whole_steps};
use crate::traits::{Inject, PointerGeometry};
use crate::translate::is_injectable;
use crate::x11::keymap::KeyMap;
use crate::x11::keysym::key_to_keysym;

/// `deviceid` value meaning "the X core keyboard/pointer", per the
/// XTEST extension spec — the standard choice unless targeting one
/// specific input device out of several.
const CORE_DEVICES: u8 = 0;
/// `root` value meaning "the default root of the default screen".
const DEFAULT_ROOT: u32 = x11rb::NONE;
/// `time` value meaning "let the server fill in the current time".
const CURRENT_TIME: u32 = 0;
/// Where a relative move of `(dx, dy)` from `current` lands, as the
/// absolute root coordinates `XTestFakeInput` takes. See the `MouseMove`
/// arm of [`X11Inject::inject`] for why moves are placed absolutely.
fn absolute_motion_target(current: (i32, i32), dx: i32, dy: i32) -> (i16, i16) {
    let clamp = |v: i32| v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    (
        clamp(current.0.saturating_add(dx)),
        clamp(current.1.saturating_add(dy)),
    )
}

/// `detail` value for `MotionNotify` meaning the coordinates are an
/// absolute position in the root window's coordinate space — the
/// other branch of the same request, used for [`PointerGeometry::set_cursor_position`].
const MOTION_ABSOLUTE: u8 = 0;

/// Injects keyboard/mouse events on this machine via `XTestFakeInput`.
/// Needs an active X11 connection with XTEST-permitted access — no
/// elevated privileges beyond ordinary X client access (see ADR-0008).
pub struct X11Inject {
    conn: RustConnection,
    keymap: KeyMap,
    root: Window,
    /// Scroll units not yet worth a whole notch, carried into the next
    /// scroll event (`crate::scroll::take_whole_steps`), as `(dx, dy)`.
    scroll_remainder: (i32, i32),
}

impl X11Inject {
    pub fn new() -> Result<Self, InputError> {
        let (conn, screen_num) = x11rb::connect(None).map_err(|e| {
            InputError::InjectFailed(format!("failed to connect to the X server: {e}"))
        })?;
        let keymap = KeyMap::query(&conn).map_err(InputError::InjectFailed)?;
        let root = conn.setup().roots[screen_num].root;
        Ok(Self {
            conn,
            keymap,
            root,
            scroll_remainder: (0, 0),
        })
    }
}

impl Inject for X11Inject {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError> {
        match *event {
            InputMessage::Key {
                key,
                state,
                source_os,
                ..
            } => {
                // A raw code from another platform is that platform's
                // number, not an X11 keysym -- see `translate::is_injectable`.
                if !is_injectable(key, source_os, PlatformKind::Linux) {
                    return Err(InputError::Unsupported(format!(
                        "{key:?} is a raw {source_os:?} key code with no meaning on X11 -- not injected"
                    )));
                }
                let keysym = key_to_keysym(key);
                let Some(keycode) = self.keymap.key_to_keycode(key) else {
                    return Err(InputError::Unsupported(format!(
                        "{key:?} (keysym {keysym:#x}) has no keycode bound on this X server's current keyboard mapping"
                    )));
                };
                let type_ = match state {
                    ButtonState::Pressed => KEY_PRESS_EVENT,
                    ButtonState::Released => KEY_RELEASE_EVENT,
                };
                self.fake_input(type_, keycode, 0, 0)
            }
            InputMessage::CapsLockState { on } => {
                // Match the sender's state: press Caps Lock only if the X
                // server's own (the Lock modifier) differs -- see ADR-0007's
                // Caps Lock update.
                let reply = self
                    .conn
                    .query_pointer(self.root)
                    .map_err(|e| {
                        InputError::InjectFailed(format!("QueryPointer request failed: {e}"))
                    })?
                    .reply()
                    .map_err(|e| {
                        InputError::InjectFailed(format!("QueryPointer reply failed: {e}"))
                    })?;
                let current = u16::from(reply.mask) & u16::from(KeyButMask::LOCK) != 0;
                if current == on {
                    return Ok(());
                }
                let Some(keycode) = self.keymap.key_to_keycode(Key::CapsLock) else {
                    return Err(InputError::Unsupported(
                        "Caps Lock has no keycode bound on this X server's current keyboard mapping"
                            .to_string(),
                    ));
                };
                self.fake_input(KEY_PRESS_EVENT, keycode, 0, 0)?;
                self.fake_input(KEY_RELEASE_EVENT, keycode, 0, 0)
            }
            InputMessage::MouseMove { dx, dy } => {
                // Placed absolutely -- read the pointer, add the delta --
                // not as XTEST *relative* motion, which the X server runs
                // through pointer acceleration. The sender's `Router`
                // tracks this screen's cursor with 1:1 deltas, so an
                // accelerated move drifts away from where it thinks the
                // cursor is: the same failure the Windows injector had
                // with relative `SendInput` (ADR-0009, fixed there with
                // `GetCursorPos`+`SetCursorPos`). ADR-0009 decision 24.
                let before = self.cursor_position()?;
                let (target_x, target_y) = absolute_motion_target(before, dx, dy);
                let result =
                    self.fake_input(MOTION_NOTIFY_EVENT, MOTION_ABSOLUTE, target_x, target_y);
                // Diagnostic readback, same shape as the Windows injector's
                // stage 4: a disagreement away from a real screen edge
                // would be direct evidence of target-side rescaling.
                let actual = self.cursor_position().ok();
                let intended = (i32::from(target_x), i32::from(target_y));
                tracing::info!(
                    stage = "4-x11-inject",
                    ?before,
                    dx,
                    dy,
                    ?intended,
                    ?actual,
                    matched = actual == Some(intended),
                    "X11Inject: relative move applied via QueryPointer+absolute XTEST motion"
                );
                result
            }
            InputMessage::MouseButton { button, state } => {
                let detail = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                    MouseButton::Other(code) => code,
                };
                let type_ = match state {
                    ButtonState::Pressed => BUTTON_PRESS_EVENT,
                    ButtonState::Released => BUTTON_RELEASE_EVENT,
                };
                self.fake_input(type_, detail, 0, 0)
            }
            InputMessage::MouseScroll { dx, dy } => {
                // The classic X11 scroll-wheel-as-buttons convention —
                // see events.rs's capture-side note. The protocol's units
                // (`crate::scroll`) become whole notches, the remainder
                // carried into the next event, and each notch one
                // synthetic press+release pair on the scroll button.
                let (notches_y, rest_y) =
                    take_whole_steps(self.scroll_remainder.1.saturating_add(dy), UNITS_PER_NOTCH);
                let (notches_x, rest_x) =
                    take_whole_steps(self.scroll_remainder.0.saturating_add(dx), UNITS_PER_NOTCH);
                self.scroll_remainder = (rest_x, rest_y);
                if notches_y != 0 {
                    let button = if notches_y > 0 { 4 } else { 5 };
                    for _ in 0..notches_y.unsigned_abs() {
                        self.fake_input(BUTTON_PRESS_EVENT, button, 0, 0)?;
                        self.fake_input(BUTTON_RELEASE_EVENT, button, 0, 0)?;
                    }
                }
                if notches_x != 0 {
                    let button = if notches_x > 0 { 7 } else { 6 };
                    for _ in 0..notches_x.unsigned_abs() {
                        self.fake_input(BUTTON_PRESS_EVENT, button, 0, 0)?;
                        self.fake_input(BUTTON_RELEASE_EVENT, button, 0, 0)?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl PointerGeometry for X11Inject {
    fn cursor_position(&self) -> Result<(i32, i32), InputError> {
        let reply = self
            .conn
            .query_pointer(self.root)
            .map_err(|e| InputError::InjectFailed(format!("QueryPointer request failed: {e}")))?
            .reply()
            .map_err(|e| InputError::InjectFailed(format!("QueryPointer reply failed: {e}")))?;
        Ok((i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    fn screen_size(&self) -> Result<(u32, u32), InputError> {
        let reply = self
            .conn
            .get_geometry(self.root)
            .map_err(|e| InputError::InjectFailed(format!("GetGeometry request failed: {e}")))?
            .reply()
            .map_err(|e| InputError::InjectFailed(format!("GetGeometry reply failed: {e}")))?;
        Ok((u32::from(reply.width), u32::from(reply.height)))
    }

    fn set_cursor_position(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        let root_x = x.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        let root_y = y.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
        self.fake_input(MOTION_NOTIFY_EVENT, MOTION_ABSOLUTE, root_x, root_y)
    }
}

impl X11Inject {
    fn fake_input(
        &self,
        type_: u8,
        detail: u8,
        root_x: i16,
        root_y: i16,
    ) -> Result<(), InputError> {
        let cookie = self
            .conn
            .xtest_fake_input(
                type_,
                detail,
                CURRENT_TIME,
                DEFAULT_ROOT,
                root_x,
                root_y,
                CORE_DEVICES,
            )
            .map_err(|e| InputError::InjectFailed(format!("XTestFakeInput request failed: {e}")))?;
        cookie
            .check()
            .map_err(|e| InputError::InjectFailed(format!("XTestFakeInput was rejected: {e}")))?;
        self.conn
            .flush()
            .map_err(|e| InputError::InjectFailed(format!("failed to flush injected event: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_lands_at_the_current_position_plus_the_delta() {
        assert_eq!(absolute_motion_target((100, 100), 5, -3), (105, 97));
        assert_eq!(absolute_motion_target((0, 0), -4, -4), (-4, -4));
    }

    #[test]
    fn a_move_never_overflows_the_protocols_coordinate_range() {
        assert_eq!(
            absolute_motion_target((i32::from(i16::MAX), 0), 10, 0),
            (i16::MAX, 0)
        );
        assert_eq!(
            absolute_motion_target((i32::MAX, 0), i32::MAX, 0),
            (i16::MAX, 0)
        );
        assert_eq!(absolute_motion_target((0, i32::MIN), 0, -5), (0, i16::MIN));
    }
}
