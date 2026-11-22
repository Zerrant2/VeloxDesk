//! # veloxdesk-service
//!
//! Host side of VeloxDesk: configuration/CLI ([`config`]), the per-viewer session
//! ([`session::run_session`]): control handshake → capture thread → encode thread → FEC
//! datagrams, input stream → injector thread, Ping/Pong, ABR, PLI/NACK — and
//! [`run_host`], the QUIC listener accepting sessions.
//!
//! Threading follows ADR-006: capture, encode and injection run on dedicated OS threads
//! connected by "latest frame wins" slots; networking/control is tokio.

pub mod audio;
pub mod auth;
pub mod backends;
pub mod config;
pub mod daemon;
pub mod host;
pub mod inject;
pub mod ipc;
pub mod latest;
pub mod session;
pub mod stream;
pub mod video;

pub use auth::{ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostAuthState, TrustedPeer};
pub use backends::{
    AudioCaptureFactory, AudioEncoderFactory, AudioPlaybackFactory, ClipboardFactory, HostBackends, RateController,
    RateDecision, SimpleRate, VideoSink,
};
pub use config::{HostArgs, HostConfig, PortablePaths, resolve_portable_paths};
pub use daemon::{
    DesktopKind, ServiceStatus, detect_desktop, install_service, run_service_or_console, service_status,
    uninstall_service, wait_for_shutdown,
};
pub use host::{
    HostEventCallback, HostStatusEvent, RunningHost, TransportAbr, TransportVideoSink, run_host,
    run_host_with_backends, serve_p2p_until, serve_until,
};
pub use ipc::{DEFAULT_IPC_PORT, IpcClient, IpcHandler, IpcRequest, IpcResponse, IpcServer};
pub use session::{SessionSummary, run_session, run_session_until};
