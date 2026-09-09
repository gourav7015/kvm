//! Reads and writes [`kvm_protocol::Message`] frames on top of a raw QUIC
//! bidirectional stream.

use kvm_protocol::{DecodeStatus, Message, decode_frame, encode_frame};

use crate::error::NetError;

const READ_CHUNK_SIZE: usize = 4096;

/// One concern's stream, framed for exchanging [`Message`]s.
pub struct MessageStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    read_buf: Vec<u8>,
}

impl MessageStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self {
            send,
            recv,
            read_buf: Vec::new(),
        }
    }

    /// Encodes and sends one message.
    pub async fn send(&mut self, message: &Message) -> Result<(), NetError> {
        let frame = encode_frame(message)?;
        self.send
            .write_all(&frame)
            .await
            .map_err(|e| NetError::Transport(e.to_string()))
    }

    /// Reads and decodes the next message, buffering partial frames
    /// across reads.
    pub async fn recv(&mut self) -> Result<Message, NetError> {
        loop {
            match decode_frame(&self.read_buf)? {
                DecodeStatus::Ready { message, consumed } => {
                    self.read_buf.drain(..consumed);
                    return Ok(message);
                }
                DecodeStatus::Incomplete => {
                    let mut chunk = [0u8; READ_CHUNK_SIZE];
                    let n = self
                        .recv
                        .read(&mut chunk)
                        .await
                        .map_err(|e| NetError::Transport(e.to_string()))?;
                    match n {
                        None => return Err(NetError::ConnectionClosed),
                        Some(n) => self.read_buf.extend_from_slice(&chunk[..n]),
                    }
                }
            }
        }
    }
}
