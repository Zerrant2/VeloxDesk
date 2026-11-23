//! Pure-Rust clipboard synchronization over `StreamKind::Clipboard` with `ruzstd` decompression
//! (16 MiB bomb protection) and SHA-256 echo-loop suppression.

use std::io::Read;

use bytes::{Bytes, BytesMut};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::{CoreError, Result};
use veloxdesk_protocol::control::{ClipFormat, ClipboardData, Envelope, envelope::Msg};
use veloxdesk_protocol::framing::{decode_frame, encode_frame};

/// Upper bound on clipboard payloads (16 MiB — matches ADR-004 and `veloxdesk-clipboard`).
pub const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;

/// Stable SHA-256 digest for text payloads (matches `veloxdesk-clipboard`'s `ClipboardItem::Text` tag).
pub fn text_hash(text: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update([0u8]);
    h.update(text.as_bytes());
    h.finalize().into()
}

/// Decode a wire [`ClipboardData`] into plain UTF-8 text (handling zstd decompression with a
/// 16 MiB size-bomb limit). Non-text formats (`Html`, `ImagePng`, `FileList`) return `Ok(None)`.
pub fn decode_clipboard_text(wire: &ClipboardData) -> Result<Option<String>> {
    let fmt = ClipFormat::try_from(wire.format)
        .map_err(|_| CoreError::invalid(format!("unknown clipboard format {}", wire.format)))?;
    if !matches!(fmt, ClipFormat::Text | ClipFormat::Rtf) {
        return Ok(None);
    }

    let raw = if wire.compressed {
        decompress_zstd_bounded(&wire.data, MAX_CLIPBOARD_BYTES)?
    } else {
        if wire.data.len() > MAX_CLIPBOARD_BYTES {
            return Err(CoreError::invalid("clipboard payload exceeds 16 MiB limit"));
        }
        wire.data.to_vec()
    };

    let text = String::from_utf8(raw).map_err(|e| CoreError::invalid(format!("invalid UTF-8 clipboard text: {e}")))?;
    Ok(Some(text))
}

/// Decompress zstd data using pure-Rust `ruzstd`, enforcing an upper bound of `max_bytes`
/// to prevent decompression bombs.
pub fn decompress_zstd_bounded(compressed: &[u8], max_bytes: usize) -> Result<Vec<u8>> {
    let dec = ruzstd::decoding::StreamingDecoder::new(compressed)
        .map_err(|e| CoreError::invalid(format!("zstd header error: {e}")))?;
    let mut limited = dec.take((max_bytes + 1) as u64);
    let mut out = Vec::new();
    limited
        .read_to_end(&mut out)
        .map_err(|e| CoreError::invalid(format!("zstd decompress error: {e}")))?;
    if out.len() > max_bytes {
        return Err(CoreError::invalid(format!(
            "decompressed clipboard payload exceeds {} MiB limit",
            max_bytes / (1024 * 1024)
        )));
    }
    Ok(out)
}

/// Encode local text into an uncompressed wire [`ClipboardData`] envelope payload.
pub fn encode_clipboard_text(text: &str) -> Result<ClipboardData> {
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Err(CoreError::invalid(format!(
            "clipboard text exceeds {} MiB limit ({} bytes)",
            MAX_CLIPBOARD_BYTES / (1024 * 1024),
            text.len()
        )));
    }
    Ok(ClipboardData {
        format: ClipFormat::Text as i32,
        data: Bytes::copy_from_slice(text.as_bytes()),
        compressed: false,
    })
}

/// Tracks the last synced clipboard hash to suppress bidirectional echo loops.
#[derive(Debug, Default)]
pub struct EchoTracker {
    last_hash: Option<[u8; 32]>,
}

impl EchoTracker {
    pub fn new() -> Self {
        Self { last_hash: None }
    }

    /// Returns `true` if `text` differs from the last synced value (and records its hash).
    pub fn accept_if_changed(&mut self, text: &str) -> bool {
        let hash = text_hash(text);
        if self.last_hash == Some(hash) {
            return false;
        }
        self.last_hash = Some(hash);
        true
    }
}

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

/// Run the Android bidirectional text clipboard stream until `stop` becomes `true` or the stream closes.
pub async fn run_android_clipboard_stream<F>(
    mut send: SendStream,
    mut recv: RecvStream,
    mut out_rx: mpsc::UnboundedReceiver<String>,
    on_remote_text: F,
    mut stop: watch::Receiver<bool>,
) -> Result<()>
where
    F: Fn(&str) + Send + Sync,
{
    let mut tx_buf = BytesMut::with_capacity(1024);
    let mut rx_buf = BytesMut::with_capacity(4096);
    let mut tracker = EchoTracker::new();

    loop {
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            biased;
            res = stop.changed() => {
                if res.is_err() || *stop.borrow() {
                    break;
                }
            }
            msg = read_envelope(&mut recv, &mut rx_buf) => {
                match msg {
                    Ok(Envelope { msg: Some(Msg::Clipboard(wire)) }) => {
                        match decode_clipboard_text(&wire) {
                            Ok(Some(text)) => {
                                if tracker.accept_if_changed(&text) {
                                    on_remote_text(&text);
                                }
                            }
                            Ok(None) => {}
                            Err(e) => tracing::warn!("ignoring invalid clipboard payload: {e}"),
                        }
                    }
                    Ok(_) => {}
                    Err(CoreError::Closed) => break,
                    Err(e) => return Err(e),
                }
            }
            out = out_rx.recv() => {
                match out {
                    Some(text) => {
                        if tracker.accept_if_changed(&text) {
                            match encode_clipboard_text(&text) {
                                Ok(wire) => {
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
                    None => break,
                }
            }
        }
    }

    let _ = send.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_suppression_tracks_changes() {
        let mut tracker = EchoTracker::new();
        assert!(tracker.accept_if_changed("hello"));
        assert!(!tracker.accept_if_changed("hello"));
        assert!(tracker.accept_if_changed("world"));
        assert!(!tracker.accept_if_changed("world"));
        assert!(tracker.accept_if_changed("hello"));
    }

    #[test]
    fn encode_decode_uncompressed_and_non_text_ignored() {
        let wire = encode_clipboard_text("Привет VeloxDesk!").unwrap();
        assert!(!wire.compressed);
        assert_eq!(decode_clipboard_text(&wire).unwrap().as_deref(), Some("Привет VeloxDesk!"));

        let png = ClipboardData {
            format: ClipFormat::ImagePng as i32,
            data: Bytes::from_static(b"\x89PNG"),
            compressed: false,
        };
        assert_eq!(decode_clipboard_text(&png).unwrap(), None);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn ruzstd_decompresses_desktop_zstd_and_blocks_size_bomb() {
        use veloxdesk_clipboard::ClipboardItem;

        let text = "VeloxDesk Android clipboard sync! ".repeat(200);
        let wire = ClipboardItem::Text(text.clone()).to_wire(true).unwrap();
        assert!(wire.compressed);
        let decoded = decode_clipboard_text(&wire).unwrap();
        assert_eq!(decoded.as_deref(), Some(text.as_str()));

        // Size bomb test: compress > limit repetitive zeros into a tiny zstd payload,
        // verify bounded decompressor rejects it cleanly without allocating past the cap.
        let bomb_raw = vec![b'A'; 64 * 1024];
        let bomb_wire = ClipboardItem::Text(String::from_utf8(bomb_raw).unwrap()).to_wire(true).unwrap();
        assert!(bomb_wire.data.len() < 1024);
        assert!(decompress_zstd_bounded(&bomb_wire.data, 16 * 1024).is_err());
        assert_eq!(decompress_zstd_bounded(&bomb_wire.data, 64 * 1024).unwrap().len(), 64 * 1024);
    }
}
