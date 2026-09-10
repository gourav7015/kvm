//! Bridges the `input` crate's synchronous, thread-based `Capture`/
//! `Inject` traits to an authenticated [`Peer`]'s input stream — the
//! "core/input logic" layer ADR-0007 commits to, so that neither `input`
//! (no networking) nor `net` (no OS input APIs) needs to know about the
//! other.
//!
//! `Capture`/`Inject` are deliberately synchronous (see ADR-0007 §2), so
//! forwarding captured events onto an async [`MessageStream`] needs a
//! hand-off: a blocking task drains the `std::sync::mpsc::Receiver`
//! capture writes to and forwards each event into a `tokio` channel that
//! the async send loop can await.

use std::sync::mpsc as std_mpsc;

use kvm_input::{Capture, Inject};
use kvm_net::{MessageStream, NetError};
use kvm_protocol::Message;

use crate::error::CoreError;

/// Runs `capture` and forwards every event it produces onto `stream`
/// until the stream closes or errors. Stops capture before returning,
/// whichever way this ends.
pub async fn forward_capture_to_peer(
    capture: &mut dyn Capture,
    stream: &mut MessageStream,
) -> Result<(), CoreError> {
    let (sink, source) = std_mpsc::channel();
    capture.start(sink)?;

    let (async_tx, mut async_rx) = tokio::sync::mpsc::unbounded_channel();
    let drain = tokio::task::spawn_blocking(move || {
        while let Ok(event) = source.recv() {
            if async_tx.send(event).is_err() {
                break;
            }
        }
    });

    let result = loop {
        match async_rx.recv().await {
            Some(event) => {
                if let Err(e) = stream.send(&Message::Input(event)).await {
                    break Err(CoreError::Net(e));
                }
            }
            None => break Ok(()),
        }
    };

    capture.stop();
    let _ = drain.await;
    result
}

/// Reads [`kvm_protocol::InputMessage`]s from `stream` and injects each
/// one via `inject`, until the stream closes or a non-Input message
/// arrives (a protocol violation on the input stream — every other
/// concern has its own stream).
///
/// A single injection failure (e.g. a still-missing macOS Accessibility
/// grant) ends the loop and is returned rather than swallowed — per
/// ADR-0007, injection failure must be surfaced, never silently
/// pretended-successful. The caller decides whether/how to retry.
pub async fn inject_from_peer(
    inject: &mut dyn Inject,
    stream: &mut MessageStream,
) -> Result<(), CoreError> {
    loop {
        match stream.recv().await {
            Ok(Message::Input(event)) => inject.inject(&event)?,
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
