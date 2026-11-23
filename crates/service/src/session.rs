//! One viewer session on the host: control handshake, video pipeline, input, feedback.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;
use veloxdesk_core_traits::audio::AudioCodecId;
use veloxdesk_core_traits::input::{DisplayGeometry, InputEvent, InputEventKind, InputFeedback};
use veloxdesk_core_traits::transport::{RecvStream, SendStream};
use veloxdesk_core_traits::video::{ChromaFormat, Display, VideoCodec};
use veloxdesk_core_traits::{CoreError, Timestamp, TransportChannel};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AudioConfig, BitrateUpdate, Close, DisplayInfo, DisplayList, Envelope, FileAck, Hello, Permissions, Pong,
    PrivacyModeResult, SessionConfig, SessionMode, TerminalExit, TunnelResult, VideoConfig, envelope::Msg,
};
use veloxdesk_protocol::media::MediaKind;
use veloxdesk_transport::{
    AudioReceiver, ControlReceiver, ControlSender, datagram_kind, decode_gamepad_datagram, framed,
};
use veloxdesk_virtual_display::PrivacyGuard;

use crate::audio::{AudioParams, AudioPipeline, MicHandle, MicPlayback, frame_ms_to_us};
use crate::auth::CLOSE_AUTH_FAILED;
use crate::backends::{HostBackends, RateController, RateDecision};
use crate::config::HostConfig;
use crate::host::HostStatusEvent;
use crate::inject::{InputHandle, InputThread};
use crate::video::{VideoEvent, VideoParams, VideoPipeline};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Close codes sent in `Close.code`.
pub const CLOSE_NORMAL: u32 = 0;
pub const CLOSE_BUSY: u32 = 1;
pub const CLOSE_PROTOCOL: u32 = 2;
pub const CLOSE_HOST_ERROR: u32 = 3;

/// Result of a finished session.
#[derive(Debug, Clone, Default)]
pub struct SessionSummary {
    pub session_id: u64,
    pub peer_device_id: String,
    pub frames_sent: u64,
    pub frames_dropped: u64,
    pub input_events: u64,
    pub close_reason: String,
}

fn session_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    (t << 16) ^ NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Pick the stream codec: the configured one if the viewer can decode it, else H.264.
pub fn negotiate_codec(wanted: VideoCodec, hello: &Hello) -> VideoCodec {
    if hello.video_caps.is_empty() || hello.video_caps.iter().any(|c| c.codec == wanted as i32) {
        wanted
    } else {
        VideoCodec::H264
    }
}

/// Effective stream parameters from host config + viewer `Hello`.
pub fn negotiate_params(cfg: &HostConfig, hello: &Hello) -> VideoParams {
    let mut fps = cfg.max_fps;
    if hello.viewer_refresh_hz > 0 {
        fps = fps.min(hello.viewer_refresh_hz.max(30));
    }
    let mut bitrate = cfg.bitrate_kbps;
    if hello.max_bitrate_kbps > 0 {
        bitrate = bitrate.min(hello.max_bitrate_kbps.max(cfg.min_bitrate_kbps));
    }
    let gaming = hello.mode == SessionMode::Gaming as i32;
    let codec = negotiate_codec(cfg.codec, hello);
    let viewer_cap = hello.video_caps.iter().find(|c| c.codec == codec as i32);
    let ten_bit = cfg.ten_bit && viewer_cap.is_none_or(|c| c.ten_bit);
    let chroma = if cfg.chroma == ChromaFormat::Yuv444 && viewer_cap.is_none_or(|c| c.yuv444) {
        ChromaFormat::Yuv444
    } else {
        ChromaFormat::Yuv420
    };
    VideoParams {
        stream_id: 0,
        display: cfg.display,
        max_fps: fps,
        bitrate_kbps: bitrate,
        codec,
        prefer_hw: cfg.prefer_hw,
        prefer_gpu: cfg.prefer_gpu,
        draw_cursor: !gaming,
        idle_fps: cfg.idle_fps,
        parity_percent: cfg.fec_parity_percent,
        ten_bit,
        chroma,
    }
}

fn display_info(d: &Display) -> DisplayInfo {
    DisplayInfo {
        index: d.index,
        name: d.name.clone(),
        x: d.x,
        y: d.y,
        width: d.width,
        height: d.height,
        refresh_hz: d.refresh_hz,
        scale_factor: d.scale_factor,
        primary: d.primary,
        hdr: d.hdr,
    }
}

fn geometry(ds: &[Display]) -> Vec<DisplayGeometry> {
    ds.iter().map(|d| DisplayGeometry { index: d.index, x: d.x, y: d.y, width: d.width, height: d.height }).collect()
}

fn host_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "veloxdesk-host".into())
}

struct State {
    displays: Vec<Display>,
    active: u32,
    video: Option<VideoConfig>,
    audio: Option<AudioConfig>,
    mode: i32,
}

impl State {
    fn session_config(&self) -> SessionConfig {
        SessionConfig {
            displays: self.displays.iter().map(display_info).collect(),
            active_display: self.active,
            video: self.video.clone(),
            audio: self.audio.clone(),
            host_name: host_name(),
            host_os: std::env::consts::OS.to_string(),
            mode: self.mode,
        }
    }
}

/// Reject a connection (e.g. too many sessions): sends `Close` on the control stream if the
/// viewer opens one quickly, flushes cleanly, then closes the connection.
pub async fn reject(ch: Arc<dyn TransportChannel>, code: u32, reason: &str) {
    if let Ok(Ok((StreamKind::Control, send, _keep_recv))) =
        timeout(Duration::from_millis(200), ch.accept_bi()).await
    {
        let mut w = ControlSender::new(send);
        let _ = w.send(&Msg::Close(Close { code, reason: reason.into() }).into()).await;
        let _ = w.finish().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    ch.close(code, reason);
}

/// Run one session to completion. Never panics on peer misbehaviour; returns why it ended.
pub async fn run_session(
    ch: Arc<dyn TransportChannel>,
    cfg: Arc<HostConfig>,
    backends: HostBackends,
) -> veloxdesk_core_traits::Result<SessionSummary> {
    let (_tx, rx) = watch::channel(false);
    run_session_until(ch, cfg, backends, rx).await
}

/// Internal lifecycle hooks supplied by [`crate::host::RunningHost`] / `serve_listener`.
#[derive(Clone, Default)]
pub(crate) struct SessionHooks {
    pub active_sessions: Option<Arc<AtomicUsize>>,
    pub on_event: Option<Arc<dyn Fn(HostStatusEvent) + Send + Sync>>,
}

struct ActiveSessionGuard {
    hooks: SessionHooks,
    peer_device_id: String,
    reason: Option<String>,
}

impl ActiveSessionGuard {
    fn enter(hooks: SessionHooks, peer_device_id: String) -> Self {
        if let Some(counter) = &hooks.active_sessions {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(cb) = &hooks.on_event {
            cb(HostStatusEvent::SessionStarted { peer_device_id: peer_device_id.clone() });
        }
        Self { hooks, peer_device_id, reason: None }
    }

    fn set_reason(&mut self, reason: String) {
        self.reason = Some(reason);
    }
}

impl Drop for ActiveSessionGuard {
    fn drop(&mut self) {
        if let Some(counter) = &self.hooks.active_sessions {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
        if let Some(cb) = &self.hooks.on_event {
            let reason = self.reason.take().unwrap_or_else(|| "session ended".to_string());
            cb(HostStatusEvent::SessionEnded { peer_device_id: self.peer_device_id.clone(), reason });
        }
    }
}

/// Like [`run_session`], plus a shutdown signal that triggers a clean `Close` and teardown.
pub async fn run_session_until(
    ch: Arc<dyn TransportChannel>,
    cfg: Arc<HostConfig>,
    backends: HostBackends,
    stop: watch::Receiver<bool>,
) -> veloxdesk_core_traits::Result<SessionSummary> {
    run_session_with_hooks(ch, cfg, backends, stop, SessionHooks::default()).await
}

async fn wait_stop(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow() {
        if stop.changed().await.is_err() {
            break;
        }
    }
}

pub(crate) async fn run_session_with_hooks(
    ch: Arc<dyn TransportChannel>,
    cfg: Arc<HostConfig>,
    backends: HostBackends,
    mut stop: watch::Receiver<bool>,
    hooks: SessionHooks,
) -> veloxdesk_core_traits::Result<SessionSummary> {
    let sid = session_id();
    // 1. control stream + Hello (cancellable on host shutdown or same-peer supersede)
    let (send, recv) = loop {
        let accepted = tokio::select! {
            () = wait_stop(&mut stop) => {
                ch.close(CLOSE_NORMAL, "host stopping");
                return Err(CoreError::Closed);
            }
            res = timeout(HANDSHAKE_TIMEOUT, ch.accept_bi()) => res.map_err(|_| CoreError::Timeout)??,
        };
        let (kind, s, r) = accepted;
        if kind == StreamKind::Control {
            break (s, r);
        }
        tracing::debug!(?kind, "stream before control stream ignored");
    };
    let (mut writer, mut reader) = framed(send, recv);
    let hello = tokio::select! {
        () = wait_stop(&mut stop) => {
            let _ = writer.send(&Msg::Close(Close { code: CLOSE_NORMAL, reason: "host stopping".into() }).into()).await;
            let _ = writer.finish().await;
            ch.close(CLOSE_NORMAL, "host stopping");
            return Err(CoreError::Closed);
        }
        res = timeout(HANDSHAKE_TIMEOUT, reader.recv()) => match res.map_err(|_| CoreError::Timeout)??.msg {
            Some(Msg::Hello(h)) => h,
            other => {
                let _ = writer.send(&Msg::Close(Close { code: CLOSE_PROTOCOL, reason: "expected Hello".into() }).into()).await;
                ch.close(CLOSE_PROTOCOL, "expected Hello");
                return Err(CoreError::invalid(format!("expected Hello, got {other:?}")));
            }
        }
    };
    tracing::info!(session = sid, peer = %hello.device_id, name = %hello.device_name, os = %hello.os,
        mode = hello.mode, viewer = format!("{}x{}@{}", hello.viewer_width, hello.viewer_height, hello.viewer_refresh_hz),
        "viewer hello");
    let mut summary = SessionSummary { session_id: sid, peer_device_id: hello.device_id.clone(), ..Default::default() };

    // 2. Authentication & authorization (M1 open mode fast-path or M2 challenge-response).
    let auth = crate::auth::authenticate(sid, &hello, &ch, &cfg, &mut writer, &mut reader).await?;
    summary.peer_device_id = auth.peer_device_id;
    let mut active_guard = ActiveSessionGuard::enter(hooks, summary.peer_device_id.clone());
    let allow_input = auth.permissions.input && cfg.allow_input;
    let viewer_accepts_opus =
        hello.audio_codecs.is_empty() || hello.audio_codecs.contains(&(AudioCodecId::Opus as i32));
    let allow_audio = cfg.audio_enabled && auth.permissions.audio && viewer_accepts_opus;
    let allow_mic = cfg.allow_mic && auth.permissions.audio;

    // 3. start video, audio and input pipelines, wait until displays + encoder are known.
    let params = negotiate_params(&cfg, &hello);
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel();
    let pipeline = VideoPipeline::start(params.clone(), &backends, ch.clone(), ev_tx)?;
    let (fb_tx, mut fb_rx) = mpsc::unbounded_channel::<InputFeedback>();
    let input =
        if allow_input { Some(InputThread::start_with_feedback(backends.injector.clone(), Some(fb_tx))?) } else { None };
    let audio_frame_us = frame_ms_to_us(cfg.audio_frame_ms).unwrap_or(5_000);
    let (audio, audio_cfg) = if allow_audio && backends.audio_capture.is_some() && backends.audio_encoder.is_some() {
        match AudioPipeline::start(
            AudioParams { stream_id: 0, bitrate_kbps: cfg.audio_bitrate_kbps, frame_duration_us: audio_frame_us },
            &backends,
            ch.clone(),
        ) {
            Ok((p, c)) => (Some(p), Some(c)),
            Err(e) => {
                tracing::warn!(session = sid, "host audio unavailable (continuing video-only): {e}");
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    let mic = if allow_mic && backends.audio_playback.is_some() {
        match MicPlayback::start(audio_frame_us, &backends) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(session = sid, "host mic playback unavailable: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut st = State { displays: Vec::new(), active: 0, video: None, audio: audio_cfg, mode: hello.mode };
    let ready = timeout(HANDSHAKE_TIMEOUT, async {
        while st.video.is_none() || st.displays.is_empty() {
            match ev_rx.recv().await {
                Some(VideoEvent::Displays { displays, active }) => {
                    if let Some(i) = &input {
                        i.handle().set_displays(geometry(&displays));
                    }
                    st.displays = displays;
                    st.active = active;
                }
                Some(VideoEvent::Config(v)) => st.video = Some(v),
                Some(VideoEvent::Fatal(e)) => return Err(e),
                Some(_) => {}
                None => return Err("video pipeline ended".to_string()),
            }
        }
        Ok(())
    })
    .await;
    let startup_err = match ready {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some("timed out starting capture/encoder".to_string()),
    };
    if let Some(e) = startup_err {
        tracing::error!(session = sid, "session start failed: {e}");
        let _ = writer.send(&Msg::Close(Close { code: CLOSE_HOST_ERROR, reason: e.clone() }).into()).await;
        let _ = writer.finish().await;
        shutdown(pipeline, input, audio, mic).await;
        ch.close(CLOSE_HOST_ERROR, &e);
        return Err(CoreError::platform(e));
    }
    writer.send(&Msg::SessionConfig(st.session_config()).into()).await?;

    // 4. writer task: everything host → viewer on control goes through `out`.
    let (out, mut out_rx) = mpsc::unbounded_channel::<Envelope>();
    let mut tasks = JoinSet::new();
    let writer_task = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if writer.send(&m).await.is_err() {
                break;
            }
        }
        let _ = writer.finish().await;
    });
    // 5. extra streams (input, clipboard, file-transfer, tunnel, terminal) and incoming media datagrams.
    let (session_stop_tx, session_stop_rx) = watch::channel(false);
    let input_handle = input.as_ref().map(|i| i.handle());
    tasks.spawn(accept_streams(
        ch.clone(),
        input_handle.clone(),
        auth.permissions.clone(),
        backends.clone(),
        session_stop_rx,
    ));
    let mic_handle = mic.as_ref().and_then(|m| m.handle());
    if input_handle.is_some() || mic_handle.is_some() {
        tasks.spawn(recv_media_datagrams(ch.clone(), input_handle.clone(), mic_handle));
    }

    // 6. main loop
    let mut rate: Box<dyn RateController> =
        (backends.rate)(params.bitrate_kbps, cfg.min_bitrate_kbps, params.bitrate_kbps);
    let mut viewer_cap = params.bitrate_kbps;
    // RAII guard: guaranteed to restore display & local input on normal exit, error, disconnect, or abort.
    let mut privacy_guard: Option<PrivacyGuard> = None;
    let reason = loop {
        if *stop.borrow() {
            let _ = out.send(Msg::Close(Close { code: CLOSE_NORMAL, reason: "host shutting down".into() }).into());
            break "host shutting down".to_string();
        }
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    let _ = out.send(Msg::Close(Close { code: CLOSE_NORMAL, reason: "host shutting down".into() }).into());
                    break "host shutting down".to_string();
                }
            }
            Some(fb) = fb_rx.recv() => {
                match fb {
                    InputFeedback::Rumble(r) => {
                        let _ = out.send(Msg::Rumble(r).into());
                    }
                }
            }
            msg = reader.recv() => {
                let msg = match msg {
                    Ok(m) => m,
                    Err(CoreError::Closed) => break "viewer closed control stream".to_string(),
                    Err(e) => break format!("control stream error: {e}"),
                };
                match msg.msg {
                    Some(Msg::Ping(p)) => {
                        let now = Timestamp::now().as_micros();
                        let _ = out.send(Msg::Pong(Pong { seq: p.seq, ping_sent_us: p.sent_us, peer_recv_us: now,
                            peer_send_us: Timestamp::now().as_micros() }).into());
                    }
                    Some(Msg::KeyframeRequest(_)) => pipeline.request_keyframe(),
                    Some(Msg::Nack(n)) => pipeline.nack(n),
                    Some(Msg::ReceiverReport(rr)) => {
                        if let Some(a) = &audio {
                            let total = rr.packets_received + rr.packets_lost;
                            let loss = if total == 0 { 0.0 } else { rr.packets_lost as f64 / total as f64 };
                            a.set_redundant(loss > 0.02);
                        }
                        if let Some(d) = rate.on_report(&rr, Timestamp::now()) {
                            let d = RateDecision { bitrate_kbps: d.bitrate_kbps.min(viewer_cap), ..d };
                            pipeline.apply_rate(d);
                            let _ = out.send(Msg::BitrateUpdate(BitrateUpdate { stream_id: rr.stream_id,
                                bitrate_kbps: d.bitrate_kbps, fps: params.max_fps,
                                fec_parity_percent: d.parity_percent as u32 }).into());
                        }
                    }
                    Some(Msg::BitrateUpdate(b)) if b.bitrate_kbps > 0 => {
                        // Viewer quality picker: new ceiling for the ABR.
                        viewer_cap = b.bitrate_kbps.clamp(cfg.min_bitrate_kbps, cfg.bitrate_kbps);
                        rate = (backends.rate)(viewer_cap, cfg.min_bitrate_kbps.min(viewer_cap), viewer_cap);
                        pipeline.apply_rate(RateDecision { bitrate_kbps: viewer_cap,
                            parity_percent: cfg.fec_parity_percent });
                    }
                    Some(Msg::SwitchDisplay(s)) => pipeline.switch_display(s.index),
                    Some(Msg::PrivacyMode(pm)) => {
                        let res = handle_privacy_mode(pm.enable, &auth.permissions, &backends, &mut privacy_guard);
                        let _ = out.send(Msg::PrivacyModeResult(res).into());
                    }
                    Some(Msg::Input(ev)) => {
                        if let Some(h) = &input_handle { h.inject(ev); }
                    }
                    Some(Msg::Close(c)) => break format!("viewer closed: {} ({})", c.reason, c.code),
                    Some(other) => tracing::trace!(?other, "ignored control message"),
                    None => {}
                }
            }
            ev = ev_rx.recv() => {
                match ev {
                    Some(VideoEvent::Timing(t)) => {
                        summary.frames_sent += 1;
                        let _ = out.send(Msg::FrameTiming(t).into());
                    }
                    Some(VideoEvent::Config(v)) => {
                        st.video = Some(v);
                        let _ = out.send(Msg::SessionConfig(st.session_config()).into());
                    }
                    Some(VideoEvent::Displays { displays, active }) => {
                        if let Some(h) = &input_handle { h.set_displays(geometry(&displays)); }
                        st.displays = displays;
                        st.active = active;
                        let dl = DisplayList { displays: st.displays.iter().map(display_info).collect() };
                        let _ = out.send(Msg::DisplayList(dl).into());
                        let _ = out.send(Msg::SessionConfig(st.session_config()).into());
                    }
                    Some(VideoEvent::Cursor(c)) => { let _ = out.send(Msg::CursorPosition(c).into()); }
                    Some(VideoEvent::CursorShape(c)) => { let _ = out.send(Msg::CursorShape(c).into()); }
                    Some(VideoEvent::Fatal(e)) => {
                        let _ = out.send(Msg::Close(Close { code: CLOSE_HOST_ERROR, reason: e.clone() }).into());
                        break format!("host pipeline failed: {e}");
                    }
                    None => break "video pipeline ended".to_string(),
                }
            }
        }
    };
    tracing::info!(session = sid, frames = summary.frames_sent, dropped = pipeline.dropped_frames(), "session end: {reason}");
    summary.frames_dropped = pipeline.dropped_frames();
    summary.close_reason = reason;
    active_guard.set_reason(summary.close_reason.clone());

    // 7. teardown: restore privacy mode, signal M4 streams, stop input readers, flush control.
    drop(privacy_guard);
    let _ = session_stop_tx.send(true);
    drop(input_handle);
    drop(out);
    // the writer task ends once `out` is dropped and drained
    let _ = timeout(Duration::from_millis(300), writer_task).await;
    tasks.abort_all();
    let input_stats = shutdown(pipeline, input, audio, mic).await;
    summary.input_events = input_stats;
    ch.close(CLOSE_NORMAL, "session ended");
    drop(active_guard);
    Ok(summary)
}

/// Stop video, audio and input threads (releasing all keys and virtual gamepads); returns input events.
async fn shutdown(
    pipeline: VideoPipeline,
    input: Option<InputThread>,
    audio: Option<AudioPipeline>,
    mic: Option<MicPlayback>,
) -> u64 {
    tokio::task::spawn_blocking(move || {
        pipeline.stop();
        if let Some(a) = audio {
            a.stop();
        }
        if let Some(m) = mic {
            m.stop();
        }
        input.map(|i| i.stop().events).unwrap_or(0)
    })
    .await
    .unwrap_or(0)
}

/// Per-pad wrap-safe monotonic sequence filter for incoming `MediaKind::GamepadState` datagrams.
#[derive(Default)]
struct GamepadSeqFilter {
    last: [Option<u32>; 4],
}

impl GamepadSeqFilter {
    fn accept(&mut self, pad_id: u32, seq: u32) -> bool {
        let idx = pad_id as usize;
        if idx >= self.last.len() {
            return false;
        }
        let fresh = match self.last[idx] {
            None => true,
            Some(0) if seq == 0 => true,
            Some(prev) => (seq.wrapping_sub(prev) as i32) > 0,
        };
        if fresh {
            self.last[idx] = Some(seq);
        }
        fresh
    }
}

/// Receive viewer → host media datagrams (`GamepadState` and optional viewer microphone `Audio` track 1).
async fn recv_media_datagrams(
    ch: Arc<dyn TransportChannel>,
    input: Option<InputHandle>,
    mic: Option<MicHandle>,
) {
    let mut seq_filter = GamepadSeqFilter::default();
    while let Ok(dgram) = ch.recv_datagram().await {
        match datagram_kind(&dgram) {
            Some(MediaKind::GamepadState) => {
                if let Some(h) = &input
                    && let Some(gp) = decode_gamepad_datagram(&dgram)
                    && seq_filter.accept(gp.pad_id, gp.seq)
                {
                    h.inject(InputEvent {
                        ts_us: Timestamp::now().as_micros(),
                        event: Some(InputEventKind::Gamepad(gp)),
                    });
                }
            }
            Some(MediaKind::Audio) => {
                if let Some(m) = &mic
                    && let Some((1, pkt)) = AudioReceiver::parse_datagram(&dgram)
                {
                    m.push(pkt);
                }
            }
            _ => {}
        }
    }
}

fn handle_privacy_mode(
    enable: bool,
    perms: &Permissions,
    backends: &HostBackends,
    guard: &mut Option<PrivacyGuard>,
) -> PrivacyModeResult {
    if !perms.privacy_mode {
        return PrivacyModeResult {
            enabled: guard.is_some(),
            error: "privacy mode permission denied".into(),
        };
    }
    if enable {
        if guard.is_some() {
            return PrivacyModeResult { enabled: true, error: String::new() };
        }
        let ctrl = backends
            .privacy
            .clone()
            .unwrap_or_else(veloxdesk_virtual_display::create_privacy_controller);
        match PrivacyGuard::activate(ctrl) {
            Ok(g) => {
                *guard = Some(g);
                PrivacyModeResult { enabled: true, error: String::new() }
            }
            Err(e) => PrivacyModeResult { enabled: false, error: e.to_string() },
        }
    } else if let Some(g) = guard.take() {
        match g.disable() {
            Ok(()) => PrivacyModeResult { enabled: false, error: String::new() },
            Err(e) => PrivacyModeResult { enabled: false, error: e.to_string() },
        }
    } else {
        PrivacyModeResult { enabled: false, error: String::new() }
    }
}

/// Accept viewer-opened streams (`Input`, `Clipboard`, `FileTransfer`, `Tunnel`, `Terminal`),
/// strictly gated by the granted session [`Permissions`].
async fn accept_streams(
    ch: Arc<dyn TransportChannel>,
    input: Option<InputHandle>,
    perms: Permissions,
    backends: HostBackends,
    stop: watch::Receiver<bool>,
) {
    let mut readers = JoinSet::new();
    let file_cfg = backends.file_transfer.clone().unwrap_or_default();
    let tunnel_policy = backends.tunnel_policy.clone().unwrap_or_default();

    loop {
        let incoming: (StreamKind, Option<SendStream>, RecvStream) = tokio::select! {
            r = ch.accept_bi() => match r {
                Ok((kind, send, recv)) => (kind, Some(send), recv),
                Err(_) => break,
            },
            r = ch.accept_uni() => match r {
                Ok((kind, recv)) => (kind, None, recv),
                Err(_) => break,
            },
        };
        let (kind, send_opt, recv) = incoming;
        match kind {
            StreamKind::Input => {
                if let Some(input) = input.clone() {
                    let mut reader = ControlReceiver::new(recv);
                    readers.spawn(async move {
                        while let Ok(env) = reader.recv().await {
                            if let Some(Msg::Input(ev)) = env.msg {
                                input.inject(ev);
                            }
                        }
                        // stream ended: don't leave keys stuck
                        input.release_all();
                    });
                } else {
                    tracing::debug!("input stream denied (input permission disabled)");
                }
            }
            StreamKind::Clipboard => {
                let Some(send) = send_opt else { continue };
                if !perms.clipboard {
                    tracing::info!("clipboard stream denied by permissions");
                    readers.spawn(deny_with_close(send, "clipboard permission denied"));
                    continue;
                }
                let clip_factory = backends.clipboard.clone();
                let stop_rx = stop.clone();
                readers.spawn(async move {
                    let backend = match clip_factory {
                        Some(f) => f(),
                        None => veloxdesk_clipboard::create_system_clipboard(),
                    };
                    match backend {
                        Ok(b) => {
                            if let Err(e) = veloxdesk_clipboard::run_clipboard_stream(
                                send,
                                recv,
                                b,
                                Duration::from_millis(150),
                                stop_rx,
                            )
                            .await
                            {
                                tracing::debug!("clipboard stream ended: {e}");
                            }
                        }
                        Err(e) => {
                            tracing::warn!("clipboard backend unavailable: {e}");
                            deny_with_close(send, &format!("clipboard unavailable: {e}")).await;
                        }
                    }
                });
            }
            StreamKind::FileTransfer => {
                let Some(send) = send_opt else { continue };
                if !perms.file_transfer {
                    tracing::info!("file transfer stream denied by permissions");
                    readers.spawn(deny_file_transfer(send, recv));
                    continue;
                }
                let cfg = file_cfg.clone();
                readers.spawn(async move {
                    if let Err(e) = veloxdesk_file_transfer::serve_file_stream(send, recv, &cfg).await {
                        tracing::debug!("file transfer stream ended: {e}");
                    }
                });
            }
            StreamKind::Tunnel => {
                let Some(send) = send_opt else { continue };
                if !perms.tunnel {
                    tracing::info!("tunnel stream denied by permissions");
                    readers.spawn(deny_tunnel(send, recv));
                    continue;
                }
                let policy = tunnel_policy.clone();
                readers.spawn(async move {
                    if let Err(e) = veloxdesk_tunnel::serve_tunnel_stream(send, recv, &policy).await {
                        tracing::debug!("tunnel stream ended: {e}");
                    }
                });
            }
            StreamKind::Terminal => {
                let Some(send) = send_opt else { continue };
                if !perms.terminal {
                    tracing::info!("terminal stream denied by permissions");
                    readers.spawn(deny_terminal(send, recv));
                    continue;
                }
                readers.spawn(async move {
                    if let Err(e) = veloxdesk_terminal::serve_terminal_stream(send, recv).await {
                        tracing::debug!("terminal stream ended: {e}");
                    }
                });
            }
            other => tracing::debug!(?other, "unhandled stream kind"),
        }
    }
    readers.abort_all();
}

async fn deny_with_close(send: SendStream, reason: &str) {
    let mut w = ControlSender::new(send);
    let _ = w.send(&Msg::Close(Close { code: CLOSE_AUTH_FAILED, reason: reason.into() }).into()).await;
    let _ = w.finish().await;
}

async fn deny_file_transfer(send: SendStream, recv: RecvStream) {
    let (mut w, mut r) = framed(send, recv);
    let id = match timeout(Duration::from_secs(2), r.recv()).await {
        Ok(Ok(env)) => match env.msg {
            Some(Msg::FileRequest(req)) => req.id,
            _ => 0,
        },
        _ => 0,
    };
    let _ = w
        .send(
            &Msg::FileAck(FileAck {
                id,
                offset: 0,
                error: "file transfer permission denied".into(),
                digest: Vec::new(),
            })
            .into(),
        )
        .await;
    let _ = w.finish().await;
}

async fn deny_tunnel(send: SendStream, recv: RecvStream) {
    let (mut w, mut r) = framed(send, recv);
    let tunnel_id = match timeout(Duration::from_secs(2), r.recv()).await {
        Ok(Ok(env)) => match env.msg {
            Some(Msg::TunnelOpen(open)) => open.tunnel_id,
            _ => 0,
        },
        _ => 0,
    };
    let _ = w
        .send(
            &Msg::TunnelResult(TunnelResult {
                tunnel_id,
                ok: false,
                error: "tunnel permission denied".into(),
            })
            .into(),
        )
        .await;
    let _ = w.finish().await;
}

async fn deny_terminal(send: SendStream, recv: RecvStream) {
    let (mut w, mut r) = framed(send, recv);
    let term_id = match timeout(Duration::from_secs(2), r.recv()).await {
        Ok(Ok(env)) => match env.msg {
            Some(Msg::TerminalOpen(open)) => open.term_id,
            _ => 0,
        },
        _ => 0,
    };
    let _ = w.send(&Msg::TerminalExit(TerminalExit { term_id, code: -1 }).into()).await;
    let _ = w.send(&Msg::Close(Close { code: CLOSE_AUTH_FAILED, reason: "terminal permission denied".into() }).into()).await;
    let _ = w.finish().await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use veloxdesk_protocol::control::VideoCodecCap;

    #[test]
    fn negotiation() {
        let cfg = HostConfig { codec: VideoCodec::H265, max_fps: 144, bitrate_kbps: 50_000, ..Default::default() };
        let mut hello = Hello { viewer_refresh_hz: 120, max_bitrate_kbps: 30_000, mode: 1, ..Default::default() };
        let p = negotiate_params(&cfg, &hello);
        assert_eq!(p.max_fps, 120);
        assert_eq!(p.bitrate_kbps, 30_000);
        assert_eq!(p.codec, VideoCodec::H265); // no caps advertised → trust config
        assert!(!p.draw_cursor);
        hello.video_caps = vec![VideoCodecCap { codec: VideoCodec::H264 as i32, ..Default::default() }];
        assert_eq!(negotiate_params(&cfg, &hello).codec, VideoCodec::H264);
        hello.mode = 0;
        assert!(negotiate_params(&cfg, &hello).draw_cursor);
    }
}
