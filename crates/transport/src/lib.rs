//! # veloxdesk-transport
//!
//! QUIC (quinn) sessions for VeloxDesk (ARCHITECTURE ADR-003/004/005, §4, §6):
//!
//! * [`listen`] / [`connect`] → [`QuicChannel`] (`impl TransportChannel`): mutual-TLS 1.3
//!   authenticated by Ed25519 device keys, RFC 9221 datagrams, one stream per feature
//!   tagged with a [`StreamKind`](veloxdesk_protocol::StreamKind) preamble.
//! * [`framed`] → [`ControlSender`] / [`ControlReceiver`] for protobuf `Envelope`s.
//! * [`VideoSender`] / [`VideoReceiver`]: Reed-Solomon FEC media plane with NACK / PLI and
//!   receiver reports; [`AbrController`] turns reports into encoder bitrate + parity.

pub mod abr;
pub mod control;
pub mod fec;
pub mod p2p;
pub mod quic;
pub mod receiver;

pub use abr::{AbrConfig, AbrController, AbrDecision, UsageState};
pub use control::{ControlReceiver, ControlSender, framed};
pub use fec::{FrameLayout, Packetizer, SendStats, VideoSender};
pub use p2p::{P2pConnectOptions, P2pListener, connect_p2p};
pub use quic::{Listener, QuicChannel, connect, listen, listen_with, low_latency_transport_config};
pub use receiver::{ReceivedFrame, ReceiverConfig, ReceiverStats, VideoReceiver};

pub use veloxdesk_core_traits::transport::{RecvStream, SendStream};
pub use veloxdesk_crypto::tls::PeerPolicy;
