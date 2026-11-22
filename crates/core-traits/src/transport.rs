//! Transport abstraction over a single peer session (QUIC: datagrams + streams).

use std::net::SocketAddr;
use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncWrite};

use veloxdesk_protocol::StreamKind;

use crate::Result;

pub type SendStream = Pin<Box<dyn AsyncWrite + Send + Sync>>;
pub type RecvStream = Pin<Box<dyn AsyncRead + Send + Sync>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathKind {
    #[default]
    Unknown,
    /// Direct LAN / IP connection.
    Direct,
    /// NAT hole-punched P2P.
    P2p,
    /// Via relay server.
    Relay,
}

impl PathKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Direct => "Direct",
            Self::P2p => "P2P Direct",
            Self::Relay => "Relay",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TransportStats {
    pub path: PathKind,
    pub remote_addr: Option<SocketAddr>,
    pub rtt_us: u32,
    /// Estimated congestion window based send capacity (kbps), if known.
    pub estimated_kbps: u32,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub datagrams_sent: u64,
    pub datagrams_received: u64,
    /// Datagrams dropped locally because the send buffer was full.
    pub datagrams_dropped: u64,
    pub lost_packets: u64,
}

/// A bidirectional, authenticated, encrypted channel to one peer.
///
/// Media uses unreliable datagrams (RFC 9221). Every feature opens its own reliable
/// stream, tagged by [`StreamKind`] (the implementation writes/reads the preamble byte).
#[async_trait]
pub trait TransportChannel: Send + Sync {
    /// Largest datagram payload currently accepted (`None` = datagrams unsupported).
    fn max_datagram_size(&self) -> Option<usize>;
    /// Queue a datagram without blocking. Drops (and counts) on full buffer.
    fn send_datagram(&self, data: Bytes) -> Result<()>;
    async fn recv_datagram(&self) -> Result<Bytes>;
    async fn open_bi(&self, kind: StreamKind) -> Result<(SendStream, RecvStream)>;
    async fn open_uni(&self, kind: StreamKind) -> Result<SendStream>;
    /// Accept the next peer-initiated bidirectional stream.
    async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)>;
    /// Accept the next peer-initiated unidirectional stream.
    async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)>;
    fn stats(&self) -> TransportStats;
    /// Peer's authenticated Ed25519 public key (32 bytes), if verified.
    fn peer_public_key(&self) -> Option<[u8; 32]>;
    fn close(&self, code: u32, reason: &str);
}
