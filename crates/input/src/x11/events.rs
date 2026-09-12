//! XInput2 raw event ↔ [`InputMessage`] conversion — the other half of
//! the "thin shim" alongside [`super::keymap`]. Pure event-shape
//! conversion, no cross-platform translation logic.

use kvm_protocol::{ButtonState, InputMessage, MouseButton, PlatformKind};
use x11rb::protocol::xinput::{
    KeyEventFlags, RawButtonPressEvent, RawKeyPressEvent, RawMotionEvent,
};

use super::keymap::KeyMap;
use crate::scroll::UNITS_PER_NOTCH;

/// Converts a captured `RawKeyPress`/`RawKeyRelease` event into an
/// [`InputMessage`]. `pressed` distinguishes which of the two XI2 event
/// types this is — the two share an identical struct shape
/// (`RawKeyReleaseEvent` is a type alias for `RawKeyPressEvent`), so the
/// caller must say which one it actually received.
pub fn key_event_to_message(
    keymap: &KeyMap,
    event: &RawKeyPressEvent,
    pressed: bool,
) -> InputMessage {
    // XI2's raw key `detail` is the X11 keycode of the physical key.
    let key = keymap.keycode_to_key(event.detail as u8);
    InputMessage::Key {
        key,
        state: if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        },
        repeat: event.flags.contains(KeyEventFlags::KEY_REPEAT),
        source_os: PlatformKind::Linux,
    }
}

/// Converts a captured `RawButtonPress`/`RawButtonRelease` event into
/// an [`InputMessage`], or `None` for a button number this doesn't map
/// to any [`MouseButton`] concern — scroll wheel motion arrives as
/// button presses too (the classic X11 convention: buttons 4/5 are
/// vertical scroll, 6/7 horizontal), handled separately by
/// [`scroll_delta_for_button`] rather than as a button click.
pub fn button_event_to_message(event: &RawButtonPressEvent, pressed: bool) -> Option<InputMessage> {
    let state = if pressed {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    };
    // In the protocol's platform-neutral numbering (`crate::mouse`): X11's
    // Back/Forward (8/9) become Other(4)/Other(5). Buttons 4–7 are the
    // scroll wheel -- see scroll_delta_for_button.
    let button: MouseButton = crate::mouse::from_x11_button(event.detail)?;
    Some(InputMessage::MouseButton { button, state })
}

/// The scroll delta a `RawButtonPress` (never `Release` — X11 reports
/// a wheel notch as a single, immediate press+release pair, and only
/// the press carries the direction we care about) represents, or
/// `None` if this button isn't a scroll button.
pub fn scroll_delta_for_button(event: &RawButtonPressEvent) -> Option<InputMessage> {
    // The classic X11 scroll-wheel-as-buttons convention: 4 = up,
    // 5 = down, 6 = left, 7 = right. One notch per event, in the
    // protocol's scroll unit (`crate::scroll`).
    match event.detail {
        4 => Some(InputMessage::MouseScroll {
            dx: 0,
            dy: UNITS_PER_NOTCH,
        }),
        5 => Some(InputMessage::MouseScroll {
            dx: 0,
            dy: -UNITS_PER_NOTCH,
        }),
        6 => Some(InputMessage::MouseScroll {
            dx: -UNITS_PER_NOTCH,
            dy: 0,
        }),
        7 => Some(InputMessage::MouseScroll {
            dx: UNITS_PER_NOTCH,
            dy: 0,
        }),
        _ => None,
    }
}

/// Converts a `RawMotion` event into a relative [`InputMessage::MouseMove`],
/// or `None` if it carries neither an X nor a Y axis value (shouldn't
/// happen for a real pointer-motion event, but a malformed/empty
/// valuator list must not panic).
///
/// XInput2 reports only the axes that actually changed, packed
/// positionally: `valuator_mask` is a bitmask of which of the device's
/// axes are present, and `axisvalues` holds one entry per *set* bit, in
/// ascending bit order — so axis 0 (X)'s value, if present, is always
/// first, and axis 1 (Y)'s value is next if axis 0 is also present,
/// otherwise first. Sub-pixel fractional motion is truncated to whole
/// pixels (`Fp3232::integral`) — a precision loss, not a correctness
/// issue, for a relative-delta protocol already dealing in whole units
/// on every other platform.
pub fn motion_event_to_message(event: &RawMotionEvent) -> Option<InputMessage> {
    const AXIS_X_BIT: u32 = 1 << 0;
    const AXIS_Y_BIT: u32 = 1 << 1;

    let mask = event.valuator_mask.first().copied().unwrap_or(0);
    let mut values = event.axisvalues.iter();

    let dx = if mask & AXIS_X_BIT != 0 {
        values.next().map(|v| v.integral)
    } else {
        None
    };
    let dy = if mask & AXIS_Y_BIT != 0 {
        values.next().map(|v| v.integral)
    } else {
        None
    };

    if dx.is_none() && dy.is_none() {
        return None;
    }
    Some(InputMessage::MouseMove {
        dx: dx.unwrap_or(0),
        dy: dy.unwrap_or(0),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use kvm_protocol::Key;
    use x11rb::protocol::xinput::Fp3232;

    use super::super::keymap::KeyMap;

    fn key_event(detail: u32, flags: KeyEventFlags) -> RawKeyPressEvent {
        RawKeyPressEvent {
            detail,
            flags,
            ..Default::default()
        }
    }

    fn button_event(detail: u32) -> RawButtonPressEvent {
        RawButtonPressEvent {
            detail,
            ..Default::default()
        }
    }

    fn motion_event(mask: u32, values: &[i32]) -> RawMotionEvent {
        RawMotionEvent {
            valuator_mask: vec![mask],
            axisvalues: values
                .iter()
                .map(|&v| Fp3232 {
                    integral: v,
                    frac: 0,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn key_press_and_release_are_translated_with_the_keymap() {
        let map = KeyMap::from_pairs(&[(38, Key::A)]);
        let event = key_event(38, KeyEventFlags::default());

        assert_eq!(
            key_event_to_message(&map, &event, true),
            InputMessage::Key {
                key: Key::A,
                state: ButtonState::Pressed,
                repeat: false,
                source_os: PlatformKind::Linux,
            }
        );
        assert_eq!(
            key_event_to_message(&map, &event, false),
            InputMessage::Key {
                key: Key::A,
                state: ButtonState::Released,
                repeat: false,
                source_os: PlatformKind::Linux,
            }
        );
    }

    #[test]
    fn key_repeat_flag_is_carried_through() {
        let map = KeyMap::from_pairs(&[(38, Key::A)]);
        let event = key_event(38, KeyEventFlags::KEY_REPEAT);
        assert_eq!(
            key_event_to_message(&map, &event, true),
            InputMessage::Key {
                key: Key::A,
                state: ButtonState::Pressed,
                repeat: true,
                source_os: PlatformKind::Linux,
            }
        );
    }

    #[test]
    fn unmapped_keycode_becomes_unknown_not_dropped() {
        let map = KeyMap::from_pairs(&[]);
        let event = key_event(255, KeyEventFlags::default());
        assert_eq!(
            key_event_to_message(&map, &event, true),
            InputMessage::Key {
                key: Key::Unknown(255),
                state: ButtonState::Pressed,
                repeat: false,
                source_os: PlatformKind::Linux,
            }
        );
    }

    #[test]
    fn left_middle_right_buttons_map_correctly() {
        assert_eq!(
            button_event_to_message(&button_event(1), true),
            Some(InputMessage::MouseButton {
                button: MouseButton::Left,
                state: ButtonState::Pressed
            })
        );
        assert_eq!(
            button_event_to_message(&button_event(2), false),
            Some(InputMessage::MouseButton {
                button: MouseButton::Middle,
                state: ButtonState::Released
            })
        );
        assert_eq!(
            button_event_to_message(&button_event(3), true),
            Some(InputMessage::MouseButton {
                button: MouseButton::Right,
                state: ButtonState::Pressed
            })
        );
    }

    #[test]
    fn scroll_buttons_are_not_reported_as_mouse_buttons() {
        for detail in 4..=7 {
            assert_eq!(button_event_to_message(&button_event(detail), true), None);
        }
    }

    #[test]
    fn extra_buttons_beyond_scroll_map_to_other() {
        // X11's Back/Forward (8/9) arrive in the protocol's
        // platform-neutral numbering (`crate::mouse`): HID 4/5.
        assert_eq!(
            button_event_to_message(&button_event(8), true),
            Some(InputMessage::MouseButton {
                button: MouseButton::Other(crate::mouse::BACK),
                state: ButtonState::Pressed
            })
        );
        assert_eq!(
            button_event_to_message(&button_event(9), false),
            Some(InputMessage::MouseButton {
                button: MouseButton::Other(crate::mouse::FORWARD),
                state: ButtonState::Released
            })
        );
    }

    #[test]
    fn scroll_wheel_directions_produce_one_notch_each() {
        let notch = crate::scroll::UNITS_PER_NOTCH;
        assert_eq!(
            scroll_delta_for_button(&button_event(4)),
            Some(InputMessage::MouseScroll { dx: 0, dy: notch })
        );
        assert_eq!(
            scroll_delta_for_button(&button_event(5)),
            Some(InputMessage::MouseScroll { dx: 0, dy: -notch })
        );
        assert_eq!(
            scroll_delta_for_button(&button_event(6)),
            Some(InputMessage::MouseScroll { dx: -notch, dy: 0 })
        );
        assert_eq!(
            scroll_delta_for_button(&button_event(7)),
            Some(InputMessage::MouseScroll { dx: notch, dy: 0 })
        );
    }

    #[test]
    fn non_scroll_button_has_no_scroll_delta() {
        assert_eq!(scroll_delta_for_button(&button_event(1)), None);
    }

    #[test]
    fn motion_with_both_axes_present_reports_both_deltas() {
        let event = motion_event(0b11, &[5, -3]);
        assert_eq!(
            motion_event_to_message(&event),
            Some(InputMessage::MouseMove { dx: 5, dy: -3 })
        );
    }

    #[test]
    fn motion_with_only_x_axis_present_reports_zero_y() {
        let event = motion_event(0b01, &[7]);
        assert_eq!(
            motion_event_to_message(&event),
            Some(InputMessage::MouseMove { dx: 7, dy: 0 })
        );
    }

    #[test]
    fn motion_with_only_y_axis_present_reports_zero_x() {
        let event = motion_event(0b10, &[9]);
        assert_eq!(
            motion_event_to_message(&event),
            Some(InputMessage::MouseMove { dx: 0, dy: 9 })
        );
    }

    #[test]
    fn motion_with_no_relevant_axes_is_not_reported() {
        let event = motion_event(0, &[]);
        assert_eq!(motion_event_to_message(&event), None);
    }

    #[test]
    fn malformed_empty_valuator_mask_does_not_panic() {
        let event = RawMotionEvent {
            valuator_mask: vec![],
            axisvalues: vec![],
            ..Default::default()
        };
        assert_eq!(motion_event_to_message(&event), None);
    }
}
