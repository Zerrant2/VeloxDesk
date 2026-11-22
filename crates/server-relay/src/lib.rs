//! # veloxdesk-server-relay
//!
//! Zero-knowledge UDP relay server for peers behind symmetric NAT or strict firewalls
//! (ARCHITECTURE §4, §7, M2).
//!
//! Wire framing (zero collision with QUIC fixed bit `0x40`):
//! `[0x02, b'V', b'D', b'R'] || token[16] || raw_quic_payload`
//!
//! Empty payload registers/refreshes a peer's address for `token`; non-empty payloads are
//! forwarded verbatim (ciphertext only — never parsed or decrypted) to the paired peer.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::watch;

use veloxdesk_core_traits::Result;
use veloxdesk_protocol::DEFAULT_RELAY_PORT;

/// 4-byte framing prefix for relay UDP datagrams.
pub const MAGIC_RELAY: [u8; 4] = [0x02, b'V', b'D', b'R'];
/// Header length: 4-byte magic + 16-byte session token.
pub const RELAY_HEADER_LEN: usize = 20;

/// UDP socket buffer size requested from the OS (keyframes over relay).
const SOCKET_BUFFER_BYTES: usize = 8 * 1024 * 1024;
/// Max early packets buffered while waiting for the second peer to register.
const MAX_PENDING_PACKETS: usize = 16;

/// Configuration for [`RelayServer`].
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub bind: SocketAddr,
    pub session_ttl: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            bind: (Ipv4Addr::UNSPECIFIED, DEFAULT_RELAY_PORT).into(),
            session_ttl: Duration::from_secs(120),
        }
    }
}

struct RelaySession {
    peers: [Option<SocketAddr>; 2],
    last_active: Instant,
    /// Early packets from peer 0 before peer 1 has registered (prevents Initial PTO stall).
    pending_for_peer1: Vec<Vec<u8>>,
}

/// Self-hosted zero-knowledge UDP relay server.
pub struct RelayServer {
    cfg: RelayConfig,
    sock: UdpSocket,
    local: SocketAddr,
    sessions: HashMap<[u8; 16], RelaySession>,
}

impl RelayServer {
    /// Bind the relay UDP socket.
    pub async fn bind(cfg: RelayConfig) -> Result<Self> {
        let std_sock = make_udp_socket(cfg.bind)?;
        let sock = UdpSocket::from_std(std_sock)?;
        let local = sock.local_addr()?;
        tracing::info!(%local, ttl_s = cfg.session_ttl.as_secs(), "relay server bound");
        Ok(Self { cfg, sock, local, sessions: HashMap::new() })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Run indefinitely until an unrecoverable socket error occurs.
    pub async fn run(self) -> Result<()> {
        let (_tx, rx) = watch::channel(false);
        self.run_until(rx).await
    }

    /// Run until `shutdown` becomes `true` (or its sender is dropped).
    pub async fn run_until(mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        if *shutdown.borrow() {
            return Ok(());
        }
        let mut buf = vec![0u8; 65_535];
        let cleanup_period = (self.cfg.session_ttl / 3).clamp(Duration::from_secs(1), Duration::from_secs(15));
        let mut cleanup = tokio::time::interval(cleanup_period);
        cleanup.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                res = self.sock.recv_from(&mut buf) => {
                    match res {
                        Ok((n, from)) => self.handle_packet(&buf[..n], from).await,
                        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {
                            // Windows WSAECONNRESET after ICMP Port Unreachable; ignore.
                            continue;
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                _ = cleanup.tick() => {
                    self.evict_expired();
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        tracing::info!(local = %self.local, "relay server shutting down");
                        return Ok(());
                    }
                }
            }
        }
    }

    fn evict_expired(&mut self) {
        let now = Instant::now();
        let ttl = self.cfg.session_ttl;
        let before = self.sessions.len();
        self.sessions.retain(|_, s| now.duration_since(s.last_active) <= ttl);
        let removed = before - self.sessions.len();
        if removed > 0 {
            tracing::debug!(removed, remaining = self.sessions.len(), "evicted expired relay sessions");
        }
    }

    async fn handle_packet(&mut self, data: &[u8], from: SocketAddr) {
        if data.len() < RELAY_HEADER_LEN || data[..4] != MAGIC_RELAY {
            return;
        }
        let mut token = [0u8; 16];
        token.copy_from_slice(&data[4..20]);

        let now = Instant::now();
        let session = self.sessions.entry(token).or_insert_with(|| RelaySession {
            peers: [None, None],
            last_active: now,
            pending_for_peer1: Vec::new(),
        });
        session.last_active = now;

        let mut flush_to_peer1: Vec<Vec<u8>> = Vec::new();
        let slot = if session.peers[0] == Some(from) {
            0
        } else if session.peers[1] == Some(from) {
            1
        } else if session.peers[0].is_none() {
            session.peers[0] = Some(from);
            tracing::trace!(%from, "relay peer 0 registered");
            0
        } else if session.peers[1].is_none() {
            session.peers[1] = Some(from);
            tracing::trace!(%from, "relay peer 1 registered");
            flush_to_peer1 = std::mem::take(&mut session.pending_for_peer1);
            1
        } else {
            tracing::warn!(%from, "ignoring packet from 3rd address on active relay token");
            return;
        };

        let dst = session.peers[1 - slot];
        let has_payload = data.len() > RELAY_HEADER_LEN;
        if has_payload && dst.is_none() && slot == 0 && session.pending_for_peer1.len() < MAX_PENDING_PACKETS {
            session.pending_for_peer1.push(data.to_vec());
        }

        // Flush any buffered early packets now that peer 1 has appeared.
        for pkt in flush_to_peer1 {
            let _ = self.sock.send_to(&pkt, from).await;
        }

        if has_payload && let Some(other) = dst {
            let _ = self.sock.send_to(data, other).await;
        }
    }
}

fn make_udp_socket(bind: SocketAddr) -> io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let sock = Socket::new(Domain::for_address(bind), Type::DGRAM, Some(Protocol::UDP))?;
    if bind.is_ipv6() {
        let _ = sock.set_only_v6(false);
    }
    let _ = sock.set_recv_buffer_size(SOCKET_BUFFER_BYTES);
    let _ = sock.set_send_buffer_size(SOCKET_BUFFER_BYTES);
    sock.bind(&bind.into())?;
    sock.set_nonblocking(true)?;
    Ok(sock.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(token: &[u8; 16], payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(RELAY_HEADER_LEN + payload.len());
        out.extend_from_slice(&MAGIC_RELAY);
        out.extend_from_slice(token);
        out.extend_from_slice(payload);
        out
    }

    #[tokio::test]
    async fn zero_knowledge_bidirectional_forward_and_early_packet_flush() {
        let srv = RelayServer::bind(RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_ttl: Duration::from_secs(30),
        })
        .await
        .unwrap();
        let relay = srv.local_addr();
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(srv.run_until(rx));

        let a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let token = [0x42u8; 16];

        // Peer A sends payload even before Peer B registers.
        let p1 = frame(&token, b"opaque-quic-initial");
        a.send_to(&p1, relay).await.unwrap();

        // Peer B registers with empty payload -> receives buffered p1 immediately.
        b.send_to(&frame(&token, b""), relay).await.unwrap();
        let mut buf = vec![0u8; 2048];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), b.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], p1.as_slice());

        // Peer B replies -> Peer A receives verbatim.
        let p2 = frame(&token, b"opaque-quic-handshake");
        b.send_to(&p2, relay).await.unwrap();
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), a.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], p2.as_slice());

        let _ = tx.send(true);
        task.await.unwrap().unwrap();
    }
}
