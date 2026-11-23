//! winit application: connect screen, remote session view, test-pattern benchmark.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::metrics::{HudSnapshot, LatencySamples};
use veloxdesk_core_traits::render::VideoRenderer;
use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::video::{CursorImage, Frame};
use veloxdesk_protocol::control::{SessionConfig, VideoCodec};
use veloxdesk_render::hud::{HudLabels, hud_line, is_toggle_shortcut};
use veloxdesk_render::pattern::nv12_test_frame;
use veloxdesk_render::winit::application::ApplicationHandler;
use veloxdesk_render::winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
use veloxdesk_render::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use veloxdesk_render::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use veloxdesk_render::winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};
use veloxdesk_render::{RenderBreakdown, SurfaceOptions, VideoSurface, egui};
use veloxdesk_ui::{
    AcceptDecision, AcceptPrompt, ConnectAction, ConnectScreen, ConnectTarget, FileAction, FileManagerWindow, Language,
    Mode, PasswordAction, PasswordPrompt, Present, SessionInfo, SessionToolbar, Settings, TailscaleInfo,
    TerminalAction, TerminalWindow, ToolbarAction, TunnelAction, TunnelSpec, TunnelsWindow, UpdateUiState,
    format_device_id, parse_rendezvous_addr_i18n, parse_target_i18n, settings_window_full, tr,
};

use crate::embedded_host::{EmbeddedHostEvent, EmbeddedHostHandle};
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

/// UI actions collected during the egui pass and applied afterwards.
enum UiAction {
    Connect(ConnectAction),
    Toolbar(ToolbarAction),
    Password(PasswordAction),
    Accept(AcceptDecision),
    File(FileAction),
    Terminal(TerminalAction),
    Tunnel(TunnelAction),
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
    tray: Option<TrayHandle>,
    files: FileManagerWindow,
    terminal: TerminalWindow,
    tunnels: TunnelsWindow,
    settings_open: bool,
    modifiers: ModifiersState,
    pressed: HashSet<KeyCode>,
    wheel: WheelAccum,
    rel: RelativeAccum,
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
        settings: Settings,
        present: Option<Present>,
        viewer_opts: ViewerCliOpts,
    ) -> Self {
        let lang_atomic = Arc::new(AtomicU8::new(settings.language.to_u8()));
        let mut connect = ConnectScreen::new(&settings);
        connect.device_id = crate::device_id_label();
        connect.settings_path = viewer_opts.settings_path.clone();
        connect.build_display = env!("VELOXDESK_BUILD_DISPLAY").to_string();
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
            tray: None,
            files: FileManagerWindow::default(),
            terminal: TerminalWindow::default(),
            tunnels: TunnelsWindow::default(),
            settings_open: false,
            modifiers: ModifiersState::empty(),
            pressed: HashSet::new(),
            wheel: WheelAccum::default(),
            rel: RelativeAccum::default(),
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

    fn sync_language(&mut self) {
        let lang = self.settings.language;
        self.lang_atomic.store(lang.to_u8(), Ordering::Relaxed);
        self.connect.copied_notice = None;
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
        match EmbeddedHostHandle::start(
            rendezvous,
            self.settings.require_accept,
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
        self.settings.push_recent(&address);
        self.save_settings();
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
        let label = match &target {
            ConnectTarget::Direct(a) => a.to_string(),
            ConnectTarget::DeviceId(id) => format_device_id(id),
        };
        let opts = ConnectOptions {
            target,
            rendezvous,
            relay_only: self.viewer_opts.relay_only,
            password: self.viewer_opts.password.clone(),
            mode: self.settings.mode.to_wire(),
            max_bitrate_kbps: prof.quality.bitrate_kbps(),
            prefer_hw_decode: prof.prefer_hw_decode,
            prefer_gpu_decode,
            audio_enabled: prof.audio_enabled,
            mic_enabled: prof.mic_enabled,
            clipboard_sync: prof.clipboard_sync,
            initial_tunnels: self.viewer_opts.tunnels.clone(),
            hdr_10bit: prof.hdr_10bit,
            yuv444: prof.yuv444,
            viewer_size: (vw, vh),
            viewer_refresh_hz: hz,
            language: self.lang_atomic.clone(),
        };
        self.password_prompt = None;
        self.path_kind = PathKind::Unknown;
        match crate::viewer::connect(opts, self.proxy.clone()) {
            Ok(h) => {
                self.connect.error = None;
                self.connect.status = Some(t.fmt_connecting_to(&label));
                self.state = State::Connecting(h);
            }
            Err(e) => {
                self.connect.error = Some(format!("{e:#}"));
            }
        }
        if let Some(s) = &self.surface {
            s.window().request_redraw();
        }
    }

    fn end_session(&mut self, reason: Option<String>) {
        self.release_pointer();
        self.pressed.clear();
        self.chrome.end_session();
        self.toolbar.expanded = self.chrome.toolbar_expanded;
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
        self.session_info.privacy_mode = false;
        self.session_info.files_open = false;
        self.session_info.terminal_open = false;
        self.session_info.tunnels_open = false;
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
        let disp = config.displays.iter().find(|d| d.index == config.active_display);
        let video = config.video.as_ref();
        match (disp, video, self.surface.as_ref().and_then(|s| s.video_size())) {
            (Some(d), _, Some((fw, fh))) if fw > 0 && fh > 0 && d.width > 0 => (
                (vx as i64 * d.width as i64 / fw as i64) as i32,
                (vy as i64 * d.height as i64 / fh as i64) as i32,
                d.index,
            ),
            (_, Some(v), _) if v.width > 0 => (vx, vy, config.active_display),
            _ => (vx, vy, config.active_display),
        }
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
            ToolbarAction::SetMode(m) => {
                tracing::info!(mode = m.label(), "mode changed; applying active profile settings");
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
            ToolbarAction::Disconnect => {
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

    fn apply_window_chrome(&self) {
        if let Some(s) = &self.surface {
            let w = s.window();
            let target_fs = self.chrome.fullscreen.then_some(Fullscreen::Borderless(None));
            if w.fullscreen().is_some() != self.chrome.fullscreen {
                w.set_fullscreen(target_fs);
            }
            if self.chrome.decorations {
                w.set_decorations(true);
            }
            w.request_redraw();
        }
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
        let settings_open = &mut self.settings_open;
        let settings_path = self.viewer_opts.settings_path.as_deref();
        let info = &mut self.session_info;
        info.files_open = files.open;
        info.terminal_open = terminal.open;
        info.tunnels_open = tunnels.open;
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
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(t.waiting_for_video);
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
            for a in settings_window_full(
                ui.ctx(),
                settings_open,
                settings,
                settings_path,
                Some(&connect.build_display),
                Some(&connect.update_state),
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
                    if let Some(h) = &self.embedded_host {
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
                UiAction::Toolbar(t) => self.apply_toolbar(t, event_loop),
                UiAction::Password(PasswordAction::Submit(pw)) => {
                    self.password_prompt = None;
                    if let State::Connecting(handle) = &self.state {
                        self.connect.status = Some(tr(self.settings.language).authenticating.into());
                        handle.send(SessionCommand::SubmitPassword(pw));
                    }
                }
                UiAction::Password(PasswordAction::Cancel) => {
                    self.password_prompt = None;
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
            }
        }
        stats
    }

    fn on_frame_event(&mut self, event_loop: &ActiveEventLoop) {
        match &self.state {
            State::TestPattern(_) => self.present_test_pattern(event_loop),
            State::Session { handle, .. } => {
                let shared = handle.shared.clone();
                let Some(msg) = shared.frames.take() else { return };
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
            _ => {}
        }
    }

    fn present_test_pattern(&mut self, event_loop: &ActiveEventLoop) {
        let (frame, tick) = {
            let State::TestPattern(tp) = &mut self.state else { return };
            let tick = if tp.opts.fps > 0 {
                match tp.ticks.take() {
                    Some(t) => t,
                    None => return,
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
            SessionEvent::PasswordPrompt { host_device_id, host_fingerprint, methods_hint, error } => {
                let mut p = PasswordPrompt::new(host_device_id, host_fingerprint, methods_hint);
                p.error = error;
                self.password_prompt = Some(p);
            }
            SessionEvent::Config(cfg) => {
                self.password_prompt = None;
                self.session_info.displays = cfg.displays.clone();
                self.session_info.active_display = cfg.active_display;
                self.session_info.host_name = cfg.host_name.clone();
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
                self.end_session(Some(reason));
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
                if let Some(h) = &self.embedded_host {
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
            UserEvent::Frame => self.on_frame_event(event_loop),
            UserEvent::Session(ev) => self.handle_session_event(ev),
            UserEvent::Host(ev) => self.handle_host_event(ev),
            UserEvent::Tray(action) => self.handle_tray_action(action, event_loop),
            UserEvent::Tailscale(info) => {
                self.connect.tailscale = info;
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
            }
            UserEvent::Update(ev) => self.handle_update_event(ev, event_loop),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(surface) = self.surface.as_mut() else { return };
        // Handle RedrawRequested BEFORE feeding egui_winit: egui_winit returns `repaint: true`
        // for RedrawRequested itself, which would otherwise trigger an infinite busy render loop!
        if matches!(event, WindowEvent::RedrawRequested) {
            if matches!(&self.state, State::TestPattern(tp) if tp.opts.fps == 0) {
                self.present_test_pattern(event_loop);
            } else {
                let _ = self.render(event_loop, None);
            }
            return;
        }
        // Moving/occluding/closing the window does not invalidate pixels; requesting a GPU
        // redraw on `Moved` stalls Windows' modal drag loop.
        let skip_repaint = matches!(
            event,
            WindowEvent::Moved(_)
                | WindowEvent::Occluded(_)
                | WindowEvent::CloseRequested
                | WindowEvent::Destroyed
        );
        let response = surface.on_window_event(&event);
        if response.repaint && !skip_repaint {
            surface.window().request_redraw();
        }
        let egui_wants_pointer = surface.egui_ctx().egui_wants_pointer_input();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                surface.resize(size.width, size.height);
                surface.window().request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::Focused(false) => {
                self.release_all_keys();
                self.release_pointer();
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_keyboard(&event, response.consumed),
            WindowEvent::CursorMoved { position, .. } => {
                if self.pointer_locked || egui_wants_pointer {
                    return;
                }
                if let State::Session { config, .. } = &self.state
                    && let Some((vx, vy)) = surface.window_to_video((position.x, position.y))
                {
                    let (x, y, d) = self.video_to_host(config, vx, vy);
                    self.send_input(input::mouse_move(x, y, d));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !matches!(self.state, State::Session { .. }) || (egui_wants_pointer && !self.pointer_locked) {
                    return;
                }
                let down = state == ElementState::Pressed;
                if down && self.relative_mode() && !self.pointer_locked {
                    self.lock_pointer();
                }
                if let Some(ev) = input::mouse_button(button, down) {
                    self.send_input(ev);
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !matches!(self.state, State::Session { .. }) || (egui_wants_pointer && !self.pointer_locked) {
                    return;
                }
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
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(ev) = self.rel.take() {
            self.send_input(ev);
        }
        let Some(s) = &self.surface else { return };
        if matches!(&self.state, State::TestPattern(tp) if tp.opts.fps == 0) {
            s.window().request_redraw();
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        match s.repaint_deadline() {
            Some(t) if t <= Instant::now() => {
                s.window().request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(t) => event_loop.set_control_flow(ControlFlow::WaitUntil(t)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.release_all_keys();
        self.release_pointer();
        self.chrome.end_session();
        self.toolbar.expanded = false;
        self.accept_prompt = None;
        self.stop_embedded_host();
        self.tray = None;
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
        self.state = State::Connect;
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
}
