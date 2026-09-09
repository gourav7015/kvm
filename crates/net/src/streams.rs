//! Opens/accepts the four per-concern streams multiplexed over one QUIC
//! connection: control, input, clipboard, transfer.
//!
//! QUIC guarantees in-order delivery *within* a stream but not arrival
//! order *across* streams — under packet loss or reordering, the streams
//! this side opened first are not guaranteed to be the streams the peer's
//! `accept_bi()` observes first. So each stream is tagged with a one-byte
//! kind identifier as the very first thing written to it, and the
//! accepting side reads that tag to route the stream correctly rather
//! than relying on acceptance order.

use crate::error::NetError;
use crate::framed::MessageStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    Control,
    Input,
    Clipboard,
    Transfer,
}

impl StreamKind {
    const ALL: [StreamKind; 4] = [
        StreamKind::Control,
        StreamKind::Input,
        StreamKind::Clipboard,
        StreamKind::Transfer,
    ];

    fn tag(self) -> u8 {
        match self {
            StreamKind::Control => 0,
            StreamKind::Input => 1,
            StreamKind::Clipboard => 2,
            StreamKind::Transfer => 3,
        }
    }

    fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(StreamKind::Control),
            1 => Some(StreamKind::Input),
            2 => Some(StreamKind::Clipboard),
            3 => Some(StreamKind::Transfer),
            _ => None,
        }
    }
}

/// The four concern streams for one peer connection.
pub struct Streams {
    pub control: MessageStream,
    pub input: MessageStream,
    pub clipboard: MessageStream,
    pub transfer: MessageStream,
}

/// Opens all four concern streams as the connection initiator.
pub async fn open_streams(connection: &quinn::Connection) -> Result<Streams, NetError> {
    let mut opened = Vec::with_capacity(StreamKind::ALL.len());
    for kind in StreamKind::ALL {
        let (mut send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| NetError::Transport(e.to_string()))?;
        send.write_all(&[kind.tag()])
            .await
            .map_err(|e| NetError::Transport(e.to_string()))?;
        opened.push((kind, MessageStream::new(send, recv)));
    }
    assemble(opened)
}

/// Accepts all four concern streams as the connection acceptor, routing
/// each by its leading tag byte rather than acceptance order.
pub async fn accept_streams(connection: &quinn::Connection) -> Result<Streams, NetError> {
    let mut opened = Vec::with_capacity(StreamKind::ALL.len());
    for _ in 0..StreamKind::ALL.len() {
        let (send, mut recv) = connection
            .accept_bi()
            .await
            .map_err(|e| NetError::Transport(e.to_string()))?;
        let mut tag = [0u8; 1];
        recv.read_exact(&mut tag)
            .await
            .map_err(|e| NetError::Transport(e.to_string()))?;
        let kind = StreamKind::from_tag(tag[0])
            .ok_or_else(|| NetError::ProtocolViolation(format!("unknown stream tag {}", tag[0])))?;
        opened.push((kind, MessageStream::new(send, recv)));
    }
    assemble(opened)
}

fn assemble(opened: Vec<(StreamKind, MessageStream)>) -> Result<Streams, NetError> {
    let mut control = None;
    let mut input = None;
    let mut clipboard = None;
    let mut transfer = None;

    for (kind, stream) in opened {
        let slot = match kind {
            StreamKind::Control => &mut control,
            StreamKind::Input => &mut input,
            StreamKind::Clipboard => &mut clipboard,
            StreamKind::Transfer => &mut transfer,
        };
        if slot.is_some() {
            return Err(NetError::ProtocolViolation(format!(
                "peer opened more than one {kind:?} stream"
            )));
        }
        *slot = Some(stream);
    }

    Ok(Streams {
        control: control.ok_or_else(|| missing("control"))?,
        input: input.ok_or_else(|| missing("input"))?,
        clipboard: clipboard.ok_or_else(|| missing("clipboard"))?,
        transfer: transfer.ok_or_else(|| missing("transfer"))?,
    })
}

fn missing(kind: &str) -> NetError {
    NetError::ProtocolViolation(format!("peer never opened a {kind} stream"))
}
