//! # veloxdesk-core-traits
//!
//! Frozen (M0) platform-neutral contracts between VeloxDesk subsystems.
//! See `docs/ARCHITECTURE.md` §2–§6.
//!
//! * Media-path traits ([`FrameCapture`], [`VideoEncoder`], [`VideoDecoder`],
//!   [`AudioCapture`], [`AudioEncoder`], [`AudioDecoder`], [`AudioPlayback`],
//!   [`InputInjector`], [`VideoRenderer`]) are **synchronous** and run on dedicated threads.
//! * [`TransportChannel`] is `async` (tokio) and dyn-compatible.
//! * Frames may live on the CPU or the GPU ([`video::FrameData`]) so zero-copy paths can be
//!   expressed without changing signatures.

pub mod audio;
pub mod error;
pub mod input;
pub mod metrics;
pub mod render;
pub mod time;
pub mod transport;
pub mod video;

pub use audio::{AudioCapture, AudioCodec, AudioDecoder, AudioEncoder, AudioPlayback};
pub use error::{CoreError, Result};
pub use input::InputInjector;
pub use metrics::StageTimings;
pub use render::VideoRenderer;
pub use time::Timestamp;
pub use transport::TransportChannel;
pub use video::{FrameCapture, VideoDecoder, VideoEncoder};

/// Re-export of the wire protocol for convenience.
pub use veloxdesk_protocol as protocol;
