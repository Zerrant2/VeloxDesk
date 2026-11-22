//! winit application: connect screen, remote session view, test-pattern benchmark.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::metrics::{HudSnapshot, LatencySamples};
use veloxdesk_core_traits::render::VideoRenderer;
use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_core_traits::video::{CursorImage, Frame};
use veloxdesk_protocol::control::{SessionConfig, VideoCodec};
use veloxdesk_render::hud::{hud_line, is_toggle_shortcut};
use veloxdesk_render::pattern::nv12_test_frame;
use veloxdesk_render::winit::application::ApplicationHandler;
use veloxdesk_render::winit::event::{DeviceEvent, DeviceId, ElementState, WindowEvent};
use veloxdesk_render::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use veloxdesk_render::winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use veloxdesk_render::winit::window::{CursorGrabMode, Fullscreen, Window, WindowId};
use veloxdesk_render::{RenderBreakdown, SurfaceOptions, VideoSurface, egui};
use veloxdesk_ui::{
    ConnectAction, ConnectScreen, ConnectTarget, FileAction, FileManagerWindow, Mode, PasswordAction, PasswordPrompt,
    Present, SessionInfo, SessionToolbar, Settings, TerminalAction, TerminalWindow, ToolbarAction, TunnelAction,
    TunnelSpec, TunnelsWindow, format_device_id, parse_rendezvous_addr, parse_target, settings_window,
};

use crate::input::{self, RelativeAccum, WheelAccum};
use crate::latest::Latest;
use crate::session::{ConnectOptions, SessionCommand, SessionEvent, SessionHandle};
use crate::stats::PresentedFrame;

/// Events delivered to the winit loop from other threads.
#[derive(Debug)]
pub enum UserEvent {
    /// A new decoded / synthetic frame is waiting in a `Latest` slot.
    Frame,
    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    Session(SessionEvent),
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
    File(FileAction),
    Terminal(TerminalAction),
    Tunnel(TunnelAction),
}

pub struct App {
    launch: Option<Launch>,
    proxy: EventLoopProxy<UserEvent>,
    settings: Settings,
    viewer_opts: ViewerCliOpts,
    present_override: Option<Present>,
    surface: Option<VideoSurface>,
    state: State,
    connect: ConnectScreen,
    toolbar: SessionToolbar,
    password_prompt: Option<PasswordPrompt>,
    files: FileManagerWindow,
    terminal: TerminalWindow,
    tunnels: TunnelsWindow,
    settings_open: bool,
    modifiers: ModifiersState,
    pressed: HashSet<KeyCode>,
    wheel: WheelAccum,
    rel: RelativeAccum,
    pointer_locked: bool,
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
        let mut connect = ConnectScreen::new(&settings);
        connect.device_id = crate::device_id_label();
        Self {
            launch: Some(launch),
            proxy,
            viewer_opts,
            present_override: present,
            surface: None,
            state: State::Connect,
            connect,
            toolbar: SessionToolbar::default(),
            password_prompt: None,
            files: FileManagerWindow::default(),
            terminal: TerminalWindow::default(),
            tunnels: TunnelsWindow::default(),
            settings_open: false,
            modifiers: ModifiersState::empty(),
            pressed: HashSet::new(),
            wheel: WheelAccum::default(),
            rel: RelativeAccum::default(),
            pointer_locked: false,
            session_info: SessionInfo::default(),
            decoder: (String::new(), false),
            path_kind: PathKind::Direct,
            cursor_image: None,
            settings,
            exit_code: 0,
        }
    }

    fn save_settings(&self) {
        if let Some(p) = &self.viewer_opts.settings_path {
            if let Err(e) = self.settings.save(p) {
                tracing::warn!("saving settings to {}: {e}", p.display());
            }
        } else {
            self.settings.save_default();
        }
    }

    fn present_pref(&self) -> Present {
        self.present_override.unwrap_or(self.settings.present)
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
            Some(s) => parse_rendezvous_addr(s).map(Some),
            None => Ok(None),
        }
    }

    fn start_connect(&mut self, address: String, target: ConnectTarget) {
        let rendezvous = match self.resolve_rendezvous() {
            Ok(r) => r,
            Err(e) => {
                self.connect.error = Some(format!("Invalid rendezvous server: {e}"));
                if let Some(s) = &self.surface {
                    s.window().request_redraw();
                }
                return;
            }
        };
        if matches!(target, ConnectTarget::DeviceId(_)) && rendezvous.is_none() {
            self.connect.error =
                Some("Set a Rendezvous server in Settings (or pass --rendezvous) to connect by Device ID".into());
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
        let hz = self.settings.fps_cap.fps().max(monitor_hz.min(self.settings.fps_cap.fps()));
        let prefer_gpu_decode = self.settings.prefer_hw_decode
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
            max_bitrate_kbps: self.settings.quality.bitrate_kbps(),
            prefer_hw_decode: self.settings.prefer_hw_decode,
            prefer_gpu_decode,
            audio_enabled: self.settings.audio_enabled,
            mic_enabled: self.settings.mic_enabled,
            clipboard_sync: self.settings.clipboard_sync,
            initial_tunnels: self.viewer_opts.tunnels.clone(),
            hdr_10bit: self.settings.hdr_10bit,
            yuv444: self.settings.yuv444,
            viewer_size: (vw, vh),
            viewer_refresh_hz: hz,
        };
        self.password_prompt = None;
        self.path_kind = PathKind::Unknown;
        match crate::viewer::connect(opts, self.proxy.clone()) {
            Ok(h) => {
                self.connect.error = None;
                self.connect.status = Some(format!("Connecting to {label}…"));
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
            s.window().set_title("VeloxDesk");
            s.window().request_redraw();
        }
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
        self.settings.mode == Mode::Gaming && self.settings.relative_mouse_in_gaming
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
            ToolbarAction::SetMode(m) => {
                tracing::info!(mode = m.label(), "mode changed (input mode now; stream settings on reconnect)");
                if m == Mode::Desktop {
                    self.release_pointer();
                }
                self.settings.save_default();
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
                self.settings.save_default();
            }
            ToolbarAction::SetFpsCap(f) => {
                tracing::info!(fps = f.fps(), "target FPS cap updated (applies on reconnect)");
                self.settings.save_default();
            }
            ToolbarAction::SetAudio(on) => {
                if let State::Session { handle, .. } = &self.state {
                    handle.send(SessionCommand::SetAudio(on));
                }
                self.settings.save_default();
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
                if let Some(s) = &mut self.surface {
                    let _ = s.set_present_mode(p.to_core());
                }
            }
            ToolbarAction::SetScale(sc) => {
                if let Some(s) = &mut self.surface {
                    s.set_scale_mode(sc.to_core());
                }
                self.settings.save_default();
            }
            ToolbarAction::ToggleHud => self.toggle_hud(),
            ToolbarAction::ToggleFullscreen => self.toggle_fullscreen(),
            ToolbarAction::OpenSettings => self.settings_open = true,
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

    fn toggle_fullscreen(&mut self) {
        if let Some(s) = &self.surface {
            let w = s.window();
            w.set_fullscreen(if w.fullscreen().is_some() { None } else { Some(Fullscreen::Borderless(None)) });
        }
    }

    /// Render one frame (or a UI-only redraw) with the UI for the current state.
    fn render(&mut self, event_loop: &ActiveEventLoop, frame: Option<&Frame>) -> Option<veloxdesk_core_traits::render::PresentStats> {
        let surface = self.surface.as_mut()?;
        let mut actions: Vec<UiAction> = Vec::new();
        let settings = &mut self.settings;
        let connect = &mut self.connect;
        let toolbar = &mut self.toolbar;
        let password_prompt = &mut self.password_prompt;
        let files = &mut self.files;
        let terminal = &mut self.terminal;
        let tunnels = &mut self.tunnels;
        let settings_open = &mut self.settings_open;
        let info = &mut self.session_info;
        info.files_open = files.open;
        info.terminal_open = terminal.open;
        info.tunnels_open = tunnels.open;
        let state = &self.state;
        let mut ui_fn = |ui: &mut egui::Ui| {
            match state {
                State::Connect | State::Connecting(_) => {
                    if let Some(a) = connect.show(ui, settings) {
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
                                    ui.label("Waiting for video…");
                                });
                            });
                    }
                    if files.open {
                        for a in files.show(ui.ctx()) {
                            actions.push(UiAction::File(a));
                        }
                    }
                    if terminal.open {
                        for a in terminal.show(ui.ctx()) {
                            actions.push(UiAction::Terminal(a));
                        }
                    }
                    if tunnels.open {
                        for a in tunnels.show(ui.ctx()) {
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
                                egui::RichText::new(format!(
                                    "Test pattern {}x{} · Ctrl+Shift+H HUD · Esc quit",
                                    tp.opts.width, tp.opts.height
                                ))
                                .color(egui::Color32::WHITE)
                                .background_color(egui::Color32::from_black_alpha(160)),
                            );
                        });
                }
            }
            if let Some(pw) = password_prompt.as_mut()
                && let Some(a) = pw.show(ui.ctx())
            {
                actions.push(UiAction::Password(a));
            }
            for a in settings_window(ui.ctx(), settings_open, settings) {
                actions.push(UiAction::Toolbar(a));
            }
        };
        let result = surface.render(frame, &mut ui_fn);
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
                UiAction::Toolbar(t) => self.apply_toolbar(t, event_loop),
                UiAction::Password(PasswordAction::Submit(pw)) => {
                    self.password_prompt = None;
                    if let State::Connecting(handle) = &self.state {
                        self.connect.status = Some("Authenticating…".into());
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
                    let audio_ms = shared.audio_buf_us.load(Ordering::Relaxed) as f64 / 1000.0;
                    let mut parts = Vec::new();
                    if audio_ms > 0.0 || self.settings.audio_enabled {
                        parts.push(format!("audio buf {audio_ms:.1} ms"));
                    }
                    if active_pads > 0 {
                        parts.push(format!("gamepads {active_pads}"));
                    }
                    if self.pointer_locked {
                        parts.push("pointer locked (Ctrl+Alt+G)".into());
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
                    s.window().set_title(&format!("VeloxDesk — {}", cfg.host_name));
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
    }

    fn on_keyboard(&mut self, event: &veloxdesk_render::winit::event::KeyEvent, consumed: bool) {
        let PhysicalKey::Code(code) = event.physical_key else { return };
        let down = event.state == ElementState::Pressed;
        // Local hotkeys (never forwarded).
        if down && self.modifiers.control_key() && self.modifiers.alt_key() && code == KeyCode::KeyG {
            self.toggle_pointer_lock();
            return;
        }
        if down && self.modifiers.control_key() && self.modifiers.shift_key() {
            if is_toggle_shortcut(code, self.modifiers) {
                self.toggle_hud();
                return;
            }
            match code {
                KeyCode::KeyL => {
                    self.toggle_pointer_lock();
                    return;
                }
                KeyCode::KeyF => {
                    self.toggle_fullscreen();
                    return;
                }
                _ => {}
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
        let opts = SurfaceOptions {
            present_mode: self.present_pref().to_core(),
            scale_mode: self.settings.scale.to_core(),
            backends: None,
        };
        match VideoSurface::new(window, opts) {
            Ok(mut s) => {
                s.set_hud_visible(self.settings.show_hud);
                self.session_info.hud_visible = self.settings.show_hud;
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
            Launch::Gui => {}
            Launch::Connect(addr) => match parse_target(&addr) {
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
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(surface) = self.surface.as_mut() else { return };
        let response = surface.on_window_event(&event);
        if response.repaint {
            surface.window().request_redraw();
        }
        let egui_wants_pointer = surface.egui_ctx().egui_wants_pointer_input();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                surface.resize(size.width, size.height);
                surface.window().request_redraw();
            }
            WindowEvent::RedrawRequested => {
                if matches!(&self.state, State::TestPattern(tp) if tp.opts.fps == 0) {
                    self.present_test_pattern(event_loop);
                } else {
                    let _ = self.render(event_loop, None);
                }
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
