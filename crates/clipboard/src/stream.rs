//! Bidirectional framed clipboard synchronization stream with SHA-256 echo suppression.

use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::watch;
use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::{CoreError, Result};
use veloxdesk_protocol::control::{Envelope, envelope::Msg};
use veloxdesk_protocol::framing::{decode_frame, encode_frame};

use crate::{ClipboardBackend, ClipboardItem};

async fn write_envelope(send: &mut SendStream, buf: &mut BytesMut, env: &Envelope) -> Result<()> {
    buf.clear();
    encode_frame(env, buf)?;
    send.write_all(buf).await?;
    send.flush().await?;
    Ok(())
}

async fn read_envelope(recv: &mut RecvStream, buf: &mut BytesMut) -> Result<Envelope> {
    loop {
        if let Some(msg) = decode_frame::<Envelope>(buf)? {
            return Ok(msg);
        }
        if buf.capacity() - buf.len() < 1024 {
            buf.reserve(4096);
        }
        let n = recv.read_buf(buf).await?;
        if n == 0 {
            return Err(CoreError::Closed);
        }
    }
}

/// Run bidirectional clipboard synchronization over one `StreamKind::Clipboard` bidi stream.
///
/// Polls `backend` every `poll_interval` for local changes (sending zstd-compressed
/// `ClipboardData` envelopes when `content_hash()` changes) and applies incoming remote
/// `ClipboardData` envelopes while recording their SHA-256 hash to prevent echo loops.
pub async fn run_clipboard_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    mut backend: Box<dyn ClipboardBackend>,
    poll_interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut tx_buf = BytesMut::with_capacity(1024);
    let mut rx_buf = BytesMut::with_capacity(4096);
    let mut last_hash: Option<[u8; 32]> = None;
    let mut tick = tokio::time::interval(poll_interval.max(Duration::from_millis(10)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            biased;
            res = shutdown.changed() => {
                if res.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            msg = read_envelope(&mut recv, &mut rx_buf) => {
                match msg {
                    Ok(Envelope { msg: Some(Msg::Clipboard(wire)) }) => {
                        match ClipboardItem::from_wire(&wire) {
                            Ok(item) => {
                                let hash = item.content_hash();
                                if last_hash != Some(hash) {
                                    // Record hash before writing so the next local poll won't echo it back.
                                    last_hash = Some(hash);
                                    if let Err(e) = backend.write(&item) {
                                        tracing::warn!("clipboard write failed: {e}");
                                    }
                                }
                            }
                            Err(e) => tracing::warn!("ignoring invalid clipboard payload: {e}"),
                        }
                    }
                    Ok(_) => {}
                    Err(CoreError::Closed) => break,
                    Err(e) => return Err(e),
                }
            }
            _ = tick.tick() => {
                match backend.read() {
                    Ok(Some(item)) => {
                        let hash = item.content_hash();
                        if last_hash != Some(hash) {
                            let compress = !matches!(item, ClipboardItem::ImagePng(_));
                            match item.to_wire(compress) {
                                Ok(wire) => {
                                    last_hash = Some(hash);
                                    let env = Envelope { msg: Some(Msg::Clipboard(wire)) };
                                    if let Err(e) = write_envelope(&mut send, &mut tx_buf, &env).await {
                                        if matches!(e, CoreError::Closed) {
                                            break;
                                        }
                                        return Err(e);
                                    }
                                }
                                Err(e) => tracing::warn!("clipboard encode failed: {e}"),
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => tracing::debug!("clipboard poll read failed: {e}"),
                }
            }
        }
    }

    let _ = send.shutdown().await;
    Ok(())
}
