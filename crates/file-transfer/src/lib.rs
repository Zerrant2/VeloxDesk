//! # veloxdesk-file-transfer
//!
//! Chunked, resumable, zstd-compressed file transfer with full-file SHA-256 verification
//! over `StreamKind::FileTransfer` QUIC streams (ARCHITECTURE §4.3, M4).

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::UNIX_EPOCH;

use bytes::{Bytes, BytesMut};
use prost::Message;
use sha2::{Digest, Sha256};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    Envelope, FileAck, FileChunk, FileEntry, FileListing, FileOp, FileRequest, envelope::Msg,
};
use veloxdesk_protocol::framing::{MAX_FRAME_LEN, encode_frame};

/// Default chunk size (256 KiB).
pub const DEFAULT_CHUNK_SIZE: usize = 256 * 1024;
/// Maximum allowed uncompressed chunk size accepted from a peer (8 MiB, DoS guard).
const MAX_CHUNK_SIZE: usize = 8 * 1024 * 1024;
/// Zstd compression level (fast & good ratio for LAN/WAN).
const ZSTD_LEVEL: i32 = 3;
/// Sentinel `FileRequest::offset` asking the host to auto-resume an upload from its
/// existing partial file size.
const AUTO_RESUME_OFFSET: u64 = u64::MAX;

/// Progress callback invoked as `(bytes_done, total_bytes)`.
pub type ProgressCb = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// Host-side configuration for file transfer requests.
#[derive(Debug, Clone)]
pub struct FileTransferConfig {
    /// Optional jail root directory. When `Some`, all paths are confined inside `root_dir`
    /// with path-traversal (`..` / drive prefix / escaping symlink) protection.
    pub root_dir: Option<PathBuf>,
    /// Uncompressed chunk size for streaming transfers.
    pub chunk_size: usize,
    /// Maximum concurrent transfers for multi-file helpers.
    pub max_parallel: usize,
    /// Allow zstd chunk compression when requested by the client.
    pub compress: bool,
}

impl Default for FileTransferConfig {
    fn default() -> Self {
        Self {
            root_dir: None,
            chunk_size: DEFAULT_CHUNK_SIZE,
            max_parallel: 4,
            compress: true,
        }
    }
}

/// Summary returned after a verified upload or download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferReport {
    /// Uncompressed payload bytes sent/received in this transfer call (excluding already-present prefix).
    pub bytes_transferred: u64,
    /// Final total file size in bytes.
    pub total_bytes: u64,
    /// Byte offset resumed from (`0` if started from scratch).
    pub resumed_from: u64,
    /// Wire payload bytes sent/received (after optional zstd compression).
    pub wire_bytes: u64,
    /// Verified full-file SHA-256 digest.
    pub sha256: [u8; 32],
}

// ───────────────────────────── Host server ─────────────────────────────

/// Host side: serve one or more `FileRequest`s (`ListDir/Stat/Download/Upload/Delete/Mkdir/Rename`)
/// on a `StreamKind::FileTransfer` bidirectional stream until the stream closes.
pub async fn serve_file_stream(mut send: SendStream, mut recv: RecvStream, cfg: &FileTransferConfig) -> Result<()> {
    loop {
        let env = match read_envelope(&mut recv).await {
            Ok(env) => env,
            Err(CoreError::Closed) => return Ok(()),
            Err(e) => return Err(e),
        };
        let Some(Msg::FileRequest(req)) = env.msg else {
            return Err(CoreError::invalid("expected FileRequest on FileTransfer stream"));
        };
        let op = FileOp::try_from(req.op).unwrap_or(FileOp::ListDir);
        match op {
            FileOp::ListDir => handle_list_dir(&mut send, &req, cfg).await?,
            FileOp::Stat => handle_stat(&mut send, &req, cfg).await?,
            FileOp::Mkdir => handle_mkdir(&mut send, &req, cfg).await?,
            FileOp::Delete => handle_delete(&mut send, &req, cfg).await?,
            FileOp::Rename => handle_rename(&mut send, &req, cfg).await?,
            FileOp::Download => handle_download(&mut send, &req, cfg).await?,
            FileOp::Upload => handle_upload(&mut send, &mut recv, &req, cfg).await?,
        }
    }
}

async fn handle_list_dir(send: &mut SendStream, req: &FileRequest, cfg: &FileTransferConfig) -> Result<()> {
    let dir = match resolve_path(cfg, &req.path, false) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    let mut rd = match fs::read_dir(&dir).await {
        Ok(rd) => rd,
        Err(e) => return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    };
    let mut entries = Vec::new();
    while let Ok(Some(ent)) = rd.next_entry().await {
        let name = ent.file_name().to_string_lossy().into_owned();
        if let Ok(meta) = ent.metadata().await {
            entries.push(meta_to_entry(name, &meta));
        }
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    let env: Envelope = Msg::FileListing(FileListing { id: req.id, path: req.path.clone(), entries }).into();
    write_envelope(send, &env).await
}

async fn handle_stat(send: &mut SendStream, req: &FileRequest, cfg: &FileTransferConfig) -> Result<()> {
    let path = match resolve_path(cfg, &req.path, false) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    match fs::metadata(&path).await {
        Ok(meta) => {
            let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let entry = meta_to_entry(name, &meta);
            let env: Envelope =
                Msg::FileListing(FileListing { id: req.id, path: req.path.clone(), entries: vec![entry] }).into();
            write_envelope(send, &env).await
        }
        Err(e) => send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    }
}

async fn handle_mkdir(send: &mut SendStream, req: &FileRequest, cfg: &FileTransferConfig) -> Result<()> {
    let path = match resolve_path(cfg, &req.path, true) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    match fs::create_dir_all(&path).await {
        Ok(()) => send_ack(send, req.id, 0, String::new(), Vec::new()).await,
        Err(e) => send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    }
}

async fn handle_delete(send: &mut SendStream, req: &FileRequest, cfg: &FileTransferConfig) -> Result<()> {
    let path = match resolve_path(cfg, &req.path, false) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    if let Some(root) = &cfg.root_dir
        && let (Ok(cr), Ok(cp)) = (std::fs::canonicalize(root), std::fs::canonicalize(&path))
        && cr == cp
    {
        return send_ack(send, req.id, 0, "cannot delete root_dir".into(), Vec::new()).await;
    }
    let meta = match fs::metadata(&path).await {
        Ok(m) => m,
        Err(e) => return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    };
    let res = if meta.is_dir() { fs::remove_dir_all(&path).await } else { fs::remove_file(&path).await };
    match res {
        Ok(()) => send_ack(send, req.id, 0, String::new(), Vec::new()).await,
        Err(e) => send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    }
}

async fn handle_rename(send: &mut SendStream, req: &FileRequest, cfg: &FileTransferConfig) -> Result<()> {
    let from = match resolve_path(cfg, &req.path, false) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    let to = match resolve_path(cfg, &req.dest_path, true) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    match fs::rename(&from, &to).await {
        Ok(()) => send_ack(send, req.id, 0, String::new(), Vec::new()).await,
        Err(e) => send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    }
}

async fn handle_download(send: &mut SendStream, req: &FileRequest, cfg: &FileTransferConfig) -> Result<()> {
    let path = match resolve_path(cfg, &req.path, false) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    let mut file = match File::open(&path).await {
        Ok(f) => f,
        Err(e) => return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    };
    let total_size = match file.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        Ok(_) => return send_ack(send, req.id, 0, "path is not a regular file".into(), Vec::new()).await,
        Err(e) => return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    };
    if req.offset > total_size {
        return send_ack(send, req.id, total_size, "resume offset exceeds file size".into(), Vec::new()).await;
    }

    // Initial Ack announces accepted download + total file size in `offset`.
    send_ack(send, req.id, total_size, String::new(), Vec::new()).await?;

    let chunk_size = cfg.chunk_size.clamp(4096, MAX_CHUNK_SIZE);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; chunk_size];

    // Hash the skipped prefix (0..req.offset) so final digest always covers the full file.
    hash_prefix(&mut file, req.offset, &mut hasher, &mut buf).await?;

    let mut offset = req.offset;
    let use_zstd = req.compress && cfg.compress;
    if offset == total_size {
        let env: Envelope = Msg::FileChunk(FileChunk {
            id: req.id,
            offset,
            data: Bytes::new(),
            compressed: false,
            last: true,
        })
        .into();
        write_envelope(send, &env).await?;
    } else {
        while offset < total_size {
            let want = ((total_size - offset) as usize).min(chunk_size);
            let n = read_up_to(&mut file, &mut buf[..want]).await?;
            if n == 0 {
                break;
            }
            let plain = &buf[..n];
            hasher.update(plain);
            let (payload, compressed) = maybe_compress(plain, use_zstd)?;
            let next_offset = offset + n as u64;
            let last = next_offset >= total_size;
            let env: Envelope = Msg::FileChunk(FileChunk {
                id: req.id,
                offset,
                data: payload,
                compressed,
                last,
            })
            .into();
            write_envelope(send, &env).await?;
            offset = next_offset;
        }
    }

    let digest = hasher.finalize().to_vec();
    send_ack(send, req.id, total_size, String::new(), digest).await
}

async fn handle_upload(
    send: &mut SendStream,
    recv: &mut RecvStream,
    req: &FileRequest,
    cfg: &FileTransferConfig,
) -> Result<()> {
    let path = match resolve_path(cfg, &req.path, true) {
        Ok(p) => p,
        Err(msg) => return send_ack(send, req.id, 0, msg, Vec::new()).await,
    };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = fs::create_dir_all(parent).await
    {
        return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await;
    }

    let existing_len = fs::metadata(&path).await.ok().filter(|m| m.is_file()).map_or(0, |m| m.len());
    let start_offset = if req.offset == AUTO_RESUME_OFFSET {
        if existing_len <= req.size { existing_len } else { 0 }
    } else if req.offset > 0 {
        if existing_len >= req.offset {
            req.offset
        } else {
            return send_ack(send, req.id, existing_len, "requested upload resume offset not present".into(), Vec::new())
                .await;
        }
    } else {
        0
    };

    let mut opts = OpenOptions::new();
    opts.create(true).write(true).read(true);
    if start_offset == 0 {
        opts.truncate(true);
    }
    let mut file = match opts.open(&path).await {
        Ok(f) => f,
        Err(e) => return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await,
    };
    if start_offset > 0 && existing_len > start_offset {
        if let Err(e) = file.set_len(start_offset).await {
            return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await;
        }
    }
    if let Err(e) = file.seek(io::SeekFrom::Start(start_offset)).await {
        return send_ack(send, req.id, 0, e.to_string(), Vec::new()).await;
    }

    // Tell the client the accepted start offset.
    send_ack(send, req.id, start_offset, String::new(), Vec::new()).await?;

    let mut written_end = start_offset;
    loop {
        let env = read_envelope(recv).await?;
        let Some(Msg::FileChunk(chunk)) = env.msg else {
            return send_ack(send, req.id, written_end, "expected FileChunk during upload".into(), Vec::new()).await;
        };
        if chunk.id != req.id {
            return send_ack(send, req.id, written_end, "mismatched transfer id".into(), Vec::new()).await;
        }
        let plain = if chunk.compressed {
            decompress_chunk(&chunk.data)?
        } else {
            chunk.data.to_vec()
        };
        if !plain.is_empty() {
            if chunk.offset != written_end {
                file.seek(io::SeekFrom::Start(chunk.offset)).await?;
            }
            file.write_all(&plain).await?;
            file.flush().await?;
            written_end = written_end.max(chunk.offset + plain.len() as u64);
        }
        if chunk.last {
            break;
        }
    }

    file.flush().await?;
    let digest = sha256_file(&path).await?;
    send_ack(send, req.id, written_end, String::new(), digest.to_vec()).await
}

// ───────────────────────────── Viewer client ─────────────────────────────

/// Viewer side: high-level client opening `StreamKind::FileTransfer` streams for directory
/// management and chunked resumable zstd transfers with SHA-256 verification.
pub struct FileTransferClient {
    ch: Arc<dyn TransportChannel>,
    chunk_size: usize,
    next_id: AtomicU32,
}

impl FileTransferClient {
    pub fn new(ch: Arc<dyn TransportChannel>) -> Self {
        Self { ch, chunk_size: DEFAULT_CHUNK_SIZE, next_id: AtomicU32::new(1) }
    }

    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size.clamp(4096, MAX_CHUNK_SIZE);
        self
    }

    fn alloc_id(&self) -> u32 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>> {
        let (mut send, mut recv) = self.ch.open_bi(StreamKind::FileTransfer).await?;
        let id = self.alloc_id();
        send_request(&mut send, FileRequest { id, op: FileOp::ListDir as i32, path: path.into(), ..Default::default() })
            .await?;
        let reply = read_envelope(&mut recv).await?;
        let _ = send.shutdown().await;
        match reply.msg {
            Some(Msg::FileListing(l)) => Ok(l.entries),
            Some(Msg::FileAck(a)) if !a.error.is_empty() => Err(ft_err(a.error)),
            _ => Err(CoreError::invalid("unexpected response to ListDir")),
        }
    }

    pub async fn stat(&self, path: &str) -> Result<FileEntry> {
        let (mut send, mut recv) = self.ch.open_bi(StreamKind::FileTransfer).await?;
        let id = self.alloc_id();
        send_request(&mut send, FileRequest { id, op: FileOp::Stat as i32, path: path.into(), ..Default::default() })
            .await?;
        let reply = read_envelope(&mut recv).await?;
        let _ = send.shutdown().await;
        match reply.msg {
            Some(Msg::FileListing(mut l)) if !l.entries.is_empty() => Ok(l.entries.remove(0)),
            Some(Msg::FileAck(a)) if !a.error.is_empty() => Err(ft_err(a.error)),
            _ => Err(CoreError::invalid("unexpected response to Stat")),
        }
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        self.simple_op(FileOp::Mkdir, path, "").await
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        self.simple_op(FileOp::Delete, path, "").await
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.simple_op(FileOp::Rename, from, to).await
    }

    async fn simple_op(&self, op: FileOp, path: &str, dest_path: &str) -> Result<()> {
        let (mut send, mut recv) = self.ch.open_bi(StreamKind::FileTransfer).await?;
        let id = self.alloc_id();
        send_request(
            &mut send,
            FileRequest {
                id,
                op: op as i32,
                path: path.into(),
                dest_path: dest_path.into(),
                ..Default::default()
            },
        )
        .await?;
        let ack = read_ack(&mut recv).await?;
        let _ = send.shutdown().await;
        if !ack.error.is_empty() {
            return Err(ft_err(ack.error));
        }
        Ok(())
    }

    /// Download `remote_path` to `local_path` with optional resume, zstd compression, progress
    /// reporting, and full-file SHA-256 verification.
    pub async fn download(
        &self,
        remote_path: &str,
        local_path: &Path,
        resume: bool,
        compress: bool,
        on_progress: Option<ProgressCb>,
    ) -> Result<TransferReport> {
        let existing_len = if resume {
            fs::metadata(local_path).await.ok().filter(|m| m.is_file()).map_or(0, |m| m.len())
        } else {
            0
        };
        if let Some(parent) = local_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).await?;
        }

        let (mut send, mut recv) = self.ch.open_bi(StreamKind::FileTransfer).await?;
        let id = self.alloc_id();
        send_request(
            &mut send,
            FileRequest {
                id,
                op: FileOp::Download as i32,
                path: remote_path.into(),
                offset: existing_len,
                compress,
                ..Default::default()
            },
        )
        .await?;

        let init = read_ack(&mut recv).await?;
        if !init.error.is_empty() {
            return Err(ft_err(init.error));
        }
        let total_bytes = init.offset;

        let mut opts = OpenOptions::new();
        opts.create(true).write(true);
        if existing_len == 0 {
            opts.truncate(true);
        }
        let mut file = opts.open(local_path).await?;
        file.seek(io::SeekFrom::Start(existing_len)).await?;

        let mut cursor = existing_len;
        let mut bytes_transferred = 0u64;
        let mut wire_bytes = 0u64;
        if let Some(cb) = &on_progress {
            cb(cursor, total_bytes);
        }

        loop {
            let env = read_envelope(&mut recv).await?;
            match env.msg {
                Some(Msg::FileChunk(chunk)) => {
                    wire_bytes += chunk.data.len() as u64;
                    let plain = if chunk.compressed {
                        decompress_chunk(&chunk.data)?
                    } else {
                        chunk.data.to_vec()
                    };
                    if !plain.is_empty() {
                        if chunk.offset != cursor {
                            file.seek(io::SeekFrom::Start(chunk.offset)).await?;
                            cursor = chunk.offset;
                        }
                        file.write_all(&plain).await?;
                        file.flush().await?;
                        cursor += plain.len() as u64;
                        bytes_transferred += plain.len() as u64;
                        if let Some(cb) = &on_progress {
                            cb(cursor, total_bytes);
                        }
                    }
                    if chunk.last {
                        break;
                    }
                }
                Some(Msg::FileAck(ack)) if !ack.error.is_empty() => return Err(ft_err(ack.error)),
                _ => return Err(CoreError::invalid("unexpected message during download")),
            }
        }

        let final_ack = read_ack(&mut recv).await?;
        let _ = send.shutdown().await;
        if !final_ack.error.is_empty() {
            return Err(ft_err(final_ack.error));
        }
        file.flush().await?;
        drop(file);

        let expected: [u8; 32] = final_ack
            .digest
            .as_slice()
            .try_into()
            .map_err(|_| ft_err("missing or invalid SHA-256 digest from host"))?;
        let actual = sha256_file(local_path).await?;
        if actual != expected {
            return Err(ft_err("downloaded file SHA-256 digest mismatch"));
        }

        Ok(TransferReport {
            bytes_transferred,
            total_bytes,
            resumed_from: existing_len,
            wire_bytes,
            sha256: actual,
        })
    }

    /// Upload `local_path` to `remote_path` with optional auto-resume, zstd compression,
    /// progress reporting, and full-file SHA-256 verification.
    pub async fn upload(
        &self,
        local_path: &Path,
        remote_path: &str,
        resume: bool,
        compress: bool,
        on_progress: Option<ProgressCb>,
    ) -> Result<TransferReport> {
        let mut file = File::open(local_path).await?;
        let meta = file.metadata().await?;
        if !meta.is_file() {
            return Err(CoreError::invalid("upload source is not a regular file"));
        }
        let total_bytes = meta.len();

        let (mut send, mut recv) = self.ch.open_bi(StreamKind::FileTransfer).await?;
        let id = self.alloc_id();
        send_request(
            &mut send,
            FileRequest {
                id,
                op: FileOp::Upload as i32,
                path: remote_path.into(),
                offset: if resume { AUTO_RESUME_OFFSET } else { 0 },
                size: total_bytes,
                compress,
                ..Default::default()
            },
        )
        .await?;

        let init = read_ack(&mut recv).await?;
        if !init.error.is_empty() {
            return Err(ft_err(init.error));
        }
        let start_offset = init.offset;
        if start_offset > total_bytes {
            return Err(ft_err("host returned upload start_offset > local file size"));
        }

        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; self.chunk_size];
        hash_prefix(&mut file, start_offset, &mut hasher, &mut buf).await?;

        let mut offset = start_offset;
        let mut bytes_transferred = 0u64;
        let mut wire_bytes = 0u64;
        if let Some(cb) = &on_progress {
            cb(offset, total_bytes);
        }

        if offset == total_bytes {
            let env: Envelope = Msg::FileChunk(FileChunk {
                id,
                offset,
                data: Bytes::new(),
                compressed: false,
                last: true,
            })
            .into();
            write_envelope(&mut send, &env).await?;
        } else {
            while offset < total_bytes {
                let want = ((total_bytes - offset) as usize).min(self.chunk_size);
                let n = read_up_to(&mut file, &mut buf[..want]).await?;
                if n == 0 {
                    break;
                }
                let plain = &buf[..n];
                hasher.update(plain);
                let (payload, compressed) = maybe_compress(plain, compress)?;
                wire_bytes += payload.len() as u64;
                let next_offset = offset + n as u64;
                let last = next_offset >= total_bytes;
                let env: Envelope = Msg::FileChunk(FileChunk {
                    id,
                    offset,
                    data: payload,
                    compressed,
                    last,
                })
                .into();
                write_envelope(&mut send, &env).await?;
                offset = next_offset;
                bytes_transferred += n as u64;
                if let Some(cb) = &on_progress {
                    cb(offset, total_bytes);
                }
            }
        }

        let final_ack = read_ack(&mut recv).await?;
        let _ = send.shutdown().await;
        if !final_ack.error.is_empty() {
            return Err(ft_err(final_ack.error));
        }

        let local_sha: [u8; 32] = hasher.finalize().into();
        let remote_sha: [u8; 32] = final_ack
            .digest
            .as_slice()
            .try_into()
            .map_err(|_| ft_err("missing or invalid SHA-256 digest from host"))?;
        if local_sha != remote_sha {
            return Err(ft_err("uploaded file SHA-256 digest mismatch"));
        }

        Ok(TransferReport {
            bytes_transferred,
            total_bytes,
            resumed_from: start_offset,
            wire_bytes,
            sha256: local_sha,
        })
    }

    /// Parallel multi-file download helper bounded by `max_parallel`.
    pub async fn download_many(
        self: &Arc<Self>,
        items: Vec<(String, PathBuf)>,
        resume: bool,
        compress: bool,
        max_parallel: usize,
    ) -> Result<Vec<TransferReport>> {
        let sem = Arc::new(Semaphore::new(max_parallel.max(1)));
        let mut handles = Vec::with_capacity(items.len());
        for (remote, local) in items {
            let permit = sem.clone().acquire_owned().await.map_err(|_| CoreError::Closed)?;
            let client = self.clone();
            handles.push(tokio::spawn(async move {
                let _p = permit;
                client.download(&remote, &local, resume, compress, None).await
            }));
        }
        let mut reports = Vec::with_capacity(handles.len());
        for h in handles {
            reports.push(h.await.map_err(|e| ft_err(format!("transfer task failed: {e}")))??);
        }
        Ok(reports)
    }

    /// Parallel multi-file upload helper bounded by `max_parallel`.
    pub async fn upload_many(
        self: &Arc<Self>,
        items: Vec<(PathBuf, String)>,
        resume: bool,
        compress: bool,
        max_parallel: usize,
    ) -> Result<Vec<TransferReport>> {
        let sem = Arc::new(Semaphore::new(max_parallel.max(1)));
        let mut handles = Vec::with_capacity(items.len());
        for (local, remote) in items {
            let permit = sem.clone().acquire_owned().await.map_err(|_| CoreError::Closed)?;
            let client = self.clone();
            handles.push(tokio::spawn(async move {
                let _p = permit;
                client.upload(&local, &remote, resume, compress, None).await
            }));
        }
        let mut reports = Vec::with_capacity(handles.len());
        for h in handles {
            reports.push(h.await.map_err(|e| ft_err(format!("transfer task failed: {e}")))??);
        }
        Ok(reports)
    }
}

// ───────────────────────────── helpers ─────────────────────────────

fn ft_err(msg: impl Into<String>) -> CoreError {
    CoreError::other(io::Error::other(msg.into()))
}

fn meta_to_entry(name: String, meta: &std::fs::Metadata) -> FileEntry {
    let modified_unix = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    FileEntry {
        name,
        is_dir: meta.is_dir(),
        size: if meta.is_file() { meta.len() } else { 0 },
        modified_unix,
    }
}

/// Resolve a wire path against `cfg.root_dir`, rejecting path-traversal attempts.
fn resolve_path(cfg: &FileTransferConfig, raw: &str, allow_nonexistent: bool) -> std::result::Result<PathBuf, String> {
    let Some(root) = &cfg.root_dir else {
        if raw.is_empty() {
            return Ok(PathBuf::from("."));
        }
        return Ok(PathBuf::from(raw));
    };

    let canon_root = std::fs::canonicalize(root).map_err(|e| format!("invalid root_dir: {e}"))?;
    let mut rel = PathBuf::new();
    for comp in Path::new(raw).components() {
        match comp {
            Component::Normal(seg) => rel.push(seg),
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err("path traversal outside root_dir is forbidden".into());
            }
        }
    }
    let candidate = canon_root.join(&rel);
    if candidate.exists() {
        let canon = std::fs::canonicalize(&candidate).map_err(|e| e.to_string())?;
        if !canon.starts_with(&canon_root) {
            return Err("symlink escapes root_dir".into());
        }
        return Ok(canon);
    }
    if !allow_nonexistent {
        return Err(format!("path not found: {raw}"));
    }
    // Check nearest existing ancestor against symlink escape.
    let mut anc = candidate.parent();
    while let Some(a) = anc {
        if a.exists() {
            let canon_anc = std::fs::canonicalize(a).map_err(|e| e.to_string())?;
            if !canon_anc.starts_with(&canon_root) {
                return Err("target ancestor escapes root_dir".into());
            }
            break;
        }
        anc = a.parent();
    }
    Ok(candidate)
}

fn maybe_compress(plain: &[u8], enable: bool) -> Result<(Bytes, bool)> {
    if !enable || plain.is_empty() {
        return Ok((Bytes::copy_from_slice(plain), false));
    }
    let compressed = zstd::bulk::compress(plain, ZSTD_LEVEL).map_err(|e| ft_err(format!("zstd compress: {e}")))?;
    if compressed.len() < plain.len() {
        Ok((Bytes::from(compressed), true))
    } else {
        Ok((Bytes::copy_from_slice(plain), false))
    }
}

fn decompress_chunk(data: &[u8]) -> Result<Vec<u8>> {
    zstd::bulk::decompress(data, MAX_CHUNK_SIZE).map_err(|e| ft_err(format!("zstd decompress: {e}")))
}

async fn read_up_to(file: &mut File, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

async fn hash_prefix(file: &mut File, mut remaining: u64, hasher: &mut Sha256, buf: &mut [u8]) -> Result<()> {
    while remaining > 0 {
        let want = (remaining as usize).min(buf.len());
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(ft_err("unexpected EOF while hashing resume prefix"));
        }
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok(())
}

async fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut f = File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().into())
}

async fn send_request(send: &mut SendStream, req: FileRequest) -> Result<()> {
    let env: Envelope = Msg::FileRequest(req).into();
    write_envelope(send, &env).await
}

async fn send_ack(send: &mut SendStream, id: u32, offset: u64, error: String, digest: Vec<u8>) -> Result<()> {
    let env: Envelope = Msg::FileAck(FileAck { id, offset, error, digest }).into();
    write_envelope(send, &env).await
}

async fn write_envelope(send: &mut SendStream, env: &Envelope) -> Result<()> {
    let mut buf = BytesMut::with_capacity(256);
    encode_frame(env, &mut buf)?;
    send.write_all(&buf).await?;
    send.flush().await?;
    Ok(())
}

async fn read_envelope(recv: &mut RecvStream) -> Result<Envelope> {
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(CoreError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_LEN {
        return Err(veloxdesk_protocol::ProtocolError::TooLarge(len).into());
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await.map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            CoreError::Closed
        } else {
            CoreError::Io(e)
        }
    })?;
    let env = Envelope::decode(payload.as_slice()).map_err(veloxdesk_protocol::ProtocolError::Decode)?;
    Ok(env)
}

async fn read_ack(recv: &mut RecvStream) -> Result<FileAck> {
    match read_envelope(recv).await?.msg {
        Some(Msg::FileAck(a)) => Ok(a),
        _ => Err(CoreError::invalid("expected FileAck")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::time::Duration;
    use veloxdesk_crypto::DeviceIdentity;
    use veloxdesk_transport::{connect, listen};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "vdx-ft-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn path_traversal_blocked_when_root_set() {
        let root = temp_dir("jail");
        let cfg = FileTransferConfig { root_dir: Some(root.clone()), ..Default::default() };
        assert!(resolve_path(&cfg, "../outside.txt", true).is_err());
        assert!(resolve_path(&cfg, "sub/../../escape.txt", true).is_err());
        assert!(resolve_path(&cfg, "ok/file.bin", true).is_ok());
        assert!(resolve_path(&cfg, "/ok/file.bin", true).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_crud_zstd_resume_parallel_and_sha256_over_quic() {
        tokio::time::timeout(Duration::from_secs(20), async {
            let host_root = temp_dir("host");
            let viewer_dir = temp_dir("viewer");

            let host_id = DeviceIdentity::generate();
            let viewer_id = DeviceIdentity::generate();
            let listener = listen("127.0.0.1:0".parse().unwrap(), &host_id).await.unwrap();
            let addr = listener.local_addr();
            let accept_task = tokio::spawn(async move { listener.accept().await.unwrap() });
            let viewer = Arc::new(connect(addr, &viewer_id, Some(host_id.public_key())).await.unwrap());
            let host = Arc::new(accept_task.await.unwrap());

            let cfg = Arc::new(FileTransferConfig {
                root_dir: Some(host_root.clone()),
                chunk_size: 32 * 1024,
                max_parallel: 4,
                compress: true,
            });
            let host_clone = host.clone();
            let cfg_clone = cfg.clone();
            let host_loop = tokio::spawn(async move {
                while let Ok((kind, send, recv)) = host_clone.accept_bi().await {
                    if kind == StreamKind::FileTransfer {
                        let c = cfg_clone.clone();
                        tokio::spawn(async move {
                            let _ = serve_file_stream(send, recv, &c).await;
                        });
                    }
                }
            });

            let client = Arc::new(FileTransferClient::new(viewer.clone()).with_chunk_size(32 * 1024));

            // 1. Mkdir, ListDir, path traversal rejected.
            client.mkdir("sub/nested").await.unwrap();
            assert!(client.list_dir("../").await.is_err());
            let entries = client.list_dir("").await.unwrap();
            assert!(entries.iter().any(|e| e.name == "sub" && e.is_dir));

            // 2. Upload compressible 512 KiB file with zstd + progress callback.
            let src_path = viewer_dir.join("big.dat");
            let mut content = Vec::with_capacity(512 * 1024);
            for i in 0..(512 * 1024 / 16) {
                content.extend_from_slice(b"VELOXDESK-ZSTD!!");
                let _ = i;
            }
            content[100] = 0xAA;
            content[500_000] = 0x55;
            std::fs::write(&src_path, &content).unwrap();

            let last_prog = Arc::new(AtomicU64::new(0));
            let lp = last_prog.clone();
            let up = client
                .upload(&src_path, "sub/big.dat", false, true, Some(Arc::new(move |done, _| {
                    lp.store(done, Ordering::Relaxed);
                })))
                .await
                .unwrap();
            assert_eq!(up.total_bytes, content.len() as u64);
            assert_eq!(up.bytes_transferred, content.len() as u64);
            assert_eq!(up.resumed_from, 0);
            assert!(up.wire_bytes < up.total_bytes / 10, "expected strong zstd ratio, wire={}", up.wire_bytes);
            assert_eq!(last_prog.load(Ordering::Relaxed), content.len() as u64);

            // 3. Simulated mid-transfer abort on Download -> resume from 100 KiB prefix!
            let dl_path = viewer_dir.join("big_dl.dat");
            let partial_len = 100_000usize;
            std::fs::write(&dl_path, &content[..partial_len]).unwrap();
            let dl = client.download("sub/big.dat", &dl_path, true, true, None).await.unwrap();
            assert_eq!(dl.resumed_from, partial_len as u64);
            assert_eq!(dl.bytes_transferred, (content.len() - partial_len) as u64);
            assert_eq!(dl.sha256, up.sha256);
            assert_eq!(std::fs::read(&dl_path).unwrap(), content);

            // 4. Simulated mid-transfer abort on Upload -> resume from partial host file!
            let partial_remote = host_root.join("sub/partial_up.dat");
            let partial_up_len = 150_000usize;
            std::fs::write(&partial_remote, &content[..partial_up_len]).unwrap();
            let up2 = client.upload(&src_path, "sub/partial_up.dat", true, true, None).await.unwrap();
            assert_eq!(up2.resumed_from, partial_up_len as u64);
            assert_eq!(up2.bytes_transferred, (content.len() - partial_up_len) as u64);
            assert_eq!(up2.sha256, up.sha256);
            assert_eq!(std::fs::read(&partial_remote).unwrap(), content);

            // 5. Stat, Rename, Delete, and Parallel download_many.
            let st = client.stat("sub/big.dat").await.unwrap();
            assert_eq!(st.size, content.len() as u64);
            client.rename("sub/partial_up.dat", "sub/renamed.dat").await.unwrap();

            let multi = client
                .download_many(
                    vec![
                        ("sub/big.dat".into(), viewer_dir.join("p1.dat")),
                        ("sub/renamed.dat".into(), viewer_dir.join("p2.dat")),
                    ],
                    false,
                    true,
                    2,
                )
                .await
                .unwrap();
            assert_eq!(multi.len(), 2);
            assert_eq!(multi[0].sha256, up.sha256);
            assert_eq!(multi[1].sha256, up.sha256);

            client.delete("sub/renamed.dat").await.unwrap();
            assert!(client.stat("sub/renamed.dat").await.is_err());

            host_loop.abort();
            let _ = std::fs::remove_dir_all(&host_root);
            let _ = std::fs::remove_dir_all(&viewer_dir);
        })
        .await
        .expect("file transfer test timed out (UDP loopback blocked?)");
    }
}
