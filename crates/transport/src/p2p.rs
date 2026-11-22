//! Multiplexed UDP socket (`MuxUdpSocket`), P2P host listener ([`P2pListener`]), and
//! connect-by-ID viewer ([`connect_p2p`]) with IPv6/LAN-first UDP hole punching and
//! automatic zero-knowledge relay fallback (ARCHITECTURE §4, M2).
//!
//! All four traffic classes share ONE UDP socket (zero collision with QUIC RFC 9000
//! fixed bit `0x40`):
//! * `[0x01, b'V', b'D', b'Z'] || protobuf(RendezvousMessage)` — rendezvous signalling
//! * `[0x02, b'V', b'D', b'R'] || token[16] || raw_quic_payload` — zero-knowledge relay
//! * `[0x03, b'V', b'D', b'P'] || punch_id(u64 LE)` — NAT hole-punch probes
//! * `(byte0 & 0x40) != 0` — direct / P2P QUIC packets

use std::collections::HashMap;
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use prost::Message;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::{CoreError, Result};
use veloxdesk_crypto::auth::{generate_nonce, sign_register_peer};
use veloxdesk_crypto::tls::{self, PeerPolicy};
use veloxdesk_crypto::{DeviceIdentity, device_id_from_public_key};
use veloxdesk_protocol::ALPN_SESSION;
use veloxdesk_protocol::rendezvous::{
    NatType, PeerInfo, PeerLookup, PunchAck, PunchRequest, RelayOffer, RelayRequest,
    RendezvousMessage, rendezvous_message::Msg,
};

use crate::quic::{QuicChannel, client_config, make_socket, map_conn_err, server_config};

/// Framing prefix for rendezvous signalling packets.
pub const MAGIC_RENDEZVOUS: [u8; 4] = [0x01, b'V', b'D', b'Z'];
/// Framing prefix for zero-knowledge relay packets.
pub const MAGIC_RELAY: [u8; 4] = [0x02, b'V', b'D', b'R'];
/// Framing prefix for NAT hole-punch probe packets.
pub const MAGIC_PUNCH: [u8; 4] = [0x03, b'V', b'D', b'P'];
/// Relay header length: 4-byte magic + 16-byte session token.
pub const RELAY_HEADER_LEN: usize = 20;

const PUNCHED_ADDR_TTL: Duration = Duration::from_secs(30);
/// Evict idle relay routes after 5 minutes so long-running hosts never leak memory.
const RELAY_ROUTE_TTL_SECS: u64 = 5 * 60;

/// Options for [`connect_p2p`].
#[derive(Debug, Clone)]
pub struct P2pConnectOptions {
    pub rendezvous: SocketAddr,
    /// 9-digit device ID (spaces/dashes are ignored).
    pub target_id: String,
    /// How long to attempt direct / UDP hole-punched P2P before falling back to relay.
    pub direct_timeout: Duration,
    /// Skip direct / P2P attempts and connect immediately through the zero-knowledge relay.
    pub relay_only: bool,
    /// Optional pinned Ed25519 public key; when `None`, the key from `PeerInfo` (verified
    /// to derive `target_id`) is pinned for mutual-TLS 1.3 authentication.
    pub expected_peer: Option<[u8; 32]>,
}

impl P2pConnectOptions {
    pub fn new(rendezvous: SocketAddr, target_id: impl Into<String>) -> Self {
        Self {
            rendezvous,
            target_id: target_id.into(),
            direct_timeout: Duration::from_millis(2500),
            relay_only: false,
            expected_peer: None,
        }
    }
}

// ───────────────────────────── MuxUdpSocket ─────────────────────────────

struct RelayRoute {
    virtual_addr: SocketAddr,
    last_used_secs: Arc<AtomicU64>,
}

struct RelayTable {
    epoch: Instant,
    by_token: HashMap<[u8; 16], RelayRoute>,
    by_virtual: HashMap<SocketAddr, (SocketAddr, [u8; 16], Arc<AtomicU64>)>,
    next_id: u32,
}

impl RelayTable {
    fn new() -> Self {
        Self { epoch: Instant::now(), by_token: HashMap::new(), by_virtual: HashMap::new(), next_id: 0 }
    }

    fn now_secs(&self) -> u64 {
        self.epoch.elapsed().as_secs()
    }

    fn evict_stale(&mut self, ttl_secs: u64) {
        let now = self.now_secs();
        let mut stale_vaddrs = Vec::new();
        self.by_token.retain(|_, route| {
            let last = route.last_used_secs.load(Ordering::Relaxed);
            if now.saturating_sub(last) > ttl_secs {
                stale_vaddrs.push(route.virtual_addr);
                false
            } else {
                true
            }
        });
        for vaddr in stale_vaddrs {
            self.by_virtual.remove(&vaddr);
        }
    }

    fn register(&mut self, local: SocketAddr, relay_addr: SocketAddr, token: [u8; 16]) -> SocketAddr {
        self.evict_stale(RELAY_ROUTE_TTL_SECS);
        let now = self.now_secs();
        if let Some(route) = self.by_token.get(&token) {
            route.last_used_secs.store(now, Ordering::Relaxed);
            return route.virtual_addr;
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = self.next_id;
        let port = 50_000 + (id % 10_000) as u16;
        let vaddr = if local.is_ipv6() {
            SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::new(0xfd00, 0x7664, 0x7200, 0, 0, 0, (id >> 16) as u16, id as u16),
                port,
                0,
                0,
            ))
        } else {
            SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(127, 254, ((id >> 8) & 0xff) as u8, (id & 0xff) as u8),
                port,
            ))
        };
        let last_used = Arc::new(AtomicU64::new(now));
        self.by_token.insert(token, RelayRoute { virtual_addr: vaddr, last_used_secs: last_used.clone() });
        self.by_virtual.insert(vaddr, (relay_addr, token, last_used));
        vaddr
    }
}

/// One UDP socket shared by QUIC (direct + P2P + relayed) and rendezvous / punch signalling.
pub(crate) struct MuxUdpSocket {
    sock: Arc<UdpSocket>,
    local: SocketAddr,
    relays: RwLock<RelayTable>,
    rdv_tx: mpsc::UnboundedSender<(SocketAddr, Vec<u8>)>,
    punch_tx: mpsc::UnboundedSender<(SocketAddr, u64)>,
}

impl fmt::Debug for MuxUdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MuxUdpSocket").field("local", &self.local).finish()
    }
}

type RdvRx = mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>;
type PunchRx = mpsc::UnboundedReceiver<(SocketAddr, u64)>;

impl MuxUdpSocket {
    fn bind(bind: SocketAddr) -> Result<(Arc<Self>, RdvRx, PunchRx)> {
        let std_sock = make_socket(bind)?;
        let sock = UdpSocket::from_std(std_sock)?;
        let local = sock.local_addr()?;
        let (rdv_tx, rdv_rx) = mpsc::unbounded_channel();
        let (punch_tx, punch_rx) = mpsc::unbounded_channel();
        Ok((
            Arc::new(Self {
                sock: Arc::new(sock),
                local,
                relays: RwLock::new(RelayTable::new()),
                rdv_tx,
                punch_tx,
            }),
            rdv_rx,
            punch_rx,
        ))
    }

    fn local_addr_cached(&self) -> SocketAddr {
        self.local
    }

    fn register_relay(&self, relay_addr: SocketAddr, token: [u8; 16]) -> SocketAddr {
        self.relays.write().register(self.local, relay_addr, token)
    }

    fn is_relay_addr(&self, addr: SocketAddr) -> bool {
        self.relays.read().by_virtual.contains_key(&addr)
    }

    async fn send_rendezvous(&self, to: SocketAddr, msg: Msg) -> io::Result<()> {
        let env: RendezvousMessage = msg.into();
        let body = env.to_vec();
        let mut pkt = Vec::with_capacity(MAGIC_RENDEZVOUS.len() + body.len());
        pkt.extend_from_slice(&MAGIC_RENDEZVOUS);
        pkt.extend_from_slice(&body);
        self.sock.send_to(&pkt, normalize_dest(self.local, to)).await?;
        Ok(())
    }

    async fn send_punch(&self, to: SocketAddr, punch_id: u64) -> io::Result<()> {
        let mut pkt = [0u8; 12];
        pkt[..4].copy_from_slice(&MAGIC_PUNCH);
        pkt[4..12].copy_from_slice(&punch_id.to_le_bytes());
        self.sock.send_to(&pkt, normalize_dest(self.local, to)).await?;
        Ok(())
    }

    async fn send_relay_register(&self, relay_addr: SocketAddr, token: [u8; 16]) -> io::Result<()> {
        let mut pkt = [0u8; RELAY_HEADER_LEN];
        pkt[..4].copy_from_slice(&MAGIC_RELAY);
        pkt[4..20].copy_from_slice(&token);
        self.sock.send_to(&pkt, normalize_dest(self.local, relay_addr)).await?;
        Ok(())
    }
}

#[derive(Debug)]
struct TokioUdpPoller {
    sock: Arc<UdpSocket>,
}

impl quinn::UdpPoller for TokioUdpPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.sock.poll_send_ready(cx)
    }
}

impl quinn::AsyncUdpSocket for MuxUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn quinn::UdpPoller>> {
        Box::pin(TokioUdpPoller { sock: self.sock.clone() })
    }

    fn try_send(&self, transmit: &quinn::udp::Transmit) -> io::Result<()> {
        let relay = {
            let table = self.relays.read();
            table.by_virtual.get(&transmit.destination).map(|(addr, tok, last)| {
                let now = table.now_secs();
                if last.load(Ordering::Relaxed) != now {
                    last.store(now, Ordering::Relaxed);
                }
                (*addr, *tok)
            })
        };
        let seg = transmit.segment_size.unwrap_or(transmit.contents.len()).max(1);
        for chunk in transmit.contents.chunks(seg) {
            if let Some((relay_addr, token)) = relay {
                let total = RELAY_HEADER_LEN + chunk.len();
                let target = normalize_dest(self.local, relay_addr);
                if total <= 2048 {
                    let mut buf = [0u8; 2048];
                    buf[..4].copy_from_slice(&MAGIC_RELAY);
                    buf[4..20].copy_from_slice(&token);
                    buf[20..total].copy_from_slice(chunk);
                    self.sock.try_send_to(&buf[..total], target)?;
                } else {
                    let mut buf = Vec::with_capacity(total);
                    buf.extend_from_slice(&MAGIC_RELAY);
                    buf.extend_from_slice(&token);
                    buf.extend_from_slice(chunk);
                    self.sock.try_send_to(&buf, target)?;
                }
            } else {
                self.sock.try_send_to(chunk, normalize_dest(self.local, transmit.destination))?;
            }
        }
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        if bufs.is_empty() || meta.is_empty() {
            return Poll::Ready(Ok(0));
        }
        loop {
            let mut rb = tokio::io::ReadBuf::new(&mut bufs[0]);
            match self.sock.poll_recv_from(cx, &mut rb) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) if e.kind() == io::ErrorKind::ConnectionReset => {
                    // Windows WSAECONNRESET after ICMP Port Unreachable (common during punching).
                    continue;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(from)) => {
                    let n = rb.filled().len();
                    let pkt = &bufs[0][..n];
                    if n >= 4 && pkt[..4] == MAGIC_RENDEZVOUS {
                        let _ = self.rdv_tx.send((from, pkt[4..].to_vec()));
                        continue;
                    }
                    if n >= 12 && pkt[..4] == MAGIC_PUNCH {
                        let punch_id = u64::from_le_bytes(pkt[4..12].try_into().unwrap_or([0; 8]));
                        let _ = self.punch_tx.send((from, punch_id));
                        continue;
                    }
                    if n >= RELAY_HEADER_LEN && pkt[..4] == MAGIC_RELAY {
                        if n == RELAY_HEADER_LEN {
                            continue;
                        }
                        let mut token = [0u8; 16];
                        token.copy_from_slice(&pkt[4..20]);
                        let vaddr = {
                            let table = self.relays.read();
                            table.by_token.get(&token).map(|route| {
                                let now = table.now_secs();
                                if route.last_used_secs.load(Ordering::Relaxed) != now {
                                    route.last_used_secs.store(now, Ordering::Relaxed);
                                }
                                route.virtual_addr
                            })
                        };
                        let Some(vaddr) = vaddr else {
                            continue;
                        };
                        let payload_len = n - RELAY_HEADER_LEN;
                        bufs[0].copy_within(RELAY_HEADER_LEN..n, 0);
                        meta[0] = quinn::udp::RecvMeta {
                            addr: vaddr,
                            len: payload_len,
                            stride: payload_len,
                            ecn: None,
                            dst_ip: None,
                        };
                        return Poll::Ready(Ok(1));
                    }
                    meta[0] = quinn::udp::RecvMeta {
                        addr: from,
                        len: n,
                        stride: n,
                        ecn: None,
                        dst_ip: None,
                    };
                    return Poll::Ready(Ok(1));
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }
}

// ───────────────────────────── P2pListener (host) ─────────────────────────────

/// Host listener multiplexing direct LAN QUIC, rendezvous registration + NAT punching,
/// and zero-knowledge relay tunneling on one UDP socket.
pub struct P2pListener {
    endpoint: quinn::Endpoint,
    mux: Arc<MuxUdpSocket>,
    local: SocketAddr,
    punched_addrs: Arc<RwLock<HashMap<SocketAddr, Instant>>>,
    shutdown_tx: watch::Sender<bool>,
}

impl P2pListener {
    /// Bind a host listener on `bind`. When `rendezvous` is `Some`, registers this device ID
    /// with the rendezvous server, responds to NAT hole-punch requests, and accepts relay offers.
    pub async fn bind(bind: SocketAddr, rendezvous: Option<SocketAddr>, id: &DeviceIdentity) -> Result<Self> {
        let (mux, rdv_rx, punch_rx) = MuxUdpSocket::bind(bind)?;
        let local = mux.local_addr_cached();
        let srv_cfg = server_config(id, &[ALPN_SESSION], PeerPolicy::Any)?;
        let runtime =
            quinn::default_runtime().ok_or_else(|| CoreError::platform("no async runtime (call inside tokio)"))?;
        let endpoint =
            quinn::Endpoint::new_with_abstract_socket(quinn::EndpointConfig::default(), Some(srv_cfg), mux.clone(), runtime)
                .map_err(CoreError::Io)?;

        let punched_addrs: Arc<RwLock<HashMap<SocketAddr, Instant>>> = Arc::new(RwLock::new(HashMap::new()));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        if let Some(rdv_addr) = rendezvous {
            let (reg_tx, reg_rx) = tokio::sync::oneshot::channel::<()>();
            let task_mux = mux.clone();
            let task_punched = punched_addrs.clone();
            let task_id = id.clone();
            tokio::spawn(host_signalling_loop(
                task_mux,
                rdv_addr,
                task_id,
                rdv_rx,
                punch_rx,
                task_punched,
                shutdown_rx,
                Some(reg_tx),
            ));
            // Wait briefly for initial RegisterOk so immediate connect_p2p calls don't race.
            let _ = tokio::time::timeout(Duration::from_millis(1500), reg_rx).await;
        } else {
            // Keep receivers drained in case stray punch/rdv packets arrive on a direct-only host.
            let mut sd = shutdown_rx;
            let mut rdv_rx = rdv_rx;
            let mut punch_rx = punch_rx;
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = rdv_rx.recv() => {}
                        _ = punch_rx.recv() => {}
                        _ = sd.changed() => break,
                    }
                }
            });
        }

        Ok(Self { endpoint, mux, local, punched_addrs, shutdown_tx })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Accept the next authenticated QUIC session with accurate [`PathKind`].
    pub async fn accept(&self) -> Result<QuicChannel> {
        loop {
            let incoming = self.endpoint.accept().await.ok_or(CoreError::Closed)?;
            let remote = incoming.remote_address();
            match incoming.await {
                Ok(conn) => {
                    let path = if self.mux.is_relay_addr(remote) {
                        PathKind::Relay
                    } else if self.is_punched(remote) {
                        PathKind::P2p
                    } else {
                        PathKind::Direct
                    };
                    tracing::debug!(%remote, ?path, "accepted P2pListener session");
                    return Ok(QuicChannel::from_connection(conn, path, None));
                }
                Err(e) => tracing::warn!(%remote, error = %e, "incoming P2P QUIC handshake failed"),
            }
        }
    }

    /// Stop background rendezvous signalling and close the QUIC endpoint.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        self.endpoint.close(quinn::VarInt::from_u32(0), b"shutdown");
    }

    fn is_punched(&self, remote: SocketAddr) -> bool {
        let now = Instant::now();
        let mut map = self.punched_addrs.write();
        map.retain(|_, ts| now.duration_since(*ts) <= PUNCHED_ADDR_TTL);
        if map.contains_key(&remote) {
            return true;
        }
        // Also match IPv4-mapped IPv6 <-> IPv4 representations.
        if let Some(alt) = unmap_v4_or_map_v6(remote) {
            return map.contains_key(&alt);
        }
        false
    }
}

#[allow(clippy::too_many_arguments)]
async fn host_signalling_loop(
    mux: Arc<MuxUdpSocket>,
    rdv_addr: SocketAddr,
    id: DeviceIdentity,
    mut rdv_rx: mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>,
    mut punch_rx: mpsc::UnboundedReceiver<(SocketAddr, u64)>,
    punched_addrs: Arc<RwLock<HashMap<SocketAddr, Instant>>>,
    mut shutdown: watch::Receiver<bool>,
    mut registered_tx: Option<tokio::sync::oneshot::Sender<()>>,
) {
    let local_addrs = discover_local_addrs(mux.local_addr_cached(), rdv_addr);
    let mut registered = false;
    let mut keepalive = Duration::from_secs(30);

    // Send initial RegisterPeer immediately.
    let reg = sign_register_peer(&id, local_addrs.clone(), NatType::Unknown, now_unix_ms());
    let _ = mux.send_rendezvous(rdv_addr, Msg::RegisterPeer(reg)).await;

    let mut retry_tick = tokio::time::interval(Duration::from_millis(250));
    retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    retry_tick.tick().await; // consume immediate tick

    let mut next_keepalive = Instant::now() + keepalive;

    loop {
        if *shutdown.borrow() {
            return;
        }
        tokio::select! {
            _ = retry_tick.tick() => {
                if !registered || Instant::now() >= next_keepalive {
                    let msg = sign_register_peer(&id, local_addrs.clone(), NatType::Unknown, now_unix_ms());
                    let _ = mux.send_rendezvous(rdv_addr, Msg::RegisterPeer(msg)).await;
                    if registered {
                        next_keepalive = Instant::now() + keepalive;
                    }
                }
            }
            Some((from, payload)) = rdv_rx.recv() => {
                if !same_endpoint(from, rdv_addr) {
                    tracing::debug!(%from, expected = %rdv_addr, "dropping spoofed rendezvous packet on host");
                    continue;
                }
                let Ok(env) = RendezvousMessage::decode(payload.as_slice()) else { continue };
                let Some(msg) = env.msg else { continue };
                match msg {
                    Msg::RegisterOk(ok) => {
                        registered = true;
                        let secs = ok.keepalive_secs.clamp(5, 60) as u64;
                        keepalive = Duration::from_secs(secs);
                        next_keepalive = Instant::now() + keepalive;
                        if let Some(tx) = registered_tx.take() {
                            let _ = tx.send(());
                        }
                        tracing::debug!(observed = %ok.observed_addr, keepalive_s = secs, "registered with rendezvous");
                    }
                    Msg::PunchHole(hole) => {
                        let targets = parse_compatible_addrs(mux.local_addr_cached(), &hole.requester_addrs);
                        {
                            let mut map = punched_addrs.write();
                            let now = Instant::now();
                            for &t in &targets {
                                map.insert(t, now);
                            }
                        }
                        let _ = mux.send_rendezvous(
                            rdv_addr,
                            Msg::PunchAck(PunchAck { punch_id: hole.punch_id, accepted: true }),
                        ).await;
                        // Spawn a short punch probe burst to open NAT mappings.
                        let pmux = mux.clone();
                        let punch_id = hole.punch_id;
                        tokio::spawn(async move {
                            for _ in 0..8 {
                                for &t in &targets {
                                    let _ = pmux.send_punch(t, punch_id).await;
                                }
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        });
                    }
                    Msg::RelayOffer(offer) => {
                        if let Some(relay_addr) = resolve_relay_addr(&offer.relay_addr, rdv_addr)
                            && let Ok(token) = <[u8; 16]>::try_from(offer.token.as_slice())
                        {
                            let _vaddr = mux.register_relay(relay_addr, token);
                            let rmux = mux.clone();
                            tokio::spawn(async move {
                                for _ in 0..12 {
                                    let _ = rmux.send_relay_register(relay_addr, token).await;
                                    tokio::time::sleep(Duration::from_millis(200)).await;
                                }
                            });
                        }
                    }
                    Msg::Error(e) => {
                        tracing::warn!(code = e.code, msg = %e.message, "rendezvous server error");
                    }
                    _ => {}
                }
            }
            Some((from, punch_id)) = punch_rx.recv() => {
                punched_addrs.write().insert(from, Instant::now());
                let _ = mux.send_punch(from, punch_id).await;
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return;
                }
            }
        }
    }
}

// ───────────────────────────── connect_p2p (viewer) ─────────────────────────────

/// Connect to a host by 9-digit Device ID via rendezvous lookup + IPv6/LAN-first UDP hole
/// punching, with automatic fallback to zero-knowledge relay (or immediate relay when
/// `opts.relay_only` is set).
pub async fn connect_p2p(opts: P2pConnectOptions, id: &DeviceIdentity) -> Result<QuicChannel> {
    let target_id = normalize_id(&opts.target_id);
    if target_id.is_empty() {
        return Err(CoreError::invalid("target_id must not be empty"));
    }

    let bind: SocketAddr = if opts.rendezvous.is_ipv6() {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    };
    let (mux, mut rdv_rx, mut punch_rx) = MuxUdpSocket::bind(bind)?;
    let local = mux.local_addr_cached();
    let runtime =
        quinn::default_runtime().ok_or_else(|| CoreError::platform("no async runtime (call inside tokio)"))?;
    let endpoint =
        quinn::Endpoint::new_with_abstract_socket(quinn::EndpointConfig::default(), None, mux.clone(), runtime)
            .map_err(CoreError::Io)?;

    let local_addrs = discover_local_addrs(local, opts.rendezvous);

    // 1. Discover host public key & candidate addresses (PunchRequest also alerts host to punch).
    let req_msg = if opts.relay_only {
        Msg::PeerLookup(PeerLookup { target_id: target_id.clone() })
    } else {
        Msg::PunchRequest(PunchRequest {
            target_id: target_id.clone(),
            requester_id: id.device_id(),
            requester_public_key: id.public_key().to_vec(),
            requester_local_addrs: local_addrs,
        })
    };

    let peer_info = request_peer_info(&mux, &mut rdv_rx, opts.rendezvous, req_msg, Duration::from_secs(3)).await?;
    if !peer_info.online {
        return Err(CoreError::other(io::Error::other(format!("peer {target_id} is offline"))));
    }

    let peer_pk: [u8; 32] = peer_info
        .public_key
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::other(io::Error::other("rendezvous returned invalid peer public_key length")))?;
    if device_id_from_public_key(&peer_pk) != target_id {
        return Err(CoreError::other(io::Error::other(
            "rendezvous returned public_key that does not match target_id",
        )));
    }

    // Pin the expected peer public key in mutual-TLS 1.3 (uses explicit expected_peer if set,
    // otherwise the verified PeerInfo key).
    let pinned_pk = opts.expected_peer.unwrap_or(peer_pk);
    let client_cfg = client_config(id, &[ALPN_SESSION], PeerPolicy::Exact(pinned_pk))?;

    // 2. Direct / NAT hole-punched P2P attempt (IPv6 -> LAN -> Public).
    if !opts.relay_only {
        let candidates = build_candidate_list(local, &peer_info);
        if !candidates.is_empty() {
            match try_direct_p2p(&endpoint, &mux, &mut punch_rx, &client_cfg, &candidates, opts.direct_timeout).await {
                Ok(conn) => {
                    return Ok(QuicChannel::from_connection(conn, PathKind::P2p, Some(endpoint)));
                }
                Err(DirectFailure::TlsRejected(err)) => {
                    // Peer identity mismatch / TLS failure: do NOT fall back to relay.
                    return Err(err);
                }
                Err(DirectFailure::TimedOutOrUnreachable(reason)) => {
                    tracing::info!(%target_id, %reason, "direct/P2P timed out; falling back to zero-knowledge relay");
                }
            }
        }
    }

    // 3. Zero-knowledge relay fallback.
    let offer = request_relay_offer(
        &mux,
        &mut rdv_rx,
        opts.rendezvous,
        RelayRequest { target_id: target_id.clone(), requester_id: id.device_id() },
        Duration::from_secs(3),
    )
    .await?;

    let relay_addr = resolve_relay_addr(&offer.relay_addr, opts.rendezvous)
        .ok_or_else(|| CoreError::other(io::Error::other("rendezvous returned invalid relay_addr")))?;
    let token: [u8; 16] = offer
        .token
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::other(io::Error::other("rendezvous returned invalid relay token length")))?;

    let vaddr = mux.register_relay(relay_addr, token);
    let _ = mux.send_relay_register(relay_addr, token).await;

    // Keep sending empty register probes while connecting in case of UDP packet loss.
    let (ka_tx, mut ka_rx) = watch::channel(false);
    let ka_mux = mux.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.tick().await;
        for _ in 0..25 {
            tokio::select! {
                _ = tick.tick() => { let _ = ka_mux.send_relay_register(relay_addr, token).await; }
                _ = ka_rx.changed() => break,
            }
        }
    });

    let connecting = endpoint
        .connect_with(client_cfg, vaddr, tls::PEER_SERVER_NAME)
        .map_err(CoreError::other)?;
    let conn = tokio::time::timeout(Duration::from_secs(6), connecting)
        .await
        .map_err(|_| CoreError::Timeout)?
        .map_err(map_conn_err)?;
    let _ = ka_tx.send(true);

    Ok(QuicChannel::from_connection(conn, PathKind::Relay, Some(endpoint)))
}

enum DirectFailure {
    TlsRejected(CoreError),
    TimedOutOrUnreachable(String),
}

async fn try_direct_p2p(
    endpoint: &quinn::Endpoint,
    mux: &Arc<MuxUdpSocket>,
    punch_rx: &mut mpsc::UnboundedReceiver<(SocketAddr, u64)>,
    client_cfg: &quinn::ClientConfig,
    candidates: &[SocketAddr],
    timeout: Duration,
) -> std::result::Result<quinn::Connection, DirectFailure> {
    let punch_id = u64::from_le_bytes(generate_nonce()[..8].try_into().unwrap());

    // Send initial punch probes to all candidates to open our NAT pinhole.
    for &addr in candidates {
        let _ = mux.send_punch(addr, punch_id).await;
    }

    // Brief wait for a punch echo (max 40 ms) so symmetric/cone pinholes open before Initial.
    let _ = tokio::time::timeout(Duration::from_millis(40), punch_rx.recv()).await;

    // Background punch refresher during the direct attempt.
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let pmux = mux.clone();
    let p_cands = candidates.to_vec();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        tick.tick().await;
        for _ in 0..10 {
            tokio::select! {
                _ = tick.tick() => {
                    for &a in &p_cands {
                        let _ = pmux.send_punch(a, punch_id).await;
                    }
                }
                _ = stop_rx.changed() => break,
            }
        }
    });

    // Race candidates with Happy-Eyeballs stagger (120 ms between subsequent candidates).
    let (win_tx, mut win_rx) = mpsc::channel::<std::result::Result<quinn::Connection, quinn::ConnectionError>>(candidates.len().max(1));
    let mut handles = Vec::with_capacity(candidates.len());
    for (idx, &addr) in candidates.iter().enumerate() {
        let ep = endpoint.clone();
        let cfg = client_cfg.clone();
        let tx = win_tx.clone();
        let stagger = Duration::from_millis(120 * idx as u64);
        handles.push(tokio::spawn(async move {
            if !stagger.is_zero() {
                tokio::time::sleep(stagger).await;
            }
            let Ok(connecting) = ep.connect_with(cfg, addr, tls::PEER_SERVER_NAME) else {
                return;
            };
            let _ = tx.send(connecting.await).await;
        }));
    }
    drop(win_tx);

    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_err = String::from("direct timeout");
    let result = loop {
        match tokio::time::timeout_at(deadline, win_rx.recv()).await {
            Ok(Some(Ok(conn))) => break Ok(conn),
            Ok(Some(Err(e))) => {
                if is_tls_or_crypto_error(&e) {
                    break Err(DirectFailure::TlsRejected(map_conn_err(e)));
                }
                last_err = e.to_string();
            }
            Ok(None) => break Err(DirectFailure::TimedOutOrUnreachable(last_err)),
            Err(_) => break Err(DirectFailure::TimedOutOrUnreachable(last_err)),
        }
    };

    let _ = stop_tx.send(true);
    for h in handles {
        h.abort();
    }
    result
}

fn is_tls_or_crypto_error(e: &quinn::ConnectionError) -> bool {
    matches!(
        e,
        quinn::ConnectionError::TransportError(te) if u64::from(te.code) >= 0x100 && u64::from(te.code) <= 0x1ff
    )
}

async fn request_peer_info(
    mux: &MuxUdpSocket,
    rdv_rx: &mut mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>,
    rdv_addr: SocketAddr,
    req: Msg,
    timeout: Duration,
) -> Result<PeerInfo> {
    let deadline = tokio::time::Instant::now() + timeout;
    let _ = mux.send_rendezvous(rdv_addr, req.clone()).await;
    let mut retry = tokio::time::interval(Duration::from_millis(300));
    retry.tick().await;

    loop {
        tokio::select! {
            _ = retry.tick() => {
                let _ = mux.send_rendezvous(rdv_addr, req.clone()).await;
            }
            res = tokio::time::timeout_at(deadline, rdv_rx.recv()) => {
                match res {
                    Ok(Some((from, bytes))) => {
                        if !same_endpoint(from, rdv_addr) {
                            tracing::debug!(%from, expected = %rdv_addr, "dropping spoofed rendezvous PeerInfo packet");
                            continue;
                        }
                        let Ok(env) = RendezvousMessage::decode(bytes.as_slice()) else { continue };
                        match env.msg {
                            Some(Msg::PeerInfo(info)) => return Ok(info),
                            Some(Msg::Error(e)) => {
                                return Err(CoreError::other(io::Error::other(format!(
                                    "rendezvous error {}: {}",
                                    e.code, e.message
                                ))));
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => return Err(CoreError::Closed),
                    Err(_) => return Err(CoreError::Timeout),
                }
            }
        }
    }
}

async fn request_relay_offer(
    mux: &MuxUdpSocket,
    rdv_rx: &mut mpsc::UnboundedReceiver<(SocketAddr, Vec<u8>)>,
    rdv_addr: SocketAddr,
    req: RelayRequest,
    timeout: Duration,
) -> Result<RelayOffer> {
    let deadline = tokio::time::Instant::now() + timeout;
    let _ = mux.send_rendezvous(rdv_addr, Msg::RelayRequest(req.clone())).await;
    let mut retry = tokio::time::interval(Duration::from_millis(300));
    retry.tick().await;

    loop {
        tokio::select! {
            _ = retry.tick() => {
                let _ = mux.send_rendezvous(rdv_addr, Msg::RelayRequest(req.clone())).await;
            }
            res = tokio::time::timeout_at(deadline, rdv_rx.recv()) => {
                match res {
                    Ok(Some((from, bytes))) => {
                        if !same_endpoint(from, rdv_addr) {
                            tracing::debug!(%from, expected = %rdv_addr, "dropping spoofed rendezvous RelayOffer packet");
                            continue;
                        }
                        let Ok(env) = RendezvousMessage::decode(bytes.as_slice()) else { continue };
                        match env.msg {
                            Some(Msg::RelayOffer(offer)) => return Ok(offer),
                            Some(Msg::Error(e)) => {
                                return Err(CoreError::other(io::Error::other(format!(
                                    "relay error {}: {}",
                                    e.code, e.message
                                ))));
                            }
                            _ => {}
                        }
                    }
                    Ok(None) => return Err(CoreError::Closed),
                    Err(_) => return Err(CoreError::Timeout),
                }
            }
        }
    }
}

// ───────────────────────────── address helpers ─────────────────────────────

fn normalize_id(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace() && *c != '-').collect()
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn discover_local_addrs(local: SocketAddr, rdv_addr: SocketAddr) -> Vec<String> {
    let mut out = Vec::new();
    let port = local.port();
    if !local.ip().is_unspecified() {
        out.push(local.to_string());
    } else {
        // Probe outbound interface IP used to reach the rendezvous server.
        let bind_any: SocketAddr = if rdv_addr.is_ipv6() {
            (Ipv6Addr::UNSPECIFIED, 0).into()
        } else {
            (Ipv4Addr::UNSPECIFIED, 0).into()
        };
        if let Ok(probe) = std::net::UdpSocket::bind(bind_any)
            && probe.connect(rdv_addr).is_ok()
            && let Ok(la) = probe.local_addr()
            && !la.ip().is_unspecified()
        {
            out.push(SocketAddr::new(la.ip(), port).to_string());
        }
        if out.is_empty() && rdv_addr.ip().is_loopback() {
            let lb = if rdv_addr.is_ipv6() {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
            } else {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
            };
            out.push(lb.to_string());
        }
    }
    out
}

fn normalize_dest(local: SocketAddr, dest: SocketAddr) -> SocketAddr {
    match (local, dest) {
        (SocketAddr::V6(_), SocketAddr::V4(v4)) => {
            SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))
        }
        _ => dest,
    }
}

fn unmap_v4_or_map_v6(addr: SocketAddr) -> Option<SocketAddr> {
    match addr {
        SocketAddr::V6(v6) => v6.ip().to_ipv4_mapped().map(|v4| SocketAddr::V4(SocketAddrV4::new(v4, v6.port()))),
        SocketAddr::V4(v4) => Some(SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))),
    }
}

fn is_compatible(local: SocketAddr, candidate: SocketAddr) -> bool {
    match (local, candidate) {
        (SocketAddr::V4(_), SocketAddr::V4(_)) => true,
        (SocketAddr::V6(_), SocketAddr::V6(_)) => true,
        (SocketAddr::V6(_), SocketAddr::V4(_)) => true, // dual-stack mapped
        (SocketAddr::V4(_), SocketAddr::V6(_)) => false,
    }
}

fn parse_compatible_addrs(local: SocketAddr, raw: &[String]) -> Vec<SocketAddr> {
    let mut out = Vec::new();
    for s in raw {
        if let Ok(a) = s.parse::<SocketAddr>()
            && is_compatible(local, a)
        {
            let norm = normalize_dest(local, a);
            if !out.contains(&norm) {
                out.push(norm);
            }
        }
    }
    out
}

/// Priority: IPv6 first, then local/LAN addrs, then public reflexive addrs.
fn build_candidate_list(local: SocketAddr, info: &PeerInfo) -> Vec<SocketAddr> {
    let mut v6 = Vec::new();
    let mut lan = Vec::new();
    let mut pub_v4 = Vec::new();

    for (is_local, list) in [(true, &info.local_addrs), (false, &info.public_addrs)] {
        for s in list {
            let Ok(a) = s.parse::<SocketAddr>() else { continue };
            if !is_compatible(local, a) {
                continue;
            }
            if a.is_ipv6() {
                if !v6.contains(&a) {
                    v6.push(a);
                }
            } else {
                let norm = normalize_dest(local, a);
                if is_local {
                    if !lan.contains(&norm) {
                        lan.push(norm);
                    }
                } else if !lan.contains(&norm) && !pub_v4.contains(&norm) {
                    pub_v4.push(norm);
                }
            }
        }
    }
    let mut out = Vec::with_capacity(v6.len() + lan.len() + pub_v4.len());
    out.extend(v6);
    out.extend(lan);
    out.extend(pub_v4);
    out
}

/// Compare two UDP endpoints, treating IPv4 and IPv4-mapped IPv6 (`::ffff:a.b.c.d`) as equal.
fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(IpAddr::V6(v6), IpAddr::V4),
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

fn same_endpoint(a: SocketAddr, b: SocketAddr) -> bool {
    a.port() == b.port() && canonical_ip(a.ip()) == canonical_ip(b.ip())
}

/// Parse `RelayOffer.relay_addr`, substituting `rdv_addr.ip()` when the server advertises
/// an unspecified bind IP (`0.0.0.0` or `::`).
fn resolve_relay_addr(raw: &str, rdv_addr: SocketAddr) -> Option<SocketAddr> {
    let parsed = raw.parse::<SocketAddr>().ok()?;
    if parsed.ip().is_unspecified() {
        Some(SocketAddr::new(rdv_addr.ip(), parsed.port()))
    } else {
        Some(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_endpoint_matches_v4_and_v4_mapped_v6() {
        let a: SocketAddr = "127.0.0.1:21116".parse().unwrap();
        let b: SocketAddr = "[::ffff:127.0.0.1]:21116".parse().unwrap();
        let c: SocketAddr = "127.0.0.1:21117".parse().unwrap();
        let d: SocketAddr = "10.0.0.1:21116".parse().unwrap();
        assert!(same_endpoint(a, a));
        assert!(same_endpoint(a, b));
        assert!(!same_endpoint(a, c));
        assert!(!same_endpoint(a, d));
    }

    #[test]
    fn resolve_relay_addr_substitutes_unspecified_ip() {
        let rdv_v4: SocketAddr = "198.51.100.10:21116".parse().unwrap();
        assert_eq!(resolve_relay_addr("0.0.0.0:21117", rdv_v4), Some("198.51.100.10:21117".parse().unwrap()));
        assert_eq!(resolve_relay_addr("[::]:21117", rdv_v4), Some("198.51.100.10:21117".parse().unwrap()));
        assert_eq!(resolve_relay_addr("203.0.113.5:21117", rdv_v4), Some("203.0.113.5:21117".parse().unwrap()));
        assert_eq!(resolve_relay_addr("not-an-addr", rdv_v4), None);

        let rdv_v6: SocketAddr = "[2001:db8::1]:21116".parse().unwrap();
        assert_eq!(resolve_relay_addr("[::]:21117", rdv_v6), Some("[2001:db8::1]:21117".parse().unwrap()));
    }

    #[test]
    fn relay_table_evicts_stale_entries() {
        let mut table = RelayTable::new();
        let local: SocketAddr = "127.0.0.1:21118".parse().unwrap();
        let relay: SocketAddr = "127.0.0.1:21117".parse().unwrap();
        let tok_old = [1u8; 16];
        let tok_fresh = [2u8; 16];

        let v_old = table.register(local, relay, tok_old);
        let v_fresh = table.register(local, relay, tok_fresh);
        assert_eq!(table.by_token.len(), 2);
        assert_eq!(table.by_virtual.len(), 2);

        // Simulate `epoch` advancing by backdating `epoch` and keeping `tok_fresh` recent.
        table.epoch = Instant::now() - Duration::from_secs(RELAY_ROUTE_TTL_SECS + 10);
        table.by_token.get(&tok_fresh).unwrap().last_used_secs.store(table.now_secs(), Ordering::Relaxed);

        let tok_new = [3u8; 16];
        let _v_new = table.register(local, relay, tok_new);

        assert!(!table.by_token.contains_key(&tok_old));
        assert!(!table.by_virtual.contains_key(&v_old));
        assert!(table.by_token.contains_key(&tok_fresh));
        assert!(table.by_virtual.contains_key(&v_fresh));
        assert!(table.by_token.contains_key(&tok_new));
    }
}
