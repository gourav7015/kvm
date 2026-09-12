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
//! **Local input suppression while forwarding (ADR-0009 decision 9)**:
//! `handle_captured` toggles `Capture::set_local_suppression` exactly on
//! `Local`<->`Forwarding` transitions, so the capturing device's own OS
//! stops visibly acting on captured input for the duration of a
//! `Forwarding` session (real hardware QA found the original
//! listen-only-forever behavior genuinely disruptive, not merely
//! cosmetic). Only macOS/Windows actually implement blocking capture so
//! far — X11 keeps the trait's default no-op until a later milestone
//! (see the ADR); this call is harmless either way.

use std::collections::HashMap;
use std::time::Duration;

use kvm_input::{Capture, Inject, PointerGeometry};
use kvm_net::{NetError, Peer};
use kvm_protocol::{ControlMessage, DeviceId, InputMessage, Message};
use tokio::time::Instant;

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
    /// When each peer last proved it is still *processing input* — see
    /// [`TARGET_LIVENESS_TIMEOUT`]. Armed when a peer becomes the active
    /// target; a peer with no entry is not monitored.
    last_heard: HashMap<DeviceId, Instant>,
    liveness_timeout: Duration,
    next_ping_nonce: u64,
}

/// How long the active forwarding target may go without answering a
/// `Ping` before this device gives up on it and takes local input back.
/// See ADR-0009 decision 19.
///
/// **Why this exists — a real-hardware finding.** A target can stay
/// *connected* while no longer processing input at all: on Windows,
/// clicking into the relay's own console window started a QuickEdit
/// selection, which pauses any write to that console, and the target's
/// input loop writes a log line per injected event. QUIC kept the
/// connection alive (its keep-alives run on other threads), so neither
/// the transport's idle timeout nor the closed-connection watchdog could
/// ever fire, and this device kept suppressing its own input while
/// forwarding into the void — recovered only by killing the target.
///
/// A target answers a `Ping` from the same loop that injects input (see
/// [`run_target`]), so answers stop exactly when injection stops. On a
/// LAN an answer takes one round trip (milliseconds), and the caller
/// pings several times a second, so 2 s is many missed answers — never
/// ordinary jitter — while still handing a user who has lost their input
/// back within a couple of seconds. It is a safety bound, not a tuning
/// knob; it is deliberately far below QUIC's own 10 s idle timeout,
/// which only ever catches connections that are actually gone.
pub const TARGET_LIVENESS_TIMEOUT: Duration = Duration::from_secs(2);

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
            last_heard: HashMap::new(),
            liveness_timeout: TARGET_LIVENESS_TIMEOUT,
            next_ping_nonce: 0,
        }
    }

    /// Overrides [`TARGET_LIVENESS_TIMEOUT`] — for tests that need to
    /// observe the timeout without waiting seconds for it.
    pub fn set_liveness_timeout(&mut self, timeout: Duration) {
        self.liveness_timeout = timeout;
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
    ///
    /// **Also restores local input suppression itself, right here** —
    /// not left to the caller to notice. A real-hardware safety finding
    /// (see ADR-0009's Update): the previous design only restored
    /// suppression inside `handle_captured`, computed by comparing
    /// ownership state before/after that one call. Any *other* path
    /// that can force a disconnect back to `Local` (this method, called
    /// directly, or a proactive watchdog with no captured event driving
    /// it at all -- see `edge_switch_relay.rs`) would leave suppression
    /// stuck with no further captured input ever able to clear it,
    /// which is exactly the failure mode real hardware QA hit: local
    /// keyboard and trackpad both suppressed, with no way left to even
    /// reach Activity Monitor to kill the process. Centralizing the
    /// restore here means every path that can force `Forwarding` ->
    /// `Local` goes through one place that's guaranteed to lift it.
    pub fn remove_peer(&mut self, device_id: DeviceId, local_capture: &mut dyn Capture) {
        let was_active_target = matches!(
            self.router.state(),
            OwnershipState::Forwarding { target, .. } if target == device_id
        );
        self.peers.remove(&device_id);
        self.last_heard.remove(&device_id);
        self.router.set_peer_disconnected(device_id);
        if was_active_target {
            tracing::warn!(
                ?device_id,
                "active forwarding target disconnected, falling back to local input"
            );
            local_capture.set_local_suppression(false);
        } else {
            tracing::info!(?device_id, "peer disconnected");
        }
    }

    /// Checks whether the *current* forwarding target's underlying
    /// connection has already been reported closed by the transport
    /// (`quinn::Connection::close_reason`), without needing to attempt
    /// a send first. Returns the target's `DeviceId` if so.
    ///
    /// This is what makes the watchdog in `edge_switch_relay.rs`
    /// proactive rather than lazy: without it, a dead connection is
    /// only ever discovered as a *side effect* of the next captured
    /// input event failing to send -- which never happens if the user
    /// has stopped moving the mouse/keyboard entirely (exactly the
    /// state a stuck suppression leaves them in). Polling this
    /// independently of any captured event closes that gap: local
    /// input gets restored within one watchdog tick of the transport
    /// itself noticing the connection is gone, with no user action
    /// required at all.
    pub fn active_target_connection_closed(&self) -> Option<DeviceId> {
        let OwnershipState::Forwarding { target, .. } = self.router.state() else {
            return None;
        };
        let peer = self.peers.get(&target)?;
        peer.connection.close_reason().is_some().then_some(target)
    }

    /// Re-anchors the router's tracked cursor position to a real OS
    /// reading. Callers should do this after regaining `Local`
    /// ownership (a normal switch back, or a forced-local after a
    /// disconnect) — `Router` has no way to know the real cursor
    /// position on its own.
    pub fn resync_local_position(&mut self, x: i32, y: i32) {
        self.router.resync_position(x, y);
    }

    /// Sends a `Ping` to the current forwarding target (a no-op while
    /// `Local`). Call it several times a second while `Forwarding`; the
    /// answers are read by [`Self::recv_from_active_target`] and judged by
    /// [`Self::check_target_liveness`]. A failed send means the connection
    /// is gone: the peer is removed (restoring local input, via
    /// [`Self::remove_peer`]) and returned.
    pub async fn ping_active_target(
        &mut self,
        local_capture: &mut dyn Capture,
    ) -> Option<DeviceId> {
        let OwnershipState::Forwarding { target, .. } = self.router.state() else {
            return None;
        };
        let peer = self.peers.get_mut(&target)?;
        let nonce = self.next_ping_nonce;
        self.next_ping_nonce = self.next_ping_nonce.wrapping_add(1);
        let ping = Message::Control(ControlMessage::Ping { nonce });
        match peer.streams.control.send(&ping).await {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!(?target, error = %e, "ping to active target failed");
                self.remove_peer(target, local_capture);
                Some(target)
            }
        }
    }

    /// Waits for the next control message from the current forwarding
    /// target; pends forever while `Local`. Cancel-safe
    /// (`MessageStream::recv` keeps partially-read frames in its own
    /// buffer), so it can sit in a `select!` beside captured input and be
    /// dropped whenever another branch wins. Hand the result to
    /// [`Self::handle_target_control`].
    pub async fn recv_from_active_target(&mut self) -> (DeviceId, Result<Message, NetError>) {
        let OwnershipState::Forwarding { target, .. } = self.router.state() else {
            return std::future::pending().await;
        };
        let Some(peer) = self.peers.get_mut(&target) else {
            return std::future::pending().await;
        };
        (target, peer.streams.control.recv().await)
    }

    /// Applies one result from [`Self::recv_from_active_target`]: a `Pong`
    /// proves `from` is still processing input; a broken stream means
    /// it's gone, and is handled exactly like any other disconnect.
    pub fn handle_target_control(
        &mut self,
        from: DeviceId,
        result: Result<Message, NetError>,
        local_capture: &mut dyn Capture,
    ) {
        match result {
            Ok(Message::Control(ControlMessage::Pong { nonce })) => {
                tracing::trace!(?from, nonce, "pong from target");
                if self.peers.contains_key(&from) {
                    self.last_heard.insert(from, Instant::now());
                }
            }
            Ok(other) => {
                tracing::warn!(
                    ?from,
                    ?other,
                    "unexpected control message from target -- ignored"
                );
            }
            Err(e) => {
                tracing::warn!(?from, error = %e, "control stream from target failed");
                self.remove_peer(from, local_capture);
            }
        }
    }

    /// If the current forwarding target has not answered a `Ping` within
    /// the liveness timeout ([`TARGET_LIVENESS_TIMEOUT`] by default), gives
    /// up on it: the peer is removed — dropping the connection, so input
    /// already queued for it is discarded rather than replayed if it ever
    /// recovers — and local input comes back via [`Self::remove_peer`].
    /// Returns the target given up on. A no-op while `Local`, or for a
    /// target that was never armed.
    pub fn check_target_liveness(&mut self, local_capture: &mut dyn Capture) -> Option<DeviceId> {
        let OwnershipState::Forwarding { target, .. } = self.router.state() else {
            return None;
        };
        let silent_for = self.last_heard.get(&target)?.elapsed();
        if silent_for < self.liveness_timeout {
            return None;
        }
        tracing::warn!(
            ?target,
            ?silent_for,
            "active target is connected but has stopped answering pings -- \
             it is no longer processing input; dropping it and giving local input back"
        );
        self.remove_peer(target, local_capture);
        Some(target)
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
    ///
    /// `local_capture` is the same `Capture` object driving this very
    /// call (its captured events are what `event` comes from) — used
    /// only to toggle `set_local_suppression` exactly when ownership
    /// crosses the `Local`/`Forwarding` boundary (ADR-0009 decision 9),
    /// never on every event.
    ///
    /// **Real-hardware safety regression, fixed here**: a target
    /// disconnecting abruptly (the peer process exiting, a network
    /// drop) makes the *next* captured event's `apply()` call fail --
    /// `MessageStream::send` always reports a broken connection as
    /// `NetError::Transport`, never the more specific `ConnectionClosed`
    /// (see `crates/net/src/framed.rs`), so `apply()`'s generic-error
    /// arm returns `Err` after already calling `remove_peer` (which
    /// correctly forces `Router` back to `Local`). The old code applied
    /// every effect with `?` inside the loop below, so that `Err`
    /// propagated straight out of this function *before* ever reaching
    /// the suppression-restore check that follows the loop -- silently
    /// leaving local input suppressed forever after a disconnect, in
    /// direct violation of this project's own explicit safety
    /// requirement ("never leave local input permanently disabled after
    /// disconnect, crash, or transition failure"). Fixed by always
    /// running the suppression check against `Router`'s actual final
    /// state, regardless of whether applying any individual effect
    /// failed -- the first error (if any) is still returned to the
    /// caller afterward, unchanged from before.
    pub async fn handle_captured(
        &mut self,
        event: InputMessage,
        local_geometry: &mut dyn PointerGeometry,
        local_capture: &mut dyn Capture,
    ) -> Result<(), CoreError> {
        let was_local = matches!(self.router.state(), OwnershipState::Local);
        let mut first_error = None;
        for effect in self.router.handle_captured(event) {
            if let Err(e) = self.apply(effect, local_geometry, local_capture).await {
                first_error.get_or_insert(e);
            }
        }
        let is_local = matches!(self.router.state(), OwnershipState::Local);
        if was_local && !is_local {
            local_capture.set_local_suppression(true);
        } else if !was_local && is_local {
            local_capture.set_local_suppression(false);
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    async fn apply(
        &mut self,
        effect: Effect,
        local_geometry: &mut dyn PointerGeometry,
        local_capture: &mut dyn Capture,
    ) -> Result<(), CoreError> {
        let (to, message) = match effect {
            Effect::RecenterLocal { x, y } => {
                tracing::info!(x, y, "recentering local cursor away from its own edge");
                local_geometry.set_cursor_position(x, y)?;
                return Ok(());
            }
            Effect::Send { to, message } => {
                // DIAGNOSTIC (Phase 4 pointer-range root-cause hunt):
                // stage 3 -- exactly what goes onto the wire, immediately
                // before serialization/QUIC. The target side logs what it
                // receives and what it does with it (see
                // `WindowsInject::inject`), so stage 3 vs. the target's
                // own `dx`/`dy` isolates the transport.
                tracing::debug!(
                    stage = "3-session-send",
                    ?to,
                    ?message,
                    "sending input to active target"
                );
                (to, Message::Input(message))
            }
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
            Ok(()) => {
                tracing::trace!(?to, "send succeeded");
                // Becoming the active target starts its liveness clock --
                // see `TARGET_LIVENESS_TIMEOUT`.
                if matches!(
                    message,
                    Message::Control(ControlMessage::SwitchActive { .. })
                ) {
                    self.last_heard.insert(to, Instant::now());
                }
                Ok(())
            }
            Err(NetError::ConnectionClosed) => {
                tracing::warn!(?to, "send failed: connection closed");
                self.remove_peer(to, local_capture);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(?to, error = %e, "send failed");
                self.remove_peer(to, local_capture);
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
                        // A single failed cursor warp (the OS can refuse
                        // one for all sorts of transient reasons) must
                        // not tear down the whole target session --
                        // surfaced via tracing (per ADR-0007: never
                        // silently pretended successful), but the
                        // connection and injection loop carry on.
                        if let Err(e) = geometry.set_cursor_position(x, y) {
                            tracing::warn!(error = %e, x, y, "cursor warp failed -- continuing");
                        }
                    }
                    // Answered from this same loop that injects input, so
                    // answers stop exactly when injection stops -- which
                    // is what the capturing device's liveness check needs
                    // to see (`Session::check_target_liveness`, ADR-0009
                    // decision 19).
                    Ok(Message::Control(ControlMessage::Ping { nonce })) => {
                        let pong = Message::Control(ControlMessage::Pong { nonce });
                        if let Err(e) = control.send(&pong).await {
                            return Err(CoreError::Net(e));
                        }
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
                    Ok(Message::Input(event)) => {
                        // Same reasoning: one failed injection (e.g. a
                        // transient SendInput rejection due to a focus
                        // change or UIPI restriction on Windows) must
                        // not silently kill the entire remote-control
                        // session -- see ADR-0009's Update note for the
                        // real hardware failure this was found from
                        // (the whole session died on the first rejected
                        // injection, with no way to recover without a
                        // full manual restart).
                        if let Err(e) = inject.inject(&translate_for_local_platform(event)) {
                            tracing::warn!(error = %e, "injection failed for one event -- continuing");
                        }
                    }
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
