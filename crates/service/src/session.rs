//! One viewer session on the host: control handshake, video pipeline, input, feedback.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;
use veloxdesk_core_traits::audio::AudioCodecId;
use veloxdesk_core_traits::input::{DisplayGeometry, InputEvent, InputEventKind, InputFeedback};
use veloxdesk_core_traits::video::{ChromaFormat, Display, VideoCodec};
use veloxdesk_core_traits::{CoreError, Timestamp, TransportChannel};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AudioConfig, BitrateUpdate, Close, DisplayInfo, Envelope, Hello, Pong, SessionConfig, SessionMode, VideoConfig,
    envelope::Msg,
};
use veloxdesk_protocol::media::MediaKind;
use veloxdesk_transport::{
    AudioReceiver, ControlReceiver, ControlSender, datagram_kind, decode_gamepad_datagram, framed,
};

use crate::audio::{AudioParams, AudioPipeline, MicHandle, MicPlayback, frame_ms_to_us};
use crate::backends::{HostBackends, RateController, RateDecision};
use crate::config::HostConfig;
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
/// viewer opens one quickly, then closes the connection.
pub async fn reject(ch: Arc<dyn TransportChannel>, code: u32, reason: &str) {
    if let Ok(Ok((StreamKind::Control, send, _recv))) = timeout(Duration::from_secs(2), ch.accept_bi()).await {
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

/// Like [`run_session`], plus a shutdown signal that triggers a clean `Close` and teardown.
pub async fn run_session_until(
    ch: Arc<dyn TransportChannel>,
    cfg: Arc<HostConfig>,
    backends: HostBackends,
    mut stop: watch::Receiver<bool>,
) -> veloxdesk_core_traits::Result<SessionSummary> {
    let sid = session_id();
    // 1. control stream + Hello
    let (send, recv) = loop {
        let (kind, s, r) = timeout(HANDSHAKE_TIMEOUT, ch.accept_bi()).await.map_err(|_| CoreError::Timeout)??;
        if kind == StreamKind::Control {
            break (s, r);
        }
        tracing::debug!(?kind, "stream before control stream ignored");
    };
    let (mut writer, mut reader) = framed(send, recv);
    let hello = match timeout(HANDSHAKE_TIMEOUT, reader.recv()).await.map_err(|_| CoreError::Timeout)??.msg {
        Some(Msg::Hello(h)) => h,
        other => {
            let _ = writer.send(&Msg::Close(Close { code: CLOSE_PROTOCOL, reason: "expected Hello".into() }).into()).await;
            ch.close(CLOSE_PROTOCOL, "expected Hello");
            return Err(CoreError::invalid(format!("expected Hello, got {other:?}")));
        }
    };
    tracing::info!(session = sid, peer = %hello.device_id, name = %hello.device_name, os = %hello.os,
        mode = hello.mode, viewer = format!("{}x{}@{}", hello.viewer_width, hello.viewer_height, hello.viewer_refresh_hz),
        "viewer hello");
    let mut summary = SessionSummary { session_id: sid, peer_device_id: hello.device_id.clone(), ..Default::default() };

    // 2. Authentication & authorization (M1 open mode fast-path or M2 challenge-response).
    let auth = crate::auth::authenticate(sid, &hello, &ch, &cfg, &mut writer, &mut reader).await?;
    summary.peer_device_id = auth.peer_device_id;
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
    // 5. extra streams (input) and incoming media datagrams (gamepads + optional mic).
    if let Some(i) = &input {
        tasks.spawn(accept_streams(ch.clone(), i.handle()));
    }
    let input_handle = input.as_ref().map(|i| i.handle());
    let mic_handle = mic.as_ref().and_then(|m| m.handle());
    if input_handle.is_some() || mic_handle.is_some() {
        tasks.spawn(recv_media_datagrams(ch.clone(), input_handle.clone(), mic_handle));
    }

    // 6. main loop
    let mut rate: Box<dyn RateController> =
        (backends.rate)(params.bitrate_kbps, cfg.min_bitrate_kbps, params.bitrate_kbps);
    let mut viewer_cap = params.bitrate_kbps;
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

    // 7. teardown: stop input readers first (they hold input handles), flush control.
    drop(input_handle);
    drop(out);
    // the writer task ends once `out` is dropped and drained
    let _ = timeout(Duration::from_millis(300), writer_task).await;
    tasks.abort_all();
    let input_stats = shutdown(pipeline, input, audio, mic).await;
    summary.input_events = input_stats;
    ch.close(CLOSE_NORMAL, "session ended");
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

/// Accept viewer-opened streams; `Input` streams feed the injector.
async fn accept_streams(ch: Arc<dyn TransportChannel>, input: InputHandle) {
    let mut readers = JoinSet::new();
    loop {
        let rx = tokio::select! {
            r = ch.accept_bi() => match r {
                Ok((kind, _send, recv)) => (kind, recv),
                Err(_) => break,
            },
            r = ch.accept_uni() => match r {
                Ok((kind, recv)) => (kind, recv),
                Err(_) => break,
            },
        };
        match rx.0 {
            StreamKind::Input => {
                let input = input.clone();
                let mut reader = ControlReceiver::new(rx.1);
                readers.spawn(async move {
                    while let Ok(env) = reader.recv().await {
                        if let Some(Msg::Input(ev)) = env.msg {
                            input.inject(ev);
                        }
                    }
                    // stream ended: don't leave keys stuck
                    input.release_all();
                });
            }
            other => tracing::debug!(?other, "stream kind not handled in M1"),
        }
    }
    readers.abort_all();
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
