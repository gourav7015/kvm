//! macOS injection via synthetic `CGEvent`s posted at the HID level.

use core_graphics::event::{CGEvent, CGEventTapLocation, CGMouseButton, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use core_graphics::geometry::CGPoint;
use kvm_protocol::{ButtonState, InputMessage, MouseButton};

use crate::error::InputError;
use crate::macos::keycode::key_to_keycode;
use crate::macos::permission::has_accessibility_permission;
use crate::traits::Inject;

#[derive(Default)]
pub struct MacInject;

impl MacInject {
    pub fn new() -> Self {
        Self
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
            InputMessage::Key { key, state, .. } => {
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
                cg_event.post(CGEventTapLocation::HID);
                Ok(())
            }
            InputMessage::MouseScroll { dx, dy } => {
                let source = event_source()?;
                let cg_event =
                    CGEvent::new_scroll_event(source, ScrollEventUnit::PIXEL, 2, dy, dx, 0)
                        .map_err(|()| {
                            InputError::InjectFailed("failed to create scroll CGEvent".to_string())
                        })?;
                cg_event.post(CGEventTapLocation::HID);
                Ok(())
            }
        }
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
