//! QUIC endpoints and [`QuicChannel`] (ARCHITECTURE ADR-003, §4.1).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use tokio::io::AsyncReadExt;

use veloxdesk_core_traits::transport::{PathKind, RecvStream, SendStream, TransportStats};
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::tls::{self, PeerPolicy};
use veloxdesk_protocol::{ALPN_SESSION, StreamKind};

/// UDP socket buffer size requested from the OS (bursty keyframes must not overflow it).
const SOCKET_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// Low-latency QUIC transport parameters used for every session.
///
/// * datagrams: 4 MiB send buffer (≈ several keyframes; quinn drops the *oldest* queued
///   datagram when full, which is what we want for media) and 8 MiB receive buffer;
/// * BBR congestion control with a large initial window so the first keyframe is not
///   paced over several RTTs (datagrams are still subject to CC — our ABR keeps the
///   media rate below the path capacity, so CC never becomes the queue);
/// * keep-alive 2 s, idle timeout 10 s.
pub fn low_latency_transport_config() -> quinn::TransportConfig {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(Some(quinn::IdleTimeout::from(quinn::VarInt::from_u32(10_000))));
    t.keep_alive_interval(Some(Duration::from_secs(2)));
    t.datagram_receive_buffer_size(Some(8 * 1024 * 1024));
    t.datagram_send_buffer_size(4 * 1024 * 1024);
    t.max_concurrent_bidi_streams(quinn::VarInt::from_u32(256));
    t.max_concurrent_uni_streams(quinn::VarInt::from_u32(256));
    t.stream_receive_window(quinn::VarInt::from_u32(8 * 1024 * 1024));
    t.receive_window(quinn::VarInt::from_u32(32 * 1024 * 1024));
    t.send_window(32 * 1024 * 1024);
    t.initial_rtt(Duration::from_millis(10));
    let mut bbr = quinn::congestion::BbrConfig::default();
    bbr.initial_window(1024 * 1024);
    t.congestion_controller_factory(Arc::new(bbr));
    t
}

fn make_socket(bind: SocketAddr) -> Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
    if bind.is_ipv6() {
        // Dual-stack where the OS allows it.
        let _ = sock.set_only_v6(false);
    }
    // Best effort: the OS may clamp these.
    let _ = sock.set_recv_buffer_size(SOCKET_BUFFER_BYTES);
    let _ = sock.set_send_buffer_size(SOCKET_BUFFER_BYTES);
    sock.bind(&bind.into())?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

fn new_endpoint(bind: SocketAddr, server: Option<quinn::ServerConfig>) -> Result<quinn::Endpoint> {
    let socket = make_socket(bind)?;
    let runtime = quinn::default_runtime().ok_or_else(|| CoreError::platform("no async runtime (call inside tokio)"))?;
    quinn::Endpoint::new(quinn::EndpointConfig::default(), server, socket, runtime).map_err(CoreError::Io)
}

fn server_config(id: &DeviceIdentity, alpn: &[&[u8]], clients: PeerPolicy) -> Result<quinn::ServerConfig> {
    let rustls_cfg = tls::server_config(id, alpn, clients)?;
    let quic = QuicServerConfig::try_from(rustls_cfg).map_err(CoreError::other)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(Arc::new(low_latency_transport_config()));
    Ok(cfg)
}

fn client_config(id: &DeviceIdentity, alpn: &[&[u8]], server: PeerPolicy) -> Result<quinn::ClientConfig> {
    let rustls_cfg = tls::client_config(id, alpn, server)?;
    let quic = QuicClientConfig::try_from(rustls_cfg).map_err(CoreError::other)?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(Arc::new(low_latency_transport_config()));
    Ok(cfg)
}

/// Accepts incoming peer sessions.
pub struct Listener {
    endpoint: quinn::Endpoint,
    local: SocketAddr,
}

impl Listener {
    /// Wait for the next successfully authenticated session. Failed handshakes are
    /// logged and skipped; returns `Err(Closed)` once the endpoint is closed.
    pub async fn accept(&self) -> Result<QuicChannel> {
        loop {
            let incoming = self.endpoint.accept().await.ok_or(CoreError::Closed)?;
            let remote = incoming.remote_address();
            match incoming.await {
                Ok(conn) => return Ok(QuicChannel::from_connection(conn, PathKind::Direct, None)),
                Err(e) => tracing::warn!(%remote, error = %e, "incoming QUIC handshake failed"),
            }
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// The underlying quinn endpoint (e.g. to also initiate outgoing connections from
    /// the same socket for hole punching).
    pub fn endpoint(&self) -> &quinn::Endpoint {
        &self.endpoint
    }

    pub fn close(&self) {
        self.endpoint.close(quinn::VarInt::from_u32(0), b"shutdown");
    }
}

/// Listen for direct (LAN / IP) sessions. Any client key is accepted at the TLS layer;
/// the application authorizes it (password / whitelist) via [`QuicChannel::peer_public_key`].
pub async fn listen(bind: SocketAddr, id: &DeviceIdentity) -> Result<Listener> {
    listen_with(bind, id, PeerPolicy::Any).await
}

/// [`listen`] with an explicit client-key policy.
pub async fn listen_with(bind: SocketAddr, id: &DeviceIdentity, clients: PeerPolicy) -> Result<Listener> {
    let endpoint = new_endpoint(bind, Some(server_config(id, &[ALPN_SESSION], clients)?))?;
    let local = endpoint.local_addr()?;
    Ok(Listener { endpoint, local })
}

/// Connect to a host. `expected_peer = None` accepts any host key (M1 LAN); the verified
/// key is then available from [`TransportChannel::peer_public_key`].
pub async fn connect(addr: SocketAddr, id: &DeviceIdentity, expected_peer: Option<[u8; 32]>) -> Result<QuicChannel> {
    let bind: SocketAddr = if addr.is_ipv6() {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    };
    let endpoint = new_endpoint(bind, None)?;
    let cfg = client_config(id, &[ALPN_SESSION], PeerPolicy::from_expected(expected_peer))?;
    let connecting = endpoint.connect_with(cfg, addr, tls::PEER_SERVER_NAME).map_err(CoreError::other)?;
    let conn = connecting.await.map_err(map_conn_err)?;
    Ok(QuicChannel::from_connection(conn, PathKind::Direct, Some(endpoint)))
}

struct Inner {
    conn: quinn::Connection,
    path: PathKind,
    peer_key: Option<[u8; 32]>,
    /// Client-side endpoint kept alive for the lifetime of the channel.
    _endpoint: Option<quinn::Endpoint>,
    dgram_sent: AtomicU64,
    dgram_recv: AtomicU64,
    dgram_dropped: AtomicU64,
}

/// One authenticated QUIC session (cheap to clone).
#[derive(Clone)]
pub struct QuicChannel {
    inner: Arc<Inner>,
}

impl QuicChannel {
    /// Wrap an established quinn connection (used by direct, P2P and relay paths).
    pub fn from_connection(conn: quinn::Connection, path: PathKind, endpoint: Option<quinn::Endpoint>) -> Self {
        let peer_key = conn
            .peer_identity()
            .and_then(|any| any.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok())
            .and_then(|certs| certs.first().and_then(|c| tls::public_key_from_cert(c.as_ref()).ok()));
        Self {
            inner: Arc::new(Inner {
                conn,
                path,
                peer_key,
                _endpoint: endpoint,
                dgram_sent: AtomicU64::new(0),
                dgram_recv: AtomicU64::new(0),
                dgram_dropped: AtomicU64::new(0),
            }),
        }
    }

    pub fn connection(&self) -> &quinn::Connection {
        &self.inner.conn
    }

    pub fn remote_addr(&self) -> SocketAddr {
        self.inner.conn.remote_address()
    }

    pub fn rtt(&self) -> Duration {
        self.inner.conn.rtt()
    }

    /// Resolves when the connection is closed (by either side or by timeout).
    pub async fn closed(&self) -> CoreError {
        map_conn_err(self.inner.conn.closed().await)
    }
}

pub(crate) fn map_conn_err(e: quinn::ConnectionError) -> CoreError {
    use quinn::ConnectionError as E;
    match e {
        E::TimedOut => CoreError::Timeout,
        E::ApplicationClosed(_) | E::ConnectionClosed(_) | E::LocallyClosed | E::Reset => CoreError::Closed,
        other => CoreError::other(other),
    }
}

fn map_write_err(e: quinn::WriteError) -> CoreError {
    match e {
        quinn::WriteError::ConnectionLost(c) => map_conn_err(c),
        other => CoreError::other(other),
    }
}

async fn read_preamble(recv: &mut quinn::RecvStream) -> Result<StreamKind> {
    let mut b = [0u8; 1];
    match recv.read_exact(&mut b).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(_)) => return Err(CoreError::Closed),
        Err(quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(c))) => return Err(map_conn_err(c)),
        Err(e) => return Err(CoreError::other(e)),
    }
    StreamKind::from_u8(b[0])
        .ok_or(CoreError::Protocol(veloxdesk_protocol::ProtocolError::Invalid("unknown stream kind")))
}

#[async_trait]
impl TransportChannel for QuicChannel {
    fn max_datagram_size(&self) -> Option<usize> {
        self.inner.conn.max_datagram_size()
    }

    fn send_datagram(&self, data: Bytes) -> Result<()> {
        let len = data.len();
        if self.inner.conn.datagram_send_buffer_space() < len {
            // quinn will evict the oldest queued datagram(s) to make room.
            self.inner.dgram_dropped.fetch_add(1, Ordering::Relaxed);
        }
        match self.inner.conn.send_datagram(data) {
            Ok(()) => {
                self.inner.dgram_sent.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(quinn::SendDatagramError::ConnectionLost(c)) => Err(map_conn_err(c)),
            Err(quinn::SendDatagramError::TooLarge) => {
                self.inner.dgram_dropped.fetch_add(1, Ordering::Relaxed);
                Err(CoreError::invalid("datagram larger than max_datagram_size"))
            }
            Err(e) => Err(CoreError::unsupported(e.to_string())),
        }
    }

    async fn recv_datagram(&self) -> Result<Bytes> {
        let b = self.inner.conn.read_datagram().await.map_err(map_conn_err)?;
        self.inner.dgram_recv.fetch_add(1, Ordering::Relaxed);
        Ok(b)
    }

    async fn open_bi(&self, kind: StreamKind) -> Result<(SendStream, RecvStream)> {
        let (mut send, recv) = self.inner.conn.open_bi().await.map_err(map_conn_err)?;
        send.write_all(&veloxdesk_protocol::framing::stream_preamble(kind)).await.map_err(map_write_err)?;
        Ok((Box::pin(send), Box::pin(recv)))
    }

    async fn open_uni(&self, kind: StreamKind) -> Result<SendStream> {
        let mut send = self.inner.conn.open_uni().await.map_err(map_conn_err)?;
        send.write_all(&veloxdesk_protocol::framing::stream_preamble(kind)).await.map_err(map_write_err)?;
        Ok(Box::pin(send))
    }

    async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)> {
        let (send, mut recv) = self.inner.conn.accept_bi().await.map_err(map_conn_err)?;
        let kind = read_preamble(&mut recv).await?;
        Ok((kind, Box::pin(send), Box::pin(recv)))
    }

    async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)> {
        let mut recv = self.inner.conn.accept_uni().await.map_err(map_conn_err)?;
        let kind = read_preamble(&mut recv).await?;
        Ok((kind, Box::pin(recv)))
    }

    fn stats(&self) -> TransportStats {
        let s = self.inner.conn.stats();
        let rtt_us = s.path.rtt.as_micros().min(u32::MAX as u128) as u32;
        let estimated_kbps = if rtt_us > 0 {
            // cwnd bytes per RTT → kbit/s
            (s.path.cwnd.saturating_mul(8_000) / rtt_us as u64).min(u32::MAX as u64) as u32
        } else {
            0
        };
        TransportStats {
            path: self.inner.path,
            remote_addr: Some(self.inner.conn.remote_address()),
            rtt_us,
            estimated_kbps,
            bytes_sent: s.udp_tx.bytes,
            bytes_received: s.udp_rx.bytes,
            datagrams_sent: self.inner.dgram_sent.load(Ordering::Relaxed),
            datagrams_received: self.inner.dgram_recv.load(Ordering::Relaxed),
            datagrams_dropped: self.inner.dgram_dropped.load(Ordering::Relaxed),
            lost_packets: s.path.lost_packets,
        }
    }

    fn peer_public_key(&self) -> Option<[u8; 32]> {
        self.inner.peer_key
    }

    fn close(&self, code: u32, reason: &str) {
        self.inner.conn.close(quinn::VarInt::from_u32(code), reason.as_bytes());
    }
}

/// Helper: read the whole remaining stream (used in tests / small transfers).
pub async fn read_to_end(recv: &mut RecvStream, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let n = recv.read(&mut chunk).await?;
        if n == 0 {
            return Ok(out);
        }
        if out.len() + n > limit {
            return Err(CoreError::invalid("stream exceeds limit"));
        }
        out.extend_from_slice(&chunk[..n]);
    }
}
