//! # veloxdesk-clipboard
//!
//! Bidirectional clipboard synchronization (text, HTML, PNG images, file lists) with
//! zstd compression and SHA-256 echo-loop suppression.
//!
//! See `docs/SWARM.md` (M4 cross-crate APIs) for the binding public contract.

mod memory;
mod stream;
mod system;
#[cfg(windows)]
mod win_files;

use std::io::Read;

use bytes::Bytes;
use sha2::{Digest, Sha256};
use veloxdesk_core_traits::{CoreError, Result};
use veloxdesk_protocol::control::{ClipFormat, ClipboardData};

pub use memory::MemoryClipboard;
pub use stream::run_clipboard_stream;
pub use system::SystemClipboard;

/// Upper bound on decompressed clipboard payloads (matches ADR-004 16 MiB stream frame limit).
pub const MAX_CLIPBOARD_BYTES: usize = 16 * 1024 * 1024;

/// A normalized clipboard payload exchangeable across OSes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardItem {
    Text(String),
    Html(String),
    ImagePng(Bytes),
    FileList(Vec<String>),
}

impl ClipboardItem {
    /// Stable SHA-256 digest over format tag + raw payload bytes, used for echo-loop suppression.
    pub fn content_hash(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        match self {
            Self::Text(s) => {
                h.update([0u8]);
                h.update(s.as_bytes());
            }
            Self::Html(s) => {
                h.update([1u8]);
                h.update(s.as_bytes());
            }
            Self::ImagePng(b) => {
                h.update([3u8]);
                h.update(b);
            }
            Self::FileList(files) => {
                h.update([4u8]);
                for f in files {
                    h.update(f.as_bytes());
                    h.update([0u8]);
                }
            }
        }
        h.finalize().into()
    }

    /// Encode this item into a wire [`ClipboardData`] message, optionally compressing with zstd.
    pub fn to_wire(&self, compress: bool) -> Result<ClipboardData> {
        let (fmt, raw): (ClipFormat, Vec<u8>) = match self {
            Self::Text(s) => (ClipFormat::Text, s.as_bytes().to_vec()),
            Self::Html(s) => (ClipFormat::Html, s.as_bytes().to_vec()),
            Self::ImagePng(b) => (ClipFormat::ImagePng, b.to_vec()),
            Self::FileList(files) => (ClipFormat::FileList, files.join("\n").into_bytes()),
        };
        if raw.len() > MAX_CLIPBOARD_BYTES {
            return Err(CoreError::invalid(format!(
                "clipboard payload exceeds {} MiB limit ({} bytes)",
                MAX_CLIPBOARD_BYTES / (1024 * 1024),
                raw.len()
            )));
        }
        let (data, compressed) = if compress {
            let enc = zstd::bulk::compress(&raw, 3).map_err(|e| CoreError::platform(format!("zstd compress: {e}")))?;
            (Bytes::from(enc), true)
        } else {
            (Bytes::from(raw), false)
        };
        Ok(ClipboardData { format: fmt as i32, data, compressed })
    }

    /// Decode a wire [`ClipboardData`] message (decompressing zstd with size-bomb protection).
    pub fn from_wire(data: &ClipboardData) -> Result<Self> {
        let raw: Vec<u8> = if data.compressed {
            let dec = zstd::stream::read::Decoder::new(&data.data[..])
                .map_err(|e| CoreError::invalid(format!("zstd header: {e}")))?;
            let mut limited = dec.take((MAX_CLIPBOARD_BYTES + 1) as u64);
            let mut out = Vec::new();
            limited.read_to_end(&mut out).map_err(|e| CoreError::invalid(format!("zstd decompress: {e}")))?;
            if out.len() > MAX_CLIPBOARD_BYTES {
                return Err(CoreError::invalid("decompressed clipboard payload exceeds 16 MiB limit"));
            }
            out
        } else {
            if data.data.len() > MAX_CLIPBOARD_BYTES {
                return Err(CoreError::invalid("clipboard payload exceeds 16 MiB limit"));
            }
            data.data.to_vec()
        };

        let fmt = ClipFormat::try_from(data.format)
            .map_err(|_| CoreError::invalid(format!("unknown clipboard format {}", data.format)))?;
        match fmt {
            ClipFormat::Text | ClipFormat::Rtf => {
                let s = String::from_utf8(raw).map_err(|e| CoreError::invalid(format!("invalid UTF-8 text: {e}")))?;
                Ok(Self::Text(s))
            }
            ClipFormat::Html => {
                let s = String::from_utf8(raw).map_err(|e| CoreError::invalid(format!("invalid UTF-8 html: {e}")))?;
                Ok(Self::Html(s))
            }
            ClipFormat::ImagePng => Ok(Self::ImagePng(Bytes::from(raw))),
            ClipFormat::FileList => {
                let s = String::from_utf8(raw).map_err(|e| CoreError::invalid(format!("invalid UTF-8 file list: {e}")))?;
                let files = s.lines().filter(|l| !l.is_empty()).map(ToOwned::to_owned).collect();
                Ok(Self::FileList(files))
            }
        }
    }
}

/// Platform clipboard read/write abstraction.
pub trait ClipboardBackend: Send {
    fn read(&mut self) -> Result<Option<ClipboardItem>>;
    fn write(&mut self, item: &ClipboardItem) -> Result<()>;
}

/// Create the native OS clipboard backend (`SystemClipboard`).
pub fn create_system_clipboard() -> Result<Box<dyn ClipboardBackend>> {
    Ok(Box::new(SystemClipboard::new()?))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::watch;

    use super::*;

    #[test]
    fn wire_roundtrip_all_formats_and_zstd() {
        let items = [
            ClipboardItem::Text("Hello VeloxDesk! ".repeat(50)),
            ClipboardItem::Html("<b>Bold</b> &amp; <i>Italic</i>".into()),
            ClipboardItem::ImagePng(Bytes::from_static(b"\x89PNG\r\n\x1a\nfake-png-bytes")),
            ClipboardItem::FileList(vec![r"C:\Users\demo\a.txt".into(), r"C:\Users\demo\b.png".into()]),
        ];

        for item in &items {
            let raw = item.to_wire(false).expect("raw wire");
            assert!(!raw.compressed);
            assert_eq!(ClipboardItem::from_wire(&raw).expect("from raw"), *item);

            let comp = item.to_wire(true).expect("compressed wire");
            assert!(comp.compressed);
            assert_eq!(ClipboardItem::from_wire(&comp).expect("from comp"), *item);
        }

        // Verify zstd actually shrinks repetitive text.
        let text_raw = items[0].to_wire(false).unwrap();
        let text_comp = items[0].to_wire(true).unwrap();
        assert!(text_comp.data.len() < text_raw.data.len() / 2);

        // Distinct formats with same bytes have distinct hashes.
        let t = ClipboardItem::Text("abc".into());
        let h = ClipboardItem::Html("abc".into());
        assert_ne!(t.content_hash(), h.content_hash());
    }

    #[tokio::test]
    async fn bidirectional_stream_sync_and_zero_echo_loop() {
        let (a_to_b_w, a_to_b_r) = tokio::io::duplex(64 * 1024);
        let (b_to_a_w, b_to_a_r) = tokio::io::duplex(64 * 1024);

        let mem_a = MemoryClipboard::new();
        let mem_b = MemoryClipboard::new();
        let (tx_stop, rx_stop_a) = watch::channel(false);
        let rx_stop_b = tx_stop.subscribe();

        let task_a = tokio::spawn(run_clipboard_stream(
            Box::pin(a_to_b_w),
            Box::pin(b_to_a_r),
            Box::new(mem_a.clone()),
            Duration::from_millis(15),
            rx_stop_a,
        ));
        let task_b = tokio::spawn(run_clipboard_stream(
            Box::pin(b_to_a_w),
            Box::pin(a_to_b_r),
            Box::new(mem_b.clone()),
            Duration::from_millis(15),
            rx_stop_b,
        ));

        // 1. Copy text on A -> arrives on B without echoing back to A.
        let text = ClipboardItem::Text("VeloxDesk M4 Clipboard!".into());
        mem_a.set(Some(text.clone()));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(mem_b.get(), Some(text));
        assert_eq!(mem_b.write_count(), 1);
        assert_eq!(mem_a.write_count(), 0, "must not echo back to sender A");

        // 2. Copy PNG image on B -> arrives on A without echoing back to B.
        let img = ClipboardItem::ImagePng(Bytes::from_static(b"\x89PNG\r\n\x1a\n1234"));
        mem_b.set(Some(img.clone()));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(mem_a.get(), Some(img));
        assert_eq!(mem_a.write_count(), 1);
        assert_eq!(mem_b.write_count(), 1, "must not echo back to sender B");

        // 3. Copy file list on A -> arrives on B without extra writes after settling.
        let files = ClipboardItem::FileList(vec!["/tmp/one.bin".into(), "/tmp/two.txt".into()]);
        mem_a.set(Some(files.clone()));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(mem_b.get(), Some(files));
        assert_eq!(mem_b.write_count(), 2);
        assert_eq!(mem_a.write_count(), 1);

        let _ = tx_stop.send(true);
        task_a.await.unwrap().unwrap();
        task_b.await.unwrap().unwrap();
    }
}
