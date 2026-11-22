//! # veloxdesk-terminal
//!
//! Interactive remote terminal over `StreamKind::Terminal`:
//! * Windows: native ConPTY (`CreatePseudoConsole` / `ResizePseudoConsole`).
//! * Unix: native POSIX PTY (`posix_openpt` / `TIOCSWINSZ`).
//! * Wire protocol: `TerminalOpen` → bidirectional `TerminalData` + `TerminalResize` → `TerminalExit`.

#[cfg(unix)]
mod unix_pty;
#[cfg(windows)]
mod win_pty;

#[cfg(unix)]
use unix_pty::PtyProcess;
#[cfg(windows)]
use win_pty::PtyProcess;

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use bytes::Bytes;
use tokio::time::timeout;
use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    TerminalData, TerminalExit, TerminalOpen, TerminalResize, envelope::Msg,
};
use veloxdesk_transport::{ControlReceiver, ControlSender, framed};

/// Events received by [`TerminalClient::recv`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    Data(Bytes),
    Exit(i32),
}

const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Host side: handles one `StreamKind::Terminal` bidi stream.
///
/// Reads `TerminalOpen`, spawns the real OS PTY (`ConPTY` on Windows, POSIX PTY on Unix),
/// bridges `TerminalData` and `TerminalResize`, emits `TerminalExit { code }`, and kills
/// the child process if the stream closes or is dropped.
pub async fn serve_terminal_stream(send: SendStream, recv: RecvStream) -> Result<()> {
    let (mut writer, mut reader) = framed(send, recv);
    let open = match timeout(OPEN_TIMEOUT, reader.recv()).await.map_err(|_| CoreError::Timeout)??.msg {
        Some(Msg::TerminalOpen(o)) => o,
        other => return Err(CoreError::invalid(format!("expected TerminalOpen, got {other:?}"))),
    };
    let term_id = open.term_id;
    let cols = open.cols.clamp(2, 1000) as u16;
    let rows = open.rows.clamp(2, 1000) as u16;
    tracing::info!(term_id, cols, rows, shell = %open.shell, "opening remote terminal");

    #[cfg(not(any(windows, unix)))]
    {
        let _ = writer.send(&Msg::TerminalExit(TerminalExit { term_id, code: -1 }).into()).await;
        let _ = writer.finish().await;
        return Err(CoreError::unsupported("PTY not implemented on this OS"));
    }

    #[cfg(any(windows, unix))]
    {
        let mut streams = match PtyProcess::spawn(cols, rows, &open.shell) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(term_id, "failed to spawn PTY: {e}");
                let _ = writer
                    .send(&Msg::TerminalData(TerminalData { term_id, data: Bytes::from(format!("terminal error: {e}\r\n")) }).into())
                    .await;
                let _ = writer.send(&Msg::TerminalExit(TerminalExit { term_id, code: -1 }).into()).await;
                let _ = writer.finish().await;
                return Err(e);
            }
        };

        let mut exit_code: Option<i32> = None;
        let mut output_open = true;

        loop {
            tokio::select! {
                biased;
                chunk = streams.output.recv(), if output_open => {
                    match chunk {
                        Some(data) => {
                            if writer.send(&Msg::TerminalData(TerminalData { term_id, data }).into()).await.is_err() {
                                break;
                            }
                        }
                        None => {
                            output_open = false;
                            if exit_code.is_some() {
                                break;
                            }
                        }
                    }
                }
                res = &mut streams.exit, if exit_code.is_none() => {
                    exit_code = Some(res.unwrap_or(-1));
                    if !output_open {
                        break;
                    }
                    // Give the reader thread a brief window to flush any final buffered bytes.
                    while let Ok(Some(data)) = timeout(Duration::from_millis(150), streams.output.recv()).await {
                        if writer.send(&Msg::TerminalData(TerminalData { term_id, data }).into()).await.is_err() {
                            break;
                        }
                    }
                    break;
                }
                msg = reader.recv() => {
                    match msg {
                        Ok(env) => match env.msg {
                            Some(Msg::TerminalData(d)) => {
                                if let Err(e) = streams.pty.write(&d.data) {
                                    tracing::debug!(term_id, "pty write ended: {e}");
                                    break;
                                }
                            }
                            Some(Msg::TerminalResize(r)) => {
                                let _ = streams.pty.resize(r.cols.clamp(2, 1000) as u16, r.rows.clamp(2, 1000) as u16);
                            }
                            Some(Msg::Close(_)) => break,
                            _ => {}
                        },
                        Err(_) => break,
                    }
                }
            }
        }

        let code = exit_code.unwrap_or(-1);
        let _ = writer.send(&Msg::TerminalExit(TerminalExit { term_id, code }).into()).await;
        let _ = writer.finish().await;
        drop(streams.pty);
        tracing::info!(term_id, code, "remote terminal closed");
        Ok(())
    }
}

/// Viewer side: interactive remote terminal session over `StreamKind::Terminal`.
pub struct TerminalClient {
    term_id: u32,
    writer: ControlSender,
    reader: ControlReceiver,
}

impl TerminalClient {
    /// Open a new remote terminal stream with dimensions `cols`×`rows` and optional `shell`
    /// (empty = host platform default shell).
    pub async fn open(ch: &dyn TransportChannel, cols: u32, rows: u32, shell: String) -> Result<Self> {
        static NEXT_ID: AtomicU32 = AtomicU32::new(1);
        let term_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (send, recv) = ch.open_bi(StreamKind::Terminal).await?;
        let (mut writer, reader) = framed(send, recv);
        writer.send(&Msg::TerminalOpen(TerminalOpen { term_id, cols, rows, shell }).into()).await?;
        Ok(Self { term_id, writer, reader })
    }

    pub fn term_id(&self) -> u32 {
        self.term_id
    }

    /// Send raw keyboard/stdin bytes to the remote PTY.
    pub async fn write(&mut self, data: &[u8]) -> Result<()> {
        self.writer
            .send(&Msg::TerminalData(TerminalData { term_id: self.term_id, data: Bytes::copy_from_slice(data) }).into())
            .await
    }

    /// Resize the remote PTY (`cols`×`rows`).
    pub async fn resize(&mut self, cols: u32, rows: u32) -> Result<()> {
        self.writer
            .send(&Msg::TerminalResize(TerminalResize { term_id: self.term_id, cols, rows }).into())
            .await
    }

    /// Receive the next [`TerminalEvent`] (`Data` or `Exit`).
    pub async fn recv(&mut self) -> Result<TerminalEvent> {
        loop {
            let env = self.reader.recv().await?;
            match env.msg {
                Some(Msg::TerminalData(d)) => return Ok(TerminalEvent::Data(d.data)),
                Some(Msg::TerminalExit(e)) => return Ok(TerminalEvent::Exit(e.code)),
                Some(Msg::Close(_)) => return Err(CoreError::Closed),
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Mutex as AsyncMutex, mpsc};
    use veloxdesk_core_traits::transport::TransportStats;

    type Incoming = (StreamKind, SendStream, RecvStream);

    struct MemCh {
        peer_tx: mpsc::UnboundedSender<Incoming>,
        incoming: AsyncMutex<mpsc::UnboundedReceiver<Incoming>>,
    }

    fn mem_pair() -> (Arc<MemCh>, Arc<MemCh>) {
        let (a_tx, a_rx) = mpsc::unbounded_channel();
        let (b_tx, b_rx) = mpsc::unbounded_channel();
        (
            Arc::new(MemCh { peer_tx: b_tx, incoming: AsyncMutex::new(a_rx) }),
            Arc::new(MemCh { peer_tx: a_tx, incoming: AsyncMutex::new(b_rx) }),
        )
    }

    #[async_trait::async_trait]
    impl TransportChannel for MemCh {
        fn max_datagram_size(&self) -> Option<usize> {
            None
        }
        fn send_datagram(&self, _: Bytes) -> Result<()> {
            Err(CoreError::Closed)
        }
        async fn recv_datagram(&self) -> Result<Bytes> {
            std::future::pending().await
        }
        async fn open_bi(&self, kind: StreamKind) -> Result<(SendStream, RecvStream)> {
            let (local, remote) = tokio::io::duplex(1 << 16);
            let (lr, lw) = tokio::io::split(local);
            let (rr, rw) = tokio::io::split(remote);
            self.peer_tx.send((kind, Box::pin(rw), Box::pin(rr))).map_err(|_| CoreError::Closed)?;
            Ok((Box::pin(lw) as SendStream, Box::pin(lr) as RecvStream))
        }
        async fn open_uni(&self, _: StreamKind) -> Result<SendStream> {
            Err(CoreError::unsupported("uni"))
        }
        async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)> {
            self.incoming.lock().await.recv().await.ok_or(CoreError::Closed)
        }
        async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)> {
            std::future::pending().await
        }
        fn stats(&self) -> TransportStats {
            TransportStats::default()
        }
        fn peer_public_key(&self) -> Option<[u8; 32]> {
            None
        }
        fn close(&self, _: u32, _: &str) {}
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn real_pty_echo_resize_and_exit() {
        let (host_ch, client_ch) = mem_pair();
        let server = tokio::spawn(async move {
            let (kind, s, r) = host_ch.accept_bi().await.unwrap();
            assert_eq!(kind, StreamKind::Terminal);
            serve_terminal_stream(s, r).await
        });

        #[cfg(windows)]
        let shell = "cmd.exe /C echo VELOXDESK_M4_OK".to_string();
        #[cfg(unix)]
        let shell = "echo VELOXDESK_M4_OK".to_string();

        let mut client = TerminalClient::open(client_ch.as_ref(), 80, 24, shell).await.unwrap();
        client.resize(100, 30).await.unwrap();

        let mut output = Vec::new();
        let mut exit_code = None;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Ok(ev) = client.recv().await {
                match ev {
                    TerminalEvent::Data(b) => output.extend_from_slice(&b),
                    TerminalEvent::Exit(code) => {
                        exit_code = Some(code);
                        break;
                    }
                }
            }
        })
        .await
        .expect("timed out waiting for PTY output & exit");

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("VELOXDESK_M4_OK"), "unexpected PTY output: {text:?}");
        assert_eq!(exit_code, Some(0));
        server.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn interactive_stdin_write_and_exit() {
        let (host_ch, client_ch) = mem_pair();
        let server = tokio::spawn(async move {
            let (_kind, s, r) = host_ch.accept_bi().await.unwrap();
            serve_terminal_stream(s, r).await
        });

        let mut client = TerminalClient::open(client_ch.as_ref(), 80, 24, String::new()).await.unwrap();
        #[cfg(windows)]
        client.write(b"echo VELOXDESK_STDIN_OK\r\nexit 0\r\n").await.unwrap();
        #[cfg(unix)]
        client.write(b"echo VELOXDESK_STDIN_OK\nexit 0\n").await.unwrap();

        let mut output = Vec::new();
        let mut exit_code = None;
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Ok(ev) = client.recv().await {
                match ev {
                    TerminalEvent::Data(b) => output.extend_from_slice(&b),
                    TerminalEvent::Exit(code) => {
                        exit_code = Some(code);
                        break;
                    }
                }
            }
        })
        .await
        .expect("timed out waiting for interactive PTY");

        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("VELOXDESK_STDIN_OK"), "unexpected interactive PTY output: {text:?}");
        assert_eq!(exit_code, Some(0));
        server.await.unwrap().unwrap();
    }
}
