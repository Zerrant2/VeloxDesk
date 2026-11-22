//! # veloxdesk-protocol
//!
//! Versioned wire protocol of VeloxDesk (contract frozen at M0, see `docs/ARCHITECTURE.md` §4).
//!
//! * [`media`] — fixed 24-byte little-endian header for unreliable QUIC datagrams
//!   (video/audio shards with Reed-Solomon FEC metadata). Zero-allocation parse.
//! * [`control`] — protobuf (prost) messages carried on reliable QUIC streams,
//!   wrapped in [`control::Envelope`].
//! * [`rendezvous`] — protobuf messages between peers and the rendezvous/relay server.
//! * [`framing`] — `u32 LE length | protobuf` stream framing and [`StreamKind`] preamble.
//!
//! Compatibility rules: protobuf tags are never reused or renumbered; the media header
//! version nibble is bumped on any layout change.

pub mod control;
pub mod framing;
pub mod media;
pub mod rendezvous;

/// QUIC ALPN for peer-to-peer sessions.
pub const ALPN_SESSION: &[u8] = b"veloxdesk/1";
/// QUIC ALPN for the rendezvous / relay server.
pub const ALPN_RENDEZVOUS: &[u8] = b"veloxdesk-rdv/1";
/// Protocol version advertised in `Hello`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Default UDP port of the rendezvous server.
pub const DEFAULT_RENDEZVOUS_PORT: u16 = 21116;
/// Default UDP port of the relay server.
pub const DEFAULT_RELAY_PORT: u16 = 21117;
/// Default UDP port of a host accepting direct (LAN / IP) connections.
pub const DEFAULT_DIRECT_PORT: u16 = 21118;

/// First byte written on every newly opened QUIC stream.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamKind {
    Control = 1,
    Input = 2,
    Clipboard = 3,
    FileTransfer = 4,
    Tunnel = 5,
    Terminal = 6,
    /// Reliable fallback for audio when datagrams are unavailable.
    Audio = 7,
    /// Reliable fallback for video when datagrams are unavailable.
    Video = 8,
}

impl StreamKind {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Control,
            2 => Self::Input,
            3 => Self::Clipboard,
            4 => Self::FileTransfer,
            5 => Self::Tunnel,
            6 => Self::Terminal,
            7 => Self::Audio,
            8 => Self::Video,
            _ => return None,
        })
    }
}

/// Errors produced while encoding/decoding wire data.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("buffer too short: need {need} bytes, have {have}")]
    Truncated { need: usize, have: usize },
    #[error("unsupported media header version {0}")]
    BadVersion(u8),
    #[error("unknown media kind {0}")]
    BadKind(u8),
    #[error("invalid field: {0}")]
    Invalid(&'static str),
    #[error("message too large: {0} bytes")]
    TooLarge(usize),
    #[error("protobuf decode: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("protobuf encode: {0}")]
    Encode(#[from] prost::EncodeError),
}

pub type Result<T, E = ProtocolError> = std::result::Result<T, E>;
