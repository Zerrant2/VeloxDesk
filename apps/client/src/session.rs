//! Interface between the winit/render thread and a remote session (network + decode).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32};

use parking_lot::Mutex;
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::time::ClockSync;
use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::video::{CursorImage, Frame};
use veloxdesk_protocol::control::{DisplayInfo, FileEntry, InputEvent, SessionConfig, SessionMode, VideoConfig};
use veloxdesk_ui::{
    ActiveTunnel, ConnectTarget, FileAction, TerminalAction, TransferProgress, TunnelAction, TunnelSpec,
};

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
        allow_otp: bool,
        allow_perm: bool,
        error: Option<String>,
    },
    Config(Box<SessionConfig>),
    VideoConfig(Box<VideoConfig>),
    DisplayList(Vec<DisplayInfo>),
    PrivacyMode { enabled: bool, error: Option<String> },
    FileListing { path: String, entries: Vec<FileEntry> },
    FileProgress(Option<TransferProgress>),
    FileStatus(String),
    FileError(String),
    TerminalOpened,
    TerminalOutput(Vec<u8>),
    TerminalExit(i32),
    TerminalError(String),
    TunnelAdded(ActiveTunnel),
    TunnelRemoved(u64),
    TunnelError(String),
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
    SetAudio(bool),
    SetMic(bool),
    SetClipboardSync(bool),
    SetPrivacyMode(bool),
    File(FileAction),
    Terminal(TerminalAction),
    Tunnel(TunnelAction),
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
    /// Current viewer audio buffer depth (jitter buffer + playback device) in µs, for the HUD.
    pub audio_buf_us: AtomicU32,
    /// Number of currently attached local physical gamepads (0..=4).
    pub active_gamepads: Arc<AtomicU8>,
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
    /// Request zero-copy GPU textures from `veloxdesk-codec` when `VideoSurface` supports DX12 import.
    pub prefer_gpu_decode: bool,
    pub audio_enabled: bool,
    pub mic_enabled: bool,
    pub clipboard_sync: bool,
    pub initial_tunnels: Vec<TunnelSpec>,
    pub hdr_10bit: bool,
    pub yuv444: bool,
    pub viewer_size: (u32, u32),
    pub viewer_refresh_hz: u32,
    pub language: Arc<AtomicU8>,
}

impl ConnectOptions {
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn lang(&self) -> veloxdesk_ui::Language {
        veloxdesk_ui::Language::from_u8(self.language.load(std::sync::atomic::Ordering::Relaxed))
    }
}

pub struct SessionHandle {
    pub shared: Arc<SessionShared>,
    pub cmd: tokio::sync::mpsc::UnboundedSender<SessionCommand>,
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub done: Option<std::sync::mpsc::Receiver<()>>,
}

impl SessionHandle {
    pub fn send(&self, c: SessionCommand) {
        let _ = self.cmd.send(c);
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        let _ = self.cmd.send(SessionCommand::Disconnect);
        if let Some(rx) = self.done.take() {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(150));
        }
    }
}
