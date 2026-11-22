//! # veloxdesk-tunnel
//!
//! TCP port forwarding over QUIC streams (`StreamKind::Tunnel`, ARCHITECTURE §4.3, M4).
//!
//! Wire flow on each `StreamKind::Tunnel` bidirectional stream:
//! 1. Viewer → Host: framed `Envelope(TunnelOpen { tunnel_id, remote_host, remote_port })`.
//! 2. Host checks [`TunnelPolicy`], connects to `remote_host:remote_port`, replies framed
//!    `Envelope(TunnelResult { tunnel_id, ok, error })`.
//! 3. If `ok`, both sides immediately switch the same stream to raw full-duplex TCP bytes
//!    via `tokio::io::copy_bidirectional`.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::{Context, Poll};

use bytes::BytesMut;
use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{Envelope, TunnelOpen, TunnelResult, envelope::Msg};
use veloxdesk_protocol::framing::{MAX_FRAME_LEN, encode_frame};

/// Host-side access policy for outgoing TCP tunnel targets.
#[derive(Debug, Clone, Default)]
pub struct TunnelPolicy {
    /// When `true`, only loopback target addresses (`127.0.0.0/8`, `::1`) are allowed.
    pub allow_loopback_only: bool,
    /// When `Some`, only listed destination ports are allowed.
    pub allowed_ports: Option<Vec<u16>>,
}

impl TunnelPolicy {
    /// Check port restriction synchronously; address restriction is also enforced after DNS resolution.
    pub fn check_port(&self, port: u16) -> std::result::Result<(), String> {
        if port == 0 {
            return Err("invalid tunnel port 0".into());
        }
        if let Some(allowed) = &self.allowed_ports
            && !allowed.contains(&port)
        {
            return Err(format!("port {port} is not allowed by tunnel policy"));
        }
        Ok(())
    }
}

/// Host side: handle one `StreamKind::Tunnel` bidirectional stream.
///
/// Reads exact-length framed `TunnelOpen`, validates [`TunnelPolicy`], dials the TCP target,
/// replies `TunnelResult`, and bridges raw bytes until either side closes.
pub async fn serve_tunnel_stream(mut send: SendStream, mut recv: RecvStream, policy: &TunnelPolicy) -> Result<()> {
    let env = read_exact_envelope(&mut recv).await?;
    let open = match env.msg {
        Some(Msg::TunnelOpen(open)) => open,
        _ => return Err(CoreError::invalid("expected TunnelOpen on Tunnel stream")),
    };

    let target = match dial_target(&open, policy).await {
        Ok(tcp) => tcp,
        Err(reason) => {
            tracing::warn!(
                tunnel_id = open.tunnel_id,
                host = %open.remote_host,
                port = open.remote_port,
                %reason,
                "tunnel open rejected/failed"
            );
            let reply: Envelope = Msg::TunnelResult(TunnelResult {
                tunnel_id: open.tunnel_id,
                ok: false,
                error: reason,
            })
            .into();
            let _ = write_envelope(&mut send, &reply).await;
            let _ = send.shutdown().await;
            return Ok(());
        }
    };

    let ok_reply: Envelope = Msg::TunnelResult(TunnelResult {
        tunnel_id: open.tunnel_id,
        ok: true,
        error: String::new(),
    })
    .into();
    write_envelope(&mut send, &ok_reply).await?;

    let mut tcp = target;
    let mut quic = StreamDuplex { send, recv };
    match tokio::io::copy_bidirectional(&mut tcp, &mut quic).await {
        Ok((to_quic, to_tcp)) => {
            tracing::debug!(tunnel_id = open.tunnel_id, to_quic, to_tcp, "tunnel closed");
        }
        Err(e) => {
            tracing::debug!(tunnel_id = open.tunnel_id, error = %e, "tunnel copy ended");
        }
    }
    Ok(())
}

async fn dial_target(open: &TunnelOpen, policy: &TunnelPolicy) -> std::result::Result<TcpStream, String> {
    let port = u16::try_from(open.remote_port).map_err(|_| format!("invalid port {}", open.remote_port))?;
    policy.check_port(port)?;
    if open.remote_host.trim().is_empty() {
        return Err("empty remote_host".into());
    }

    let resolved: Vec<SocketAddr> = tokio::net::lookup_host((open.remote_host.as_str(), port))
        .await
        .map_err(|e| format!("resolve {}:{} failed: {e}", open.remote_host, port))?
        .collect();
    if resolved.is_empty() {
        return Err(format!("no addresses resolved for {}:{}", open.remote_host, port));
    }

    let filtered: Vec<SocketAddr> = if policy.allow_loopback_only {
        resolved.into_iter().filter(|a| a.ip().is_loopback()).collect()
    } else {
        resolved
    };
    if filtered.is_empty() {
        return Err("tunnel policy allows loopback targets only".into());
    }

    let tcp = TcpStream::connect(filtered.as_slice())
        .await
        .map_err(|e| format!("connect {}:{} failed: {e}", open.remote_host, port))?;
    let _ = tcp.set_nodelay(true);
    Ok(tcp)
}

/// Viewer side: listens on a local TCP port and forwards every accepted client over a
/// dedicated `StreamKind::Tunnel` QUIC bidirectional stream to `remote_host:remote_port`.
pub struct LocalForwarder {
    local: SocketAddr,
    remote_host: String,
    remote_port: u16,
    shutdown_tx: watch::Sender<bool>,
}

impl LocalForwarder {
    /// Bind a local TCP listener and spawn the background forwarding loop.
    pub async fn bind(
        local_bind: SocketAddr,
        remote_host: String,
        remote_port: u16,
        ch: Arc<dyn TransportChannel>,
    ) -> Result<Self> {
        let listener = TcpListener::bind(local_bind).await?;
        let local = listener.local_addr()?;
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        let host_clone = remote_host.clone();
        let next_id = Arc::new(AtomicU32::new(1));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    res = listener.accept() => {
                        match res {
                            Ok((tcp, peer)) => {
                                let _ = tcp.set_nodelay(true);
                                let tid = next_id.fetch_add(1, Ordering::Relaxed);
                                let ch = ch.clone();
                                let rhost = host_clone.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = forward_one(tcp, tid, rhost, remote_port, ch).await {
                                        tracing::debug!(%peer, tunnel_id = tid, error = %e, "forwarded TCP connection ended");
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!(%local, error = %e, "LocalForwarder accept error");
                                break;
                            }
                        }
                    }
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            tracing::debug!(%local, "LocalForwarder shutting down");
                            break;
                        }
                    }
                }
            }
        });

        Ok(Self { local, remote_host, remote_port, shutdown_tx })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn remote_host(&self) -> &str {
        &self.remote_host
    }

    pub fn remote_port(&self) -> u16 {
        self.remote_port
    }

    /// Stop accepting new local TCP connections.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }
}

impl Drop for LocalForwarder {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(true);
    }
}

async fn forward_one(
    mut tcp: TcpStream,
    tunnel_id: u32,
    remote_host: String,
    remote_port: u16,
    ch: Arc<dyn TransportChannel>,
) -> Result<()> {
    let (mut send, mut recv) = ch.open_bi(StreamKind::Tunnel).await?;
    let open: Envelope = Msg::TunnelOpen(TunnelOpen {
        tunnel_id,
        remote_host,
        remote_port: remote_port as u32,
    })
    .into();
    write_envelope(&mut send, &open).await?;

    let reply = read_exact_envelope(&mut recv).await?;
    match reply.msg {
        Some(Msg::TunnelResult(res)) if res.ok => {}
        Some(Msg::TunnelResult(res)) => {
            return Err(CoreError::other(io::Error::other(format!("remote tunnel rejected: {}", res.error))));
        }
        _ => return Err(CoreError::invalid("expected TunnelResult on Tunnel stream")),
    }

    let mut quic = StreamDuplex { send, recv };
    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut quic).await;
    Ok(())
}

// ───────────────────────────── exact framing & duplex ─────────────────────────────

async fn write_envelope(send: &mut SendStream, env: &Envelope) -> Result<()> {
    let mut buf = BytesMut::with_capacity(128);
    encode_frame(env, &mut buf)?;
    send.write_all(&buf).await?;
    send.flush().await?;
    Ok(())
}

/// Read EXACTLY one length-prefixed protobuf `Envelope` without over-reading into any
/// trailing raw TCP bytes on the same stream.
async fn read_exact_envelope(recv: &mut RecvStream) -> Result<Envelope> {
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

struct StreamDuplex {
    send: SendStream,
    recv: RecvStream,
}

impl AsyncRead for StreamDuplex {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        self.recv.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for StreamDuplex {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        self.send.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.send.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.send.as_mut().poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use veloxdesk_crypto::DeviceIdentity;
    use veloxdesk_transport::{connect, listen};

    #[test]
    fn policy_port_check() {
        let p = TunnelPolicy { allow_loopback_only: true, allowed_ports: Some(vec![22, 8080]) };
        assert!(p.check_port(22).is_ok());
        assert!(p.check_port(8080).is_ok());
        assert!(p.check_port(443).is_err());
        assert!(p.check_port(0).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tcp_echo_over_quic_tunnel_and_policy_denial() {
        tokio::time::timeout(Duration::from_secs(15), async {
            // 1. Start a local TCP echo server representing the target service on the host.
            let echo_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let echo_addr = echo_listener.local_addr().unwrap();
            tokio::spawn(async move {
                while let Ok((mut sock, _)) = echo_listener.accept().await {
                    tokio::spawn(async move {
                        let (mut r, mut w) = sock.split();
                        let _ = tokio::io::copy(&mut r, &mut w).await;
                    });
                }
            });

            // 2. Establish a real QUIC session between viewer and host.
            let host_id = DeviceIdentity::generate();
            let viewer_id = DeviceIdentity::generate();
            let listener = listen("127.0.0.1:0".parse().unwrap(), &host_id).await.unwrap();
            let addr = listener.local_addr();
            let accept_task = tokio::spawn(async move { listener.accept().await.unwrap() });
            let viewer = Arc::new(connect(addr, &viewer_id, Some(host_id.public_key())).await.unwrap());
            let host = Arc::new(accept_task.await.unwrap());

            // 3. Host accepts StreamKind::Tunnel with loopback + echo_addr.port() allowed.
            let policy = TunnelPolicy {
                allow_loopback_only: true,
                allowed_ports: Some(vec![echo_addr.port()]),
            };
            let host_clone = host.clone();
            let policy_clone = policy.clone();
            let host_loop = tokio::spawn(async move {
                while let Ok((kind, send, recv)) = host_clone.accept_bi().await {
                    if kind == StreamKind::Tunnel {
                        let pol = policy_clone.clone();
                        tokio::spawn(async move {
                            let _ = serve_tunnel_stream(send, recv, &pol).await;
                        });
                    }
                }
            });

            // 4. Viewer binds LocalForwarder and exchanges 128 KiB through the QUIC tunnel.
            let fwd = LocalForwarder::bind(
                "127.0.0.1:0".parse().unwrap(),
                "127.0.0.1".into(),
                echo_addr.port(),
                viewer.clone(),
            )
            .await
            .unwrap();

            let mut client = TcpStream::connect(fwd.local_addr()).await.unwrap();
            let payload: Vec<u8> = (0..128 * 1024).map(|i| (i % 251) as u8).collect();
            client.write_all(&payload).await.unwrap();
            let mut echoed = vec![0u8; payload.len()];
            client.read_exact(&mut echoed).await.unwrap();
            assert_eq!(echoed, payload);
            drop(client);
            fwd.shutdown();

            // 5. Verify disallowed port is cleanly rejected with ok=false.
            let (mut s, mut r) = viewer.open_bi(StreamKind::Tunnel).await.unwrap();
            let bad_port = echo_addr.port().wrapping_add(1).max(1);
            let open: Envelope = Msg::TunnelOpen(TunnelOpen {
                tunnel_id: 99,
                remote_host: "127.0.0.1".into(),
                remote_port: bad_port as u32,
            })
            .into();
            write_envelope(&mut s, &open).await.unwrap();
            match read_exact_envelope(&mut r).await.unwrap().msg {
                Some(Msg::TunnelResult(res)) => {
                    assert!(!res.ok);
                    assert!(!res.error.is_empty());
                }
                other => panic!("expected TunnelResult(ok=false), got {other:?}"),
            }

            host_loop.abort();
        })
        .await
        .expect("tunnel test timed out (UDP loopback blocked?)");
    }
}
