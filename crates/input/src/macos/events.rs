//! `CGEvent` ↔ [`InputMessage`] conversion — the other half of the "thin
//! shim" alongside [`crate::macos::keycode`]. Pure event-shape
//! conversion, no cross-platform logic.

use core_graphics::event::{CGEvent, CGEventFlags, CGEventType, EventField};
use core_graphics::geometry::CGPoint;
use kvm_protocol::{ButtonState, InputMessage, Key, MouseButton, PlatformKind};

use crate::macos::keycode::keycode_to_key;

/// Tags a `CGEvent` this process posted itself (e.g. an absolute
/// cursor warp from `MacInject::set_cursor_position`), written into
/// `EventField::EVENT_SOURCE_USER_DATA` — the documented mechanism for
/// exactly this purpose. `CGEventTap` sees *all* mouse-moved events,
/// including ones this same process synthesizes, so without this a
/// programmatic warp gets captured right back as if it were real user
/// motion.
///
/// **Found via real Mac->Windows hardware QA** (see ADR-0009's Update
/// note): `Session`'s `RecenterLocal` handling warps the local cursor
/// via `set_cursor_position` the moment ownership leaves `Local` --
/// that warp was itself being captured as one huge `MouseMove` delta
/// (the full jump from the pinned edge to the screen's center) and fed
/// straight back into `Router`, which misapplied it to the *virtual*
/// position it was tracking on the new target's screen, immediately
/// bouncing ownership back. An arbitrary, distinctive nonzero constant
/// -- real hardware essentially never sets this field.
pub(crate) const SYNTHETIC_EVENT_MARKER: i64 = 0x004B_564D_5741_5250;

/// Whether a captured `CGEvent` was posted by this same process (via
/// [`SYNTHETIC_EVENT_MARKER`]) rather than originating from real
/// hardware.
fn is_our_own_synthetic_event(event: &CGEvent) -> bool {
    event.get_integer_value_field(EventField::EVENT_SOURCE_USER_DATA) == SYNTHETIC_EVENT_MARKER
}

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
/// for an event this doesn't produce a message for (e.g. a
/// `FlagsChanged` transition that can't be disambiguated — see
/// [`modifier_transition`]).
///
/// `last_flags` and `last_position` carry state across calls within one
/// capture session (owned by the caller — see [`crate::macos::capture`]
/// — and expected to start at [`CGEventFlags::empty`]/`None`
/// respectively each time capture (re)starts): `last_flags` because a
/// `FlagsChanged` event only reports the *current* combined flags, not
/// which direction the specific key that triggered it just moved;
/// `last_position` because a mouse-move event's delta fields
/// (`kCGMouseEventDeltaX/Y`) are raw, pre-acceleration-curve HID counts
/// — not screen-point distances — and this project's `InputMessage::MouseMove`
/// contract is a screen-point delta (see [`point_delta`]).
pub fn to_input_message(
    event_type: CGEventType,
    event: &CGEvent,
    last_flags: &mut CGEventFlags,
    last_position: &mut Option<CGPoint>,
) -> Option<InputMessage> {
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
        // A modifier-only transition (Shift/Control/Option/Command going
        // down or up alone) arrives as FlagsChanged rather than
        // KeyDown/KeyUp. The event's own keycode field still identifies
        // *which* physical key changed (so ShiftLeft vs ShiftRight is
        // still distinguishable), but CGEventFlags only tracks one bit
        // per modifier *category* — so whether this was a press or a
        // release has to be inferred by diffing against the
        // previously-seen flags, not read directly off the event.
        CGEventType::FlagsChanged => {
            let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE) as u16;
            let key = keycode_to_key(code);
            let new_flags = event.get_flags();
            let previous_flags = std::mem::replace(last_flags, new_flags);
            modifier_transition(key, previous_flags, new_flags)
        }
        CGEventType::MouseMoved
        | CGEventType::LeftMouseDragged
        | CGEventType::RightMouseDragged
        | CGEventType::OtherMouseDragged => {
            let location = event.location();
            let previous = last_position.replace(location);
            if is_our_own_synthetic_event(event) {
                // Still re-anchor `last_position` (above) so the *next*
                // real event's delta is computed from where the warp
                // actually landed -- just don't report this one as
                // real user motion. See `SYNTHETIC_EVENT_MARKER`.
                return None;
            }
            let (dx, dy) = point_delta(previous, location);
            // DIAGNOSTIC (Phase 4 pointer-range root-cause hunt): the
            // first stage of the "follow one physical movement through
            // the whole pipeline" trace. Logs both independent readings
            // of how far the pointer just moved, side by side:
            //
            //   `location`/`dx`/`dy` -- the absolute-position-derived
            //   delta this backend actually ships (see `point_delta`).
            //
            //   `hid_dx`/`hid_dy` -- `kCGMouseEventDeltaX/Y`, the
            //   event's own relative motion fields, which come from the
            //   HID layer and are not a function of any screen position.
            //
            // These two agreeing means the Mac is reporting movement
            // faithfully and any loss is downstream. `dx`/`dy` going to
            // zero while `hid_dx`/`hid_dy` keep reporting real motion
            // means the movement is already gone by this line -- nothing
            // further down the pipeline could then possibly recover it.
            // Read alongside `crates/input/examples/mac_pointer_probe.rs`,
            // which measures the same thing standalone.
            let hid_dx = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X);
            let hid_dy = event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y);
            tracing::debug!(
                stage = "1-capture",
                loc_x = location.x,
                loc_y = location.y,
                dx,
                dy,
                hid_dx,
                hid_dy,
                point_delta_lost_real_motion = (dx == 0 && dy == 0) && (hid_dx != 0 || hid_dy != 0),
                "macOS capture: CGEvent location and the delta derived from it"
            );
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

/// The screen-point delta between two absolute cursor readings, or
/// `(0, 0)` if there's no previous reading yet (the very first
/// mouse-move event seen since capture started — nothing to diff
/// against, and reporting a large one-off jump from an arbitrary origin
/// would be worse than reporting no motion for that single event).
///
/// Pure and `CGEvent`-independent so it's fully unit-testable — the
/// surrounding `CGEvent::location()` call in [`to_input_message`] is
/// the actual "thin shim" piece (see ADR-0007/ADR-0009). Sub-point
/// motion is rounded to the nearest whole point, matching the whole-unit
/// `InputMessage::MouseMove` contract every other platform already uses.
fn point_delta(previous: Option<CGPoint>, current: CGPoint) -> (i32, i32) {
    match previous {
        Some(previous) => (
            (current.x - previous.x).round() as i32,
            (current.y - previous.y).round() as i32,
        ),
        None => (0, 0),
    }
}

/// The `CGEventFlags` bit that tracks whether *any* key in `key`'s
/// modifier category is currently held, or `None` for a key with no
/// such category (including Caps Lock, deliberately excluded — its
/// flag is a toggle rather than a held-state, a different enough
/// semantic that folding it into this same press/release inference
/// isn't attempted here).
fn modifier_mask_for_key(key: Key) -> Option<CGEventFlags> {
    match key {
        Key::ShiftLeft | Key::ShiftRight => Some(CGEventFlags::CGEventFlagShift),
        Key::ControlLeft | Key::ControlRight => Some(CGEventFlags::CGEventFlagControl),
        Key::AltLeft | Key::AltRight => Some(CGEventFlags::CGEventFlagAlternate),
        Key::MetaLeft | Key::MetaRight => Some(CGEventFlags::CGEventFlagCommand),
        _ => None,
    }
}

/// Pure decision logic for one `FlagsChanged` event: given which
/// physical key it was reported for and the modifier flags immediately
/// before and after, decide what (if anything) happened. Deliberately
/// pure and CGEvent-independent so it's fully unit-testable — the
/// surrounding `CGEvent` field extraction in [`to_input_message`] is the
/// actual "thin shim" piece, verified by manual QA instead (per
/// ADR-0007, matching every other macOS/Windows backend module).
///
/// `CGEventFlags` only has one bit per modifier *category* — e.g. a
/// single Shift bit covers both `ShiftLeft` and `ShiftRight` — so
/// holding one side of a category down while pressing or releasing the
/// other leaves the category bit unchanged. That specific case can't be
/// told apart from "nothing relevant happened" using only this event,
/// and is intentionally reported as `None` (no event) rather than
/// guessed at.
fn modifier_transition(
    key: Key,
    previous_flags: CGEventFlags,
    new_flags: CGEventFlags,
) -> Option<InputMessage> {
    let mask = modifier_mask_for_key(key)?;
    let was_set = previous_flags.contains(mask);
    let now_set = new_flags.contains(mask);
    if was_set == now_set {
        return None;
    }
    let state = if now_set {
        ButtonState::Pressed
    } else {
        ButtonState::Released
    };
    Some(InputMessage::Key {
        key,
        state,
        repeat: false,
        source_os: PlatformKind::MacOs,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use core_graphics::event::CGMouseButton;
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    use super::*;

    /// Constructs a real (unposted -- never actually injected, no
    /// Accessibility permission needed) `MouseMoved` `CGEvent`, so
    /// [`to_input_message`]'s marker-filtering can be exercised for
    /// real instead of only through the pure [`point_delta`] helper.
    fn mouse_moved_event(x: f64, y: f64, synthetic: bool) -> CGEvent {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .expect("failed to create CGEventSource");
        let event = CGEvent::new_mouse_event(
            source,
            CGEventType::MouseMoved,
            CGPoint { x, y },
            CGMouseButton::Left,
        )
        .expect("failed to create mouse-move CGEvent");
        if synthetic {
            event.set_integer_value_field(
                EventField::EVENT_SOURCE_USER_DATA,
                SYNTHETIC_EVENT_MARKER,
            );
        }
        event
    }

    /// Regression test for a real Mac->Windows hardware QA finding (see
    /// ADR-0009's Update note): `Session`'s local-cursor-recentering
    /// warp was being captured by this same process's own listen-only
    /// `CGEventTap` and fed back into position tracking as if it were
    /// real user motion, since `CGEventTap` sees every mouse-moved
    /// event including ones this process posts itself. A synthetic
    /// event must produce no `InputMessage` at all.
    #[test]
    fn a_synthetic_self_posted_warp_event_produces_no_input_message() {
        let mut last_flags = CGEventFlags::empty();
        let mut last_position = None;
        let event = mouse_moved_event(500.0, 400.0, true);
        let message = to_input_message(
            CGEventType::MouseMoved,
            &event,
            &mut last_flags,
            &mut last_position,
        );
        assert_eq!(message, None);
        // The position baseline must still update, so the *next* real
        // event's delta is computed from where the warp actually
        // landed, not some stale pre-warp location. (`CGPoint` has no
        // `PartialEq`, so compare fields directly.)
        let updated = last_position.expect("last_position must be set after any event");
        assert_eq!((updated.x, updated.y), (500.0, 400.0));
    }

    #[test]
    fn a_real_unmarked_event_still_produces_a_move() {
        let mut last_flags = CGEventFlags::empty();
        let mut last_position = Some(CGPoint { x: 490.0, y: 400.0 });
        let event = mouse_moved_event(500.0, 400.0, false);
        let message = to_input_message(
            CGEventType::MouseMoved,
            &event,
            &mut last_flags,
            &mut last_position,
        );
        assert_eq!(message, Some(InputMessage::MouseMove { dx: 10, dy: 0 }));
    }

    #[test]
    fn first_reading_with_no_previous_position_reports_zero_motion() {
        // Regression test: a real-hardware Phase 4 edge-switch failure
        // traced to this exact class of bug (see ADR-0009) -- the fix
        // is diffing absolute CGEvent locations rather than trusting
        // the raw, pre-acceleration-curve kCGMouseEventDeltaX/Y fields,
        // and this first-event case is the one part of that fix that
        // isn't simple subtraction: there's nothing to diff against yet.
        assert_eq!(point_delta(None, CGPoint { x: 500.0, y: 400.0 }), (0, 0));
    }

    #[test]
    fn subsequent_reading_reports_the_real_screen_point_delta() {
        let previous = CGPoint { x: 500.0, y: 400.0 };
        let current = CGPoint { x: 512.0, y: 397.0 };
        assert_eq!(point_delta(Some(previous), current), (12, -3));
    }

    #[test]
    fn point_delta_rounds_fractional_motion_to_the_nearest_whole_point() {
        let previous = CGPoint { x: 100.0, y: 100.0 };
        let current = CGPoint { x: 100.6, y: 99.4 };
        assert_eq!(point_delta(Some(previous), current), (1, -1));
    }

    #[test]
    fn point_delta_is_not_the_raw_hid_delta_field() {
        // The whole point of this fix: a real screen-edge-reaching
        // movement (a large point-space displacement) must not be
        // reported as some smaller, uncorrelated raw-HID-count value --
        // this asserts the actual on-screen distance is what comes out,
        // for a displacement large enough to plausibly cross a real
        // screen's width.
        let previous = CGPoint { x: 0.0, y: 0.0 };
        let current = CGPoint { x: 1470.0, y: 0.0 };
        assert_eq!(point_delta(Some(previous), current), (1470, 0));
    }

    #[test]
    fn a_bare_modifier_press_is_reported() {
        for key in [
            Key::ShiftLeft,
            Key::ShiftRight,
            Key::ControlLeft,
            Key::ControlRight,
            Key::AltLeft,
            Key::AltRight,
            Key::MetaLeft,
            Key::MetaRight,
        ] {
            let mask = modifier_mask_for_key(key).unwrap();
            let event = modifier_transition(key, CGEventFlags::empty(), mask);
            assert_eq!(
                event,
                Some(InputMessage::Key {
                    key,
                    state: ButtonState::Pressed,
                    repeat: false,
                    source_os: PlatformKind::MacOs,
                }),
                "{key:?} press was not reported"
            );
        }
    }

    #[test]
    fn a_bare_modifier_release_is_reported() {
        let mask = modifier_mask_for_key(Key::ControlLeft).unwrap();
        let event = modifier_transition(Key::ControlLeft, mask, CGEventFlags::empty());
        assert_eq!(
            event,
            Some(InputMessage::Key {
                key: Key::ControlLeft,
                state: ButtonState::Released,
                repeat: false,
                source_os: PlatformKind::MacOs,
            })
        );
    }

    #[test]
    fn no_actual_change_reports_nothing() {
        let mask = modifier_mask_for_key(Key::AltLeft).unwrap();
        // Flags identical before and after: e.g. a redundant FlagsChanged
        // firing for a key whose category bit was already set/clear.
        assert_eq!(modifier_transition(Key::AltLeft, mask, mask), None);
        assert_eq!(
            modifier_transition(Key::AltLeft, CGEventFlags::empty(), CGEventFlags::empty()),
            None
        );
    }

    #[test]
    fn releasing_one_side_while_the_other_is_still_held_is_not_guessed_at() {
        // Both Shift keys held (category bit set), then ShiftLeft's own
        // FlagsChanged fires for its release — but ShiftRight is still
        // down, so the category bit is still set on both sides of this
        // diff. Can't tell this apart from "nothing changed" using only
        // this event; must not be reported as a (wrong) press.
        let shift = modifier_mask_for_key(Key::ShiftLeft).unwrap();
        assert_eq!(modifier_transition(Key::ShiftLeft, shift, shift), None);
    }

    #[test]
    fn caps_lock_is_not_handled_by_this_inference() {
        assert_eq!(
            modifier_transition(Key::CapsLock, CGEventFlags::empty(), CGEventFlags::empty()),
            None
        );
        assert_eq!(
            modifier_transition(
                Key::CapsLock,
                CGEventFlags::empty(),
                CGEventFlags::CGEventFlagAlphaShift
            ),
            None
        );
    }

    #[test]
    fn unrelated_keys_are_unaffected() {
        assert_eq!(
            modifier_transition(Key::A, CGEventFlags::empty(), CGEventFlags::empty()),
            None
        );
    }
}
