//! `CGEvent` ↔ [`InputMessage`] conversion — the other half of the "thin
//! shim" alongside [`crate::macos::keycode`]. Pure event-shape
//! conversion, no cross-platform logic.

use core_graphics::event::{CGEvent, CGEventType, EventField};
use kvm_protocol::{ButtonState, InputMessage, MouseButton, PlatformKind};

use crate::macos::keycode::keycode_to_key;

/// Every event type [`crate::macos::MacCapture`] registers interest in.
pub const CAPTURED_EVENT_TYPES: &[CGEventType] = &[
    CGEventType::KeyDown,
    CGEventType::KeyUp,
    CGEventType::FlagsChanged,
    CGEventType::MouseMoved,
    CGEventType::LeftMouseDown,
    CGEventType::LeftMouseUp,
    CGEventType::RightMouseDown,
    CGEventType::RightMouseUp,
    CGEventType::OtherMouseDown,
    CGEventType::OtherMouseUp,
    CGEventType::LeftMouseDragged,
    CGEventType::RightMouseDragged,
    CGEventType::OtherMouseDragged,
    CGEventType::ScrollWheel,
];

/// Converts one captured `CGEvent` into an [`InputMessage`], or `None`
/// for an event type this backend doesn't turn into a message (e.g.
/// `FlagsChanged` — see the note below).
pub fn to_input_message(event_type: CGEventType, event: &CGEvent) -> Option<InputMessage> {
    match event_type {
        CGEventType::KeyDown | CGEventType::KeyUp => {
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let repeat = event.get_integer_value_field(EventField::KEYBOARD_EVENT_AUTOREPEAT) != 0;
            let state = match event_type {
                CGEventType::KeyDown => ButtonState::Pressed,
                _ => ButtonState::Released,
            };
            Some(InputMessage::Key {
                key: keycode_to_key(code),
                state,
                repeat,
                source_os: PlatformKind::MacOs,
            })
        }
        // A modifier-only transition (e.g. Shift going down or up alone)
        // arrives as FlagsChanged rather than KeyDown/KeyUp, and
        // determining which specific modifier changed and in which
        // direction requires comparing this event's flags against the
        // previous event's — genuinely stateful, unlike everything else
        // in this file. Deliberately deferred rather than guessed at:
        // documented as a known gap (see the crate's manual QA doc)
        // instead of shipping an incorrect press/release inference.
        CGEventType::FlagsChanged => None,
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32;
            let dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32;
            Some(InputMessage::MouseMove { dx, dy })
        }
        CGEventType::LeftMouseDown => Some(InputMessage::MouseButton {
            button: MouseButton::Left,
            state: ButtonState::Pressed,
        }),
        CGEventType::LeftMouseUp => Some(InputMessage::MouseButton {
            button: MouseButton::Left,
            state: ButtonState::Released,
        }),
        CGEventType::RightMouseDown => Some(InputMessage::MouseButton {
            button: MouseButton::Right,
            state: ButtonState::Pressed,
        }),
        CGEventType::RightMouseUp => Some(InputMessage::MouseButton {
            button: MouseButton::Right,
            state: ButtonState::Released,
        }),
        CGEventType::OtherMouseDown => Some(InputMessage::MouseButton {
            button: MouseButton::Middle,
            state: ButtonState::Pressed,
        }),
        CGEventType::OtherMouseUp => Some(InputMessage::MouseButton {
            button: MouseButton::Middle,
            state: ButtonState::Released,
        }),
        CGEventType::ScrollWheel => {
            let dy =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_1) as i32;
            let dx =
                event.get_integer_value_field(EventField::SCROLL_WHEEL_EVENT_DELTA_AXIS_2) as i32;
            Some(InputMessage::MouseScroll { dx, dy })
        }
        _ => None,
    }
}
