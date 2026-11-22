//! Rendezvous / relay protobuf messages (peer ↔ `veloxdesk-server`).
//!
//! Flow (ARCHITECTURE §4, M2):
//! 1. Host → server `RegisterPeer` (signed with its Ed25519 key) every 30 s, keeps NAT mapping.
//! 2. Viewer → server `PunchRequest{target_id}`; server → host `PunchHole{viewer addrs}`,
//!    server → viewer `PeerInfo{host addrs}`; both sides send QUIC Initial packets at each
//!    other simultaneously (UDP hole punching), IPv6 first.
//! 3. On failure/timeout (or symmetric NAT) viewer sends `RelayRequest`; server allocates a
//!    relay session and returns `RelayOffer` to both sides; the end-to-end QUIC/TLS session is
//!    tunnelled through the relay, which sees ciphertext only.

use prost::Message as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum NatType {
    Unknown = 0,
    Open = 1,
    Cone = 2,
    Symmetric = 3,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RegisterPeer {
    #[prost(string, tag = "1")]
    pub device_id: String,
    #[prost(bytes = "vec", tag = "2")]
    pub public_key: Vec<u8>,
    /// Local (LAN) socket addresses as "ip:port" strings, for same-LAN shortcut.
    #[prost(string, repeated, tag = "3")]
    pub local_addrs: Vec<String>,
    #[prost(enumeration = "NatType", tag = "4")]
    pub nat_type: i32,
    #[prost(uint64, tag = "5")]
    pub timestamp_unix_ms: u64,
    /// Ed25519 signature over `device_id || public_key || timestamp_unix_ms (LE)`.
    #[prost(bytes = "vec", tag = "6")]
    pub signature: Vec<u8>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RegisterOk {
    /// Public address of the peer as seen by the server (reflexive address).
    #[prost(string, tag = "1")]
    pub observed_addr: String,
    #[prost(uint32, tag = "2")]
    pub keepalive_secs: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PeerLookup {
    #[prost(string, tag = "1")]
    pub target_id: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PeerInfo {
    #[prost(string, tag = "1")]
    pub device_id: String,
    #[prost(bool, tag = "2")]
    pub online: bool,
    #[prost(bytes = "vec", tag = "3")]
    pub public_key: Vec<u8>,
    #[prost(string, repeated, tag = "4")]
    pub public_addrs: Vec<String>,
    #[prost(string, repeated, tag = "5")]
    pub local_addrs: Vec<String>,
    #[prost(enumeration = "NatType", tag = "6")]
    pub nat_type: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PunchRequest {
    #[prost(string, tag = "1")]
    pub target_id: String,
    #[prost(string, tag = "2")]
    pub requester_id: String,
    #[prost(bytes = "vec", tag = "3")]
    pub requester_public_key: Vec<u8>,
    #[prost(string, repeated, tag = "4")]
    pub requester_local_addrs: Vec<String>,
}

/// Server → host: a viewer wants to connect; start punching towards these addresses.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PunchHole {
    #[prost(string, tag = "1")]
    pub requester_id: String,
    #[prost(bytes = "vec", tag = "2")]
    pub requester_public_key: Vec<u8>,
    #[prost(string, repeated, tag = "3")]
    pub requester_addrs: Vec<String>,
    #[prost(uint64, tag = "4")]
    pub punch_id: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PunchAck {
    #[prost(uint64, tag = "1")]
    pub punch_id: u64,
    #[prost(bool, tag = "2")]
    pub accepted: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RelayRequest {
    #[prost(string, tag = "1")]
    pub target_id: String,
    #[prost(string, tag = "2")]
    pub requester_id: String,
}

/// Server → both peers: connect to `relay_addr` and present `token`.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RelayOffer {
    #[prost(string, tag = "1")]
    pub relay_addr: String,
    /// Random 128-bit session token (pairs the two halves; not a crypto secret for E2EE).
    #[prost(bytes = "vec", tag = "2")]
    pub token: Vec<u8>,
    #[prost(string, tag = "3")]
    pub peer_id: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendezvousError {
    #[prost(uint32, tag = "1")]
    pub code: u32,
    #[prost(string, tag = "2")]
    pub message: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RendezvousMessage {
    #[prost(oneof = "rendezvous_message::Msg", tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10")]
    pub msg: Option<rendezvous_message::Msg>,
}

pub mod rendezvous_message {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Msg {
        #[prost(message, tag = "1")]
        RegisterPeer(super::RegisterPeer),
        #[prost(message, tag = "2")]
        RegisterOk(super::RegisterOk),
        #[prost(message, tag = "3")]
        PeerLookup(super::PeerLookup),
        #[prost(message, tag = "4")]
        PeerInfo(super::PeerInfo),
        #[prost(message, tag = "5")]
        PunchRequest(super::PunchRequest),
        #[prost(message, tag = "6")]
        PunchHole(super::PunchHole),
        #[prost(message, tag = "7")]
        PunchAck(super::PunchAck),
        #[prost(message, tag = "8")]
        RelayRequest(super::RelayRequest),
        #[prost(message, tag = "9")]
        RelayOffer(super::RelayOffer),
        #[prost(message, tag = "10")]
        Error(super::RendezvousError),
    }
}

impl RendezvousMessage {
    pub fn to_vec(&self) -> Vec<u8> {
        self.encode_to_vec()
    }
}

impl From<rendezvous_message::Msg> for RendezvousMessage {
    fn from(m: rendezvous_message::Msg) -> Self {
        Self { msg: Some(m) }
    }
}
