//! # veloxdesk-service
//!
//! Host side of VeloxDesk: configuration/CLI ([`config`]), the per-viewer session
//! ([`session::run_session`]): control handshake → capture thread → encode thread → FEC
//! datagrams, input stream → injector thread, Ping/Pong, ABR, PLI/NACK — and
//! [`run_host`], the QUIC listener accepting sessions.
//!
//! Threading follows ADR-006: capture, encode and injection run on dedicated OS threads
//! connected by "latest frame wins" slots; networking/control is tokio.

pub mod auth;
pub mod backends;
pub mod config;
pub mod host;
pub mod inject;
pub mod latest;
pub mod session;
pub mod stream;
pub mod video;

pub use auth::{ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostAuthState, TrustedPeer};
pub use backends::{HostBackends, RateController, RateDecision, SimpleRate, VideoSink};
pub use config::{HostArgs, HostConfig};
pub use host::{
    TransportAbr, TransportVideoSink, run_host, run_host_with_backends, serve_p2p_until, serve_until,
};
pub use session::{SessionSummary, run_session, run_session_until};
