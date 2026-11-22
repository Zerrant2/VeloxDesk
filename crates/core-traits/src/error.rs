//! Common error type for all subsystem traits.

use std::borrow::Cow;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The backend is not available on this OS / hardware / driver (caller should fall back).
    #[error("unsupported: {0}")]
    Unsupported(Cow<'static, str>),
    /// Requested configuration is invalid or not supported by this backend instance.
    #[error("invalid configuration: {0}")]
    InvalidConfig(Cow<'static, str>),
    /// Device lost / desktop switch / resolution change: recreate the backend.
    #[error("device lost / needs reinit: {0}")]
    Reinit(Cow<'static, str>),
    /// Access denied (UAC secure desktop, missing portal permission, etc.).
    #[error("access denied: {0}")]
    AccessDenied(Cow<'static, str>),
    #[error("timed out")]
    Timeout,
    #[error("closed")]
    Closed,
    #[error("platform error: {0}")]
    Platform(Cow<'static, str>),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol: {0}")]
    Protocol(#[from] veloxdesk_protocol::ProtocolError),
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync + 'static>),
}

impl CoreError {
    pub fn unsupported(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Unsupported(msg.into())
    }
    pub fn platform(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Platform(msg.into())
    }
    pub fn invalid(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::InvalidConfig(msg.into())
    }
    pub fn reinit(msg: impl Into<Cow<'static, str>>) -> Self {
        Self::Reinit(msg.into())
    }
    pub fn other<E: std::error::Error + Send + Sync + 'static>(e: E) -> Self {
        Self::Other(Box::new(e))
    }
}

pub type Result<T, E = CoreError> = std::result::Result<T, E>;
