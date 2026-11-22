//! Interface between the winit/render thread and a remote session (network + decode).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

use parking_lot::Mutex;
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::time::ClockSync;
use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::video::{CursorImage, Frame};
use veloxdesk_protocol::control::{InputEvent, SessionConfig, SessionMode};
use veloxdesk_ui::ConnectTarget;

use crate::latest::Latest;
use crate::stats::HudAggregator;

/// A decoded frame waiting to be presented.
pub struct DecodedMsg {
    pub frame: Frame,
    pub frame_id: u32,
    pub decode_us: u32,
    pub recv_complete: Timestamp,
}

/// Events posted to the winit loop (via `EventLoopProxy`).
#[derive(Debug)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub enum SessionEvent {
    Status(String),
    PathKind(PathKind),
    PasswordPrompt {
        host_device_id: String,
        host_fingerprint: String,
        methods_hint: String,
        error: Option<String>,
    },
    Config(Box<SessionConfig>),
    CursorShape(CursorImage),
    CursorPos { x: i32, y: i32, visible: bool },
    /// Decoder name / HW flag, for the HUD.
    Decoder { name: String, hardware: bool },
    Closed(String),
}

/// Commands from the UI to the session task.
#[derive(Debug)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub enum SessionCommand {
    SubmitPassword(String),
    CancelAuth,
    Input(InputEvent),
    SwitchDisplay(u32),
    SetMaxBitrate(u32),
    Disconnect,
}

/// State shared between the session threads and the render thread.
#[derive(Default)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub struct SessionShared {
    pub frames: Latest<DecodedMsg>,
    pub clock: Mutex<ClockSync>,
    pub hud: Mutex<HudAggregator>,
    /// Smoothed render time (µs) reported back in `ReceiverReport`.
    pub render_us: AtomicU32,
    pub decode_us: AtomicU32,
}

#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub struct ConnectOptions {
    pub target: ConnectTarget,
    pub rendezvous: Option<SocketAddr>,
    pub relay_only: bool,
    pub password: Option<String>,
    pub mode: SessionMode,
    pub max_bitrate_kbps: u32,
    pub prefer_hw_decode: bool,
    pub viewer_size: (u32, u32),
    pub viewer_refresh_hz: u32,
}

pub struct SessionHandle {
    pub shared: Arc<SessionShared>,
    pub cmd: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
}

impl SessionHandle {
    pub fn send(&self, c: SessionCommand) {
        let _ = self.cmd.send(c);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.cmd.send(SessionCommand::Disconnect);
    }
}
