//! # veloxdesk-server-rendezvous
//!
//! Rendezvous server: Ed25519-verified device registry, peer lookup, NAT hole-punch
//! signalling, and relay session brokering over UDP (ARCHITECTURE §4, M2).
//!
//! Wire framing (zero collision with QUIC fixed bit `0x40`):
//! `[0x01, b'V', b'D', b'Z'] || protobuf(RendezvousMessage)`

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use prost::Message;
use rand_core::{OsRng, RngCore};
use tokio::net::UdpSocket;
use tokio::sync::watch;

use veloxdesk_core_traits::Result;
use veloxdesk_crypto::auth::verify_register_peer;
use veloxdesk_protocol::DEFAULT_RENDEZVOUS_PORT;
use veloxdesk_protocol::rendezvous::{
    PeerInfo, PunchHole, RegisterOk, RelayOffer, RendezvousError, RendezvousMessage,
    rendezvous_message::Msg,
};

/// 4-byte framing prefix for rendezvous UDP datagrams.
pub const MAGIC_RENDEZVOUS: [u8; 4] = [0x01, b'V', b'D', b'Z'];

/// Maximum allowed clock skew for signed `RegisterPeer` timestamps (5 minutes).
const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;

/// UDP socket buffer size requested from the OS.
const SOCKET_BUFFER_BYTES: usize = 4 * 1024 * 1024;

/// Configuration for [`RendezvousServer`].
#[derive(Debug, Clone)]
pub struct RendezvousConfig {
    pub bind: SocketAddr,
    pub relay_addr: Option<SocketAddr>,
    pub peer_ttl: Duration,
    pub keepalive_secs: u32,
}

impl Default for RendezvousConfig {
    fn default() -> Self {
        Self {
            bind: (Ipv4Addr::UNSPECIFIED, DEFAULT_RENDEZVOUS_PORT).into(),
            relay_addr: None,
            peer_ttl: Duration::from_secs(90),
            keepalive_secs: 30,
        }
    }
}

#[derive(Debug, Clone)]
struct PeerRecord {
    device_id: String,
    public_key: [u8; 32],
    public_addr: SocketAddr,
    local_addrs: Vec<String>,
    nat_type: i32,
    last_seen: Instant,
}

/// Self-hosted UDP rendezvous server.
pub struct RendezvousServer {
    cfg: RendezvousConfig,
    sock: UdpSocket,
    local: SocketAddr,
    peers: HashMap<String, PeerRecord>,
    next_punch_id: u64,
}

impl RendezvousServer {
    /// Bind the rendezvous UDP socket.
    pub async fn bind(cfg: RendezvousConfig) -> Result<Self> {
        let std_sock = make_udp_socket(cfg.bind)?;
        let sock = UdpSocket::from_std(std_sock)?;
        let local = sock.local_addr()?;
        tracing::info!(%local, relay = ?cfg.relay_addr, ttl_s = cfg.peer_ttl.as_secs(), "rendezvous server bound");
        Ok(Self { cfg, sock, local, peers: HashMap::new(), next_punch_id: 1 })
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
        let cleanup_period = (self.cfg.peer_ttl / 3).clamp(Duration::from_secs(1), Duration::from_secs(15));
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
                        tracing::info!(local = %self.local, "rendezvous server shutting down");
                        return Ok(());
                    }
                }
            }
        }
    }

    fn evict_expired(&mut self) {
        let now = Instant::now();
        let ttl = self.cfg.peer_ttl;
        let before = self.peers.len();
        self.peers.retain(|_, rec| now.duration_since(rec.last_seen) <= ttl);
        let removed = before - self.peers.len();
        if removed > 0 {
            tracing::debug!(removed, remaining = self.peers.len(), "evicted expired peers");
        }
    }

    fn get_online(&self, id: &str) -> Option<&PeerRecord> {
        let now = Instant::now();
        self.peers.get(id).filter(|rec| now.duration_since(rec.last_seen) <= self.cfg.peer_ttl)
    }

    async fn send_msg(&self, to: SocketAddr, msg: Msg) {
        let env: RendezvousMessage = msg.into();
        let encoded = env.to_vec();
        let mut pkt = Vec::with_capacity(MAGIC_RENDEZVOUS.len() + encoded.len());
        pkt.extend_from_slice(&MAGIC_RENDEZVOUS);
        pkt.extend_from_slice(&encoded);
        if let Err(e) = self.sock.send_to(&pkt, to).await {
            tracing::debug!(%to, error = %e, "rendezvous send_to failed");
        }
    }

    async fn handle_packet(&mut self, data: &[u8], from: SocketAddr) {
        if data.len() < MAGIC_RENDEZVOUS.len() || data[..4] != MAGIC_RENDEZVOUS {
            tracing::trace!(%from, len = data.len(), "ignoring non-rendezvous UDP packet");
            return;
        }
        let Ok(env) = RendezvousMessage::decode(&data[4..]) else {
            tracing::debug!(%from, "failed to decode RendezvousMessage");
            return;
        };
        let Some(msg) = env.msg else {
            return;
        };
        match msg {
            Msg::RegisterPeer(req) => {
                match verify_register_peer(&req, now_unix_ms(), MAX_CLOCK_SKEW_MS) {
                    Ok(pk) => {
                        let id = normalize_id(&req.device_id);
                        tracing::debug!(device_id = %id, %from, local_addrs = ?req.local_addrs, "registered peer");
                        self.peers.insert(
                            id.clone(),
                            PeerRecord {
                                device_id: id,
                                public_key: pk,
                                public_addr: from,
                                local_addrs: req.local_addrs,
                                nat_type: req.nat_type,
                                last_seen: Instant::now(),
                            },
                        );
                        self.send_msg(
                            from,
                            Msg::RegisterOk(RegisterOk {
                                observed_addr: from.to_string(),
                                keepalive_secs: self.cfg.keepalive_secs,
                            }),
                        )
                        .await;
                    }
                    Err(e) => {
                        tracing::warn!(%from, device_id = %req.device_id, error = %e, "rejected RegisterPeer");
                        self.send_msg(
                            from,
                            Msg::Error(RendezvousError { code: 401, message: e.to_string() }),
                        )
                        .await;
                    }
                }
            }
            Msg::PeerLookup(req) => {
                let target = normalize_id(&req.target_id);
                let info = match self.get_online(&target) {
                    Some(rec) => peer_info_online(rec),
                    None => peer_info_offline(target),
                };
                self.send_msg(from, Msg::PeerInfo(info)).await;
            }
            Msg::PunchRequest(req) => {
                let target = normalize_id(&req.target_id);
                let Some(host) = self.get_online(&target).cloned() else {
                    self.send_msg(from, Msg::PeerInfo(peer_info_offline(target))).await;
                    return;
                };
                let punch_id = self.next_punch_id;
                self.next_punch_id = self.next_punch_id.wrapping_add(1).max(1);

                let mut requester_addrs = Vec::with_capacity(1 + req.requester_local_addrs.len());
                let pub_str = from.to_string();
                requester_addrs.push(pub_str.clone());
                for a in req.requester_local_addrs {
                    if a != pub_str && !requester_addrs.contains(&a) {
                        requester_addrs.push(a);
                    }
                }

                // Notify host to start punching towards viewer.
                self.send_msg(
                    host.public_addr,
                    Msg::PunchHole(PunchHole {
                        requester_id: req.requester_id,
                        requester_public_key: req.requester_public_key,
                        requester_addrs,
                        punch_id,
                    }),
                )
                .await;

                // Reply to viewer with host's peer info and addresses.
                self.send_msg(from, Msg::PeerInfo(peer_info_online(&host))).await;
            }
            Msg::PunchAck(ack) => {
                tracing::trace!(%from, punch_id = ack.punch_id, accepted = ack.accepted, "received PunchAck");
            }
            Msg::RelayRequest(req) => {
                let Some(relay_addr) = self.cfg.relay_addr else {
                    self.send_msg(
                        from,
                        Msg::Error(RendezvousError { code: 503, message: "relay server not configured".into() }),
                    )
                    .await;
                    return;
                };
                let target = normalize_id(&req.target_id);
                let Some(host) = self.get_online(&target).cloned() else {
                    self.send_msg(
                        from,
                        Msg::Error(RendezvousError { code: 404, message: format!("peer {target} offline") }),
                    )
                    .await;
                    return;
                };
                let mut token = [0u8; 16];
                OsRng.fill_bytes(&mut token);
                let relay_str = relay_addr.to_string();
                tracing::debug!(host = %host.device_id, viewer = %req.requester_id, %relay_addr, "issuing RelayOffer");

                // Send RelayOffer to host first so it can register at the relay immediately.
                self.send_msg(
                    host.public_addr,
                    Msg::RelayOffer(RelayOffer {
                        relay_addr: relay_str.clone(),
                        token: token.to_vec(),
                        peer_id: req.requester_id,
                    }),
                )
                .await;
                self.send_msg(
                    from,
                    Msg::RelayOffer(RelayOffer {
                        relay_addr: relay_str,
                        token: token.to_vec(),
                        peer_id: host.device_id,
                    }),
                )
                .await;
            }
            _ => {
                tracing::trace!(%from, "ignoring unexpected client rendezvous message");
            }
        }
    }
}

fn peer_info_online(rec: &PeerRecord) -> PeerInfo {
    PeerInfo {
        device_id: rec.device_id.clone(),
        online: true,
        public_key: rec.public_key.to_vec(),
        public_addrs: vec![rec.public_addr.to_string()],
        local_addrs: rec.local_addrs.clone(),
        nat_type: rec.nat_type,
    }
}

fn peer_info_offline(device_id: String) -> PeerInfo {
    PeerInfo { device_id, online: false, ..Default::default() }
}

fn normalize_id(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && *c != '-').collect()
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
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
    use veloxdesk_crypto::DeviceIdentity;
    use veloxdesk_crypto::auth::sign_register_peer;
    use veloxdesk_protocol::rendezvous::{NatType, PeerLookup, PunchRequest, RelayRequest};

    async fn send_rdv(sock: &UdpSocket, to: SocketAddr, msg: Msg) {
        let env: RendezvousMessage = msg.into();
        let mut pkt = MAGIC_RENDEZVOUS.to_vec();
        pkt.extend_from_slice(&env.to_vec());
        sock.send_to(&pkt, to).await.unwrap();
    }

    async fn recv_rdv(sock: &UdpSocket) -> Msg {
        let mut buf = vec![0u8; 4096];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
            .await
            .expect("rdv recv timeout")
            .unwrap();
        assert_eq!(&buf[..4], &MAGIC_RENDEZVOUS);
        RendezvousMessage::decode(&buf[4..n]).unwrap().msg.unwrap()
    }

    #[tokio::test]
    async fn register_lookup_punch_relay_and_reject_forged() {
        let relay_addr: SocketAddr = "127.0.0.1:21117".parse().unwrap();
        let srv = RendezvousServer::bind(RendezvousConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            relay_addr: Some(relay_addr),
            peer_ttl: Duration::from_secs(30),
            keepalive_secs: 15,
        })
        .await
        .unwrap();
        let srv_addr = srv.local_addr();
        let (tx, rx) = watch::channel(false);
        let task = tokio::spawn(srv.run_until(rx));

        let host_id = DeviceIdentity::generate();
        let viewer_id = DeviceIdentity::generate();
        let host_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let viewer_sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // Forged registration is rejected with 401.
        let mut bad = sign_register_peer(&host_id, vec![], NatType::Cone, now_unix_ms());
        bad.signature[0] ^= 0xff;
        send_rdv(&host_sock, srv_addr, Msg::RegisterPeer(bad)).await;
        match recv_rdv(&host_sock).await {
            Msg::Error(e) => assert_eq!(e.code, 401),
            other => panic!("expected Error(401), got {other:?}"),
        }

        // Valid registration succeeds.
        let good = sign_register_peer(
            &host_id,
            vec![host_sock.local_addr().unwrap().to_string()],
            NatType::Cone,
            now_unix_ms(),
        );
        send_rdv(&host_sock, srv_addr, Msg::RegisterPeer(good)).await;
        match recv_rdv(&host_sock).await {
            Msg::RegisterOk(ok) => {
                assert_eq!(ok.observed_addr, host_sock.local_addr().unwrap().to_string());
                assert_eq!(ok.keepalive_secs, 15);
            }
            other => panic!("expected RegisterOk, got {other:?}"),
        }

        // Lookup returns online PeerInfo.
        send_rdv(&viewer_sock, srv_addr, Msg::PeerLookup(PeerLookup { target_id: host_id.device_id() })).await;
        match recv_rdv(&viewer_sock).await {
            Msg::PeerInfo(info) => {
                assert!(info.online);
                assert_eq!(info.public_key, host_id.public_key());
            }
            other => panic!("expected PeerInfo, got {other:?}"),
        }

        // PunchRequest notifies host with PunchHole and viewer with PeerInfo.
        send_rdv(
            &viewer_sock,
            srv_addr,
            Msg::PunchRequest(PunchRequest {
                target_id: host_id.device_id(),
                requester_id: viewer_id.device_id(),
                requester_public_key: viewer_id.public_key().to_vec(),
                requester_local_addrs: vec![viewer_sock.local_addr().unwrap().to_string()],
            }),
        )
        .await;
        match recv_rdv(&host_sock).await {
            Msg::PunchHole(hole) => {
                assert_eq!(hole.requester_id, viewer_id.device_id());
                assert!(hole.punch_id >= 1);
            }
            other => panic!("expected PunchHole, got {other:?}"),
        }
        match recv_rdv(&viewer_sock).await {
            Msg::PeerInfo(info) => assert!(info.online),
            other => panic!("expected PeerInfo, got {other:?}"),
        }

        // RelayRequest issues matching RelayOffer tokens to both peers.
        send_rdv(
            &viewer_sock,
            srv_addr,
            Msg::RelayRequest(RelayRequest {
                target_id: host_id.device_id(),
                requester_id: viewer_id.device_id(),
            }),
        )
        .await;
        let host_offer = match recv_rdv(&host_sock).await {
            Msg::RelayOffer(o) => o,
            other => panic!("expected host RelayOffer, got {other:?}"),
        };
        let viewer_offer = match recv_rdv(&viewer_sock).await {
            Msg::RelayOffer(o) => o,
            other => panic!("expected viewer RelayOffer, got {other:?}"),
        };
        assert_eq!(host_offer.token.len(), 16);
        assert_eq!(host_offer.token, viewer_offer.token);
        assert_eq!(host_offer.relay_addr, relay_addr.to_string());

        let _ = tx.send(true);
        task.await.unwrap().unwrap();
    }
}
