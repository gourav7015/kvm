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
/// `last_flags` and `post_warp_anchor` carry state across calls within
/// one capture session (owned by the caller — see
/// [`crate::macos::capture`] — and expected to start at
/// [`CGEventFlags::empty`]/`None` each time capture (re)starts):
/// `last_flags` because a `FlagsChanged` event only reports the
/// *current* combined flags, not which direction the specific key that
/// triggered it just moved; `post_warp_anchor` because the first real
/// motion event after one of this process's own warps carries the
/// warp's displacement in its delta fields — see [`mouse_delta`] and
/// ADR-0009 decision 18.
pub fn to_input_message(
    event_type: CGEventType,
    event: &CGEvent,
    last_flags: &mut CGEventFlags,
    post_warp_anchor: &mut Option<CGPoint>,
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
            if is_our_own_synthetic_event(event) {
                // See `SYNTHETIC_EVENT_MARKER` -- never reported as user
                // motion. But remember where it landed: the very next
                // *real* event's delta fields still include this warp's
                // displacement (see `mouse_delta` / ADR-0009 decision
                // 18), so that one event is measured from here instead.
                *post_warp_anchor = Some(location);
                return None;
            }
            let raw = mouse_delta(event);
            let anchor = post_warp_anchor.take();
            let (dx, dy) = match anchor {
                Some(anchor) => anchored_delta(anchor, location),
                None => raw,
            };
            // Stage 1 of the "follow one physical movement through the
            // whole pipeline" trace (stages 2-4: `core::router`,
            // `core::session`, `windows::inject`). Once the pointer is
            // pushed past the Mac's own display bounds, `loc_x`/`loc_y`
            // pin at the boundary while `dx`/`dy` keep reporting real
            // motion -- expected, see `mouse_delta`. `re_anchored` marks
            // the one event per warp measured from the warp's landing
            // point; `raw_dx`/`raw_dy` show the contaminated delta fields
            // it replaced.
            tracing::debug!(
                stage = "1-capture",
                loc_x = location.x,
                loc_y = location.y,
                dx,
                dy,
                raw_dx = raw.0,
                raw_dy = raw.1,
                re_anchored = anchor.is_some(),
                "macOS capture: mouse motion"
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

/// How far this mouse-motion event says the pointer moved, read from
/// the event's own `kCGMouseEventDeltaX`/`kCGMouseEventDeltaY` fields.
///
/// **This must not be derived from `CGEvent::location()` instead** —
/// that was the cause of the Phase 4 "the remote cursor is boxed into
/// the middle of the screen" failure, and ADR-0009 decision 17 records
/// the measurement. `location()` is an *absolute screen coordinate*,
/// clamped by the WindowServer to this Mac's own display. Once the user
/// pushes the pointer past that boundary — which a `Forwarding` session
/// does constantly, since the whole point is to keep moving after the
/// local screen runs out — successive readings are identical, so a
/// delta diffed from them is `0` forever after, no matter how far the
/// hand actually travels. Disassociating the cursor does not help: it
/// stops the cursor being *drawn*, while the underlying position keeps
/// tracking HID motion and keeps clamping.
///
/// The delta fields carry no such bound: they describe motion, not
/// position, so they stay correct with the pointer parked against a
/// boundary. Measured on real hardware (2026-09-12, macOS 26.6.2,
/// 1470x956): while the pointer was free to move, these fields and the
/// location-derived delta agreed *exactly* across the full speed range
/// (9, 17, 23, 30, 64, 94, 102 points/event); the instant `location()`
/// pinned at 1470.0 the location-derived delta collapsed to 0 while
/// these fields went on reporting 102, 44, 89, 75. They are already
/// screen-point deltas with macOS's pointer-acceleration curve applied
/// — *not* raw pre-acceleration HID counts, as an earlier revision of
/// this file asserted without measuring.
///
/// **One exception, also measured: the first real event after one of
/// this process's own warps.** macOS computes these fields against the
/// pointer's last *hardware* position, not against where a programmatic
/// warp just put it — so the first real motion event after
/// `Effect::RecenterLocal`'s warp carries the whole warp displacement as
/// if the hand had made it. Real Mac->Windows acceptance run
/// (2026-09-12): the pointer at x=1469.98 crossed the right edge, the
/// recentre warped it to (735, 478), and the next real event, at
/// x=740.08, reported `kCGMouseEventDeltaX = -729` — exactly
/// `740.08 - 1469.98`, the warp plus ~5 points of genuine motion. On the
/// remote screen that is an instant ~730-point jump left from x=5,
/// straight back across the edge home: all 20 switches in that run
/// bounced back within ~8ms. [`to_input_message`] therefore measures
/// that single event from the warp's landing point instead (see
/// [`anchored_delta`]); every other event uses these fields.
fn mouse_delta(event: &CGEvent) -> (i32, i32) {
    (
        event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_X) as i32,
        event.get_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y) as i32,
    )
}

/// The delta from `anchor` — where this process's own most recent warp
/// landed — to `current`, rounded to whole points. Used for exactly one
/// event per warp (see [`mouse_delta`]'s exception). The display clamp
/// that rules out diffing locations in general doesn't reach this one
/// event: the only warp the capturing device issues (`RecenterLocal`)
/// lands at the centre of the screen, and this is the very next event,
/// a few points from it.
fn anchored_delta(anchor: CGPoint, current: CGPoint) -> (i32, i32) {
    (
        (current.x - anchor.x).round() as i32,
        (current.y - anchor.y).round() as i32,
    )
}

/// The release that completes a Caps Lock toggle into a full tap: `Some`
/// only for the `CapsLock` press [`to_input_message`] reports for a
/// `FlagsChanged` event (see `modifier_transition`). The capture callback
/// sends it immediately after that press.
pub fn caps_lock_tap_release(
    event_type: CGEventType,
    message: &InputMessage,
) -> Option<InputMessage> {
    match (event_type, message) {
        (
            CGEventType::FlagsChanged,
            InputMessage::Key {
                key: Key::CapsLock,
                state: ButtonState::Pressed,
                ..
            },
        ) => Some(InputMessage::Key {
            key: Key::CapsLock,
            state: ButtonState::Released,
            repeat: false,
            source_os: PlatformKind::MacOs,
        }),
        _ => None,
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
    // Caps Lock is a toggle, not a held modifier: macOS reports each
    // on/off change of its flag, not the key's own down/up. Every change
    // is reported as a press, completed by a release in
    // `caps_lock_tap_release`, so the receiver sees one full tap per toggle
    // and toggles its own Caps Lock to match. Real hardware: Caps Lock
    // previously never reached the target at all.
    if key == Key::CapsLock {
        let toggled = previous_flags.contains(CGEventFlags::CGEventFlagAlphaShift)
            != new_flags.contains(CGEventFlags::CGEventFlagAlphaShift);
        return toggled.then_some(InputMessage::Key {
            key: Key::CapsLock,
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::MacOs,
        });
    }
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
    /// real instead of only through the pure [`anchored_delta`] helper.
    fn mouse_moved_event(x: f64, y: f64, synthetic: bool) -> CGEvent {
        mouse_moved_event_with_delta(x, y, 0, 0, synthetic)
    }

    /// As [`mouse_moved_event`], but also sets the event's own motion
    /// fields — the pair that matters is `(x, y)` (where the OS says the
    /// pointer *is*) versus `(delta_x, delta_y)` (how far it just
    /// *moved*). Real hardware drives these independently: once the
    /// pointer is clamped against a display boundary the position stops
    /// changing while the deltas keep reporting motion, which is exactly
    /// the case these tests need to construct.
    fn mouse_moved_event_with_delta(
        x: f64,
        y: f64,
        delta_x: i64,
        delta_y: i64,
        synthetic: bool,
    ) -> CGEvent {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .expect("failed to create CGEventSource");
        let event = CGEvent::new_mouse_event(
            source,
            CGEventType::MouseMoved,
            CGPoint { x, y },
            CGMouseButton::Left,
        )
        .expect("failed to create mouse-move CGEvent");
        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_X, delta_x);
        event.set_integer_value_field(EventField::MOUSE_EVENT_DELTA_Y, delta_y);
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
        let mut anchor = None;
        let event = mouse_moved_event(500.0, 400.0, true);
        let message = to_input_message(
            CGEventType::MouseMoved,
            &event,
            &mut last_flags,
            &mut anchor,
        );
        assert_eq!(message, None);
    }

    /// **Regression test for the Phase 4 acceptance-run bounce** (ADR-0009
    /// decision 18), built verbatim from the real hardware log.
    ///
    /// Every one of 20 Mac->Windows switches bounced straight back home
    /// within ~8ms. The first real event after `RecenterLocal`'s warp
    /// reported `kCGMouseEventDeltaX = -729`: macOS computes the delta
    /// fields against the last *hardware* position (the right edge,
    /// x=1469.98), so the warp's own ~734-point jump to the centre was
    /// folded into an unmarked, genuinely-hardware event that the
    /// synthetic-event marker cannot catch. The remote cursor went from
    /// x=5 to x=-724 -- across the left edge, home.
    #[test]
    fn the_first_real_event_after_our_own_warp_is_measured_from_where_the_warp_landed() {
        let (mut last_flags, mut anchor) = (CGEventFlags::empty(), None);

        // Last real event before the switch: pointer at the right edge.
        let at_edge = mouse_moved_event_with_delta(1469.98046875, 486.6796875, 6, 1, false);
        assert_eq!(
            to_input_message(
                CGEventType::MouseMoved,
                &at_edge,
                &mut last_flags,
                &mut anchor
            ),
            Some(InputMessage::MouseMove { dx: 6, dy: 1 })
        );

        // RecenterLocal's own warp to the screen centre: filtered.
        let warp = mouse_moved_event_with_delta(735.0, 478.0, 0, 0, true);
        assert_eq!(
            to_input_message(CGEventType::MouseMoved, &warp, &mut last_flags, &mut anchor),
            None
        );

        // The next real event, verbatim: its delta fields carry the
        // warp's whole displacement (-729, -7). The hand actually moved
        // the ~5 points from where the warp landed.
        let first_after_warp =
            mouse_moved_event_with_delta(740.08203125, 478.5078125, -729, -7, false);
        assert_eq!(
            to_input_message(
                CGEventType::MouseMoved,
                &first_after_warp,
                &mut last_flags,
                &mut anchor
            ),
            Some(InputMessage::MouseMove { dx: 5, dy: 1 }),
            "the first real event after our own warp must be measured from where the warp \
             landed -- its delta fields include the warp itself, which bounced every Phase 4 \
             switch straight back home"
        );

        // The event after that is clean again, and must come from its
        // own delta fields (5, 0) -- diffing locations would give (6, 1)
        // here -- proving the anchor is used for exactly one event.
        let next = mouse_moved_event_with_delta(745.98046875, 479.578125, 5, 0, false);
        assert_eq!(
            to_input_message(CGEventType::MouseMoved, &next, &mut last_flags, &mut anchor),
            Some(InputMessage::MouseMove { dx: 5, dy: 0 })
        );
    }

    /// The second bounce from the same run: the warp's *vertical*
    /// displacement (523.41 -> 478.0, reported as `dy = -45`) is
    /// contamination too, not just the horizontal one.
    #[test]
    fn warp_contamination_is_removed_from_both_axes() {
        let (mut last_flags, mut anchor) = (CGEventFlags::empty(), None);
        let warp = mouse_moved_event_with_delta(735.0, 478.0, 0, 0, true);
        assert_eq!(
            to_input_message(CGEventType::MouseMoved, &warp, &mut last_flags, &mut anchor),
            None
        );
        let first_after_warp = mouse_moved_event_with_delta(752.15625, 478.0, -695, -45, false);
        assert_eq!(
            to_input_message(
                CGEventType::MouseMoved,
                &first_after_warp,
                &mut last_flags,
                &mut anchor
            ),
            Some(InputMessage::MouseMove { dx: 17, dy: 0 })
        );
    }

    #[test]
    fn a_real_unmarked_event_still_produces_a_move() {
        let mut last_flags = CGEventFlags::empty();
        let mut anchor = None;
        let event = mouse_moved_event_with_delta(500.0, 400.0, 10, 0, false);
        let message = to_input_message(
            CGEventType::MouseMoved,
            &event,
            &mut last_flags,
            &mut anchor,
        );
        assert_eq!(message, Some(InputMessage::MouseMove { dx: 10, dy: 0 }));
    }

    /// **The Phase 4 pointer-range regression test**, at the lowest layer
    /// that can actually reproduce the failure (ADR-0009 decision 17).
    ///
    /// Real hardware measurement, macOS 26.6.2 on a 1470x956 display:
    /// pushing the pointer past the Mac's own right edge pinned
    /// `CGEvent::location()` at exactly 1470.0 while the mouse kept
    /// physically moving. The previous implementation derived
    /// `InputMessage::MouseMove` by diffing successive `location()`
    /// readings, so it reported `dx: 0` for every one of those events —
    /// destroying the movement at the first stage of the pipeline, with
    /// no possibility of any later stage recovering it. On the remote
    /// screen that showed up as a cursor that could not reach the far
    /// edges: "boxed into the middle."
    ///
    /// This reconstructs exactly that situation — consecutive events at
    /// an identical, clamped location, each carrying real motion — and
    /// asserts the real motion is what gets reported.
    #[test]
    fn motion_is_still_reported_when_the_pointer_is_clamped_at_the_screen_edge() {
        let mut last_flags = CGEventFlags::empty();
        let mut anchor = None;
        // Verbatim from the real hardware capture (samples 86-89): the
        // location is pinned at the display's boundary and never
        // changes, while the mouse reports substantial continued motion.
        let clamped_at_right_edge = [102, 44, 89, 75];
        for hid_dx in clamped_at_right_edge {
            let event = mouse_moved_event_with_delta(1470.0, 411.4, hid_dx, 0, false);
            assert_eq!(
                to_input_message(
                    CGEventType::MouseMoved,
                    &event,
                    &mut last_flags,
                    &mut anchor
                ),
                Some(InputMessage::MouseMove {
                    dx: hid_dx as i32,
                    dy: 0
                }),
                "an event whose location is clamped at the screen edge must still report \
                 the motion it actually carries -- diffing locations reports 0 here, which \
                 is the Phase 4 'remote cursor is boxed into the middle' bug"
            );
        }

        // Symmetrically for the vertical clamp (real samples 314-320,
        // location pinned at y=956.0) and for the negative direction.
        for hid_dy in [2, 5, 3, -8, -12] {
            let event = mouse_moved_event_with_delta(1470.0, 956.0, 0, hid_dy, false);
            assert_eq!(
                to_input_message(
                    CGEventType::MouseMoved,
                    &event,
                    &mut last_flags,
                    &mut anchor
                ),
                Some(InputMessage::MouseMove {
                    dx: 0,
                    dy: hid_dy as i32
                }),
            );
        }
    }

    /// A genuinely stationary pointer must still report no motion — the
    /// fix must not manufacture movement out of a clamped position.
    #[test]
    fn a_stationary_pointer_reports_no_motion() {
        let mut last_flags = CGEventFlags::empty();
        let mut anchor = None;
        let event = mouse_moved_event_with_delta(1470.0, 411.4, 0, 0, false);
        assert_eq!(
            to_input_message(
                CGEventType::MouseMoved,
                &event,
                &mut last_flags,
                &mut anchor
            ),
            Some(InputMessage::MouseMove { dx: 0, dy: 0 })
        );
    }

    /// Drag events carry motion the same way plain moves do — they are
    /// forwarded through the identical arm, so the fix must cover them.
    #[test]
    fn drag_events_report_motion_too() {
        let mut last_flags = CGEventFlags::empty();
        let mut anchor = None;
        for event_type in [
            CGEventType::LeftMouseDragged,
            CGEventType::RightMouseDragged,
            CGEventType::OtherMouseDragged,
        ] {
            let event = mouse_moved_event_with_delta(1470.0, 400.0, 33, -16, false);
            assert_eq!(
                to_input_message(event_type, &event, &mut last_flags, &mut anchor),
                Some(InputMessage::MouseMove { dx: 33, dy: -16 }),
                "{event_type:?} must report motion identically to a plain move"
            );
        }
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

    /// Regression test for Caps Lock never reaching the target (real
    /// hardware, ADR-0007's 2026-09-12 update): turning it on *and* turning
    /// it off must each produce a press.
    #[test]
    fn caps_lock_on_and_off_each_produce_a_press() {
        let caps_press = Some(InputMessage::Key {
            key: Key::CapsLock,
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::MacOs,
        });
        let alpha = CGEventFlags::CGEventFlagAlphaShift;
        assert_eq!(
            modifier_transition(Key::CapsLock, CGEventFlags::empty(), alpha),
            caps_press
        );
        assert_eq!(
            modifier_transition(Key::CapsLock, alpha, CGEventFlags::empty()),
            caps_press
        );
    }

    #[test]
    fn caps_lock_with_no_flag_change_reports_nothing() {
        let alpha = CGEventFlags::CGEventFlagAlphaShift;
        assert_eq!(
            modifier_transition(Key::CapsLock, CGEventFlags::empty(), CGEventFlags::empty()),
            None
        );
        assert_eq!(modifier_transition(Key::CapsLock, alpha, alpha), None);
    }

    #[test]
    fn a_caps_lock_toggle_is_completed_into_a_full_tap() {
        let press = InputMessage::Key {
            key: Key::CapsLock,
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::MacOs,
        };
        assert_eq!(
            caps_lock_tap_release(CGEventType::FlagsChanged, &press),
            Some(InputMessage::Key {
                key: Key::CapsLock,
                state: ButtonState::Released,
                repeat: false,
                source_os: PlatformKind::MacOs,
            })
        );
        // Nothing else is ever completed: other modifiers keep their real
        // down/up, and no other event type is touched.
        let shift = InputMessage::Key {
            key: Key::ShiftLeft,
            state: ButtonState::Pressed,
            repeat: false,
            source_os: PlatformKind::MacOs,
        };
        assert_eq!(
            caps_lock_tap_release(CGEventType::FlagsChanged, &shift),
            None
        );
        assert_eq!(caps_lock_tap_release(CGEventType::KeyDown, &press), None);
    }

    #[test]
    fn unrelated_keys_are_unaffected() {
        assert_eq!(
            modifier_transition(Key::A, CGEventFlags::empty(), CGEventFlags::empty()),
            None
        );
    }
}
