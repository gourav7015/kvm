//! macOS injection via synthetic `CGEvent`s posted at the HID level.

use core_graphics::display::CGDisplay;
use core_graphics::event::{
    CGEvent, CGEventTapLocation, CGMouseButton, EventField, ScrollEventUnit,
};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use kvm_protocol::{ButtonState, InputMessage, MouseButton, PlatformKind};

use crate::error::InputError;
use crate::macos::keycode::key_to_keycode;
use crate::macos::permission::has_accessibility_permission;
use crate::mouse::to_macos_button_number;
use crate::scroll::{UNITS_PER_LINE, take_whole_steps};
use crate::traits::{Inject, PointerGeometry};
use crate::translate::is_injectable;

#[derive(Default)]
pub struct MacInject {
    /// Scroll units not yet worth a whole line, carried into the next
    /// scroll event (`crate::scroll::take_whole_steps`), as `(dx, dy)`.
    scroll_remainder: (i32, i32),
}

impl MacInject {
    pub fn new() -> Self {
        Self::default()
    }
}

fn event_source() -> Result<CGEventSource, InputError> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|()| InputError::InjectFailed("failed to create CGEventSource".to_string()))
}

impl Inject for MacInject {
    fn inject(&mut self, event: &InputMessage) -> Result<(), InputError> {
        if !has_accessibility_permission() {
            return Err(InputError::PermissionDenied(
                "Accessibility permission not granted for this process — grant it in \
                 System Settings > Privacy & Security > Accessibility, then restart"
                    .to_string(),
            ));
        }

        match *event {
            InputMessage::Key {
                key,
                state,
                source_os,
                ..
            } => {
                // A raw code from another platform is that platform's
                // number, not a macOS keycode -- see `translate::is_injectable`.
                if !is_injectable(key, source_os, PlatformKind::MacOs) {
                    return Err(InputError::Unsupported(format!(
                        "{key:?} is a raw {source_os:?} key code with no meaning on macOS -- not injected"
                    )));
                }
                let Some(code) = key_to_keycode(key) else {
                    return Err(InputError::Unsupported(format!(
                        "{key:?} has no macOS keycode to inject"
                    )));
                };
                let source = event_source()?;
                let cg_event =
                    CGEvent::new_keyboard_event(source, code, state == ButtonState::Pressed)
                        .map_err(|()| {
                            InputError::InjectFailed(
                                "failed to create keyboard CGEvent".to_string(),
                            )
                        })?;
                cg_event.post(CGEventTapLocation::HID);
                Ok(())
            }
            InputMessage::CapsLockState { .. } => Err(InputError::Unsupported(
                "setting Caps Lock state on a macOS target is not implemented -- ignored"
                    .to_string(),
            )),
            InputMessage::MouseMove { dx, dy } => {
                // CGEvent mouse-move events are expressed as an absolute
                // cursor position, not a delta; `CGEvent::new_mouse_event`
                // moves the cursor to that position. We approximate a
                // relative move by reading the system's current cursor
                // location and adding the delta — a small extra CF call,
                // not a correctness issue, and still well within the
                // "no file I/O / no blocking" latency budget.
                let current = CGEvent::new(event_source()?)
                    .map_err(|()| {
                        InputError::InjectFailed(
                            "failed to read current pointer location".to_string(),
                        )
                    })?
                    .location();
                let target = CGPoint {
                    x: current.x + dx as f64,
                    y: current.y + dy as f64,
                };
                let source = event_source()?;
                let cg_event = CGEvent::new_mouse_event(
                    source,
                    core_graphics::event::CGEventType::MouseMoved,
                    target,
                    CGMouseButton::Left,
                )
                .map_err(|()| {
                    InputError::InjectFailed("failed to create mouse-move CGEvent".to_string())
                })?;
                cg_event.post(CGEventTapLocation::HID);
                Ok(())
            }
            InputMessage::MouseButton { button, state } => {
                let (event_type, cg_button) = mouse_button_event_type(button, state);
                let current = CGEvent::new(event_source()?)
                    .map_err(|()| {
                        InputError::InjectFailed(
                            "failed to read current pointer location".to_string(),
                        )
                    })?
                    .location();
                let source = event_source()?;
                let cg_event = CGEvent::new_mouse_event(source, event_type, current, cg_button)
                    .map_err(|()| {
                        InputError::InjectFailed(
                            "failed to create mouse-button CGEvent".to_string(),
                        )
                    })?;
                // Middle, Back, Forward and further buttons are all "other"
                // mouse events; which one is the button-number field.
                if matches!(button, MouseButton::Middle | MouseButton::Other(_)) {
                    cg_event.set_integer_value_field(
                        EventField::MOUSE_EVENT_BUTTON_NUMBER,
                        to_macos_button_number(button),
                    );
                }
                cg_event.post(CGEventTapLocation::HID);
                Ok(())
            }
            InputMessage::MouseScroll { dx, dy } => {
                // Protocol units (`crate::scroll`) to whole lines, carrying
                // the remainder, so fine deltas still add up.
                let (lines_y, rest_y) =
                    take_whole_steps(self.scroll_remainder.1.saturating_add(dy), UNITS_PER_LINE);
                let (lines_x, rest_x) =
                    take_whole_steps(self.scroll_remainder.0.saturating_add(dx), UNITS_PER_LINE);
                self.scroll_remainder = (rest_x, rest_y);
                if lines_x == 0 && lines_y == 0 {
                    return Ok(());
                }
                let source = event_source()?;
                let cg_event = CGEvent::new_scroll_event(
                    source,
                    ScrollEventUnit::LINE,
                    2,
                    lines_y,
                    // macOS's horizontal axis is positive toward the left;
                    // the protocol's `dx` toward the right.
                    -lines_x,
                    0,
                )
                .map_err(|()| {
                    InputError::InjectFailed("failed to create scroll CGEvent".to_string())
                })?;
                cg_event.post(CGEventTapLocation::HID);
                Ok(())
            }
        }
    }
}

impl PointerGeometry for MacInject {
    fn cursor_position(&self) -> Result<(i32, i32), InputError> {
        // Same call `inject`'s MouseMove/MouseButton arms already use to
        // read the current pointer location — reused verbatim, just
        // exposed as a public query instead of an internal step.
        let location = CGEvent::new(event_source()?)
            .map_err(|()| {
                InputError::InjectFailed("failed to read current pointer location".to_string())
            })?
            .location();
        Ok((location.x as i32, location.y as i32))
    }

    fn screen_size(&self) -> Result<(u32, u32), InputError> {
        let display = CGDisplay::main();
        let bounds = display.bounds();
        Ok((bounds.size.width as u32, bounds.size.height as u32))
    }

    fn set_cursor_position(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        let target = CGPoint {
            x: x as f64,
            y: y as f64,
        };
        let source = event_source()?;
        let cg_event = CGEvent::new_mouse_event(
            source,
            core_graphics::event::CGEventType::MouseMoved,
            target,
            CGMouseButton::Left,
        )
        .map_err(|()| {
            InputError::InjectFailed("failed to create mouse-move CGEvent".to_string())
        })?;
        // Tagged so our own listen-only CGEventTap (which sees every
        // mouse-moved event, including ones this process posts itself)
        // can recognize and skip this warp instead of feeding it back
        // into position tracking as if it were real user motion -- see
        // `crate::macos::events::SYNTHETIC_EVENT_MARKER` and ADR-0009's
        // Update note for the real hardware bug this fixes.
        cg_event.set_integer_value_field(
            core_graphics::event::EventField::EVENT_SOURCE_USER_DATA,
            crate::macos::events::SYNTHETIC_EVENT_MARKER,
        );
        cg_event.post(CGEventTapLocation::HID);
        Ok(())
    }
}

fn mouse_button_event_type(
    button: MouseButton,
    state: ButtonState,
) -> (core_graphics::event::CGEventType, CGMouseButton) {
    use core_graphics::event::CGEventType;
    match (button, state) {
        (MouseButton::Left, ButtonState::Pressed) => {
            (CGEventType::LeftMouseDown, CGMouseButton::Left)
        }
        (MouseButton::Left, ButtonState::Released) => {
            (CGEventType::LeftMouseUp, CGMouseButton::Left)
        }
        (MouseButton::Right, ButtonState::Pressed) => {
            (CGEventType::RightMouseDown, CGMouseButton::Right)
        }
        (MouseButton::Right, ButtonState::Released) => {
            (CGEventType::RightMouseUp, CGMouseButton::Right)
        }
        (MouseButton::Middle, ButtonState::Pressed) => {
            (CGEventType::OtherMouseDown, CGMouseButton::Center)
        }
        (MouseButton::Middle, ButtonState::Released) => {
            (CGEventType::OtherMouseUp, CGMouseButton::Center)
        }
        // "Other" extra buttons have no direct CGMouseButton mapping;
        // approximate with Center rather than failing the whole event.
        (MouseButton::Other(_), ButtonState::Pressed) => {
            (CGEventType::OtherMouseDown, CGMouseButton::Center)
        }
        (MouseButton::Other(_), ButtonState::Released) => {
            (CGEventType::OtherMouseUp, CGMouseButton::Center)
        }
    }
}
