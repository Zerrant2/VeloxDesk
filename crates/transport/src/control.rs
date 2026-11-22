//! Length-prefixed protobuf control streams (`u32 LE len | Envelope`, ADR-004).

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::{CoreError, Result};
use veloxdesk_protocol::control::Envelope;
use veloxdesk_protocol::framing::{decode_frame, encode_frame};

/// Writing half of a framed control stream.
pub struct ControlSender {
    send: SendStream,
    buf: BytesMut,
}

/// Reading half of a framed control stream.
pub struct ControlReceiver {
    recv: RecvStream,
    buf: BytesMut,
}

/// Split a (bi-directional) stream pair into framed sender / receiver halves.
pub fn framed(send: SendStream, recv: RecvStream) -> (ControlSender, ControlReceiver) {
    (ControlSender::new(send), ControlReceiver::new(recv))
}

impl ControlSender {
    pub fn new(send: SendStream) -> Self {
        Self { send, buf: BytesMut::with_capacity(1024) }
    }

    /// Send one message and flush it.
    pub async fn send(&mut self, msg: &Envelope) -> Result<()> {
        self.buf.clear();
        encode_frame(msg, &mut self.buf)?;
        self.send.write_all(&self.buf).await?;
        self.send.flush().await?;
        Ok(())
    }

    /// Gracefully finish the stream.
    pub async fn finish(&mut self) -> Result<()> {
        self.send.shutdown().await?;
        Ok(())
    }
}

impl ControlReceiver {
    pub fn new(recv: RecvStream) -> Self {
        Self { recv, buf: BytesMut::with_capacity(4096) }
    }

    /// Receive the next message. `Err(Closed)` when the peer finished the stream.
    pub async fn recv(&mut self) -> Result<Envelope> {
        loop {
            if let Some(msg) = decode_frame::<Envelope>(&mut self.buf)? {
                return Ok(msg);
            }
            if self.buf.capacity() - self.buf.len() < 1024 {
                self.buf.reserve(4096);
            }
            let n = self.recv.read_buf(&mut self.buf).await?;
            if n == 0 {
                return Err(CoreError::Closed);
            }
        }
    }
}
