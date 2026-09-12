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
    /// Warp *our own* local cursor to `(x, y)` — the caller applies
    /// this via its local `PointerGeometry`, not a network send.
    ///
    /// Emitted the moment ownership leaves `Local` for the first time
    /// (not on later onward hops between two remote targets). Found
    /// via real Mac->Windows hardware QA (see ADR-0009's Update note):
    /// every backend's `Capture` is listen-only, so the local OS cursor
    /// keeps moving with real captured deltas even while `Forwarding`
    /// — and since the edge crossing that triggered the switch, by
    /// definition, just pinned that cursor against its own screen
    /// boundary, it has nowhere left to go. Further pushes in that
    /// direction report `dx`/`dy` of `0` (the OS won't move a cursor
    /// past its own edge), starving the virtual position tracking of
    /// real motion and, worse, letting ordinary hand jitter near the
    /// pinned edge flicker back and forth across `EDGE_MARGIN` and
    /// bounce ownership home unpredictably. Recentering the local
    /// cursor away from every boundary the instant it stops being the
    /// input source removes the pin entirely for the rest of the
    /// `Forwarding` session.
    RecenterLocal { x: i32, y: i32 },
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

    /// The current ownership state, with `Forwarding`'s `virtual_cursor`
    /// always reflecting the live `self.position` — not whatever value
    /// was last written into `self.state`'s own copy.
    ///
    /// **Real-hardware regression, fixed here**: `self.state` is
    /// mutated only at specific moments (an actual switch, or the
    /// dead-edge clamp branch) — an ordinary in-bounds move while
    /// already `Forwarding` (the overwhelmingly common case: most
    /// captured mouse motion neither crosses an edge nor gets clamped
    /// at one) updates `self.position` and returns without ever
    /// touching `self.state` at all. Returning `self.state` directly
    /// from this accessor therefore reported a virtual_cursor frozen
    /// at whatever the *last* transition or clamp happened to leave
    /// it — not the actual current position — for every ordinary move
    /// in between, silently violating this module's own documented
    /// invariant that `state()` and `self.position` never disagree.
    /// Deriving `virtual_cursor` from `self.position` here, rather
    /// than chasing down every internal mutation site that needs to
    /// keep a second copy in sync, makes this correct by construction:
    /// `self.state`'s own internal field is untouched (still exactly
    /// what `ownership::transition`'s equality checks need), only this
    /// public accessor's *reporting* of it changes.
    pub fn state(&self) -> OwnershipState {
        match self.state {
            OwnershipState::Local => OwnershipState::Local,
            OwnershipState::Forwarding { target, .. } => OwnershipState::Forwarding {
                target,
                virtual_cursor: self.position,
            },
        }
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
                if state == ButtonState::Pressed && self.is_emergency_return_chord(key) {
                    return self.emergency_return_to_local();
                }
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
        // DIAGNOSTIC (Phase 4 pointer-range root-cause hunt): stage 2 of
        // the "follow one physical movement through the whole pipeline"
        // trace -- the normalized delta as it arrives here, and the
        // virtual position it accumulates into. Compare `dx`/`dy` against
        // stage 1 (`macos::events`) to see whether anything between
        // capture and routing changed them, and `position` against the
        // Windows side's `before`/`intended`/`actual` to see whether the
        // target applied what it was told. `debug` rather than `trace` so
        // one RUST_LOG level shows every stage of the chain at once.
        tracing::debug!(
            stage = "2-router",
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
            // Real-hardware regression (Mac<->Windows pointer-range QA):
            // `clamp_position_into_active_screen` only ever updated
            // `self.position` -- `self.state`'s own embedded
            // `virtual_cursor` (whichever value was there from the last
            // actual switch) was left stale, silently violating this
            // module's own documented invariant that `state()` and
            // `self.position` never disagree (see `nudge_off_boundary`'s
            // call site). Nothing in this file's *behavior* happened to
            // read the stale copy, but `state()` is a public, trusted
            // accessor other code (and this exact class of test) relies
            // on to reflect where the pointer actually is -- keep it in
            // sync here too, not just at switch time.
            if let OwnershipState::Forwarding { target, .. } = self.state {
                self.state = OwnershipState::Forwarding {
                    target,
                    virtual_cursor: self.position,
                };
            }
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
        let mut effects = self.flush_held_keys(old_state);
        match new_state {
            OwnershipState::Forwarding {
                target,
                virtual_cursor,
            } => {
                // `entry_position` places the handoff exactly on the
                // destination's boundary (x=0 for a Right-edge entry,
                // etc.) -- which, found via real Mac->Windows hardware
                // QA (see ADR-0009's Update note), sits *inside* our
                // own EDGE_MARGIN trigger zone for the opposite edge.
                // Landing there without adjustment immediately
                // satisfies "crossed the edge back home" on literally
                // the next captured event, bouncing straight back
                // before the user can do anything. Nudge inward past
                // the margin so the landing spot isn't self-triggering.
                // `self.state` is set to the *nudged* value here (not
                // the blanket `new_state` from `transition`), so
                // `state()`/`self.position` never disagree about where
                // the handoff actually landed.
                let landing = self
                    .screen_sizes
                    .get(&target)
                    .map(|&size| nudge_off_boundary(edge, virtual_cursor, size))
                    .unwrap_or(virtual_cursor);
                self.state = OwnershipState::Forwarding {
                    target,
                    virtual_cursor: landing,
                };
                self.position = landing;
                effects.push(Effect::Switch {
                    to: target,
                    cursor_position: landing,
                });
                if old_state == OwnershipState::Local
                    && let Some(&our_size) = self.screen_sizes.get(&self.our_device_id)
                {
                    let (cx, cy) = (our_size.0 as i32 / 2, our_size.1 as i32 / 2);
                    effects.push(Effect::RecenterLocal { x: cx, y: cy });
                }
            }
            OwnershipState::Local => {
                self.state = OwnershipState::Local;
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

    /// Whether pressing `key` completes the emergency return-to-local
    /// chord: **Control + Option/Alt + Command/Meta + Escape**, either side
    /// of each modifier, while `Forwarding`. See ADR-0009 decision 19.
    ///
    /// Decided entirely from this device's own captured input, never from
    /// anything the target reports — which is the whole point: real
    /// hardware found a target that was connected but had stopped
    /// processing input (its console was paused), leaving this device's
    /// local input suppressed with no way back short of killing the
    /// target process. This chord works however the target has failed.
    /// The modifiers were already forwarded as ordinary presses (they're
    /// released on the old target by [`Self::emergency_return_to_local`]);
    /// the `Escape` itself is never forwarded.
    fn is_emergency_return_chord(&self, key: Key) -> bool {
        const CHORD_MODIFIERS: [[Key; 2]; 3] = [
            [Key::ControlLeft, Key::ControlRight],
            [Key::AltLeft, Key::AltRight],
            [Key::MetaLeft, Key::MetaRight],
        ];
        key == Key::Escape
            && matches!(self.state, OwnershipState::Forwarding { .. })
            && CHORD_MODIFIERS
                .iter()
                .all(|either_side| either_side.iter().any(|m| self.held_keys.contains(m)))
    }

    /// Returns ownership to `Local` right now, from any `Forwarding`
    /// state, releasing every held key on the target being left so none
    /// stays stuck there. The caller (`Session::handle_captured`) lifts
    /// local suppression on seeing the `Forwarding` -> `Local` change,
    /// exactly as for an ordinary return across an edge.
    fn emergency_return_to_local(&mut self) -> Vec<Effect> {
        let old_state = self.state;
        tracing::warn!(
            ?old_state,
            "emergency return-to-local chord pressed -- giving local input back now"
        );
        let effects = self.flush_held_keys(old_state);
        let ctx = ownership::SwitchContext {
            our_device_id: self.our_device_id,
            layout: &self.layout,
            connected: &self.connected,
            screen_sizes: &self.screen_sizes,
        };
        self.state = ownership::transition(old_state, OwnershipEvent::ReturnToLocal, &ctx);
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

/// Moves a boundary-exact handoff position (from [`entry_position`])
/// safely past [`EDGE_MARGIN`] on the entered screen, so the landing
/// spot itself doesn't immediately re-satisfy the opposite edge's
/// trigger zone. `edge` is the edge that was crossed on the *source*
/// screen (same meaning as [`entry_position`]'s `edge` parameter) --
/// crossing a source's `Right` edge lands on the destination's `Left`
/// edge, so that's the axis nudged inward (positive x); symmetrically
/// for the other three.
fn nudge_off_boundary(edge: Edge, position: (i32, i32), screen_size: (u32, u32)) -> (i32, i32) {
    let inset = EDGE_MARGIN + 1;
    let (x, y) = position;
    match edge {
        Edge::Right => (inset, y),
        Edge::Left => (screen_size.0 as i32 - 1 - inset, y),
        Edge::Bottom => (x, inset),
        Edge::Top => (x, screen_size.1 as i32 - 1 - inset),
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

    fn press(router: &mut Router, key: Key) -> Vec<Effect> {
        router.handle_captured(key_event(key, ButtonState::Pressed))
    }

    /// Regression test for the real-hardware stalled-target incident
    /// (ADR-0009 decision 19): the target was connected but no longer
    /// processing input, and nothing on this side could get local input
    /// back. The chord must return to `Local` regardless of the target.
    #[test]
    fn the_emergency_chord_returns_to_local_while_forwarding() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));

        press(&mut router, Key::ControlLeft);
        press(&mut router, Key::AltLeft);
        press(&mut router, Key::MetaLeft);
        let effects = press(&mut router, Key::Escape);

        assert_eq!(router.state(), OwnershipState::Local);
        // Every held modifier is released on the target being left, so
        // none stays stuck there.
        for modifier in [Key::ControlLeft, Key::AltLeft, Key::MetaLeft] {
            assert!(
                effects.contains(&Effect::Send {
                    to: NEIGHBOR,
                    message: key_event(modifier, ButtonState::Released),
                }),
                "{modifier:?} must be released on the target, got {effects:?}"
            );
        }
        // The Escape that completed the chord is never forwarded.
        assert!(
            !effects.iter().any(|e| matches!(
                e,
                Effect::Send {
                    message: InputMessage::Key {
                        key: Key::Escape,
                        ..
                    },
                    ..
                }
            )),
            "the chord's Escape must not reach the target, got {effects:?}"
        );
    }

    #[test]
    fn the_emergency_chord_accepts_either_side_of_each_modifier() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        press(&mut router, Key::ControlRight);
        press(&mut router, Key::AltLeft);
        press(&mut router, Key::MetaRight);
        press(&mut router, Key::Escape);
        assert_eq!(router.state(), OwnershipState::Local);
    }

    #[test]
    fn escape_alone_or_an_incomplete_chord_is_forwarded_normally() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });

        // Plain Escape: an ordinary key for the target.
        assert_eq!(
            press(&mut router, Key::Escape),
            vec![Effect::Send {
                to: NEIGHBOR,
                message: key_event(Key::Escape, ButtonState::Pressed),
            }]
        );
        router.handle_captured(key_event(Key::Escape, ButtonState::Released));

        // Control + Option + Escape, missing Command: still ordinary.
        press(&mut router, Key::ControlLeft);
        press(&mut router, Key::AltLeft);
        let effects = press(&mut router, Key::Escape);
        assert!(matches!(router.state(), OwnershipState::Forwarding { .. }));
        assert_eq!(
            effects,
            vec![Effect::Send {
                to: NEIGHBOR,
                message: key_event(Key::Escape, ButtonState::Pressed),
            }]
        );
    }

    #[test]
    fn the_emergency_chord_does_nothing_special_while_already_local() {
        let mut router = router_with_right_neighbor();
        press(&mut router, Key::ControlLeft);
        press(&mut router, Key::AltLeft);
        press(&mut router, Key::MetaLeft);
        assert_eq!(press(&mut router, Key::Escape), vec![]);
        assert_eq!(router.state(), OwnershipState::Local);
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
        // The landing lands nudged in from the exact boundary (x=0) by
        // EDGE_MARGIN+1 -- see `nudge_off_boundary`'s doc comment for
        // why landing exactly on the boundary is itself a real-hardware
        // bug (ADR-0009's Update note).
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        // Switching away from Local for the first time also recenters
        // our own local cursor (see `Effect::RecenterLocal`'s doc
        // comment / ADR-0009's Update note) -- (1000,800)/2 = (500,400).
        assert_eq!(
            effects,
            vec![
                Effect::Switch {
                    to: NEIGHBOR,
                    cursor_position: (5, 400),
                },
                Effect::RecenterLocal { x: 500, y: 400 },
            ]
        );
        assert_eq!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (5, 400),
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
            if let Effect::Send { to, .. } | Effect::Switch { to, .. } = effect {
                assert_ne!(*to, UNTRUSTED);
            }
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
            vec![
                Effect::Switch {
                    to: NEIGHBOR,
                    cursor_position: (5, 400),
                },
                Effect::RecenterLocal { x: 500, y: 400 },
            ]
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
        // 2000-tall destination screen (x nudged in from the exact
        // boundary by EDGE_MARGIN+1 -- see `nudge_off_boundary`).
        assert_eq!(
            effects,
            vec![
                Effect::Switch {
                    to: NEIGHBOR,
                    cursor_position: (5, 1000),
                },
                Effect::RecenterLocal { x: 500, y: 400 },
            ]
        );
    }

    #[test]
    fn landing_on_a_new_target_never_lands_inside_its_own_edge_margin() {
        // Regression test for a real Mac->Windows hardware QA failure
        // (see ADR-0009's Update note): entry_position() lands exactly
        // on the destination's boundary, which -- once EDGE_MARGIN
        // exists -- is itself inside the trigger zone for the opposite
        // edge, causing an immediate, unrequested bounce back to the
        // previous owner on the very next captured event, milliseconds
        // after the switch (confirmed live via tracing on real
        // hardware: Local -> Forwarding -> Local within ~10ms with no
        // user input in between). Asserts the landing position is
        // never within EDGE_MARGIN of any boundary, for all four edges.
        for (edge, dx, dy) in [
            (Edge::Right, 600, 0),
            (Edge::Left, -600, 0),
            (Edge::Bottom, 0, 600),
            (Edge::Top, 0, -600),
        ] {
            let layout = layout_with(&[(US, edge, NEIGHBOR)]);
            let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
            router.set_peer_connected(NEIGHBOR, (1000, 800));

            let effects = router.handle_captured(InputMessage::MouseMove { dx, dy });
            let Some(Effect::Switch {
                cursor_position, ..
            }) = effects.first()
            else {
                panic!("expected a Switch effect for {edge:?}, got {effects:?}");
            };
            assert!(
                cursor_position.0 > EDGE_MARGIN && cursor_position.0 < 1000 - EDGE_MARGIN,
                "{edge:?}: landing x={} is within the edge margin (0..={EDGE_MARGIN} or >={}) -- \
                 would immediately re-trigger a bounce back",
                cursor_position.0,
                1000 - EDGE_MARGIN,
            );
            assert!(
                cursor_position.1 > EDGE_MARGIN && cursor_position.1 < 800 - EDGE_MARGIN,
                "{edge:?}: landing y={} is within the edge margin",
                cursor_position.1,
            );
        }
    }

    /// Regression test for a second real bug found while fixing the
    /// first: `state()` was reporting the *unnudged*, exact-boundary
    /// `virtual_cursor` (from `ownership::transition`'s return value)
    /// even after `self.position` and the `Effect::Switch` sent to the
    /// target were correctly nudged -- an internal inconsistency where
    /// the router's own reported state disagreed with the position it
    /// was actually tracking and the position it told the target to
    /// warp to. Caught immediately by the test above failing with
    /// `state()` still showing `(0, 400)` while the effect correctly
    /// showed `(5, 400)`.
    #[test]
    fn reported_state_agrees_with_the_position_actually_sent_to_the_target() {
        let mut router = router_with_right_neighbor();
        let effects = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        let Some(Effect::Switch {
            cursor_position, ..
        }) = effects.first()
        else {
            panic!("expected a Switch effect, got {effects:?}");
        };
        let OwnershipState::Forwarding {
            virtual_cursor: reported,
            ..
        } = router.state()
        else {
            panic!("expected Forwarding, got {:?}", router.state());
        };
        assert_eq!(
            *cursor_position, reported,
            "the effect sent to the target and router.state()'s virtual_cursor must never disagree"
        );
    }

    /// Regression test for a real Mac->Windows hardware QA finding (see
    /// ADR-0009's Update note): the local cursor gets pinned against
    /// its own boundary by the very edge crossing that triggers a
    /// switch, starving further movement and causing hand-jitter-driven
    /// flutter across `EDGE_MARGIN`. `RecenterLocal` fixes that -- but
    /// only needs to fire once, the moment ownership actually leaves
    /// `Local`, not on every subsequent onward hop between two already-
    /// remote targets (our own local cursor isn't involved in those at
    /// all, so recentering it again would be pointless).
    #[test]
    fn recenter_local_fires_only_on_the_first_switch_away_from_local() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Right, THIRD)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1000, 800));
        router.set_peer_connected(THIRD, (1000, 800));

        // Local -> Forwarding(NEIGHBOR): first time leaving Local.
        let first = router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert!(
            first
                .iter()
                .any(|e| matches!(e, Effect::RecenterLocal { .. })),
            "expected a RecenterLocal effect on the first switch away from Local, got {first:?}"
        );

        // Forwarding(NEIGHBOR) -> Forwarding(THIRD): an onward hop, our
        // own local cursor was never involved.
        let onward = router.handle_captured(InputMessage::MouseMove { dx: 1100, dy: 0 });
        assert!(
            !onward
                .iter()
                .any(|e| matches!(e, Effect::RecenterLocal { .. })),
            "an onward hop between two remote targets must not recenter \
             our own local cursor again, got {onward:?}"
        );
    }

    /// Regression test for the real Mac<->Windows "can't reliably reach
    /// the edges of the target screen" hardware report: on an edge with
    /// no configured further neighbor (a dead end from the target's own
    /// perspective -- e.g. `NEIGHBOR`'s Top/Right/Bottom in a two-device
    /// layout, since only its Left points back to `US`), `Router`'s own
    /// tracked position must clamp to the *exact* true boundary
    /// (`width-1`/`height-1`/`0`), not stop short of it. The message
    /// actually forwarded to the target carries the real captured delta
    /// unmodified (the target's own `SetCursorPos` independently clamps
    /// to its real screen bounds -- see `WindowsInject`), but `Router`'s
    /// own state must agree with that true edge exactly, since it is
    /// what later decides whether a *further* push crosses into a
    /// neighbor on that edge.
    #[test]
    fn staying_put_at_a_dead_edge_clamps_the_tracked_position_to_the_exact_true_boundary() {
        let mut router = router_with_right_neighbor();
        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert!(matches!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                ..
            }
        ));

        // Push far past NEIGHBOR's own Right edge (1000-wide screen, no
        // neighbor configured there) -- must clamp to exactly 999, not
        // some value short of the true boundary.
        router.handle_captured(InputMessage::MouseMove { dx: 5000, dy: 0 });
        assert_eq!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (999, 400),
            },
            "must clamp to the exact true right edge (width-1), not short of it"
        );

        // Push far past the Top edge (no neighbor configured) -- must
        // clamp to exactly 0.
        router.handle_captured(InputMessage::MouseMove { dx: 0, dy: -5000 });
        assert_eq!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (999, 0),
            },
            "must clamp to the exact true top edge (0), not short of it"
        );

        // Push far past the Bottom edge (no neighbor configured) --
        // must clamp to exactly 799 (height-1).
        router.handle_captured(InputMessage::MouseMove { dx: 0, dy: 5000 });
        assert_eq!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (999, 799),
            },
            "must clamp to the exact true bottom edge (height-1), not short of it"
        );
    }

    /// Regression test for the Phase 4 pointer-range acceptance
    /// criteria (a real Windows target: 1366x768). The common pointer
    /// model `Router` maintains -- independent of any specific OS's
    /// injection path -- must reach all four exact corners and the
    /// center, not some compressed sub-rectangle. `NEIGHBOR` has no
    /// edges of its own configured, so every one of its edges is a
    /// dead end that clamps in place rather than switching onward,
    /// letting this test explore its full extent freely. Both large
    /// (overshoot-and-clamp) and small (exact single-unit) deltas are
    /// exercised, in every direction.
    #[test]
    fn full_windows_shaped_screen_is_reachable_at_all_four_corners_and_center() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let mut router = Router::new(US, PlatformKind::MacOs, layout, (1000, 800), (500, 400));
        router.set_peer_connected(NEIGHBOR, (1366, 768));

        router.handle_captured(InputMessage::MouseMove { dx: 600, dy: 0 });
        assert!(matches!(
            router.state(),
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                ..
            }
        ));

        let assert_at = |router: &Router, expected: (i32, i32), label: &str| {
            assert_eq!(
                router.state(),
                OwnershipState::Forwarding {
                    target: NEIGHBOR,
                    virtual_cursor: expected,
                },
                "{label}: expected to reach {expected:?}"
            );
        };

        // Top-left: a huge overshoot in both axes at once must clamp
        // to exactly (0, 0), not merely "close to" it.
        router.handle_captured(InputMessage::MouseMove {
            dx: -10_000,
            dy: -10_000,
        });
        assert_at(&router, (0, 0), "top-left corner");

        // Top-right: large positive dx only.
        router.handle_captured(InputMessage::MouseMove { dx: 10_000, dy: 0 });
        assert_at(&router, (1365, 0), "top-right corner");

        // Bottom-right: large positive dy only.
        router.handle_captured(InputMessage::MouseMove { dx: 0, dy: 10_000 });
        assert_at(&router, (1365, 767), "bottom-right corner");

        // Bottom-left: large negative dx only.
        router.handle_captured(InputMessage::MouseMove { dx: -10_000, dy: 0 });
        assert_at(&router, (0, 767), "bottom-left corner");

        // A small, exact step off a corner must move by exactly that
        // much -- not stay clamped, and not overshoot.
        router.handle_captured(InputMessage::MouseMove { dx: 1, dy: -1 });
        assert_at(&router, (1, 766), "a small step off the bottom-left corner");

        // The center is freely reachable via an ordinary in-bounds
        // move (no clamping involved at all).
        router.handle_captured(InputMessage::MouseMove { dx: 682, dy: -382 });
        assert_at(&router, (683, 384), "center");
    }
}
