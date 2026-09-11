//! Async orchestration for edge-based switching — see ADR-0009. The one
//! place `core` holds more than one [`kvm_net::Peer`] at a time.
//!
//! Splits cleanly along this codebase's established pure/async line
//! (mirroring `pairing.rs`/`pairing_session.rs`): [`crate::router::Router`]
//! makes every routing decision as pure data, unit-tested with zero I/O;
//! [`Session`] here just turns those decisions into real sends over
//! already-authenticated peer connections.
//!
//! **The receiving/target side needs nothing from this module beyond
//! [`run_target`].** A device that becomes someone else's forwarding
//! target doesn't run a [`Session`] at all — it just warps its cursor
//! via [`kvm_input::PointerGeometry::set_cursor_position`] every time
//! ownership arrives (correctly handling rapid back-and-forth, not just
//! the first switch), and injects whatever arrives on the peer's input
//! stream, including the modifier-release events the source's `Router`
//! synthesizes, which travel as perfectly ordinary [`InputMessage`]s on
//! that same stream.
//!
//! **Known limitation, tracked for a later milestone**: every backend's
//! `Capture` is listen-only (no exclusive grab — ADR-0007/ADR-0008), so
//! while `Forwarding`, the capturing device's own OS still visibly
//! applies captured input locally too, even though `Router` correctly
//! avoids re-injecting or double-routing it. Actually suppressing local
//! input while forwarding needs the per-OS `Capture` backends to grow a
//! blocking mode — real-hardware work for Milestone 4/5, not something
//! `Session`'s fakes-only integration tests can exercise.

use std::collections::HashMap;

use kvm_input::{Inject, PointerGeometry};
use kvm_net::{NetError, Peer};
use kvm_protocol::{ControlMessage, DeviceId, InputMessage, Message};

use crate::error::CoreError;
use crate::input_bridge::{LOCAL_PLATFORM, translate_for_local_platform};
use crate::layout::Layout;
use crate::ownership::OwnershipState;
use crate::router::{Effect, Router};

/// Drives edge-switching from the capturing device's side. Holds every
/// peer this device might directly forward to — not just immediate
/// layout neighbors, since a multi-hop chain forwards directly once
/// ownership reaches a third/fourth device (see `router.rs`/ADR-0009).
pub struct Session {
    router: Router,
    peers: HashMap<DeviceId, Peer>,
}

impl Session {
    pub fn new(
        our_device_id: DeviceId,
        layout: Layout,
        our_screen_size: (u32, u32),
        initial_position: (i32, i32),
    ) -> Self {
        Self {
            router: Router::new(
                our_device_id,
                LOCAL_PLATFORM,
                layout,
                our_screen_size,
                initial_position,
            ),
            peers: HashMap::new(),
        }
    }

    pub fn ownership_state(&self) -> OwnershipState {
        self.router.state()
    }

    /// Registers an already-authenticated peer as a possible switch
    /// target. Only ever call this with a `Peer` obtained from
    /// `net::connect`/`accept` (already TLS/trust-store verified) — see
    /// ADR-0009's security section. There is no other way for a device
    /// to become reachable through a `Session`: `Router` cannot name a
    /// device that wasn't registered here first.
    pub fn add_peer(&mut self, device_id: DeviceId, peer: Peer, screen_size: (u32, u32)) {
        tracing::info!(
            ?device_id,
            ?screen_size,
            "peer connected, now a valid switch target"
        );
        self.router.set_peer_connected(device_id, screen_size);
        self.peers.insert(device_id, peer);
    }

    /// Removes a peer (disconnect or revocation). If it was the current
    /// forwarding target, ownership snaps back to `Local` immediately —
    /// see ADR-0009: a disconnected target must never keep holding
    /// ownership, freeze the pointer, or drop keyboard input.
    pub fn remove_peer(&mut self, device_id: DeviceId) {
        let was_active_target = matches!(
            self.router.state(),
            OwnershipState::Forwarding { target, .. } if target == device_id
        );
        self.peers.remove(&device_id);
        self.router.set_peer_disconnected(device_id);
        if was_active_target {
            tracing::warn!(
                ?device_id,
                "active forwarding target disconnected, falling back to local input"
            );
        } else {
            tracing::info!(?device_id, "peer disconnected");
        }
    }

    /// Re-anchors the router's tracked cursor position to a real OS
    /// reading. Callers should do this after regaining `Local`
    /// ownership (a normal switch back, or a forced-local after a
    /// disconnect) — `Router` has no way to know the real cursor
    /// position on its own.
    pub fn resync_local_position(&mut self, x: i32, y: i32) {
        self.router.resync_position(x, y);
    }

    /// Processes one locally captured event end to end: asks `Router`
    /// what should happen, then makes it happen for real. A send that
    /// fails because a peer's connection has actually died is treated
    /// as that peer disconnecting — handled the same way `remove_peer`
    /// handles it, not surfaced as a fatal session error, matching the
    /// DoD's "a disconnected target must not crash the session."
    ///
    /// `local_geometry` is used only for `Effect::RecenterLocal` — the
    /// moment ownership first leaves `Local`, our own cursor gets
    /// warped away from whatever boundary the triggering edge crossing
    /// just pinned it against (see ADR-0009's Update note: without
    /// this, the local OS cursor stays pinned there for the rest of the
    /// `Forwarding` session, starving further real motion and letting
    /// ordinary hand jitter near the pin flicker ownership back and
    /// forth across `EDGE_MARGIN`).
    pub async fn handle_captured(
        &mut self,
        event: InputMessage,
        local_geometry: &mut dyn PointerGeometry,
    ) -> Result<(), CoreError> {
        for effect in self.router.handle_captured(event) {
            self.apply(effect, local_geometry).await?;
        }
        Ok(())
    }

    async fn apply(
        &mut self,
        effect: Effect,
        local_geometry: &mut dyn PointerGeometry,
    ) -> Result<(), CoreError> {
        let (to, message) = match effect {
            Effect::RecenterLocal { x, y } => {
                tracing::info!(x, y, "recentering local cursor away from its own edge");
                local_geometry.set_cursor_position(x, y)?;
                return Ok(());
            }
            Effect::Send { to, message } => (to, Message::Input(message)),
            Effect::Switch {
                to,
                cursor_position,
            } => {
                tracing::info!(?to, ?cursor_position, "switching active input target");
                (
                    to,
                    Message::Control(ControlMessage::SwitchActive {
                        device_id: to,
                        cursor_position,
                    }),
                )
            }
        };

        let stream = match &message {
            Message::Control(_) => self
                .peers
                .get_mut(&to)
                .map(|peer| &mut peer.streams.control),
            _ => self.peers.get_mut(&to).map(|peer| &mut peer.streams.input),
        };
        let Some(stream) = stream else {
            return Err(CoreError::UnknownPeer(to));
        };

        match stream.send(&message).await {
            Ok(()) => Ok(()),
            Err(NetError::ConnectionClosed) => {
                self.remove_peer(to);
                Ok(())
            }
            Err(e) => {
                self.remove_peer(to);
                Err(CoreError::Net(e))
            }
        }
    }
}

/// Exchanges each side's screen size over an already-established
/// peer's control stream — real ground truth for the resolution-aware
/// pointer handoff math (ADR-0009 §6), rather than a guess. Call this
/// once, right after `net::connect`/`accept` returns and before
/// handing the `Peer` to `Session::add_peer`/`run_target` — the
/// control stream is free for ordinary use at that point, since the
/// identity handshake itself is already done.
pub async fn exchange_screen_size(
    peer: &mut Peer,
    our_screen_size: (u32, u32),
) -> Result<(u32, u32), CoreError> {
    peer.streams
        .control
        .send(&Message::Control(ControlMessage::ScreenInfo {
            width: our_screen_size.0,
            height: our_screen_size.1,
        }))
        .await?;
    match peer.recv_control().await? {
        ControlMessage::ScreenInfo { width, height } => Ok((width, height)),
        other => Err(CoreError::Net(NetError::ProtocolViolation(format!(
            "expected a ScreenInfo control message, got {other:?}"
        )))),
    }
}

/// Runs on the target side for the whole lifetime of one peer
/// connection — not just the first switch. Concurrently waits for
/// `SwitchActive` (warping the local cursor via `geometry` every time
/// ownership arrives, not only the first) and injects whatever arrives
/// on the input stream, so rapid back-and-forth switching (ownership
/// leaving this device and later returning) is handled correctly: a
/// later return warps the cursor again to the newly computed entry
/// position rather than silently resuming wherever it was left.
pub async fn run_target(
    peer: &mut Peer,
    geometry: &mut dyn PointerGeometry,
    inject: &mut dyn Inject,
) -> Result<(), CoreError> {
    let control = &mut peer.streams.control;
    let input = &mut peer.streams.input;
    loop {
        tokio::select! {
            control_msg = control.recv() => {
                match control_msg {
                    Ok(Message::Control(ControlMessage::SwitchActive { cursor_position: (x, y), .. })) => {
                        geometry.set_cursor_position(x, y)?;
                    }
                    Ok(other) => {
                        return Err(CoreError::Net(NetError::ProtocolViolation(format!(
                            "expected a SwitchActive control message, got {other:?}"
                        ))));
                    }
                    Err(NetError::ConnectionClosed) => return Ok(()),
                    Err(e) => return Err(CoreError::Net(e)),
                }
            }
            input_msg = input.recv() => {
                match input_msg {
                    Ok(Message::Input(event)) => inject.inject(&translate_for_local_platform(event))?,
                    Ok(other) => {
                        return Err(CoreError::Net(NetError::ProtocolViolation(format!(
                            "expected an Input message on the input stream, got {other:?}"
                        ))));
                    }
                    Err(NetError::ConnectionClosed) => return Ok(()),
                    Err(e) => return Err(CoreError::Net(e)),
                }
            }
        }
    }
}
