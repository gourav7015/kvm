//! Pure per-event routing decisions for the capturing device — see
//! ADR-0009. Wraps [`crate::ownership`]'s state machine with position
//! tracking and modifier-safety bookkeeping, but stays exactly as free
//! of I/O as `ownership.rs`/`layout.rs`: no `Peer`, no OS calls, no
//! async. [`crate::session`] is the thin async layer that turns this
//! module's [`Effect`]s into real network sends.
//!
//! **Only the capturing device runs a [`Router`].** A device that is
//! merely the current forwarding target needs nothing from this
//! module at all — it just keeps injecting whatever arrives on its
//! peer's input stream via the existing Phase 3 `inject_from_peer`,
//! including the modifier-release events this module synthesizes,
//! which travel as perfectly ordinary [`InputMessage`]s on that same
//! stream.
//!
//! **Why `Local` routing never produces an effect**: every backend's
//! `Capture` is listen-only (see ADR-0007/ADR-0008 — no exclusive
//! grab), so the local OS has *already* applied a captured event by
//! the time this module sees it. Re-injecting it locally would be the
//! exact duplicate-input bug the Phase 4 kickoff calls out. Only
//! `Forwarding` ever produces a [`Effect::Send`] — see the module docs
//! on `session.rs` for the real-hardware consequence this implies for
//! a later milestone (local input is not yet actually suppressed while
//! forwarding).

use std::collections::{HashMap, HashSet};

use kvm_protocol::{ButtonState, DeviceId, InputMessage, Key, PlatformKind};

use crate::layout::{Edge, Layout};
use crate::ownership::{self, OwnershipEvent, OwnershipState, entry_position};

/// How close (in screen points) the tracked position must get to a
/// screen's physical boundary before it's treated as "at the edge."
///
/// **Found via real Mac->Windows hardware QA (see ADR-0009's Update
/// note)**: a real OS cursor is clamped by the OS itself to stay
/// within a display's bounds, and the exact maximum/minimum reachable
/// coordinate lands a few points *short* of the nominal
/// `PointerGeometry::screen_size` — on the real hardware that exposed
/// this, a 1470-point-wide display's cursor topped out at x=1468, not
/// 1469 or 1470. An exact `x >= width` (or `x < 0`) predicate is
/// therefore unreachable by any real, absolute-position-tracked
/// capture: `dx` permanently reports `0` once the real cursor is
/// pinned, so the tracked position can never cross a boundary that sits
/// past what the OS itself will ever report. A small margin makes the
/// predicate satisfiable in practice without hard-coding a
/// display-specific magic number for exactly where the real clamp
/// lands (which varies by platform/cursor icon/display).
const EDGE_MARGIN: i32 = 4;

/// Something a [`Router`] decided must actually happen. Turning this
/// into reality (sending on a peer's stream) is `session.rs`'s job.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Forward `message` to `to`'s input stream — `to` is always a
    /// device already present in the connected-peers set passed to
    /// [`Router::set_peer_connected`], which callers must only ever
    /// populate from an already-authenticated `net::Peer` (see
    /// ADR-0009's security section) — a `Router` cannot itself pick an
    /// untrusted/unknown device as a target, only route to one already
    /// vouched for by the caller.
    Send { to: DeviceId, message: InputMessage },
    /// Tell `to` it is now the active input target, and where its
    /// cursor should land.
    Switch {
        to: DeviceId,
        cursor_position: (i32, i32),
    },
}

/// Drives edge-switching from the capturing device's point of view.
/// See the module docs for why only one device in a session ever needs
/// one of these.
pub struct Router {
    our_device_id: DeviceId,
    our_platform: PlatformKind,
    layout: Layout,
    state: OwnershipState,
    connected: HashSet<DeviceId>,
    screen_sizes: HashMap<DeviceId, (u32, u32)>,
    held_keys: HashSet<Key>,
    /// Our best estimate of the cursor's position on whichever screen
    /// is currently active — the real OS position while `Local`
    /// (kept in sync by the caller re-seeding it, e.g. after a
    /// disconnect forces a return to `Local`), the accumulated virtual
    /// position while `Forwarding`.
    position: (i32, i32),
}

impl Router {
    pub fn new(
        our_device_id: DeviceId,
        our_platform: PlatformKind,
        layout: Layout,
        our_screen_size: (u32, u32),
        initial_position: (i32, i32),
    ) -> Self {
        let mut screen_sizes = HashMap::new();
        screen_sizes.insert(our_device_id, our_screen_size);
        Self {
            our_device_id,
            our_platform,
            layout,
            state: OwnershipState::Local,
            connected: HashSet::new(),
            screen_sizes,
            held_keys: HashSet::new(),
            position: initial_position,
        }
    }

    pub fn state(&self) -> OwnershipState {
        self.state
    }

    /// Re-anchors our tracked position to a real OS reading — callers
    /// should do this after regaining `Local` ownership (a normal
    /// switch back, or a forced-local after a disconnect), since this
    /// module has no way to know the real cursor position on its own.
    pub fn resync_position(&mut self, x: i32, y: i32) {
        self.position = (x, y);
    }

    /// Registers `device_id` as a currently-connected, already-
    /// authenticated switch target with the given screen size. Callers
    /// must only ever call this with a device that came from a
    /// successful `net::connect`/`accept` — see ADR-0009.
    pub fn set_peer_connected(&mut self, device_id: DeviceId, screen_size: (u32, u32)) {
        self.connected.insert(device_id);
        self.screen_sizes.insert(device_id, screen_size);
    }

    /// Removes a peer (disconnect or revocation). If it was the
    /// current forwarding target, ownership snaps back to `Local`
    /// immediately and any modifiers we thought were held there are
    /// dropped from our own bookkeeping — there's no connection left
    /// to flush a `Released` onto, which is a known, documented
    /// limitation (see ADR-0009): only a *deliberate* switch away from
    /// a target is guaranteed to leave it modifier-clean.
    pub fn set_peer_disconnected(&mut self, device_id: DeviceId) {
        self.connected.remove(&device_id);
        self.screen_sizes.remove(&device_id);
        if matches!(self.state, OwnershipState::Forwarding { target, .. } if target == device_id) {
            self.state = OwnershipState::Local;
            self.held_keys.clear();
        }
    }

    fn active_screen_owner(&self) -> DeviceId {
        match self.state {
            OwnershipState::Local => self.our_device_id,
            OwnershipState::Forwarding { target, .. } => target,
        }
    }

    fn crossed_edge(&self) -> Option<Edge> {
        let &(width, height) = self.screen_sizes.get(&self.active_screen_owner())?;
        let (x, y) = self.position;
        if x <= EDGE_MARGIN {
            Some(Edge::Left)
        } else if x >= width as i32 - EDGE_MARGIN {
            Some(Edge::Right)
        } else if y <= EDGE_MARGIN {
            Some(Edge::Top)
        } else if y >= height as i32 - EDGE_MARGIN {
            Some(Edge::Bottom)
        } else {
            None
        }
    }

    fn clamp_position_into_active_screen(&mut self) {
        if let Some(&(width, height)) = self.screen_sizes.get(&self.active_screen_owner()) {
            self.position.0 = self.position.0.clamp(0, width.saturating_sub(1) as i32);
            self.position.1 = self.position.1.clamp(0, height.saturating_sub(1) as i32);
        }
    }

    /// Processes one locally captured [`InputMessage`], returning the
    /// effects it implies, in the order they must be applied.
    pub fn handle_captured(&mut self, event: InputMessage) -> Vec<Effect> {
        match event {
            InputMessage::MouseMove { dx, dy } => self.handle_mouse_move(dx, dy),
            InputMessage::Key { key, state, .. } => {
                match state {
                    ButtonState::Pressed => self.held_keys.insert(key),
                    ButtonState::Released => self.held_keys.remove(&key),
                };
                self.route(event).into_iter().collect()
            }
            other => self.route(other).into_iter().collect(),
        }
    }

    /// While `Local`, nothing needs to happen — the OS already applied
    /// this event natively (see the module-level doc on why). While
    /// `Forwarding`, it needs to reach the target.
    fn route(&self, message: InputMessage) -> Option<Effect> {
        match self.state {
            OwnershipState::Local => None,
            OwnershipState::Forwarding { target, .. } => Some(Effect::Send {
                to: target,
                message,
            }),
        }
    }

    fn handle_mouse_move(&mut self, dx: i32, dy: i32) -> Vec<Effect> {
        let old_state = self.state;
        self.position = (self.position.0 + dx, self.position.1 + dy);

        let active_screen_owner = self.active_screen_owner();
        let active_screen_size = self.screen_sizes.get(&active_screen_owner).copied();
        tracing::trace!(
            dx,
            dy,
            position = ?self.position,
            ?active_screen_owner,
            ?active_screen_size,
            "mouse move accumulated"
        );

        let Some(edge) = self.crossed_edge() else {
            return self
                .route(InputMessage::MouseMove { dx, dy })
                .into_iter()
                .collect();
        };
        tracing::debug!(?edge, position = ?self.position, "edge predicate true");

        let coordinate = match edge {
            Edge::Left | Edge::Right => self.position.1,
            Edge::Top | Edge::Bottom => self.position.0,
        };

        let new_state = {
            let ctx = ownership::SwitchContext {
                our_device_id: self.our_device_id,
                layout: &self.layout,
                connected: &self.connected,
                screen_sizes: &self.screen_sizes,
            };
            ownership::transition(
                old_state,
                OwnershipEvent::EdgeCrossed { edge, coordinate },
                &ctx,
            )
        };

        if new_state == old_state {
            // No configured/connected neighbor across this edge: stay
            // put, and clamp so a user holding the mouse against a
            // dead edge doesn't have to push disproportionately far
            // back the other way to re-enter (see the kickoff's
            // "rapid repeated edge crossing" / boundary-coordinate
            // items).
            tracing::debug!(
                ?edge,
                ?active_screen_owner,
                "edge crossed but no configured/connected neighbor -- staying put and clamping"
            );
            self.clamp_position_into_active_screen();
            return self
                .route(InputMessage::MouseMove { dx, dy })
                .into_iter()
                .collect();
        }
        tracing::info!(
            ?edge,
            ?old_state,
            ?new_state,
            "ownership transition triggered"
        );

        // A real switch. The crossing delta itself is consumed here,
        // not forwarded or injected: the new destination's cursor is
        // placed explicitly via `Effect::Switch`'s `cursor_position`,
        // not by relaying this one boundary-triggering delta.
        self.state = new_state;
        let mut effects = self.flush_held_keys(old_state);
        match new_state {
            OwnershipState::Forwarding {
                target,
                virtual_cursor,
            } => {
                self.position = virtual_cursor;
                effects.push(Effect::Switch {
                    to: target,
                    cursor_position: virtual_cursor,
                });
            }
            OwnershipState::Local => {
                // Returned home. We have no ground truth for where the
                // real local cursor now sits — the caller resyncs via
                // `resync_position` from a live OS read; until then,
                // best-effort mirror the same entry-position math using
                // our own screen size as the destination.
                if let Some(&source_size) = old_state_screen_size(old_state, &self.screen_sizes)
                    && let Some(&dest_size) = self.screen_sizes.get(&self.our_device_id)
                {
                    self.position = entry_position(edge, source_size, dest_size, coordinate);
                }
            }
        }
        effects
    }

    /// On leaving a `Forwarding` state (to a new target or back to
    /// `Local`), synthesize a `Released` for every key we believe is
    /// still held on the old target, so a switch can never leave a
    /// modifier permanently stuck there by relying on key-up ordering
    /// — see ADR-0009 and the kickoff's modifier-safety requirement.
    /// Leaving `Local` needs no such flush: the local OS's own
    /// modifier state is authoritative and untouched by any of this.
    fn flush_held_keys(&mut self, old_state: OwnershipState) -> Vec<Effect> {
        let OwnershipState::Forwarding {
            target: old_target, ..
        } = old_state
        else {
            self.held_keys.clear();
            return Vec::new();
        };
        let effects = self
            .held_keys
            .iter()
            .map(|&key| Effect::Send {
                to: old_target,
                message: InputMessage::Key {
                    key,
                    state: ButtonState::Released,
                    repeat: false,
                    source_os: self.our_platform,
                },
            })
            .collect();
        self.held_keys.clear();
        effects
    }
}

fn old_state_screen_size(
    old_state: OwnershipState,
    screen_sizes: &HashMap<DeviceId, (u32, u32)>,
) -> Option<&(u32, u32)> {
    match old_state {
        OwnershipState::Forwarding { target, .. } => screen_sizes.get(&target),
        OwnershipState::Local => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::layout::LayoutDevice;

    const US: DeviceId = [1u8; 32];
    const NEIGHBOR: DeviceId = [2u8; 32];
    const THIRD: DeviceId = [3u8; 32];
    const UNTRUSTED: DeviceId = [9u8; 32];

    fn layout_with(edges: &[(DeviceId, Edge, DeviceId)]) -> Layout {
        let mut layout = Layout::new();
        for id in [US, NEIGHBOR, THIRD] {
            layout.add_device(LayoutDevice {
                device_id: id,
                label: "device".to_string(),
                enabled: true,
            });
        }
        for &(from, edge, to) in edges {
            layout.set_neighbor(from, edge, to).unwrap();
        }
        layout
    }

    fn router_with_right_neighbor() -> Router {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));
        router
    }

    fn key_event(key: Key, state: ButtonState) -> InputMessage {
        InputMessage::Key {
            key,
            state,
            repeat: false,
            source_os: PlatformKind::MacOs,
        }
    }

    #[test]
    fn local_routing_produces_no_effect_at_all() {
        // The OS already applied this event natively (listen-only
        // capture) -- re-injecting or forwarding it while Local would
        // be duplicate input.
        let mut router = router_with_right_neighbor();
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 5, dy: 5 });
        assert_eq!(effects, vec![]);
        let effects = router.handle_captured(key_event(Key::A, ButtonState::Pressed));
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn crossing_right_edge_switches_and_does_not_forward_the_triggering_delta() {
        let mut router = router_with_right_neighbor();
        // Push from x=500 past x=1000 (screen width) -> crosses Right.
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert_eq!(
            effects,
            vec![Effect::Switch {
                to: NEIGHBOR,
                cursor_position: (0, 400),
            }]
        );
        assert_eq!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (0, 400),
            }
        );
    }

    #[test]
    fn once_forwarding_subsequent_moves_are_sent_to_the_target() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 10, dy: -3 });
        assert_eq!(
            effects,
            vec![Effect::Send {
                to: NEIGHBOR,
                message: InputMessage::MouseMove { dx: 10, dy: -3 },
            }]
        );
    }

    #[test]
    fn keyboard_follows_the_active_target_after_a_switch() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        let effects = router.handle_captured(key_event(Key::A, ButtonState::Pressed));
        assert_eq!(
            effects,
            vec![Effect::Send {
                to: NEIGHBOR,
                message: key_event(Key::A, ButtonState::Pressed),
            }]
        );
    }

    #[test]
    fn no_duplicate_input_when_forwarding_only_the_target_receives_it() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        let effects = router.handle_captured(key_event(Key::A, ButtonState::Pressed));
        // Exactly one effect, one destination -- never both a local
        // action and a forward for the same captured event.
        assert_eq!(effects.len(), 1);
    }

    #[test]
    fn disconnected_target_does_not_switch_and_keeps_local_input_working() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        // NEIGHBOR is configured but never connected.
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert_eq!(effects, vec![]);
        assert_eq!(router.state(), OwnershipState::Local);
    }

    #[test]
    fn unknown_device_id_in_layout_is_simply_not_a_neighbor() {
        // A layout that only names US has no neighbor for any edge --
        // matches `layout.rs`'s own "unknown source has no neighbors"
        // guarantee; this is the routing-level regression for it.
        let mut layout = Layout::new();
        layout.add_device(LayoutDevice {
            device_id: US,
            label: "us".to_string(),
            enabled: true,
        });
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert_eq!(effects, vec![]);
        assert_eq!(router.state(), OwnershipState::Local);
    }

    #[test]
    fn an_untrusted_or_never_connected_device_can_never_become_a_target() {
        // UNTRUSTED is not in the layout's neighbor map *and* was never
        // passed to `set_peer_connected` -- both of the independent
        // gates a real device would have to pass are absent, so no
        // sequence of local input can make it a switch target. This is
        // the router-level regression for the kickoff's "an untrusted
        // device must never receive input" requirement -- `Router`
        // structurally cannot name a device that didn't come through
        // `set_peer_connected`, which callers only ever populate from
        // an authenticated `net::Peer` (see `session.rs`/ADR-0009).
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));

        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        for effect in &effects {
            let target = match effect {
                Effect::Send { to, .. } | Effect::Switch { to, .. } => *to,
            };
            assert_ne!(target, UNTRUSTED);
        }
        assert_ne!(
            router.state(),
            OwnershipState::Forwarding {
                target: UNTRUSTED,
                virtual_cursor: (0, 0),
            }
        );
    }

    #[test]
    fn modifier_release_is_flushed_to_the_old_target_on_switch_away() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Right, THIRD)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));
        router.set_peer_connected(THIRD, (1000, 800));

        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 }); // -> Forwarding(NEIGHBOR)
        router.handle_captured(key_event(Key::ShiftLeft, ButtonState::Pressed));

        // Cross onward from NEIGHBOR's screen to THIRD.
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 1100, dy: 0 });
        assert!(
            effects.contains(&Effect::Send {
                to: NEIGHBOR,
                message: key_event(Key::ShiftLeft, ButtonState::Released),
            }),
            "expected a synthesized Released for the still-held key on the device we just left, got {effects:?}"
        );
        assert!(matches!(
            router.state(),
            OwnershipState::Forwarding { target: THIRD, .. }
        ));
    }

    #[test]
    fn modifier_release_is_flushed_when_returning_to_local() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Left, US)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));

        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 }); // -> Forwarding(NEIGHBOR)
        router.handle_captured(key_event(Key::ControlLeft, ButtonState::Pressed));

        let effects = router.handle_captured(InputMessage::MouseMove { dx: -1100, dy: 0 });
        assert!(effects.contains(&Effect::Send {
            to: NEIGHBOR,
            message: key_event(Key::ControlLeft, ButtonState::Released),
        }));
        assert_eq!(router.state(), OwnershipState::Local);
    }

    #[test]
    fn disconnecting_the_current_target_forces_local_with_no_effects_to_send() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        router.handle_captured(key_event(Key::ShiftLeft, ButtonState::Pressed));
        assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));

        router.set_peer_disconnected(NEIGHBOR);
        assert_eq!(router.state(), OwnershipState::Local);

        // Local input keeps working (produces no effect, but doesn't
        // panic or otherwise misbehave) immediately, no restart needed.
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 1, dy: 1 });
        assert_eq!(effects, vec![]);
    }

    #[test]
    fn ownership_recovers_after_reconnecting_the_same_device() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));

        router.set_peer_disconnected(NEIGHBOR);
        assert_eq!(router.state(), OwnershipState::Local);
        router.resync_position(500, 400);

        // Reconnect -- no restart, an ordinary edge crossing switches
        // again immediately.
        router.set_peer_connected(NEIGHBOR, (1000, 800));
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert_eq!(
            effects,
            vec![Effect::Switch {
                to: NEIGHBOR,
                cursor_position: (0, 400),
            }]
        );
    }

    #[test]
    fn rapid_repeated_edge_crossings_never_leave_no_effects_unaccounted_for() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Left, US)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));

        for _ in 0..25 {
            router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
            assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));
            router.handle_captured(InputMessage::MouseMove { dx: -1100, dy: 0 });
            assert_eq!(router.state(), OwnershipState::Local);
        }
    }

    #[test]
    fn a_coordinate_well_inside_the_screen_does_not_switch() {
        let mut router = router_with_right_neighbor();
        // Move to x=900 on a 1000-wide screen -- comfortably inside
        // the margin zone, must not switch.
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 400, dy: 0 });
        assert_eq!(effects, vec![]);
        assert_eq!(router.state(), OwnershipState::Local);
    }

    /// Regression test for a real Mac->Windows hardware QA failure (see
    /// ADR-0009's Update note): a real OS cursor is clamped by the OS
    /// itself and can land a few points short of the nominal screen
    /// width -- on the hardware that exposed this, a 1470-wide display's
    /// cursor topped out at x=1468, two points short of the naive
    /// `x >= width` threshold, so the predicate was never satisfied no
    /// matter how hard the edge was pushed against. This asserts a
    /// coordinate within `EDGE_MARGIN` of the boundary (but not
    /// touching it exactly) still triggers.
    #[test]
    fn a_coordinate_within_the_margin_of_the_true_edge_switches_even_short_of_the_exact_boundary() {
        let mut router = router_with_right_neighbor();
        // Move to x=998 on a 1000-wide screen (EDGE_MARGIN=4, so the
        // trigger zone is x >= 996) -- short of the exact boundary
        // (999/1000) a real clamped cursor might never reach, but must
        // still switch.
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 498, dy: 0 });
        assert!(
            !effects.is_empty(),
            "a coordinate within the edge margin must trigger a switch, \
             matching how a real OS-clamped cursor never quite reaches the exact boundary"
        );
        assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));
    }

    #[test]
    fn a_coordinate_within_the_margin_of_the_left_edge_switches_too() {
        // The same margin applies symmetrically to the left/top edges,
        // which have the identical real-hardware problem: a cursor
        // clamped at the OS's own minimum never quite reaches x < 0.
        let layout = layout_with(&[(US, Edge::Left, NEIGHBOR)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));

        // Move to x=2 (within EDGE_MARGIN=4 of the left edge, not
        // exactly at x=0).
        let effects = router.handle_captured(InputMessage::MouseMove { dx: -498, dy: 0 });
        assert!(!effects.is_empty());
        assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));
    }

    #[test]
    fn different_screen_dimensions_rescale_the_handoff_position() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (2000, 2000)); // a bigger, differently-shaped screen

        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        // Halfway down an 800-tall source screen -> halfway down a
        // 2000-tall destination screen.
        assert_eq!(
            effects,
            vec![Effect::Switch {
                to: NEIGHBOR,
                cursor_position: (0, 1000),
            }]
        );
    }
}
