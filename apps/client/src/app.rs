//! winit application: connect screen, remote session view, test-pattern benchmark.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::metrics::{HudSnapshot, LatencySamples};
use veloxdesk_core_traits::render::VideoRenderer;
use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::video::{CursorImage, Frame};
use veloxdesk_protocol::control::{SessionConfig, SystemPowerAction, VideoCodec};
use veloxdesk_render::hud::{HudLabels, hud_line, is_toggle_shortcut};
use veloxdesk_render::pattern::nv12_test_frame;
use veloxdesk_render::winit::application::ApplicationHandler;
use veloxdesk_render::winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
use veloxdesk_render::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use veloxdesk_render::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use veloxdesk_render::winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};
use veloxdesk_render::{RenderBreakdown, SurfaceOptions, VideoSurface, egui};
use veloxdesk_ui::{
    AcceptDecision, AcceptPrompt, ChatAction, ChatWindow, ConnectAction, ConnectScreen, ConnectTarget, FileAction,
    FileManagerWindow, Language, Mode, PasswordAction, PasswordMode, PasswordPrompt, Present, ReconnectAction,
    ReconnectBannerState, ReconnectOverlay, SessionInfo, SessionToolbar, Settings, TailscaleInfo, TerminalAction,
    TerminalWindow, ToolbarAction, TunnelAction, TunnelSpec, TunnelsWindow, UpdateUiState, format_device_id,
    parse_rendezvous_addr_i18n, parse_target_i18n, settings_window_with_security, tr,
};

use crate::embedded_host::{self, EmbeddedHostAuthOpts, EmbeddedHostEvent, EmbeddedHostHandle};
use crate::input::{self, RelativeAccum, WheelAccum};
use crate::latest::Latest;
use crate::session::{ConnectOptions, SessionCommand, SessionEvent, SessionHandle};
use crate::stats::PresentedFrame;
use crate::tailscale;
use crate::tray::{TrayAction, TrayHandle};
use crate::updater::{self, ReleaseInfo, UpdateCheckOutcome};

fn hud_labels_for(lang: Language) -> HudLabels {
    let t = tr(lang);
    HudLabels {
        capture: t.hud_capture,
        encode: t.hud_encode,
        net: t.hud_net,
        rtt: t.hud_rtt,
        decode: t.hud_decode,
        render: t.hud_render,
        total: t.hud_total,
        ms: t.hud_ms,
        fps: t.hud_fps,
        mbps: t.hud_mbps,
        loss: t.hud_loss,
        fec_rec: t.hud_fec_rec,
        decode_prefix: t.hud_decode_prefix,
        zero_copy: t.hud_zero_copy,
        yes: t.hud_yes,
        no: t.hud_no,
        path_unknown: t.hud_path_unknown,
        path_direct: t.hud_path_direct,
        path_p2p: t.hud_path_p2p,
        path_relay: t.hud_path_relay,
    }
}

/// Auto-updater lifecycle events delivered from background worker threads.
#[derive(Debug)]
pub enum UpdateEvent {
    Checked(Result<UpdateCheckOutcome, String>),
    Progress { downloaded: u64, total: Option<u64> },
    ReadyToRelaunch(Result<std::path::PathBuf, String>),
}

/// Events delivered to the winit loop from other threads.
#[derive(Debug)]
pub enum UserEvent {
    /// A new decoded / synthetic frame is waiting in a `Latest` slot.
    Frame,
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    Session(SessionEvent),
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    Host(EmbeddedHostEvent),
    #[cfg_attr(not(windows), allow(dead_code))]
    Tray(TrayAction),
    Tailscale(TailscaleInfo),
    Update(UpdateEvent),
    PasswordHashed { seq: u64, result: Result<String, String> },
}

#[derive(Debug, Clone)]
pub struct TestPatternOpts {
    pub width: u32,
    pub height: u32,
    /// Frame delivery rate; 0 = render as fast as the swapchain allows.
    pub fps: u32,
    /// Exit after this many presented frames and print statistics.
    pub frames: Option<u32>,
}

#[derive(Debug, Clone)]
pub enum Launch {
    Gui,
    Connect(String),
    TestPattern(TestPatternOpts),
}

struct TestPatternState {
    opts: TestPatternOpts,
    frames: Vec<Frame>,
    next: usize,
    ticks: Arc<Latest<Timestamp>>,
    stop: Arc<AtomicBool>,
    presented: u32,
    render: LatencySamples,
    latency: LatencySamples,
    sum: RenderBreakdown,
    started: Instant,
    window_start: Instant,
    window_frames: u32,
}

impl Drop for TestPatternState {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

enum State {
    Connect,
    Connecting(SessionHandle),
    Session { handle: SessionHandle, config: Box<SessionConfig>, got_frame: bool },
    TestPattern(Box<TestPatternState>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppPhase {
    Connect,
    Connecting,
    Session,
    TestPattern,
}

impl AppPhase {
    fn allows_fullscreen_entry(self) -> bool {
        matches!(self, Self::Session | Self::TestPattern)
    }
}

impl State {
    fn phase(&self) -> AppPhase {
        match self {
            Self::Connect => AppPhase::Connect,
            Self::Connecting(_) => AppPhase::Connecting,
            Self::Session { .. } => AppPhase::Session,
            Self::TestPattern(_) => AppPhase::TestPattern,
        }
    }
}

/// Window chrome state tracked independently of winit/wgpu so transitions are unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WindowChromeState {
    fullscreen: bool,
    decorations: bool,
    pointer_locked: bool,
    toolbar_expanded: bool,
}

impl Default for WindowChromeState {
    fn default() -> Self {
        Self {
            fullscreen: false,
            decorations: true,
            pointer_locked: false,
            toolbar_expanded: false,
        }
    }
}

impl WindowChromeState {
    /// Entering fullscreen is allowed only during an active `Session` or `TestPattern`;
    /// exiting fullscreen is ALWAYS allowed in every phase and restores `decorations = true`.
    fn toggle_fullscreen(&mut self, phase: AppPhase) -> bool {
        if self.fullscreen {
            self.fullscreen = false;
            self.decorations = true;
            true
        } else if phase.allows_fullscreen_entry() {
            self.fullscreen = true;
            true
        } else {
            false
        }
    }

    /// Unconditionally exit fullscreen and restore decorated window chrome.
    fn exit_fullscreen(&mut self) -> bool {
        let changed = self.fullscreen || !self.decorations;
        self.fullscreen = false;
        self.decorations = true;
        changed
    }

    /// Reset window and session chrome on disconnect, session close, auth cancel, or test-pattern Esc.
    fn end_session(&mut self) {
        self.fullscreen = false;
        self.decorations = true;
        self.pointer_locked = false;
        self.toolbar_expanded = false;
    }
}

fn is_fullscreen_shortcut(code: KeyCode, mods: ModifiersState) -> bool {
    code == KeyCode::KeyF && mods.control_key() && mods.shift_key() && !mods.alt_key()
}

/// Viewer-side CLI overrides for M2/M4 P2P, E2EE, tunnels, and portable mode.
#[derive(Debug, Clone, Default)]
pub struct ViewerCliOpts {
    pub rendezvous: Option<String>,
    pub password: Option<String>,
    pub relay_only: bool,
    pub tunnels: Vec<TunnelSpec>,
    pub settings_path: Option<std::path::PathBuf>,
}

const AUTO_RECONNECT_MAX_SECS: u64 = 300;

fn local_sender_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "VeloxDesk".into())
}

fn unix_now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Preserved connection parameters for automatic reconnect after a remote reboot (v0.5.0).
#[derive(Debug, Clone)]
struct SavedConnectParams {
    address: String,
    target: ConnectTarget,
    rendezvous: Option<SocketAddr>,
    relay_only: bool,
    password: Option<String>,
    tunnels: Vec<TunnelSpec>,
}

/// Exponential backoff schedule (`2s, 4s, 8s, 15s` cap) for auto-reconnect.
fn auto_reconnect_backoff(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => Duration::from_secs(2),
        2 => Duration::from_secs(4),
        3 => Duration::from_secs(8),
        _ => Duration::from_secs(15),
    }
}

/// Pure state machine driving auto-reconnect retries after a remote host reboot.
#[derive(Debug, Clone)]
struct AutoReconnectController {
    saved: SavedConnectParams,
    started_at: Instant,
    max_duration: Duration,
    attempt: u32,
    next_attempt_at: Instant,
    in_flight: bool,
    last_error: Option<String>,
}

impl AutoReconnectController {
    fn arm(saved: SavedConnectParams, now: Instant) -> Self {
        let initial_delay = auto_reconnect_backoff(1);
        Self {
            saved,
            started_at: now,
            max_duration: Duration::from_secs(AUTO_RECONNECT_MAX_SECS),
            attempt: 1,
            next_attempt_at: now + initial_delay,
            in_flight: false,
            last_error: None,
        }
    }

    fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at) >= self.max_duration
    }

    fn should_retry_now(&self, now: Instant) -> bool {
        !self.in_flight && now >= self.next_attempt_at && !self.is_expired(now)
    }

    fn mark_attempt_started(&mut self) {
        self.in_flight = true;
    }

    fn on_attempt_failed(&mut self, err: String, now: Instant) {
        self.in_flight = false;
        self.last_error = Some(err);
        self.attempt = self.attempt.saturating_add(1);
        self.next_attempt_at = now + auto_reconnect_backoff(self.attempt);
    }

    fn banner_state(&self, now: Instant) -> ReconnectBannerState {
        let elapsed_secs = now.saturating_duration_since(self.started_at).as_secs();
        let next_retry_secs = if self.in_flight {
            0
        } else {
            self.next_attempt_at.saturating_duration_since(now).as_secs()
        };
        ReconnectBannerState {
            active: true,
            target: self.saved.address.clone(),
            attempt: self.attempt,
            elapsed_secs,
            max_secs: self.max_duration.as_secs(),
            next_retry_secs,
            last_error: self.last_error.clone(),
        }
    }
}

/// UI actions collected during the egui pass and applied afterwards.
enum UiAction {
    Connect(ConnectAction),
    Toolbar(ToolbarAction),
    Password(PasswordAction),
    Accept(AcceptDecision),
    File(FileAction),
    Terminal(TerminalAction),
    Tunnel(TunnelAction),
    ViewerChat(ChatAction),
    HostChat(ChatAction),
    Reconnect(ReconnectAction),
}

pub struct App {
    launch: Option<Launch>,
    proxy: EventLoopProxy<UserEvent>,
    settings: Settings,
    lang_atomic: Arc<AtomicU8>,
    viewer_opts: ViewerCliOpts,
    present_override: Option<Present>,
    surface: Option<VideoSurface>,
    state: State,
    connect: ConnectScreen,
    toolbar: SessionToolbar,
    password_prompt: Option<PasswordPrompt>,
    accept_prompt: Option<(AcceptPrompt, tokio::sync::oneshot::Sender<AcceptDecision>)>,
    embedded_host: Option<EmbeddedHostHandle>,
    host_listen_info: Option<(u16, Option<SocketAddr>)>,
    host_last_peer: Option<String>,
    available_release: Option<ReleaseInfo>,
    manual_update_check: bool,
    pw_status: Option<String>,
    pw_hash_seq: u64,
    tray: Option<TrayHandle>,
    files: FileManagerWindow,
    terminal: TerminalWindow,
    tunnels: TunnelsWindow,
    viewer_chat: ChatWindow,
    host_chat: ChatWindow,
    next_chat_id: u64,
    last_connect: Option<SavedConnectParams>,
    auto_reconnect_armed: bool,
    reconnect: Option<AutoReconnectController>,
    settings_open: bool,
    modifiers: ModifiersState,
    pressed: HashSet<KeyCode>,
    wheel: WheelAccum,
    rel: RelativeAccum,
    pending_abs_move: Option<(i32, i32, u32)>,
    last_abs_send: Instant,
    last_rel_send: Instant,
    redraw_requested: bool,
    pointer_locked: bool,
    chrome: WindowChromeState,
    session_info: SessionInfo,
    decoder: (String, bool),
    path_kind: PathKind,
    cursor_image: Option<CursorImage>,
    pub exit_code: i32,
}

impl App {
    pub fn new(
        launch: Launch,
        proxy: EventLoopProxy<UserEvent>,
        mut settings: Settings,
        present: Option<Present>,
        viewer_opts: ViewerCliOpts,
    ) -> Self {
        // MAJOR #3: sanitize corrupt permanent_password_hash on load so UI and host never desync.
        if let Some(phc) = settings.permanent_password_hash.as_deref()
            && !embedded_host::is_valid_phc_hash(phc)
        {
            tracing::warn!("clearing corrupt permanent_password_hash found in settings");
            settings.permanent_password_hash = None;
        }
        let lang_atomic = Arc::new(AtomicU8::new(settings.language.to_u8()));
        let mut connect = ConnectScreen::new(&settings);
        connect.device_id = crate::device_id_label();
        connect.settings_path = viewer_opts.settings_path.clone();
        connect.build_display = env!("VELOXDESK_BUILD_DISPLAY").to_string();
        let past = Instant::now() - Duration::from_secs(1);
        Self {
            launch: Some(launch),
            proxy,
            lang_atomic,
            viewer_opts,
            present_override: present,
            surface: None,
            state: State::Connect,
            connect,
            toolbar: SessionToolbar::default(),
            password_prompt: None,
            accept_prompt: None,
            embedded_host: None,
            host_listen_info: None,
            host_last_peer: None,
            available_release: None,
            manual_update_check: false,
            pw_status: None,
            pw_hash_seq: 0,
            tray: None,
            files: FileManagerWindow::default(),
            terminal: TerminalWindow::default(),
            tunnels: TunnelsWindow::default(),
            viewer_chat: ChatWindow::new("veloxdesk-viewer-chat", false),
            host_chat: ChatWindow::new("veloxdesk-host-chat", true),
            next_chat_id: 1,
            last_connect: None,
            auto_reconnect_armed: false,
            reconnect: None,
            settings_open: false,
            modifiers: ModifiersState::empty(),
            pressed: HashSet::new(),
            wheel: WheelAccum::default(),
            rel: RelativeAccum::default(),
            pending_abs_move: None,
            last_abs_send: past,
            last_rel_send: past,
            redraw_requested: false,
            pointer_locked: false,
            chrome: WindowChromeState::default(),
            session_info: SessionInfo::default(),
            decoder: (String::new(), false),
            path_kind: PathKind::Direct,
            cursor_image: None,
            settings,
            exit_code: 0,
        }
    }

    fn effective_perm_phc(&self) -> Option<String> {
        if self.settings.password_mode == PasswordMode::OneTime {
            None
        } else {
            self.settings.permanent_password_hash.clone()
        }
    }

    fn request_redraw(&mut self) {
        if !self.redraw_requested
            && let Some(s) = &self.surface
        {
            self.redraw_requested = true;
            s.window().request_redraw();
        }
    }

    fn flush_pending_rel(&mut self) {
        if let Some(ev) = self.rel.take() {
            self.last_rel_send = Instant::now();
            self.send_input(ev);
        }
    }

    fn flush_pending_abs(&mut self) {
        if let Some((x, y, d)) = self.pending_abs_move.take() {
            self.last_abs_send = Instant::now();
            self.send_input(input::mouse_move(x, y, d));
        }
    }

    fn session_ui_active(&self) -> bool {
        self.toolbar.expanded
            || self.toolbar.power_confirm.is_some()
            || self.settings_open
            || self.files.open
            || self.terminal.open
            || self.tunnels.open
            || self.viewer_chat.open
            || self.host_chat.open
            || self.reconnect.is_some()
            || self.password_prompt.is_some()
            || self.accept_prompt.is_some()
            || matches!(self.state, State::Session { got_frame: false, .. })
    }

    fn sync_language(&mut self) {
        let lang = self.settings.language;
        self.lang_atomic.store(lang.to_u8(), Ordering::Relaxed);
        self.connect.copied_notice = None;
        self.pw_status = None;
        if let Some(s) = &mut self.surface {
            s.set_hud_labels(hud_labels_for(lang));
        }
        self.refresh_host_status_label();
        if self.connect.tailscale.error.is_some() {
            self.refresh_tailscale();
        }
    }

    fn refresh_host_status_label(&mut self) {
        let t = tr(self.settings.language);
        if !self.connect.host_running {
            self.connect.host_status = if self.embedded_host.is_some() {
                t.host_starting.into()
            } else {
                t.host_stopped.into()
            };
            return;
        }
        if self.connect.active_sessions > 0 {
            let peer = self
                .host_last_peer
                .as_deref()
                .filter(|p| !p.is_empty())
                .map(format_device_id)
                .unwrap_or_else(|| t.host_peer_viewer.into());
            self.connect.host_status = t.fmt_host_connected(&peer);
        } else if let Some((port, rdv)) = self.host_listen_info {
            self.connect.host_status = match rdv {
                Some(r) => t.fmt_host_listening_rdv(port, r),
                None => t.fmt_host_listening_port(port),
            };
        } else {
            self.connect.host_status = t.host_listening.into();
        }
    }

    fn check_for_updates_bg(&mut self, manual: bool) {
        if matches!(
            self.connect.update_state,
            UpdateUiState::Checking | UpdateUiState::Downloading { .. }
        ) {
            return;
        }
        self.manual_update_check = manual;
        self.connect.update_state = UpdateUiState::Checking;
        if manual {
            self.connect.update_dismissed = false;
        }
        let proxy = self.proxy.clone();
        let _ = std::thread::Builder::new().name("veloxdesk-update-check".into()).spawn(move || {
            let res = updater::check_for_update(env!("CARGO_PKG_VERSION")).map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Checked(res)));
        });
    }

    fn start_update_bg(&mut self) {
        let Some(release) = self.available_release.clone() else { return };
        if matches!(self.connect.update_state, UpdateUiState::Downloading { .. }) {
            return;
        }
        let t = tr(self.settings.language);
        self.connect.update_dismissed = false;
        self.connect.update_state = UpdateUiState::Downloading {
            progress: Some(0.0),
            status: t.update_downloading.into(),
        };
        let proxy = self.proxy.clone();
        let _ = std::thread::Builder::new().name("veloxdesk-update-apply".into()).spawn(move || {
            let progress_proxy = proxy.clone();
            let mut last_sent = Instant::now() - Duration::from_secs(1);
            let res = updater::install_update(&release, |downloaded, total| {
                let now = Instant::now();
                if now.duration_since(last_sent) >= Duration::from_millis(80)
                    || total.is_some_and(|tot| downloaded >= tot)
                {
                    last_sent = now;
                    let _ = progress_proxy.send_event(UserEvent::Update(UpdateEvent::Progress { downloaded, total }));
                }
            })
            .map_err(|e| format!("{e:#}"));
            let _ = proxy.send_event(UserEvent::Update(UpdateEvent::ReadyToRelaunch(res)));
        });
    }

    fn copy_to_clipboard(&self, text: &str) {
        if let Some(s) = &self.surface {
            s.egui_ctx().copy_text(text.to_string());
        }
        #[cfg(feature = "net")]
        {
            if let Ok(mut cb) = veloxdesk_clipboard::create_system_clipboard()
                && let Err(e) = cb.write(&veloxdesk_clipboard::ClipboardItem::Text(text.to_string()))
            {
                tracing::debug!("system clipboard write failed: {e}");
            }
        }
    }

    fn start_embedded_host(&mut self) {
        if self.embedded_host.is_some() {
            return;
        }
        let t = tr(self.settings.language);
        let rendezvous = match self.resolve_rendezvous() {
            Ok(r) => r,
            Err(e) => {
                self.connect.host_error = Some(t.fmt_invalid_rendezvous(&e));
                return;
            }
        };
        self.connect.host_error = None;
        self.connect.host_status = t.host_starting.into();
        let auth_opts = EmbeddedHostAuthOpts {
            password_mode: self.settings.password_mode,
            permanent_password_hash: self.settings.permanent_password_hash.clone(),
            trusted_peers: self.settings.trusted_peers.clone(),
            require_accept: self.settings.require_accept,
            echo_cancellation: self.settings.effective_echo_cancellation(),
        };
        match EmbeddedHostHandle::start(
            rendezvous,
            auth_opts,
            self.proxy.clone(),
            self.lang_atomic.clone(),
        ) {
            Ok(h) => {
                self.embedded_host = Some(h);
            }
            Err(e) => {
                self.connect.host_running = false;
                self.connect.host_status = t.host_stopped.into();
                self.connect.host_error = Some(format!("{e:#}"));
            }
        }
    }

    fn stop_embedded_host(&mut self) {
        if let Some(h) = self.embedded_host.take() {
            h.stop();
        }
        self.accept_prompt = None;
        self.host_listen_info = None;
        self.host_last_peer = None;
        self.connect.host_running = false;
        self.connect.active_sessions = 0;
        self.connect.host_otp.clear();
        self.connect.host_status = tr(self.settings.language).host_stopped.into();
    }

    fn refresh_tailscale(&self) {
        let proxy = self.proxy.clone();
        let lang = self.settings.language;
        let _ = std::thread::Builder::new().name("veloxdesk-ts".into()).spawn(move || {
            let info = tailscale::query_status_for(lang);
            let _ = proxy.send_event(UserEvent::Tailscale(info));
        });
    }

    fn start_tailscale_bg(&self) {
        let proxy = self.proxy.clone();
        let lang = self.settings.language;
        let _ = std::thread::Builder::new().name("veloxdesk-ts-start".into()).spawn(move || {
            let start_res = tailscale::start_tailscale_for(lang);
            std::thread::sleep(Duration::from_millis(1200));
            let mut info = tailscale::query_status_for(lang);
            if let Err(err) = start_res
                && !info.running
            {
                info.error = Some(err);
            }
            let _ = proxy.send_event(UserEvent::Tailscale(info));
        });
    }

    fn save_settings(&self) {
        self.settings.save_to_or_default(self.viewer_opts.settings_path.as_deref());
    }

    fn present_pref(&self) -> Present {
        self.present_override.unwrap_or(self.settings.active().present)
    }

    fn resolve_rendezvous(&self) -> Result<Option<std::net::SocketAddr>, String> {
        let raw = self
            .viewer_opts
            .rendezvous
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                let s = self.settings.rendezvous_server.trim();
                (!s.is_empty()).then_some(s)
            });
        match raw {
            Some(s) => parse_rendezvous_addr_i18n(s, self.settings.language).map(Some),
            None => Ok(None),
        }
    }

    fn start_connect(&mut self, address: String, target: ConnectTarget) {
        let t = tr(self.settings.language);
        let rendezvous = match self.resolve_rendezvous() {
            Ok(r) => r,
            Err(e) => {
                self.connect.error = Some(t.fmt_invalid_rendezvous(&e));
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
                return;
            }
        };
        if matches!(target, ConnectTarget::DeviceId(_)) && rendezvous.is_none() {
            self.connect.error = Some(t.err_no_rendezvous_for_id.into());
            if let Some(s) = &self.surface {
                s.window().request_redraw();
            }
            return;
        }
        self.reconnect = None;
        self.auto_reconnect_armed = false;
        self.settings.push_recent(&address);
        self.save_settings();
        let saved = SavedConnectParams {
            address,
            target,
            rendezvous,
            relay_only: self.viewer_opts.relay_only,
            password: self.viewer_opts.password.clone(),
            tunnels: self.viewer_opts.tunnels.clone(),
        };
        self.last_connect = Some(saved.clone());
        self.connect_with_params(saved);
    }

    fn connect_with_params(&mut self, saved: SavedConnectParams) {
        let t = tr(self.settings.language);
        let (vw, vh, monitor_hz) = self
            .surface
            .as_ref()
            .and_then(|s| s.window().current_monitor())
            .map(|m| (m.size().width, m.size().height, m.refresh_rate_millihertz().unwrap_or(60_000) / 1000))
            .unwrap_or((1920, 1080, 60));
        let prof = *self.settings.active();
        let hz = prof.fps_cap.fps().max(monitor_hz.min(prof.fps_cap.fps()));
        let prefer_gpu_decode = prof.prefer_hw_decode
            && self.surface.as_ref().is_some_and(|s| !s.caps().zero_copy_import.is_empty());
        let label = match &saved.target {
            ConnectTarget::Direct(a) => a.to_string(),
            ConnectTarget::DeviceId(id) => format_device_id(id),
        };
        let opts = ConnectOptions {
            target: saved.target,
            rendezvous: saved.rendezvous,
            relay_only: saved.relay_only,
            password: saved.password,
            mode: self.settings.mode.to_wire(),
            max_bitrate_kbps: prof.quality.bitrate_kbps(),
            prefer_hw_decode: prof.prefer_hw_decode,
            prefer_gpu_decode,
            audio_enabled: prof.audio_enabled,
            mic_enabled: prof.mic_enabled,
            echo_cancellation: self.settings.effective_echo_cancellation(),
            clipboard_sync: prof.clipboard_sync,
            initial_tunnels: saved.tunnels,
            hdr_10bit: prof.hdr_10bit,
            yuv444: prof.yuv444,
            viewer_size: (vw, vh),
            viewer_refresh_hz: hz,
            language: self.lang_atomic.clone(),
        };
        self.password_prompt = None;
        self.path_kind = PathKind::Unknown;
        self.session_info.allow_chat = true;
        self.session_info.allow_power = false;
        self.viewer_chat.allow_chat = true;
        match crate::viewer::connect(opts, self.proxy.clone()) {
            Ok(h) => {
                self.connect.error = None;
                self.connect.status = Some(t.fmt_connecting_to(&label));
                self.state = State::Connecting(h);
            }
            Err(e) => {
                if let Some(rc) = self.reconnect.as_mut() {
                    rc.on_attempt_failed(format!("{e:#}"), Instant::now());
                } else {
                    self.connect.error = Some(format!("{e:#}"));
                }
            }
        }
        if let Some(s) = &self.surface {
            s.window().request_redraw();
        }
    }

    fn end_session(&mut self, reason: Option<String>) {
        self.release_pointer();
        self.pressed.clear();
        self.pending_abs_move = None;
        self.chrome.end_session();
        self.toolbar.expanded = self.chrome.toolbar_expanded;
        self.toolbar.power_confirm = None;
        self.state = State::Connect;
        self.connect.status = None;
        self.connect.error = reason;
        self.password_prompt = None;
        self.files.open = false;
        self.files.progress = None;
        self.terminal.open = false;
        self.terminal.connected = false;
        self.tunnels.open = false;
        self.tunnels.active.clear();
        self.viewer_chat.set_open(false);
        self.viewer_chat.clear();
        self.session_info.host_os.clear();
        self.session_info.privacy_mode = false;
        self.session_info.files_open = false;
        self.session_info.terminal_open = false;
        self.session_info.tunnels_open = false;
        self.session_info.chat_open = false;
        self.session_info.chat_unread = 0;
        self.session_info.allow_chat = true;
        self.session_info.allow_power = false;
        self.session_info.recording_active = false;
        self.session_info.recording_waiting_keyframe = false;
        self.session_info.recording_elapsed_ms = 0;
        self.session_info.recording_bytes = 0;
        if self.reconnect.is_none() {
            self.auto_reconnect_armed = false;
        }
        self.cursor_image = None;
        if let Some(s) = &mut self.surface {
            s.set_cursor(None, 0, 0, false);
            let _ = s.set_present_mode(veloxdesk_core_traits::render::PresentMode::Fifo);
            let _ = s.window().set_cursor_grab(CursorGrabMode::None);
            s.window().set_cursor_visible(true);
            s.window().set_title("VeloxDesk");
        }
        self.apply_window_chrome();
    }

    fn start_test_pattern(&mut self, opts: TestPatternOpts) {
        tracing::info!(w = opts.width, h = opts.height, fps = opts.fps, "generating test pattern frames");
        let frames: Vec<Frame> = (0..8).map(|i| nv12_test_frame(opts.width, opts.height, i * 12)).collect();
        let ticks = Arc::new(Latest::default());
        let stop = Arc::new(AtomicBool::new(false));
        if opts.fps > 0 {
            let (ticks, stop, proxy) = (ticks.clone(), stop.clone(), self.proxy.clone());
            let period = Duration::from_secs_f64(1.0 / opts.fps as f64);
            std::thread::Builder::new()
                .name("test-pattern-ticker".into())
                .spawn(move || {
                    let mut next = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        next += period;
                        let now = Instant::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        } else {
                            next = now;
                        }
                        if ticks.put(Timestamp::now()) && proxy.send_event(UserEvent::Frame).is_err() {
                            break;
                        }
                    }
                })
                .expect("spawn ticker");
        }
        let now = Instant::now();
        self.state = State::TestPattern(Box::new(TestPatternState {
            opts,
            frames,
            next: 0,
            ticks,
            stop,
            presented: 0,
            render: LatencySamples::default(),
            latency: LatencySamples::default(),
            sum: RenderBreakdown::default(),
            started: now,
            window_start: now,
            window_frames: 0,
        }));
        if let Some(s) = &self.surface {
            s.window().request_redraw();
        }
    }

    fn release_pointer(&mut self) {
        if self.pointer_locked
            && let Some(s) = &self.surface
        {
            let _ = s.window().set_cursor_grab(CursorGrabMode::None);
            s.window().set_cursor_visible(true);
        }
        self.pointer_locked = false;
        self.chrome.pointer_locked = false;
        self.session_info.pointer_locked = false;
    }

    fn lock_pointer(&mut self) {
        let Some(s) = &self.surface else { return };
        let w = s.window();
        let ok = w
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined))
            .is_ok();
        if ok {
            w.set_cursor_visible(false);
            self.pointer_locked = true;
            self.chrome.pointer_locked = true;
            self.session_info.pointer_locked = true;
            tracing::info!("pointer locked (Ctrl+Alt+G to release)");
        }
    }

    fn toggle_pointer_lock(&mut self) {
        if self.pointer_locked {
            self.release_pointer();
        } else if matches!(self.state, State::Session { .. }) {
            self.lock_pointer();
        }
    }

    fn send_input(&self, ev: veloxdesk_protocol::control::InputEvent) {
        if let State::Session { handle, .. } = &self.state {
            handle.send(SessionCommand::Input(ev));
        }
    }

    fn relative_mode(&self) -> bool {
        self.settings.mode == Mode::Gaming && self.settings.active().relative_mouse_in_gaming
    }

    /// Host display pixel for a video pixel (the host may stream a scaled image).
    fn video_to_host(&self, config: &SessionConfig, vx: i32, vy: i32) -> (i32, i32, u32) {
        map_video_to_host(config, self.surface.as_ref().and_then(|s| s.video_size()), vx, vy)
    }

    fn apply_toolbar(&mut self, a: ToolbarAction, event_loop: &ActiveEventLoop) {
        match a {
            ToolbarAction::SetLanguage(_) => {
                self.sync_language();
                self.save_settings();
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
            }
            ToolbarAction::SetTheme(pref) => {
                self.settings.theme = pref;
                self.save_settings();
                if let Some(s) = &self.surface {
                    s.set_theme_preference(pref.to_egui());
                    s.window().request_redraw();
                }
            }
            ToolbarAction::SetMode(m) => {
                tracing::info!(mode = m.label(), "mode changed; applying active profile settings");
                self.settings.set_mode(m);
                let prof = *self.settings.active();
                if m == Mode::Desktop || !prof.relative_mouse_in_gaming {
                    self.release_pointer();
                }
                if let Some(s) = &mut self.surface {
                    s.set_scale_mode(prof.scale.to_core());
                    s.set_hud_visible(prof.show_hud);
                    self.session_info.hud_visible = prof.show_hud;
                    if matches!(self.state, State::Session { .. }) {
                        let pref = self.present_override.unwrap_or(prof.present).to_core();
                        if s.present_mode() != pref {
                            let _ = s.set_present_mode(pref);
                        }
                    }
                    s.window().request_redraw();
                }
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetMaxBitrate(prof.quality.bitrate_kbps()));
                    handle.send(SessionCommand::SetAudio(prof.audio_enabled));
                    handle.send(SessionCommand::SetMic(prof.mic_enabled));
                    handle.send(SessionCommand::SetEchoCancellation(self.settings.effective_echo_cancellation()));
                    handle.send(SessionCommand::SetClipboardSync(prof.clipboard_sync));
                }
                self.save_settings();
            }
            ToolbarAction::SwitchDisplay(i) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SwitchDisplay(i));
                }
            }
            ToolbarAction::SetQuality(q) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetMaxBitrate(q.bitrate_kbps()));
                }
                self.save_settings();
            }
            ToolbarAction::SetFpsCap(f) => {
                tracing::info!(fps = f.fps(), "target FPS cap updated (applies on reconnect)");
                self.save_settings();
            }
            ToolbarAction::SetAudio(on) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetAudio(on));
                }
                self.save_settings();
            }
            ToolbarAction::SetMic(on) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetMic(on));
                }
                self.save_settings();
            }
            ToolbarAction::SetEchoCancellation(on) => {
                self.settings.echo_cancellation = on;
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetEchoCancellation(self.settings.effective_echo_cancellation()));
                }
                self.save_settings();
            }
            ToolbarAction::SetClipboardSync(on) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetClipboardSync(on));
                }
                self.save_settings();
            }
            ToolbarAction::SetPrivacyMode(enable) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetPrivacyMode(enable));
                }
            }
            ToolbarAction::SetHud(on) => {
                if let Some(s) = &mut self.surface {
                    s.set_hud_visible(on);
                    self.session_info.hud_visible = on;
                    s.window().request_redraw();
                }
                self.save_settings();
            }
            ToolbarAction::ToggleFiles => {
                self.files.open = !self.files.open;
                self.session_info.files_open = self.files.open;
                if self.files.open
                    && self.files.entries.is_empty()
                    && let State::Session { handle, .. } = &self.state
                {
                    handle.send(SessionCommand::File(FileAction::ListDir(self.files.current_path.clone())));
                }
            }
            ToolbarAction::ToggleTerminal => {
                self.terminal.open = !self.terminal.open;
                self.session_info.terminal_open = self.terminal.open;
            }
            ToolbarAction::ToggleTunnels => {
                self.tunnels.open = !self.tunnels.open;
                self.session_info.tunnels_open = self.tunnels.open;
            }
            ToolbarAction::ToggleChat => {
                let next = !self.viewer_chat.open;
                self.viewer_chat.set_open(next);
                self.session_info.chat_open = self.viewer_chat.open;
                self.session_info.chat_unread = self.viewer_chat.unread;
            }
            ToolbarAction::ToggleRecording => {
                if let State::Session { handle, .. } = &self.state {
                    let active = handle.shared.recording.lock().active;
                    if active {
                        handle.send(SessionCommand::StopRecording);
                    } else {
                        let dir = if self.settings.recordings_dir.trim().is_empty() {
                            veloxdesk_recorder::default_recordings_dir()
                        } else {
                            PathBuf::from(self.settings.recordings_dir.trim())
                        };
                        let peer_label = if !self.session_info.host_name.trim().is_empty() {
                            self.session_info.host_name.clone()
                        } else {
                            self.last_connect
                                .as_ref()
                                .map(|s| s.address.clone())
                                .unwrap_or_else(|| "session".into())
                        };
                        let output_path = veloxdesk_recorder::generate_recording_path(&dir, &peer_label);
                        handle.send(SessionCommand::StartRecording { output_path });
                    }
                }
            }
            ToolbarAction::SystemPower(action) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SystemPower(action));
                }
            }
            ToolbarAction::TogglePointerLock => self.toggle_pointer_lock(),
            ToolbarAction::SetPresent(p) => {
                self.present_override = None;
                if matches!(self.state, State::Session { .. })
                    && let Some(s) = &mut self.surface
                {
                    let _ = s.set_present_mode(p.to_core());
                }
                self.save_settings();
            }
            ToolbarAction::SetScale(sc) => {
                if let Some(s) = &mut self.surface {
                    s.set_scale_mode(sc.to_core());
                }
                self.save_settings();
            }
            ToolbarAction::ToggleHud => self.toggle_hud(),
            ToolbarAction::ToggleFullscreen => self.toggle_fullscreen(),
            ToolbarAction::AndroidBack => {
                for ev in input::android_back_events() {
                    self.send_input(ev);
                }
            }
            ToolbarAction::AndroidHome => {
                for ev in input::android_home_events() {
                    self.send_input(ev);
                }
            }
            ToolbarAction::AndroidRecents => {
                for ev in input::android_recents_events() {
                    self.send_input(ev);
                }
            }
            ToolbarAction::OpenSettings => self.settings_open = true,
            ToolbarAction::CheckForUpdates => {
                self.check_for_updates_bg(true);
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
            }
            ToolbarAction::StartUpdate => {
                self.start_update_bg();
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
            }
            ToolbarAction::SetPasswordMode(mode) => {
                self.settings.password_mode = mode;
                self.save_settings();
                let otp_on = self.settings.effective_otp_enabled();
                let perm_phc = self.effective_perm_phc();
                if let Some(h) = &self.embedded_host {
                    h.set_permanent_password_phc(perm_phc);
                    h.set_otp_enabled(otp_on);
                }
                if !otp_on {
                    self.connect.host_otp.clear();
                }
                self.request_redraw();
            }
            ToolbarAction::SetPermanentPassword(pw) => {
                self.pw_hash_seq = self.pw_hash_seq.wrapping_add(1);
                let seq = self.pw_hash_seq;
                self.pw_status = Some(tr(self.settings.language).pw_saving_status.into());
                let proxy = self.proxy.clone();
                let _ = std::thread::Builder::new().name("veloxdesk-argon2".into()).spawn(move || {
                    let result = embedded_host::hash_password_phc(&pw).map_err(|e| format!("{e:#}"));
                    let _ = proxy.send_event(UserEvent::PasswordHashed { seq, result });
                });
                self.request_redraw();
            }
            ToolbarAction::ClearPermanentPassword => {
                self.pw_hash_seq = self.pw_hash_seq.wrapping_add(1);
                self.settings.permanent_password_hash = None;
                self.save_settings();
                self.pw_status = Some(tr(self.settings.language).pw_cleared_notice.into());
                let otp_on = self.settings.effective_otp_enabled();
                if let Some(h) = &self.embedded_host {
                    h.set_permanent_password_phc(None);
                    h.set_otp_enabled(otp_on);
                }
                self.request_redraw();
            }
            ToolbarAction::SetRequireAccept(req) => {
                self.settings.require_accept = req;
                self.save_settings();
                if let Some(h) = &self.embedded_host {
                    h.set_require_accept(req);
                }
            }
            ToolbarAction::RemoveTrustedPeer(hex) => {
                self.settings.remove_trusted_peer(&hex);
                self.save_settings();
                if let Some(h) = &self.embedded_host {
                    h.remove_trusted_peer(hex);
                }
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
            }
            ToolbarAction::Disconnect => {
                self.auto_reconnect_armed = false;
                self.reconnect = None;
                if self.launch_was_test() {
                    event_loop.exit();
                }
                self.end_session(None);
            }
        }
    }

    fn launch_was_test(&self) -> bool {
        matches!(self.state, State::TestPattern(_))
    }

    fn toggle_hud(&mut self) {
        if let Some(s) = &mut self.surface {
            let on = !s.hud_visible();
            s.set_hud_visible(on);
            self.session_info.hud_visible = on;
            s.window().request_redraw();
        }
    }

    fn apply_window_chrome(&mut self) {
        if let Some(s) = &self.surface {
            let w = s.window();
            let target_fs = self.chrome.fullscreen.then_some(Fullscreen::Borderless(None));
            if w.fullscreen().is_some() != self.chrome.fullscreen {
                w.set_fullscreen(target_fs);
            }
            // BUG #1: NEVER call `set_decorations(true)` while entering/in borderless fullscreen —
            // on Windows it corrupts the borderless WS_POPUP style and breaks DXGI flip presentation.
            if !self.chrome.fullscreen && self.chrome.decorations {
                w.set_decorations(true);
            }
        }
        self.request_redraw();
    }

    fn toggle_fullscreen(&mut self) {
        if let Some(s) = &self.surface {
            self.chrome.fullscreen = s.window().fullscreen().is_some();
        }
        if self.chrome.toggle_fullscreen(self.state.phase()) {
            self.apply_window_chrome();
        }
    }

    fn exit_fullscreen(&mut self) {
        if let Some(s) = &self.surface {
            self.chrome.fullscreen = s.window().fullscreen().is_some();
        }
        if self.chrome.exit_fullscreen() {
            self.apply_window_chrome();
        }
    }

    /// Render one frame (or a UI-only redraw) with the UI for the current state.
    fn render(&mut self, event_loop: &ActiveEventLoop, frame: Option<&Frame>) -> Option<veloxdesk_core_traits::render::PresentStats> {
        let surface = self.surface.as_mut()?;
        let is_fullscreen = surface.window().fullscreen().is_some();
        self.chrome.fullscreen = is_fullscreen;
        let mut actions: Vec<UiAction> = Vec::new();
        let settings = &mut self.settings;
        let connect = &mut self.connect;
        let toolbar = &mut self.toolbar;
        let password_prompt = &mut self.password_prompt;
        let accept_prompt = &mut self.accept_prompt;
        let files = &mut self.files;
        let terminal = &mut self.terminal;
        let tunnels = &mut self.tunnels;
        let viewer_chat = &mut self.viewer_chat;
        let host_chat = &mut self.host_chat;
        let settings_open = &mut self.settings_open;
        let settings_path = self.viewer_opts.settings_path.as_deref();
        let pw_status = self.pw_status.as_deref();
        let reconnect_banner = self.reconnect.as_ref().map(|rc| rc.banner_state(Instant::now()));
        let info = &mut self.session_info;
        info.files_open = files.open;
        info.terminal_open = terminal.open;
        info.tunnels_open = tunnels.open;
        info.chat_open = viewer_chat.open;
        info.chat_unread = viewer_chat.unread;
        connect.host_chat_open = host_chat.open;
        connect.host_chat_unread = host_chat.unread;
        if let State::Session { handle, .. } = &self.state {
            let rstats = handle.shared.recording.lock();
            info.recording_active = rstats.active;
            info.recording_waiting_keyframe = rstats.waiting_for_keyframe;
            info.recording_elapsed_ms = rstats.elapsed_ms;
            info.recording_bytes = rstats.bytes_written;
        }
        let state = &self.state;
        let mut ui_fn = |ui: &mut egui::Ui| {
            let lang = settings.language;
            let t = tr(lang);
            match state {
                State::Connect | State::Connecting(_) => {
                    if let Some(a) = connect.show(ui, settings, is_fullscreen) {
                        actions.push(UiAction::Connect(a));
                    }
                }
                State::Session { got_frame, .. } => {
                    for a in toolbar.show(ui, info, settings) {
                        actions.push(UiAction::Toolbar(a));
                    }
                    if !got_frame {
                        egui::Area::new(egui::Id::unique("veloxdesk-wait"))
                            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                            .show(ui.ctx(), |ui| {
                                veloxdesk_ui::theme::overlay_frame(ui).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spinner();
                                        ui.label(t.waiting_for_video);
                                    });
                                });
                            });
                    }
                    if files.open {
                        for a in files.show_for(ui.ctx(), lang) {
                            actions.push(UiAction::File(a));
                        }
                    }
                    if terminal.open {
                        for a in terminal.show_for(ui.ctx(), lang) {
                            actions.push(UiAction::Terminal(a));
                        }
                    }
                    if tunnels.open {
                        for a in tunnels.show_for(ui.ctx(), lang) {
                            actions.push(UiAction::Tunnel(a));
                        }
                    }
                    if viewer_chat.open {
                        for a in viewer_chat.show_for(ui.ctx(), lang) {
                            actions.push(UiAction::ViewerChat(a));
                        }
                    }
                }
                State::TestPattern(tp) => {
                    egui::Area::new(egui::Id::unique("veloxdesk-tp"))
                        .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-8.0, -8.0))
                        .interactable(false)
                        .show(ui.ctx(), |ui| {
                            ui.label(
                                egui::RichText::new(t.fmt_test_pattern_hint(tp.opts.width, tp.opts.height))
                                    .color(egui::Color32::WHITE)
                                    .background_color(egui::Color32::from_black_alpha(160)),
                            );
                        });
                }
            }
            let cur_lang = settings.language;
            if host_chat.open {
                for a in host_chat.show_for(ui.ctx(), cur_lang) {
                    actions.push(UiAction::HostChat(a));
                }
            }
            if let Some(banner) = &reconnect_banner
                && let Some(a) = ReconnectOverlay::show_for(ui.ctx(), banner, cur_lang)
            {
                actions.push(UiAction::Reconnect(a));
            }
            if let Some(pw) = password_prompt.as_mut()
                && let Some(a) = pw.show_for(ui.ctx(), cur_lang)
            {
                actions.push(UiAction::Password(a));
            }
            if let Some((ap, _)) = accept_prompt.as_mut()
                && let Some(dec) = ap.show_for(ui.ctx(), cur_lang)
            {
                actions.push(UiAction::Accept(dec));
            }
            for a in settings_window_with_security(
                ui.ctx(),
                settings_open,
                settings,
                settings_path,
                Some(&connect.build_display),
                Some(&connect.update_state),
                pw_status,
            ) {
                actions.push(UiAction::Toolbar(a));
            }
        };
        let result = surface.render(frame, &mut ui_fn);
        self.chrome.toolbar_expanded = self.toolbar.expanded;
        let stats = match result {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!("render: {e}");
                None
            }
        };
        for a in actions {
            match a {
                UiAction::Connect(ConnectAction::Connect { address, target }) => self.start_connect(address, target),
                UiAction::Connect(ConnectAction::OpenSettings) => self.settings_open = true,
                UiAction::Connect(ConnectAction::ToggleHost(enable)) => {
                    if enable {
                        self.start_embedded_host();
                    } else {
                        self.stop_embedded_host();
                    }
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::RotateOtp) => {
                    if self.settings.effective_otp_enabled()
                        && let Some(h) = &self.embedded_host
                    {
                        h.rotate_otp();
                    }
                }
                UiAction::Connect(ConnectAction::CopyText(text)) => {
                    self.copy_to_clipboard(&text);
                }
                UiAction::Connect(ConnectAction::SetRequireAccept(req)) => {
                    self.settings.require_accept = req;
                    self.save_settings();
                    if let Some(h) = &self.embedded_host {
                        h.set_require_accept(req);
                    }
                }
                UiAction::Connect(ConnectAction::RefreshTailscale) => {
                    self.refresh_tailscale();
                }
                UiAction::Connect(ConnectAction::StartTailscale) => {
                    self.connect.tailscale.error = Some(tr(self.settings.language).ts_starting.into());
                    self.start_tailscale_bg();
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::OpenUrl(url)) => {
                    tailscale::open_url(&url);
                }
                UiAction::Connect(ConnectAction::ExitFullscreen) => {
                    self.exit_fullscreen();
                }
                UiAction::Connect(ConnectAction::SetLanguage(_)) => {
                    self.sync_language();
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::SetTheme(pref)) => {
                    self.settings.theme = pref;
                    if let Some(s) = &self.surface {
                        s.set_theme_preference(pref.to_egui());
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::StartUpdate) => {
                    self.start_update_bg();
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::DismissUpdate) => {
                    self.connect.update_dismissed = true;
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::CheckForUpdates) => {
                    self.check_for_updates_bg(true);
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::Connect(ConnectAction::ToggleHostChat) => {
                    let next = !self.host_chat.open;
                    self.host_chat.set_open(next);
                    self.connect.host_chat_open = self.host_chat.open;
                    self.connect.host_chat_unread = self.host_chat.unread;
                    self.request_redraw();
                }
                UiAction::Toolbar(t) => self.apply_toolbar(t, event_loop),
                UiAction::Password(PasswordAction::Submit(pw)) => {
                    self.password_prompt = None;
                    if let Some(saved) = self.last_connect.as_mut() {
                        saved.password = Some(pw.clone());
                    }
                    if let State::Connecting(handle) = &self.state {
                        self.connect.status = Some(tr(self.settings.language).authenticating.into());
                        handle.send(SessionCommand::SubmitPassword(pw));
                    }
                }
                UiAction::Password(PasswordAction::Cancel) => {
                    self.password_prompt = None;
                    self.auto_reconnect_armed = false;
                    self.reconnect = None;
                    if let State::Connecting(handle) = &self.state {
                        handle.send(SessionCommand::CancelAuth);
                    }
                    self.end_session(None);
                }
                UiAction::Accept(dec) => {
                    if let Some((_, reply)) = self.accept_prompt.take() {
                        let _ = reply.send(dec);
                    }
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
                UiAction::File(fa) => {
                    if let State::Session { handle, .. } = &self.state {
                        self.files.error = None;
                        handle.send(SessionCommand::File(fa));
                    }
                }
                UiAction::Terminal(ta) => {
                    if let State::Session { handle, .. } = &self.state {
                        self.terminal.error = None;
                        handle.send(SessionCommand::Terminal(ta));
                    }
                }
                UiAction::Tunnel(ta) => {
                    if let State::Session { handle, .. } = &self.state {
                        self.tunnels.error = None;
                        handle.send(SessionCommand::Tunnel(ta));
                    }
                }
                UiAction::ViewerChat(ChatAction::Send(text)) => {
                    if let State::Session { handle, .. } = &self.state {
                        let id = self.next_chat_id;
                        self.next_chat_id = self.next_chat_id.wrapping_add(1).max(1);
                        let sender_name = local_sender_name();
                        let ts_ms = unix_now_ms();
                        self.viewer_chat.push_outgoing(id, sender_name.clone(), text.clone(), ts_ms);
                        handle.send(SessionCommand::SendChat { id, sender_name, text, ts_ms });
                        self.request_redraw();
                    }
                }
                UiAction::HostChat(ChatAction::Send(text)) => {
                    if let Some(h) = &self.embedded_host {
                        h.send_chat(text, local_sender_name());
                    }
                }
                UiAction::Reconnect(ReconnectAction::Cancel) => {
                    self.reconnect = None;
                    self.auto_reconnect_armed = false;
                    self.end_session(None);
                    self.request_redraw();
                }
            }
        }
        stats
    }

    fn present_session_redraw(&mut self, event_loop: &ActiveEventLoop) {
        let shared = match &self.state {
            State::Session { handle, .. } => handle.shared.clone(),
            _ => {
                let _ = self.render(event_loop, None);
                return;
            }
        };
        let Some(msg) = shared.frames.take() else {
            let _ = self.render(event_loop, None);
            return;
        };
        let stats = self.render(event_loop, Some(&msg.frame));
        if let State::Session { got_frame, .. } = &mut self.state {
            *got_frame = true;
        }
        let Some(stats) = stats else { return };
        let presented = Timestamp::now();
        shared.render_us.store(stats.render_us, Ordering::Relaxed);
        let clock = shared.clock.lock().clone();
        let mut hud = shared.hud.lock();
        hud.on_presented(
            PresentedFrame {
                frame_id: msg.frame_id,
                capture_ts: msg.frame.capture_ts,
                recv_complete: msg.recv_complete,
                presented,
                decode_us: msg.decode_us,
                render_us: stats.render_us,
            },
            Some(&clock),
        );
        hud.snapshot.resolution = (msg.frame.width, msg.frame.height);
        hud.snapshot.decoder = self.decoder.0.clone();
        hud.snapshot.hw_decode = self.decoder.1;
        hud.snapshot.zero_copy = stats.zero_copy;
        let active_pads = shared.active_gamepads.load(Ordering::Relaxed);
        self.session_info.active_gamepads = active_pads;
        if let Some(snap) = hud.maybe_publish(Instant::now()) {
            drop(hud);
            let t = tr(self.settings.language);
            let audio_ms = shared.audio_buf_us.load(Ordering::Relaxed) as f64 / 1000.0;
            let mut parts = Vec::new();
            if audio_ms > 0.0 || self.settings.active().audio_enabled {
                parts.push(t.fmt_hud_audio_buf(audio_ms));
            }
            if active_pads > 0 {
                parts.push(t.fmt_hud_gamepads(active_pads));
            }
            if self.pointer_locked {
                parts.push(t.hud_pointer_locked.into());
            }
            if let Some(s) = &mut self.surface {
                s.set_hud(snap);
                s.set_hud_extra((!parts.is_empty()).then(|| parts.join(" · ")));
            }
        }
    }

    fn present_test_pattern(&mut self, event_loop: &ActiveEventLoop) {
        let (frame, tick) = {
            let State::TestPattern(tp) = &mut self.state else { return };
            let tick = if tp.opts.fps > 0 {
                match tp.ticks.take() {
                    Some(t) => t,
                    None => {
                        let _ = self.render(event_loop, None);
                        return;
                    }
                }
            } else {
                Timestamp::now()
            };
            let mut f = tp.frames[tp.next % tp.frames.len()].clone();
            tp.next += 1;
            f.capture_ts = tick;
            (f, tick)
        };
        let stats = self.render(event_loop, Some(&frame));
        let bd = self.surface.as_ref().map(|s| s.last_breakdown()).unwrap_or_default();
        let present_mode = self.surface.as_ref().map(|s| s.present_mode());
        let State::TestPattern(tp) = &mut self.state else { return };
        let Some(stats) = stats else { return };
        tp.presented += 1;
        tp.window_frames += 1;
        // Skip warm-up frames (pipeline creation, first texture allocation).
        if tp.presented > 10 {
            tp.render.push(stats.render_us);
            tp.latency.push(tick.elapsed_us() as u32);
            tp.sum.upload_us += bd.upload_us;
            tp.sum.ui_us += bd.ui_us;
            tp.sum.acquire_us += bd.acquire_us;
            tp.sum.submit_us += bd.submit_us;
            tp.sum.present_us += bd.present_us;
        }
        let now = Instant::now();
        let dt = now.duration_since(tp.window_start).as_secs_f32();
        if dt >= 0.5 {
            let snap = HudSnapshot {
                timings: veloxdesk_core_traits::StageTimings {
                    render_us: stats.render_us,
                    total_us: tick.elapsed_us() as u32,
                    ..Default::default()
                },
                fps: tp.window_frames as f32 / dt,
                codec: "NV12 test pattern".into(),
                encoder: "synthetic".into(),
                decoder: "none".into(),
                resolution: (tp.opts.width, tp.opts.height),
                ..Default::default()
            };
            tracing::debug!("{}", hud_line(&snap));
            tp.window_start = now;
            tp.window_frames = 0;
            if let Some(s) = &mut self.surface {
                s.set_hud(snap);
            }
        }
        if let Some(limit) = tp.opts.frames
            && tp.presented >= limit
        {
            let info = self.surface.as_ref().map(|s| s.adapter_info());
            print_test_report(tp, present_mode, info.as_ref());
            event_loop.exit();
        }
    }

    fn handle_session_event(&mut self, ev: SessionEvent) {
        match ev {
            SessionEvent::Status(s) => {
                if matches!(self.state, State::Connecting(_)) {
                    self.connect.status = Some(s);
                }
            }
            SessionEvent::PathKind(path) => {
                self.path_kind = path;
                match &self.state {
                    State::Connecting(handle) | State::Session { handle, .. } => {
                        handle.shared.hud.lock().snapshot.path = path;
                    }
                    _ => {}
                }
            }
            SessionEvent::PasswordPrompt {
                host_device_id,
                host_fingerprint,
                methods_hint,
                allow_otp,
                allow_perm,
                error,
            } => {
                let mut p = PasswordPrompt::new_with_methods(
                    host_device_id,
                    host_fingerprint,
                    methods_hint,
                    allow_otp,
                    allow_perm,
                );
                p.error = error;
                self.password_prompt = Some(p);
            }
            SessionEvent::Config(cfg) => {
                self.password_prompt = None;
                self.reconnect = None;
                self.auto_reconnect_armed = false;
                self.session_info.displays = cfg.displays.clone();
                self.session_info.active_display = cfg.active_display;
                self.session_info.host_name = cfg.host_name.clone();
                if !cfg.host_os.is_empty() {
                    self.session_info.host_os = cfg.host_os.clone();
                }
                if let Some(s) = &self.surface {
                    s.window()
                        .set_title(&tr(self.settings.language).fmt_window_title_host(&cfg.host_name));
                }
                let state = std::mem::replace(&mut self.state, State::Connect);
                self.state = match state {
                    State::Connecting(handle) => {
                        self.on_session_started(&handle, &cfg);
                        State::Session { handle, config: cfg, got_frame: false }
                    }
                    State::Session { handle, got_frame, .. } => {
                        self.on_session_started(&handle, &cfg);
                        State::Session { handle, config: cfg, got_frame }
                    }
                    other => other,
                };
            }
            SessionEvent::Permissions(p) => {
                self.session_info.allow_chat = p.allow_chat;
                self.session_info.allow_power = p.allow_power;
                self.viewer_chat.allow_chat = p.allow_chat;
            }
            SessionEvent::VideoConfig(v) => {
                if let State::Session { handle, config, .. } = &mut self.state {
                    handle.shared.hud.lock().snapshot.resolution = (v.width, v.height);
                    config.video = Some(*v);
                }
            }
            SessionEvent::DisplayList(displays) => {
                self.session_info.displays = displays.clone();
                if let State::Session { config, .. } = &mut self.state {
                    config.displays = displays;
                }
            }
            SessionEvent::PrivacyMode { enabled, error } => {
                self.session_info.privacy_mode = enabled;
                if let Some(e) = error {
                    tracing::warn!("privacy mode: {e}");
                }
            }
            SessionEvent::ChatReceived(msg) => {
                self.viewer_chat.push_incoming(msg);
                self.session_info.chat_unread = self.viewer_chat.unread;
            }
            SessionEvent::ChatAcked(id) => {
                self.viewer_chat.mark_acked(id);
            }
            SessionEvent::SystemPowerResult(res) => {
                let action = SystemPowerAction::try_from(res.action).ok();
                if res.ok {
                    tracing::info!(?action, "remote host accepted power action");
                    if matches!(action, Some(SystemPowerAction::Reboot | SystemPowerAction::RebootSafeMode)) {
                        self.auto_reconnect_armed = true;
                    }
                } else {
                    let msg = tr(self.settings.language).fmt_power_failed(&res.error);
                    tracing::warn!("{msg}");
                    self.viewer_chat.error = Some(msg);
                }
            }
            SessionEvent::RecordingStopped(stats) => {
                self.session_info.recording_active = false;
                self.session_info.recording_waiting_keyframe = false;
                self.session_info.recording_elapsed_ms = 0;
                self.session_info.recording_bytes = 0;
                tracing::info!(path = %stats.output_path.display(), bytes = stats.bytes_written, "recording saved");
            }
            SessionEvent::RecordingError(e) => {
                self.session_info.recording_active = false;
                self.session_info.recording_waiting_keyframe = false;
                tracing::warn!("recording error: {e}");
            }
            SessionEvent::FileListing { path, entries } => {
                self.files.current_path = path;
                self.files.entries = entries;
                self.files.selected = None;
                self.files.error = None;
            }
            SessionEvent::FileProgress(p) => {
                self.files.progress = p;
            }
            SessionEvent::FileStatus(s) => {
                self.files.status = Some(s);
                self.files.progress = None;
            }
            SessionEvent::FileError(e) => {
                self.files.error = Some(e);
                self.files.progress = None;
            }
            SessionEvent::TerminalOpened => {
                self.terminal.connected = true;
                self.terminal.exit_code = None;
                self.terminal.error = None;
            }
            SessionEvent::TerminalOutput(bytes) => {
                self.terminal.append_output(&bytes);
            }
            SessionEvent::TerminalExit(code) => {
                self.terminal.connected = false;
                self.terminal.exit_code = Some(code);
            }
            SessionEvent::TerminalError(e) => {
                self.terminal.connected = false;
                self.terminal.error = Some(e);
            }
            SessionEvent::TunnelAdded(t) => {
                self.tunnels.active.push(t);
                self.tunnels.error = None;
            }
            SessionEvent::TunnelRemoved(id) => {
                self.tunnels.active.retain(|t| t.id != id);
            }
            SessionEvent::TunnelError(e) => {
                self.tunnels.error = Some(e);
            }
            SessionEvent::CursorShape(img) => {
                if let Some(s) = &mut self.surface {
                    s.set_cursor(Some(&img), 0, 0, false);
                }
                self.cursor_image = Some(img);
            }
            SessionEvent::CursorPos { x, y, visible } => {
                // Local cursor overlay only matters while the OS cursor is hidden (gaming).
                let show = visible && self.pointer_locked;
                if let Some(s) = &mut self.surface {
                    s.set_cursor(None, x, y, show);
                }
            }
            SessionEvent::Decoder { name, hardware } => self.decoder = (name, hardware),
            SessionEvent::Closed(reason) => {
                tracing::info!("session closed: {reason}");
                if self.auto_reconnect_armed
                    && let Some(saved) = self.last_connect.clone()
                {
                    self.auto_reconnect_armed = false;
                    self.reconnect = Some(AutoReconnectController::arm(saved, Instant::now()));
                    self.end_session(None);
                } else if let Some(rc) = self.reconnect.as_mut() {
                    let now = Instant::now();
                    rc.on_attempt_failed(reason, now);
                    if rc.is_expired(now) {
                        self.reconnect = None;
                        let timeout_msg = tr(self.settings.language).reconnect_timed_out.into();
                        self.end_session(Some(timeout_msg));
                    } else {
                        self.end_session(None);
                    }
                } else {
                    self.end_session(Some(reason));
                }
            }
        }
        if let Some(s) = &self.surface {
            s.window().request_redraw();
        }
    }

    fn handle_host_event(&mut self, ev: EmbeddedHostEvent) {
        match ev {
            EmbeddedHostEvent::Listening { bind, device_id, otp, rendezvous } => {
                self.connect.host_running = true;
                self.connect.host_error = None;
                if !device_id.is_empty() {
                    self.connect.device_id = device_id;
                }
                if let Some(o) = otp {
                    self.connect.host_otp = o;
                }
                self.host_listen_info = Some((bind.port(), rendezvous));
                self.refresh_host_status_label();
            }
            EmbeddedHostEvent::SessionStarted { peer_device_id } => {
                self.connect.active_sessions += 1;
                self.host_last_peer = Some(peer_device_id);
                self.refresh_host_status_label();
            }
            EmbeddedHostEvent::SessionEnded { peer_device_id, reason } => {
                tracing::info!(peer = %peer_device_id, %reason, "embedded host session ended");
                self.connect.active_sessions = self.connect.active_sessions.saturating_sub(1);
                if self.connect.active_sessions == 0 {
                    self.host_last_peer = None;
                }
                self.refresh_host_status_label();
            }
            EmbeddedHostEvent::OtpRotated { otp } => {
                self.connect.host_otp = otp;
            }
            EmbeddedHostEvent::OtpCleared => {
                self.connect.host_otp.clear();
            }
            EmbeddedHostEvent::TrustedPeerAdded(peer) => {
                tracing::info!(id = %peer.device_id, name = %peer.name, "persisting trusted peer");
                self.settings.upsert_trusted_peer(peer);
                self.save_settings();
            }
            EmbeddedHostEvent::ChatReceived { peer_device_id, mut msg } => {
                if msg.sender_name.trim().is_empty() && !peer_device_id.is_empty() {
                    msg.sender_name = format_device_id(&peer_device_id);
                }
                tracing::debug!(peer = %peer_device_id, id = msg.id, "embedded host received chat message");
                self.host_chat.push_incoming(msg);
                self.connect.host_chat_unread = self.host_chat.unread;
            }
            EmbeddedHostEvent::ChatSent { id, sender_name, text, ts_ms } => {
                self.host_chat.push_outgoing(id, sender_name, text, ts_ms);
            }
            EmbeddedHostEvent::ChatAcked { peer_device_id, id } => {
                tracing::debug!(peer = %peer_device_id, id, "embedded host chat message acked");
                self.host_chat.mark_acked(id);
            }
            EmbeddedHostEvent::ApprovalRequested { info, default_permissions, reply } => {
                self.accept_prompt = Some((AcceptPrompt::new(info, default_permissions), reply));
                if let Some(s) = &self.surface {
                    let w = s.window();
                    w.set_minimized(false);
                    w.focus_window();
                }
            }
            EmbeddedHostEvent::Stopped => {
                self.connect.host_running = false;
                self.connect.active_sessions = 0;
                self.connect.host_otp.clear();
                self.host_listen_info = None;
                self.host_last_peer = None;
                self.connect.host_status = tr(self.settings.language).host_stopped.into();
                self.accept_prompt = None;
            }
            EmbeddedHostEvent::Error(e) => {
                tracing::warn!("embedded host error: {e}");
                self.connect.host_running = false;
                self.host_listen_info = None;
                self.host_last_peer = None;
                self.connect.host_status = tr(self.settings.language).host_stopped.into();
                self.connect.host_error = Some(e);
                self.embedded_host = None;
            }
        }
        if let Some(s) = &self.surface {
            s.window().request_redraw();
        }
    }

    fn handle_update_event(&mut self, ev: UpdateEvent, event_loop: &ActiveEventLoop) {
        let t = tr(self.settings.language);
        match ev {
            UpdateEvent::Checked(Ok(UpdateCheckOutcome::UpToDate { .. })) => {
                self.available_release = None;
                self.connect.update_state = UpdateUiState::UpToDate;
            }
            UpdateEvent::Checked(Ok(UpdateCheckOutcome::Available(rel))) => {
                self.connect.update_dismissed = false;
                self.connect.update_state = UpdateUiState::Available {
                    current: env!("CARGO_PKG_VERSION").to_string(),
                    latest: rel.version.clone(),
                    notes: rel.body.clone(),
                    html_url: rel.html_url.clone(),
                };
                self.available_release = Some(rel);
            }
            UpdateEvent::Checked(Err(e)) => {
                tracing::debug!("update check failed: {e}");
                if self.manual_update_check {
                    self.connect.update_dismissed = false;
                    self.connect.update_state = UpdateUiState::Error(e);
                } else {
                    // Stay completely silent on background startup failure (offline / DNS / rate-limit).
                    self.connect.update_state = UpdateUiState::Idle;
                }
            }
            UpdateEvent::Progress { downloaded, total } => {
                let frac = total.filter(|&tot| tot > 0).map(|tot| (downloaded as f32 / tot as f32).clamp(0.0, 1.0));
                let status = if total.is_some_and(|tot| tot > 0 && downloaded >= tot) {
                    t.update_applying.into()
                } else {
                    t.fmt_update_downloading_bytes(downloaded, total)
                };
                self.connect.update_state = UpdateUiState::Downloading { progress: frac, status };
            }
            UpdateEvent::ReadyToRelaunch(Ok(exe_path)) => {
                tracing::info!(exe = %exe_path.display(), "update installed; releasing resources before relaunch");
                // Release active session, embedded host sockets (UDP :21118 / IPC :21119), and tray icon
                // BEFORE spawning the new instance so it binds cleanly without WSAEADDRINUSE or duplicate tray.
                self.end_session(None);
                self.stop_embedded_host();
                self.tray = None;
                match updater::spawn_relaunched_executable(&exe_path) {
                    Ok(()) => {
                        tracing::info!("relaunched updated executable; exiting current instance");
                        event_loop.exit();
                        return;
                    }
                    Err(e) => {
                        tracing::warn!("failed to relaunch updated executable: {e:#}");
                        self.connect.update_dismissed = false;
                        self.connect.update_state = UpdateUiState::Error(format!("{e:#}"));
                    }
                }
            }
            UpdateEvent::ReadyToRelaunch(Err(e)) => {
                tracing::warn!("update install failed: {e}");
                self.connect.update_dismissed = false;
                self.connect.update_state = UpdateUiState::Error(e);
            }
        }
        if let Some(s) = &self.surface {
            s.window().request_redraw();
        }
    }

    fn handle_tray_action(&mut self, action: TrayAction, event_loop: &ActiveEventLoop) {
        match action {
            TrayAction::ShowWindow => {
                if !self.state.phase().allows_fullscreen_entry() {
                    self.exit_fullscreen();
                }
                if let Some(s) = &self.surface {
                    let w = s.window();
                    w.set_minimized(false);
                    w.set_visible(true);
                    w.focus_window();
                    w.request_redraw();
                }
            }
            TrayAction::CopyDeviceId => {
                if !self.connect.device_id.is_empty() {
                    let formatted = format_device_id(&self.connect.device_id);
                    self.copy_to_clipboard(&formatted);
                    self.connect.copied_notice = Some(tr(self.settings.language).device_id_copied.into());
                    if let Some(s) = &self.surface {
                        s.window().request_redraw();
                    }
                }
            }
            TrayAction::RotateOtp => {
                if self.settings.effective_otp_enabled()
                    && let Some(h) = &self.embedded_host
                {
                    h.rotate_otp();
                }
            }
            TrayAction::Quit => {
                event_loop.exit();
            }
        }
    }

    fn on_session_started(&mut self, handle: &SessionHandle, cfg: &SessionConfig) {
        let mut hud = handle.shared.hud.lock();
        if let Some(v) = &cfg.video {
            hud.snapshot.codec = match VideoCodec::try_from(v.codec).unwrap_or(VideoCodec::Unspecified) {
                VideoCodec::H264 => "H.264",
                VideoCodec::H265 => "H.265",
                VideoCodec::Av1 => "AV1",
                VideoCodec::Vp9 => "VP9",
                VideoCodec::Unspecified => "?",
            }
            .into();
            hud.snapshot.encoder = v.encoder_name.clone();
            hud.snapshot.hw_encode = v.hw_encoder;
            hud.snapshot.resolution = (v.width, v.height);
        }
        hud.snapshot.path = self.path_kind;
        let prof = *self.settings.active();
        let pref = self.present_pref().to_core();
        self.session_info.hud_visible = prof.show_hud;
        if let Some(s) = &mut self.surface {
            s.set_scale_mode(prof.scale.to_core());
            s.set_hud_visible(prof.show_hud);
            if s.present_mode() != pref {
                let _ = s.set_present_mode(pref);
            }
        }
    }

    fn on_keyboard(&mut self, event: &veloxdesk_render::winit::event::KeyEvent, consumed: bool) {
        let PhysicalKey::Code(code) = event.physical_key else { return };
        let down = event.state == ElementState::Pressed;
        // Local hotkeys (never forwarded).
        if down && self.modifiers.control_key() && self.modifiers.alt_key() && code == KeyCode::KeyG {
            self.toggle_pointer_lock();
            return;
        }
        if down && is_fullscreen_shortcut(code, self.modifiers) {
            self.toggle_fullscreen();
            return;
        }
        if down && self.modifiers.control_key() && self.modifiers.shift_key() {
            if is_toggle_shortcut(code, self.modifiers) {
                self.toggle_hud();
                return;
            }
            if code == KeyCode::KeyL {
                self.toggle_pointer_lock();
                return;
            }
        }
        match &self.state {
            State::TestPattern(_) => {
                if down && code == KeyCode::Escape {
                    self.end_session(None);
                }
            }
            State::Session { .. } => {
                if consumed && down {
                    return;
                }
                if down {
                    self.pressed.insert(code);
                } else if !self.pressed.remove(&code) && consumed {
                    return;
                }
                if let Some(ev) = input::key_event(code, down, self.modifiers) {
                    self.send_input(ev);
                }
            }
            _ => {}
        }
    }

    fn release_all_keys(&mut self) {
        let keys: Vec<KeyCode> = self.pressed.drain().collect();
        for k in keys {
            if let Some(ev) = input::key_event(k, false, ModifiersState::empty()) {
                self.send_input(ev);
            }
        }
    }
}

fn map_video_to_host(config: &SessionConfig, frame_size: Option<(u32, u32)>, vx: i32, vy: i32) -> (i32, i32, u32) {
    let disp = config.displays.iter().find(|d| d.index == config.active_display);
    let video = config.video.as_ref();
    match (disp, video, frame_size) {
        (Some(d), _, Some((fw, fh))) if fw > 0 && fh > 0 && d.width > 0 && d.height > 0 => {
            // Handle mid-session portrait <-> landscape rotation (e.g. Android host) even if a
            // rotated video frame arrives before the matching DisplayList update.
            let (dw, dh) = if d.width != d.height && fw != fh && (d.width > d.height) != (fw > fh) {
                (d.height, d.width)
            } else {
                (d.width, d.height)
            };
            (
                (vx as i64 * dw as i64 / fw as i64) as i32,
                (vy as i64 * dh as i64 / fh as i64) as i32,
                d.index,
            )
        }
        (_, Some(v), _) if v.width > 0 => (vx, vy, config.active_display),
        _ => (vx, vy, config.active_display),
    }
}

fn print_test_report(tp: &TestPatternState, mode: Option<veloxdesk_core_traits::render::PresentMode>, info: Option<&veloxdesk_render::wgpu::AdapterInfo>) {
    let n = tp.render.len().max(1) as u32;
    let p = |s: &LatencySamples, q: f64| s.percentile(q).unwrap_or(0);
    let elapsed = tp.started.elapsed().as_secs_f64();
    let report = format!(
        "TEST-PATTERN REPORT {}x{} NV12 | adapter: {} ({:?}) | present: {:?} | frames {} in {:.1}s ({:.0} fps, ticks dropped {})\n\
         render_us p50 {} p95 {} p99 {} max {} | breakdown avg us: upload {} ui {} acquire {} submit {} present {}\n\
         tick->present latency us p50 {} p99 {}",
        tp.opts.width,
        tp.opts.height,
        info.map(|i| i.name.as_str()).unwrap_or("?"),
        info.map(|i| i.backend),
        mode,
        tp.presented,
        elapsed,
        tp.presented as f64 / elapsed.max(1e-3),
        tp.ticks.dropped(),
        p(&tp.render, 50.0),
        p(&tp.render, 95.0),
        p(&tp.render, 99.0),
        p(&tp.render, 100.0),
        tp.sum.upload_us / n,
        tp.sum.ui_us / n,
        tp.sum.acquire_us / n,
        tp.sum.submit_us / n,
        tp.sum.present_us / n,
        p(&tp.latency, 50.0),
        p(&tp.latency, 99.0),
    );
    println!("{report}");
    tracing::info!("{report}");
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        let launch = self.launch.take().unwrap_or(Launch::Gui);
        let mut attrs = Window::default_attributes()
            .with_title("VeloxDesk")
            .with_inner_size(veloxdesk_render::winit::dpi::LogicalSize::new(1280.0, 760.0));
        if let Launch::TestPattern(o) = &launch {
            attrs = attrs.with_inner_size(veloxdesk_render::winit::dpi::PhysicalSize::new(o.width, o.height));
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("cannot create window: {e}");
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        };
        let initial_present = if matches!(launch, Launch::Gui) {
            veloxdesk_core_traits::render::PresentMode::Fifo
        } else {
            self.present_pref().to_core()
        };
        let prof = *self.settings.active();
        let opts = SurfaceOptions {
            present_mode: initial_present,
            scale_mode: prof.scale.to_core(),
            backends: None,
        };
        match VideoSurface::new(window, opts) {
            Ok(mut s) => {
                s.set_theme_preference(self.settings.theme.to_egui());
                s.set_hud_visible(prof.show_hud);
                s.set_hud_labels(hud_labels_for(self.settings.language));
                self.session_info.hud_visible = prof.show_hud;
                let info = s.adapter_info();
                tracing::info!(adapter = %info.name, backend = ?info.backend, present = ?s.present_mode(), "renderer ready");
                self.surface = Some(s);
            }
            Err(e) => {
                eprintln!("cannot initialise GPU renderer: {e}");
                self.exit_code = 1;
                event_loop.exit();
                return;
            }
        }
        match launch {
            Launch::Gui => {
                updater::cleanup_old_executable();
                let t = tr(self.settings.language);
                let tooltip = if self.connect.device_id.is_empty() {
                    t.app_title.to_string()
                } else {
                    t.fmt_tray_tooltip_id(&format_device_id(&self.connect.device_id))
                };
                self.tray = TrayHandle::spawn(tooltip, self.proxy.clone(), self.lang_atomic.clone());
                #[cfg(feature = "net")]
                self.start_embedded_host();
                self.refresh_tailscale();
                self.check_for_updates_bg(false);
            }
            Launch::Connect(addr) => match parse_target_i18n(&addr, self.settings.language) {
                Ok(target) => self.start_connect(addr, target),
                Err(e) => self.connect.error = Some(e),
            },
            Launch::TestPattern(o) => {
                if let Some(s) = &mut self.surface {
                    s.set_hud_visible(true);
                }
                self.start_test_pattern(o)
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            // BUG #1: coalesce all video/test-pattern frame presents into `WindowEvent::RedrawRequested`
            // instead of presenting synchronously inside `user_event` (prevents double-presents & stalls).
            UserEvent::Frame => self.request_redraw(),
            UserEvent::Session(ev) => self.handle_session_event(ev),
            UserEvent::Host(ev) => self.handle_host_event(ev),
            UserEvent::Tray(action) => self.handle_tray_action(action, event_loop),
            UserEvent::Tailscale(info) => {
                self.connect.tailscale = info;
                self.request_redraw();
            }
            UserEvent::Update(ev) => self.handle_update_event(ev, event_loop),
            UserEvent::PasswordHashed { seq, result } => {
                // MAJOR #2: ignore stale Argon2id completions if user clicked Save again or Clear.
                if seq != self.pw_hash_seq {
                    return;
                }
                match result {
                    Ok(phc) => {
                        self.settings.permanent_password_hash = Some(phc);
                        self.save_settings();
                        self.pw_status = Some(tr(self.settings.language).pw_saved_notice.into());
                        let otp_on = self.settings.effective_otp_enabled();
                        let perm_phc = self.effective_perm_phc();
                        if let Some(h) = &self.embedded_host {
                            h.set_permanent_password_phc(perm_phc);
                            h.set_otp_enabled(otp_on);
                        }
                        if !otp_on {
                            self.connect.host_otp.clear();
                        }
                    }
                    Err(e) => {
                        tracing::warn!("permanent password hashing failed: {e}");
                        self.pw_status = Some(e);
                    }
                }
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.surface.is_none() {
            return;
        }
        // Handle RedrawRequested BEFORE feeding egui_winit: egui_winit returns `repaint: true`
        // for RedrawRequested itself, which would otherwise trigger an infinite busy render loop!
        if matches!(event, WindowEvent::RedrawRequested) {
            self.redraw_requested = false;
            match &self.state {
                State::Session { .. } => self.present_session_redraw(event_loop),
                State::TestPattern(_) => self.present_test_pattern(event_loop),
                State::Connect | State::Connecting(_) => {
                    let _ = self.render(event_loop, None);
                }
            }
            return;
        }

        let in_session = matches!(self.state, State::Session { .. });
        let bypass_egui_mouse = in_session
            && self.pointer_locked
            && matches!(
                event,
                WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. } | WindowEvent::MouseWheel { .. }
            );

        let (response_consumed, response_repaint, egui_wants_pointer) = {
            let Some(surface) = self.surface.as_mut() else { return };
            let (consumed, repaint) = if bypass_egui_mouse {
                (false, false)
            } else {
                let r = surface.on_window_event(&event);
                (r.consumed, r.repaint)
            };
            let wants_ptr = if bypass_egui_mouse {
                false
            } else {
                surface.egui_ctx().egui_wants_pointer_input()
            };
            (consumed, repaint, wants_ptr)
        };

        // Moving/occluding/closing the window does not invalidate pixels; requesting a GPU
        // redraw on `Moved` stalls Windows' modal drag loop (`26e488f`).
        // During an active session with no open UI overlays, also suppress mouse/keyboard-driven
        // repaints so 1000 Hz gaming mice never trigger fullscreen DXGI Present() storms (BUG #1).
        let ui_active = self.session_ui_active();
        let skip_repaint = matches!(
            event,
            WindowEvent::Moved(_)
                | WindowEvent::Occluded(_)
                | WindowEvent::CloseRequested
                | WindowEvent::Destroyed
        ) || (in_session && match &event {
            WindowEvent::CursorMoved { position, .. } => {
                let cursor_near_toolbar = position.y <= 52.0;
                !ui_active && !egui_wants_pointer && !cursor_near_toolbar
            }
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::ModifiersChanged(_)
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::MouseInput { .. } => !ui_active && !egui_wants_pointer && !response_consumed,
            _ => false,
        });
        if response_repaint && !skip_repaint {
            self.request_redraw();
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(surface) = self.surface.as_mut() {
                    surface.resize(size.width, size.height);
                }
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Focused(false) => {
                self.release_all_keys();
                self.release_pointer();
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_keyboard(&event, response_consumed),
            WindowEvent::CursorMoved { position, .. } => {
                if self.pointer_locked || egui_wants_pointer {
                    return;
                }
                let mapped = match (&self.state, self.surface.as_ref()) {
                    (State::Session { config, .. }, Some(surface)) => surface
                        .window_to_video((position.x, position.y))
                        .map(|(vx, vy)| self.video_to_host(config, vx, vy)),
                    _ => None,
                };
                if let Some((x, y, d)) = mapped {
                    self.pending_abs_move = Some((x, y, d));
                    if self.last_abs_send.elapsed() >= Duration::from_millis(2) {
                        self.flush_pending_abs();
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !in_session || (egui_wants_pointer && !self.pointer_locked) {
                    return;
                }
                self.flush_pending_abs();
                self.flush_pending_rel();
                let down = state == ElementState::Pressed;
                if down && self.relative_mode() && !self.pointer_locked {
                    self.lock_pointer();
                }
                if let Some(ev) = input::mouse_button(button, down) {
                    self.send_input(ev);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !in_session || (egui_wants_pointer && !self.pointer_locked) {
                    return;
                }
                self.flush_pending_abs();
                self.flush_pending_rel();
                if let Some(ev) = self.wheel.push(delta) {
                    self.send_input(ev);
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event
            && self.pointer_locked
        {
            self.rel.push(delta.0, delta.1);
            if self.last_rel_send.elapsed() >= Duration::from_millis(2) {
                self.flush_pending_rel();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.flush_pending_rel();
        self.flush_pending_abs();

        // Drive auto-reconnect timer & retry loop after remote reboot.
        let mut reconnect_deadline: Option<Instant> = None;
        if let Some(rc) = self.reconnect.as_mut() {
            let now = Instant::now();
            if rc.is_expired(now) {
                self.reconnect = None;
                let timeout_msg = tr(self.settings.language).reconnect_timed_out.into();
                self.end_session(Some(timeout_msg));
                self.request_redraw();
            } else if rc.should_retry_now(now) {
                rc.mark_attempt_started();
                let saved = rc.saved.clone();
                self.connect_with_params(saved);
                reconnect_deadline = Some(now + Duration::from_millis(250));
            } else {
                reconnect_deadline = Some(now + Duration::from_millis(250));
                self.request_redraw();
            }
        }

        let Some(s) = &self.surface else { return };
        if matches!(&self.state, State::TestPattern(tp) if tp.opts.fps == 0) {
            self.request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        let deadline = match (s.repaint_deadline(), reconnect_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match deadline {
            Some(t) if t <= Instant::now() => {
                self.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.release_all_keys();
        if let State::Session { handle, .. } = &self.state {
            let hud = handle.shared.hud.lock();
            let (r, t) = (&hud.render_samples, &hud.total_samples);
            tracing::info!(
                "session stats: render_us p50 {:?} p99 {:?}; glass-to-glass us p50 {:?} p99 {:?} ({} frames)",
                r.percentile(50.0),
                r.percentile(99.0),
                t.percentile(50.0),
                t.percentile(99.0),
                r.len()
            );
        }
        // BUG #2: cleanly disconnect any active/connecting viewer session so `SessionHandle::drop`
        // flushes QUIC CONNECTION_CLOSE before process termination.
        self.end_session(None);
        self.accept_prompt = None;
        self.stop_embedded_host();
        self.tray = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_defaults_windowed_and_decorated() {
        let chrome = WindowChromeState::default();
        assert!(!chrome.fullscreen);
        assert!(chrome.decorations);
        assert!(!chrome.pointer_locked);
        assert!(!chrome.toolbar_expanded);
    }

    #[test]
    fn ctrl_shift_f_entry_restricted_to_active_session_or_test_pattern() {
        let mut chrome = WindowChromeState::default();
        assert!(!chrome.toggle_fullscreen(AppPhase::Connect));
        assert!(!chrome.fullscreen);
        assert!(chrome.decorations);

        assert!(!chrome.toggle_fullscreen(AppPhase::Connecting));
        assert!(!chrome.fullscreen);
        assert!(chrome.decorations);

        assert!(chrome.toggle_fullscreen(AppPhase::Session));
        assert!(chrome.fullscreen);
        assert!(chrome.toggle_fullscreen(AppPhase::Session));
        assert!(!chrome.fullscreen);
        assert!(chrome.decorations);

        assert!(chrome.toggle_fullscreen(AppPhase::TestPattern));
        assert!(chrome.fullscreen);
    }

    #[test]
    fn exit_fullscreen_always_allowed_in_all_phases() {
        for phase in [
            AppPhase::Connect,
            AppPhase::Connecting,
            AppPhase::Session,
            AppPhase::TestPattern,
        ] {
            let mut chrome = WindowChromeState {
                fullscreen: true,
                decorations: false,
                pointer_locked: true,
                toolbar_expanded: true,
            };
            assert!(chrome.toggle_fullscreen(phase));
            assert!(!chrome.fullscreen);
            assert!(chrome.decorations);
        }

        let mut chrome = WindowChromeState {
            fullscreen: true,
            decorations: false,
            pointer_locked: false,
            toolbar_expanded: false,
        };
        assert!(chrome.exit_fullscreen());
        assert!(!chrome.fullscreen);
        assert!(chrome.decorations);
        // Second call when already windowed and decorated is a no-op.
        assert!(!chrome.exit_fullscreen());
    }

    #[test]
    fn end_session_restores_window_chrome_for_all_disconnect_paths() {
        // Covers: ToolbarAction::Disconnect, SessionEvent::Closed, PasswordAction::Cancel,
        // State::TestPattern Escape, and App::exiting cleanup.
        for _trigger in [
            "toolbar_disconnect",
            "session_closed",
            "password_cancel",
            "test_pattern_escape",
            "window_close_exiting",
        ] {
            let mut chrome = WindowChromeState {
                fullscreen: true,
                decorations: false,
                pointer_locked: true,
                toolbar_expanded: true,
            };
            chrome.end_session();
            assert!(!chrome.fullscreen);
            assert!(chrome.decorations);
            assert!(!chrome.pointer_locked);
            assert!(!chrome.toolbar_expanded);
        }
    }

    #[test]
    fn fullscreen_shortcut_matches_only_ctrl_shift_f() {
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let ctrl_shift_alt = ctrl_shift | ModifiersState::ALT;

        assert!(is_fullscreen_shortcut(KeyCode::KeyF, ctrl_shift));
        assert!(!is_fullscreen_shortcut(KeyCode::KeyF, ctrl_shift_alt));
        assert!(!is_fullscreen_shortcut(KeyCode::KeyF, ModifiersState::CONTROL));
        assert!(!is_fullscreen_shortcut(KeyCode::KeyF, ModifiersState::SHIFT));
        assert!(!is_fullscreen_shortcut(KeyCode::KeyF, ModifiersState::empty()));
        assert!(!is_fullscreen_shortcut(KeyCode::F11, ModifiersState::empty()));
        assert!(!is_fullscreen_shortcut(KeyCode::F11, ctrl_shift));
    }

    #[test]
    fn map_video_to_host_handles_android_portrait_and_landscape_rotation() {
        use veloxdesk_protocol::control::DisplayInfo;

        let mut cfg = SessionConfig {
            host_name: "Pixel-9".into(),
            host_os: "android".into(),
            active_display: 0,
            displays: vec![DisplayInfo {
                index: 0,
                name: "Android".into(),
                width: 1080,
                height: 2400,
                primary: true,
                ..Default::default()
            }],
            ..Default::default()
        };

        // 1. Portrait stream (540x1200) -> scales 2x cleanly to 1080x2400 host coordinates.
        assert_eq!(map_video_to_host(&cfg, Some((540, 1200)), 270, 600), (540, 1200, 0));

        // 2. Rotated landscape frame (1200x540) arrives before DisplayList catches up:
        //    orientation-aware fallback swaps display dimensions to 2400x1080 instead of stretching.
        assert_eq!(map_video_to_host(&cfg, Some((1200, 540)), 600, 270), (1200, 540, 0));

        // 3. DisplayList catches up to landscape (2400x1080) -> exact 2x mapping.
        cfg.displays[0].width = 2400;
        cfg.displays[0].height = 1080;
        assert_eq!(map_video_to_host(&cfg, Some((1200, 540)), 600, 270), (1200, 540, 0));
    }

    #[test]
    fn auto_reconnect_backoff_and_timeout_lifecycle() {
        assert_eq!(auto_reconnect_backoff(1), Duration::from_secs(2));
        assert_eq!(auto_reconnect_backoff(2), Duration::from_secs(4));
        assert_eq!(auto_reconnect_backoff(3), Duration::from_secs(8));
        assert_eq!(auto_reconnect_backoff(4), Duration::from_secs(15));
        assert_eq!(auto_reconnect_backoff(99), Duration::from_secs(15));

        let t0 = Instant::now();
        let saved = SavedConnectParams {
            address: "192.168.1.20:21118".into(),
            target: ConnectTarget::Direct("192.168.1.20:21118".parse().unwrap()),
            rendezvous: None,
            relay_only: false,
            password: Some("secret".into()),
            tunnels: Vec::new(),
        };
        let mut rc = AutoReconnectController::arm(saved, t0);
        assert!(!rc.should_retry_now(t0 + Duration::from_secs(1)));
        assert!(rc.should_retry_now(t0 + Duration::from_secs(2)));

        let b1 = rc.banner_state(t0 + Duration::from_secs(1));
        assert!(b1.active);
        assert_eq!(b1.attempt, 1);
        assert_eq!(b1.max_secs, 300);
        assert_eq!(b1.next_retry_secs, 1);

        rc.mark_attempt_started();
        assert!(!rc.should_retry_now(t0 + Duration::from_secs(2)));
        assert_eq!(rc.banner_state(t0 + Duration::from_secs(2)).next_retry_secs, 0);

        rc.on_attempt_failed("host rebooting".into(), t0 + Duration::from_secs(3));
        assert_eq!(rc.attempt, 2);
        assert_eq!(rc.last_error.as_deref(), Some("host rebooting"));
        // Attempt #2 waits 4 seconds -> ready at t0 + 7s.
        assert!(!rc.should_retry_now(t0 + Duration::from_secs(6)));
        assert!(rc.should_retry_now(t0 + Duration::from_secs(7)));

        // Expires cleanly at 5 minutes (300s).
        assert!(!rc.is_expired(t0 + Duration::from_secs(299)));
        assert!(rc.is_expired(t0 + Duration::from_secs(300)));
        assert!(!rc.should_retry_now(t0 + Duration::from_secs(300)));
    }
}
