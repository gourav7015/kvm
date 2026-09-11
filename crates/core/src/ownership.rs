//! Pure edge-switching / input-ownership state machine — see ADR-0009.
//! No I/O, no OS coordinates, no networking; driven entirely by events
//! and a small context struct, same shape as [`crate::pairing`]'s
//! `transition`.
//!
//! **Design note, refined from the original plan during implementation**:
//! only the device that's *actually* capturing real local input ever
//! tracks position or decides on edge crossings — a forwarding target
//! never runs its own edge-detection. Instead, the capturing device
//! keeps a `virtual_cursor` estimate of where the pointer would be *on
//! whichever screen is currently active*, computed once at each handoff
//! via [`entry_position`] and from then on updated locally as more
//! motion is captured. This avoids a distributed multi-device
//! coordination problem entirely (no risk of two devices disagreeing
//! about who currently owns input) — a plain, single, capturing-side
//! source of truth, closer to how established KVM tools model this.

use std::collections::{HashMap, HashSet};

use kvm_protocol::DeviceId;

use crate::layout::{Edge, Layout};

/// This device's role in the current input-routing session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipState {
    /// Local input stays local — the ordinary, unswitched state.
    Local,
    /// This device's own captured input is live and being forwarded to
    /// `target`. `virtual_cursor` is where the pointer is estimated to
    /// be on `target`'s screen, in `target`'s own coordinate space.
    Forwarding {
        target: DeviceId,
        virtual_cursor: (i32, i32),
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipEvent {
    /// The pointer (real, if `Local`; virtual, if `Forwarding`) reached
    /// `edge` of whichever screen is currently active, at `coordinate`
    /// along that edge (the y position for `Left`/`Right`, the x
    /// position for `Top`/`Bottom`) in that screen's own coordinate
    /// space.
    EdgeCrossed { edge: Edge, coordinate: i32 },
    /// The device we were forwarding to (or, after further onward
    /// switches, whichever device is now the target) disconnected —
    /// fall back to local input immediately, regardless of how many
    /// hops deep the forwarding chain was.
    TargetDisconnected,
    /// An explicit request to return to local input (e.g. a hotkey
    /// fallback). A no-op if already `Local`.
    ReturnToLocal,
}

/// Read-only context `transition` needs to resolve an `EdgeCrossed`
/// event — deliberately just data, no trait objects or live queries,
/// so the whole state machine stays pure and independent of how the
/// caller actually knows which peers are connected or how big their
/// screens are.
pub struct SwitchContext<'a> {
    pub our_device_id: DeviceId,
    pub layout: &'a Layout,
    /// Devices with a currently-live, already-authenticated `Peer` —
    /// a snapshot the caller (`core::session`) computes from its own
    /// connected-peers map, never re-derived in here. A neighbor not
    /// in this set is treated exactly like an unmapped edge: nothing
    /// to switch to, stay put (see the module-level DoD: a
    /// disconnected/unavailable target must never freeze or steal
    /// input).
    pub connected: &'a HashSet<DeviceId>,
    /// Screen size for every device this machine might forward to
    /// (including, when `Forwarding`, the size of its own screen — the
    /// case where a switch chain returns here). Missing an entry for a
    /// device that would otherwise be a valid switch target is treated
    /// the same as "not connected": nothing to switch to.
    pub screen_sizes: &'a HashMap<DeviceId, (u32, u32)>,
}

/// Advances `state` given `event` and `ctx`. Total — every combination
/// has a well-defined result, including a no-op, so there is no
/// `Result`/error case: an edge crossing toward an unmapped or
/// unavailable neighbor is exactly as valid an outcome as a real
/// switch, not a failure.
pub fn transition(
    state: OwnershipState,
    event: OwnershipEvent,
    ctx: &SwitchContext,
) -> OwnershipState {
    match event {
        OwnershipEvent::TargetDisconnected => OwnershipState::Local,
        OwnershipEvent::ReturnToLocal => OwnershipState::Local,
        OwnershipEvent::EdgeCrossed { edge, coordinate } => {
            let (active_screen_owner, source_size) = match state {
                OwnershipState::Local => {
                    (ctx.our_device_id, ctx.screen_sizes.get(&ctx.our_device_id))
                }
                OwnershipState::Forwarding { target, .. } => {
                    (target, ctx.screen_sizes.get(&target))
                }
            };
            let Some(neighbor) = ctx.layout.neighbor(active_screen_owner, edge) else {
                return state; // Unmapped edge: nothing to switch to.
            };
            if neighbor == ctx.our_device_id {
                // The chain led back to us — ownership returns home.
                // Checked before the connectivity/screen-size lookups
                // below: we are never a member of `ctx.connected`
                // (that set holds *peer* connections, and we don't
                // hold a `Peer` to ourselves), so those checks must
                // not gate this case.
                return OwnershipState::Local;
            }
            if !ctx.connected.contains(&neighbor) {
                return state; // Configured but not reachable right now.
            }
            let Some(&dest_size) = ctx.screen_sizes.get(&neighbor) else {
                return state; // No known screen size yet: stay put.
            };
            let source_size = match source_size {
                Some(&s) => s,
                None => return state,
            };
            OwnershipState::Forwarding {
                target: neighbor,
                virtual_cursor: entry_position(edge, source_size, dest_size, coordinate),
            }
        }
    }
}

/// Where the pointer should appear on a destination screen when
/// crossing `edge` of a source screen at `crossing_coordinate` (that
/// coordinate's meaning matches [`OwnershipEvent::EdgeCrossed`]: y for
/// `Left`/`Right`, x for `Top`/`Bottom`).
///
/// Deterministic and resolution-aware: the coordinate along the shared
/// edge is rescaled proportionally between the two screens' sizes
/// (`dest_y = crossing_coordinate / source_height * dest_height`, and
/// symmetrically for the other axis/edges), so differing resolutions
/// and aspect ratios land at the equivalent relative position rather
/// than a fragile 1:1 pixel assumption. The entry edge is always the
/// opposite of the exit edge. `crossing_coordinate` is clamped into
/// `[0, source dimension]` first, so a coordinate outside the expected
/// bounds (e.g. from a momentarily stale reading) still produces an
/// in-bounds result instead of placing the cursor off-screen.
///
/// Out of scope, by design (see ADR-0009): per-monitor multi-monitor
/// layouts. Each screen here is whatever one combined boundary the
/// caller's `PointerGeometry::screen_size` reports.
pub fn entry_position(
    edge: Edge,
    source_screen: (u32, u32),
    dest_screen: (u32, u32),
    crossing_coordinate: i32,
) -> (i32, i32) {
    fn fraction(coordinate: i32, extent: u32) -> f64 {
        let extent = extent.max(1) as f64;
        (coordinate as f64 / extent).clamp(0.0, 1.0)
    }

    let (dest_w, dest_h) = (dest_screen.0, dest_screen.1);

    match edge {
        Edge::Right => {
            let y = (fraction(crossing_coordinate, source_screen.1) * dest_h as f64) as i32;
            (0, y)
        }
        Edge::Left => {
            let y = (fraction(crossing_coordinate, source_screen.1) * dest_h as f64) as i32;
            (dest_w.saturating_sub(1) as i32, y)
        }
        Edge::Bottom => {
            let x = (fraction(crossing_coordinate, source_screen.0) * dest_w as f64) as i32;
            (x, 0)
        }
        Edge::Top => {
            let x = (fraction(crossing_coordinate, source_screen.0) * dest_w as f64) as i32;
            (x, dest_h.saturating_sub(1) as i32)
        }
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

    fn screen_sizes() -> HashMap<DeviceId, (u32, u32)> {
        HashMap::from([
            (US, (1000, 800)),
            (NEIGHBOR, (1000, 800)),
            (THIRD, (1000, 800)),
        ])
    }

    #[test]
    fn right_edge_switch_to_a_connected_neighbor() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let connected = HashSet::from([NEIGHBOR]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let next = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 400,
            },
            &ctx,
        );
        assert_eq!(
            next,
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (0, 400),
            }
        );
    }

    #[test]
    fn left_edge_switch_lands_at_the_destinations_right_edge() {
        let layout = layout_with(&[(US, Edge::Left, NEIGHBOR)]);
        let connected = HashSet::from([NEIGHBOR]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let next = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Left,
                coordinate: 200,
            },
            &ctx,
        );
        assert_eq!(
            next,
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (999, 200),
            }
        );
    }

    #[test]
    fn top_and_bottom_edges_switch_on_the_x_axis() {
        let layout = layout_with(&[(US, Edge::Bottom, NEIGHBOR)]);
        let connected = HashSet::from([NEIGHBOR]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let next = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Bottom,
                coordinate: 500,
            },
            &ctx,
        );
        assert_eq!(
            next,
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (500, 0),
            }
        );
    }

    #[test]
    fn unmapped_edge_leaves_ownership_unchanged() {
        let layout = layout_with(&[]);
        let connected = HashSet::new();
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let next = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 0,
            },
            &ctx,
        );
        assert_eq!(next, OwnershipState::Local);
    }

    #[test]
    fn disconnected_target_does_not_switch_ownership() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let connected = HashSet::new(); // NEIGHBOR is configured but not connected.
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let next = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 0,
            },
            &ctx,
        );
        assert_eq!(
            next,
            OwnershipState::Local,
            "must keep local input working when the configured target is unavailable"
        );
    }

    #[test]
    fn a_switch_chain_can_continue_onward_to_a_third_device() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Right, THIRD)]);
        let connected = HashSet::from([NEIGHBOR, THIRD]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let after_first = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 100,
            },
            &ctx,
        );
        assert_eq!(
            after_first,
            OwnershipState::Forwarding {
                target: NEIGHBOR,
                virtual_cursor: (0, 100),
            }
        );

        let after_second = transition(
            after_first,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 700,
            },
            &ctx,
        );
        assert_eq!(
            after_second,
            OwnershipState::Forwarding {
                target: THIRD,
                virtual_cursor: (0, 700),
            }
        );
    }

    #[test]
    fn crossing_back_toward_the_origin_returns_ownership_to_local() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Left, US)]);
        let connected = HashSet::from([NEIGHBOR]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let forwarding = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 100,
            },
            &ctx,
        );
        let back = transition(
            forwarding,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Left,
                coordinate: 300,
            },
            &ctx,
        );
        assert_eq!(back, OwnershipState::Local);
    }

    #[test]
    fn target_disconnected_forces_local_regardless_of_chain_depth() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR)]);
        let connected = HashSet::from([NEIGHBOR]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let forwarding = transition(
            OwnershipState::Local,
            OwnershipEvent::EdgeCrossed {
                edge: Edge::Right,
                coordinate: 100,
            },
            &ctx,
        );
        assert!(matches!(forwarding, OwnershipState::Forwarding { .. }));

        let after = transition(forwarding, OwnershipEvent::TargetDisconnected, &ctx);
        assert_eq!(after, OwnershipState::Local);
    }

    #[test]
    fn return_to_local_is_a_no_op_when_already_local() {
        let layout = layout_with(&[]);
        let connected = HashSet::new();
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };
        let next = transition(OwnershipState::Local, OwnershipEvent::ReturnToLocal, &ctx);
        assert_eq!(next, OwnershipState::Local);
    }

    #[test]
    fn rapid_repeated_edge_crossings_never_leave_an_inconsistent_state() {
        let layout = layout_with(&[(US, Edge::Right, NEIGHBOR), (NEIGHBOR, Edge::Left, US)]);
        let connected = HashSet::from([NEIGHBOR]);
        let sizes = screen_sizes();
        let ctx = SwitchContext {
            our_device_id: US,
            layout: &layout,
            connected: &connected,
            screen_sizes: &sizes,
        };

        let mut state = OwnershipState::Local;
        for i in 0..50 {
            let edge = if i % 2 == 0 { Edge::Right } else { Edge::Left };
            state = transition(
                state,
                OwnershipEvent::EdgeCrossed {
                    edge,
                    coordinate: 100,
                },
                &ctx,
            );
            assert!(matches!(
                state,
                OwnershipState::Local | OwnershipState::Forwarding { .. }
            ));
        }
        // An even number of round-trip flips returns to Local.
        assert_eq!(state, OwnershipState::Local);
    }

    #[test]
    fn entry_position_rescales_proportionally_for_different_resolutions() {
        // Crossing halfway down a 800-tall source screen should land
        // halfway down a 2000-tall destination screen, regardless of
        // the resolution mismatch.
        let pos = entry_position(Edge::Right, (1000, 800), (2000, 2000), 400);
        assert_eq!(pos, (0, 1000));
    }

    #[test]
    fn entry_position_clamps_out_of_bounds_coordinates() {
        let too_far = entry_position(Edge::Right, (1000, 800), (1000, 800), 5000);
        assert_eq!(too_far, (0, 800));

        let negative = entry_position(Edge::Right, (1000, 800), (1000, 800), -100);
        assert_eq!(negative, (0, 0));
    }

    #[test]
    fn entry_position_handles_a_zero_sized_source_without_panicking() {
        let pos = entry_position(Edge::Bottom, (0, 0), (1000, 800), 0);
        assert_eq!(pos.1, 0);
        assert!(pos.0 <= 1000);
    }
}
