//! Pure-Rust Android Viewer session core: QUIC/P2P/Relay transport, E2EE auth, FEC video
//! reassembly, Opus audio decode + jitter buffer + PLC, input/gamepad, clipboard sync, HUD stats.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::{mpsc, watch};
use tokio::time::{interval, timeout};
use veloxdesk_audio::{
    AcousticEchoCanceller, AecConfig, AecReferenceFeed, AudioJitterBuffer, DenoiseConfig, JitterConfig,
    NoiseSuppressor, OpusDecoder, OpusEncoder,
};
use veloxdesk_core_traits::audio::{AudioCodecId, AudioEncoder, AudioEncoderConfig, AudioFormat, AudioFrame, EncodedAudio};
use veloxdesk_core_traits::time::ClockSync;
use veloxdesk_core_traits::video::VideoCodec as CoreVideoCodec;
use veloxdesk_core_traits::{CoreError, Timestamp, TransportChannel};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{Argon2Params, derive_password_key, password_proof, sign_pubkey_challenge};
use veloxdesk_protocol::control::{
    AudioCodec, AudioConfig, AuthChallenge, AuthMethod, AuthResponse, BitrateUpdate, ChatAck, ChatMessage, Close,
    DisplayInfo, Envelope, GamepadState, Hello, InputEvent, KeyEvent, MouseButtonEvent, MouseMove, MouseRelative,
    MouseWheel, Ping, SessionConfig, SessionModeUpdate, SwitchDisplay, TextInput, VideoCodecCap, envelope::Msg,
    input_event,
};
use veloxdesk_protocol::media::MediaKind;
use veloxdesk_protocol::{PROTOCOL_VERSION, StreamKind};
use veloxdesk_transport::{
    AudioReceiver, AudioSender, ControlReceiver, ControlSender, P2pConnectOptions, ReceivedFrame, VideoReceiver,
    datagram_kind, encode_gamepad_datagram, framed,
};

use crate::clipboard::run_android_clipboard_stream;
use crate::constants;
use crate::stats::{HudAggregator, HudMetrics};
use crate::target::{ConnectTarget, format_device_id, parse_rendezvous_addr, parse_target};
use crate::{SecretString, decode_codec_caps_mask, f32_to_pcm16, unix_now_ms, zeroize_bytes};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
const FEEDBACK_TICK: Duration = Duration::from_millis(5);
const PING_TICK: Duration = Duration::from_millis(500);
const QUIC_CLOSE_FLUSH_MS: u64 = 20;

/// Load or create the persistent Ed25519 device identity in `<filesDir>/identity.key`.
pub fn load_or_create_identity(files_dir: &Path) -> anyhow::Result<DeviceIdentity> {
    let path = files_dir.join("identity.key");
    DeviceIdentity::load_or_create(&path).with_context(|| format!("loading device identity at {}", path.display()))
}

/// Validate a host `CursorShape` bitmap (`1..=256` on each axis and exact `width * height * 4` RGBA bytes).
pub fn is_valid_cursor_shape(width: u32, height: u32, rgba_len: usize) -> bool {
    width > 0
        && height > 0
        && width <= 256
        && height <= 256
        && rgba_len == (width as usize) * (height as usize) * 4
}

/// Callback sink decoupling the pure-Rust viewer core from JNI (also used directly in live tests).
pub trait SessionCallbacks: Send + Sync + 'static {
    fn on_status(&self, status_code: i32, detail: &str);
    fn on_auth_required(&self, host_device_id: &str, host_fingerprint: &str, allow_perm: bool, allow_otp: bool);
    fn on_session_config(&self, cfg: &SessionConfig, displays_json: &str);
    fn on_display_list(&self, displays_json: &str);
    #[allow(clippy::too_many_arguments)]
    fn on_video_frame(
        &self,
        codec: i32,
        width: u32,
        height: u32,
        frame_id: u32,
        capture_ts_us: i64,
        keyframe: bool,
        discontinuity: bool,
        data: &[u8],
    );
    fn on_audio_pcm(&self, sample_rate: i32, channels: i32, pcm16: &[i16]);
    fn on_stats(&self, stats: &HudMetrics);
    fn on_clipboard_text(&self, text: &str);
    fn on_rumble(&self, pad_id: i32, low_freq: i32, high_freq: i32);
    fn on_cursor_shape(&self, _shape_id: u64, _width: u32, _height: u32, _hotspot_x: u32, _hotspot_y: u32, _rgba: &[u8]) {}
    fn on_cursor_position(&self, _x: i32, _y: i32, _visible: bool, _shape_id: u64) {}
    fn on_chat_message(&self, _id: u64, _sender_name: &str, _text: &str, _ts_ms: u64) {}
    fn on_chat_ack(&self, _id: u64) {}
    fn on_closed(&self, reason: &str, is_error: bool);
}

/// Configuration for starting one Android viewer session.
#[derive(Clone, Debug)]
pub struct ConnectConfig {
    pub files_dir: PathBuf,
    pub target: String,
    pub rendezvous_addr: String,
    pub relay_only: bool,
    pub password: Option<SecretString>,
    pub mode: i32,
    pub max_bitrate_kbps: u32,
    pub audio_enabled: bool,
    pub clipboard_enabled: bool,
    pub viewer_width: u32,
    pub viewer_height: u32,
    pub viewer_refresh_hz: u32,
    pub device_name: String,
    pub codec_caps_mask: u32,
}

/// JSON-serializable view of `DisplayInfo` matching `BridgeModels.kt::DisplayDescriptor`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DisplayDescriptorJson<'a> {
    index: u32,
    name: &'a str,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    refresh_hz: u32,
    primary: bool,
}

/// Serialize `DisplayInfo` slice into the JSON array expected by Kotlin (`DisplayDescriptor`).
pub fn displays_to_json(displays: &[DisplayInfo]) -> String {
    let items: Vec<DisplayDescriptorJson<'_>> = displays
        .iter()
        .map(|d| DisplayDescriptorJson {
            index: d.index,
            name: &d.name,
            x: d.x,
            y: d.y,
            width: d.width,
            height: d.height,
            refresh_hz: d.refresh_hz,
            primary: d.primary,
        })
        .collect();
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// Per-pad monotonic sequence & connection state tracker (4 XInput pads: `0..4`).
#[derive(Debug, Default)]
pub struct GamepadTracker {
    seq: [u32; 4],
    connected: [bool; 4],
}

impl GamepadTracker {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn next_state(
        &mut self,
        pad_id: u32,
        connected: bool,
        buttons: u32,
        left_trigger: u32,
        right_trigger: u32,
        left_x: i32,
        left_y: i32,
        right_x: i32,
        right_y: i32,
    ) -> Option<GamepadState> {
        let idx = pad_id as usize;
        if idx >= 4 {
            return None;
        }
        self.seq[idx] = self.seq[idx].wrapping_add(1);
        self.connected[idx] = connected;
        Some(GamepadState {
            pad_id,
            connected,
            buttons,
            left_trigger: left_trigger.min(255),
            right_trigger: right_trigger.min(255),
            left_x: left_x.clamp(i16::MIN as i32, i16::MAX as i32),
            left_y: left_y.clamp(i16::MIN as i32, i16::MAX as i32),
            right_x: right_x.clamp(i16::MIN as i32, i16::MAX as i32),
            right_y: right_y.clamp(i16::MIN as i32, i16::MAX as i32),
            seq: self.seq[idx],
        })
    }

    /// Produce disconnect states for every pad currently marked connected and mark all disconnected.
    pub fn disconnect_all(&mut self) -> Vec<GamepadState> {
        let mut out = Vec::new();
        for idx in 0..4 {
            if self.connected[idx] {
                self.connected[idx] = false;
                self.seq[idx] = self.seq[idx].wrapping_add(1);
                out.push(GamepadState {
                    pad_id: idx as u32,
                    connected: false,
                    seq: self.seq[idx],
                    ..Default::default()
                });
            }
        }
        out
    }
}

enum SessionCommand {
    SubmitPassword(SecretString),
    CancelAuth,
    Disconnect,
    RequestKeyframe,
    SetMaxBitrate(u32),
    UpdateSessionMode(SessionModeUpdate),
    SwitchDisplay(u32),
    SetAudioEnabled(bool),
    SetAudioProcessing {
        echo_cancellation: bool,
        noise_suppression: bool,
    },
    PushMicPcm {
        pcm16: Vec<i16>,
        sample_rate: u32,
        channels: u16,
    },
    SetClipboardEnabled(bool),
    Input(InputEvent),
    Gamepad(GamepadState),
    SendClipboardText(String),
    SendChat(ChatMessage),
}

struct SessionShared {
    hud: Mutex<HudAggregator>,
    clock: Mutex<ClockSync>,
    decode_us: AtomicU32,
    render_us: AtomicU32,
    gamepads: Mutex<GamepadTracker>,
    next_chat_id: AtomicU64,
    echo_cancellation: AtomicBool,
    noise_suppression: AtomicBool,
    mic_active: AtomicBool,
    aec_ref: AecReferenceBus,
}

impl Default for SessionShared {
    fn default() -> Self {
        Self {
            hud: Mutex::new(HudAggregator::new()),
            clock: Mutex::new(ClockSync::default()),
            decode_us: AtomicU32::new(0),
            render_us: AtomicU32::new(0),
            gamepads: Mutex::new(GamepadTracker::new()),
            next_chat_id: AtomicU64::new(1),
            echo_cancellation: AtomicBool::new(true),
            noise_suppression: AtomicBool::new(true),
            mic_active: AtomicBool::new(false),
            aec_ref: AecReferenceBus::new(),
        }
    }
}

/// Thread-safe handle to a running Android viewer session.
pub struct AndroidSessionHandle {
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
    shared: Arc<SessionShared>,
    disconnected: AtomicBool,
}

impl AndroidSessionHandle {
    pub fn submit_password(&self, password: impl Into<SecretString>) {
        let _ = self.cmd_tx.send(SessionCommand::SubmitPassword(password.into()));
    }

    pub fn cancel_auth(&self) {
        let _ = self.cmd_tx.send(SessionCommand::CancelAuth);
    }

    /// Request graceful disconnect. Idempotent — safe to call multiple times.
    pub fn disconnect(&self) {
        if !self.disconnected.swap(true, Ordering::AcqRel) {
            let _ = self.cmd_tx.send(SessionCommand::Disconnect);
        }
    }

    pub fn request_keyframe(&self) {
        let _ = self.cmd_tx.send(SessionCommand::RequestKeyframe);
    }

    pub fn report_decode_time_us(&self, decode_us: u32, render_us: u32) {
        self.shared.decode_us.store(decode_us, Ordering::Relaxed);
        self.shared.render_us.store(render_us, Ordering::Relaxed);
        self.shared.hud.lock().on_decode_render_us(decode_us, render_us);
    }

    pub fn set_max_bitrate(&self, bitrate_kbps: u32) {
        let _ = self.cmd_tx.send(SessionCommand::SetMaxBitrate(bitrate_kbps));
    }

    /// Send a live `SessionModeUpdate` control message to switch profile / stream parameters.
    pub fn update_session_mode(
        &self,
        mode: i32,
        max_bitrate_kbps: u32,
        max_fps: u32,
        audio_frame_duration_us: u32,
        prefer_yuv444: bool,
    ) {
        let normalized_mode = if mode == constants::MODE_GAMING {
            constants::MODE_GAMING
        } else {
            constants::MODE_DESKTOP
        };
        let _ = self.cmd_tx.send(SessionCommand::UpdateSessionMode(SessionModeUpdate {
            mode: normalized_mode,
            max_bitrate_kbps,
            max_fps,
            audio_frame_duration_us,
            prefer_yuv444,
        }));
    }

    pub fn switch_display(&self, display_index: u32) {
        let _ = self.cmd_tx.send(SessionCommand::SwitchDisplay(display_index));
    }

    pub fn set_audio_enabled(&self, enabled: bool) {
        let _ = self.cmd_tx.send(SessionCommand::SetAudioEnabled(enabled));
    }

    /// Configure live viewer microphone DSP (`NoiseSuppressor` + `AcousticEchoCanceller`).
    pub fn set_audio_processing(&self, echo_cancellation: bool, noise_suppression: bool) {
        self.shared.echo_cancellation.store(echo_cancellation, Ordering::Relaxed);
        self.shared.noise_suppression.store(noise_suppression, Ordering::Relaxed);
        let _ = self.cmd_tx.send(SessionCommand::SetAudioProcessing {
            echo_cancellation,
            noise_suppression,
        });
    }

    /// Push interleaved 16-bit PCM microphone samples through `Denoise -> AEC -> Opus` uplink.
    pub fn push_mic_pcm(&self, pcm16: Vec<i16>, sample_rate: u32, channels: u16) {
        if pcm16.is_empty() || sample_rate == 0 || (channels != 1 && channels != 2) {
            return;
        }
        if self.disconnected.load(Ordering::Acquire) {
            return;
        }
        self.shared.mic_active.store(true, Ordering::Relaxed);
        let _ = self.cmd_tx.send(SessionCommand::PushMicPcm { pcm16, sample_rate, channels });
    }

    pub fn set_clipboard_enabled(&self, enabled: bool) {
        let _ = self.cmd_tx.send(SessionCommand::SetClipboardEnabled(enabled));
    }

    pub fn send_mouse_move(&self, x: i32, y: i32, display: u32) {
        let ev = InputEvent {
            ts_us: Timestamp::now().as_micros(),
            event: Some(input_event::Event::MouseMove(MouseMove { x, y, display })),
        };
        let _ = self.cmd_tx.send(SessionCommand::Input(ev));
    }

    pub fn send_mouse_relative(&self, dx: i32, dy: i32) {
        let ev = InputEvent {
            ts_us: Timestamp::now().as_micros(),
            event: Some(input_event::Event::MouseRelative(MouseRelative { dx, dy })),
        };
        let _ = self.cmd_tx.send(SessionCommand::Input(ev));
    }

    pub fn send_mouse_button(&self, button: i32, down: bool) {
        let ev = InputEvent {
            ts_us: Timestamp::now().as_micros(),
            event: Some(input_event::Event::MouseButton(MouseButtonEvent { button, down })),
        };
        let _ = self.cmd_tx.send(SessionCommand::Input(ev));
    }

    pub fn send_mouse_wheel(&self, dx: i32, dy: i32) {
        let ev = InputEvent {
            ts_us: Timestamp::now().as_micros(),
            event: Some(input_event::Event::MouseWheel(MouseWheel { dx, dy })),
        };
        let _ = self.cmd_tx.send(SessionCommand::Input(ev));
    }

    pub fn send_key(&self, hid_usage: u32, down: bool, modifiers: u32) {
        let ev = InputEvent {
            ts_us: Timestamp::now().as_micros(),
            event: Some(input_event::Event::Key(KeyEvent { hid_usage, down, modifiers })),
        };
        let _ = self.cmd_tx.send(SessionCommand::Input(ev));
    }

    pub fn send_text(&self, text: String) {
        let ev = InputEvent {
            ts_us: Timestamp::now().as_micros(),
            event: Some(input_event::Event::Text(TextInput { text })),
        };
        let _ = self.cmd_tx.send(SessionCommand::Input(ev));
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send_gamepad(
        &self,
        pad_id: u32,
        connected: bool,
        buttons: u32,
        left_trigger: u32,
        right_trigger: u32,
        left_x: i32,
        left_y: i32,
        right_x: i32,
        right_y: i32,
    ) {
        if let Some(state) = self.shared.gamepads.lock().next_state(
            pad_id,
            connected,
            buttons,
            left_trigger,
            right_trigger,
            left_x,
            left_y,
            right_x,
            right_y,
        ) {
            let _ = self.cmd_tx.send(SessionCommand::Gamepad(state));
        }
    }

    pub fn send_clipboard_text(&self, text: String) {
        let _ = self.cmd_tx.send(SessionCommand::SendClipboardText(text));
    }

    /// Assign a monotonic message ID + UNIX `ts_ms` and queue a `ChatMessage` on the control stream.
    /// Returns the assigned non-zero ID, or `0` if `text` is empty or the session is closed.
    pub fn send_chat(&self, sender_name: String, text: String) -> u64 {
        if text.is_empty() || self.disconnected.load(Ordering::Acquire) {
            return 0;
        }
        let id = self.shared.next_chat_id.fetch_add(1, Ordering::Relaxed);
        let msg = ChatMessage { id, sender_name, text, ts_ms: unix_now_ms() };
        if self.cmd_tx.send(SessionCommand::SendChat(msg)).is_ok() { id } else { 0 }
    }
}

/// Safe handle registry mapping opaque non-zero `u64` IDs to [`AndroidSessionHandle`]s.
/// Eliminates raw pointer casts across JNI and guarantees double-disconnect is a safe no-op.
pub struct HandleTable {
    next_id: AtomicU64,
    entries: Mutex<HashMap<u64, Arc<AndroidSessionHandle>>>,
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HandleTable {
    pub fn new() -> Self {
        Self { next_id: AtomicU64::new(1), entries: Mutex::new(HashMap::new()) }
    }

    pub fn insert(&self, handle: Arc<AndroidSessionHandle>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.entries.lock().insert(id, handle);
        id
    }

    pub fn get(&self, id: u64) -> Option<Arc<AndroidSessionHandle>> {
        if id == 0 {
            return None;
        }
        self.entries.lock().get(&id).cloned()
    }

    pub fn remove(&self, id: u64) -> Option<Arc<AndroidSessionHandle>> {
        if id == 0 {
            return None;
        }
        self.entries.lock().remove(&id)
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }

    /// Create a session handle, register it in this table, and spawn its background runtime thread.
    /// Automatically removes the handle when the session terminates.
    pub fn spawn_session(
        self: &Arc<Self>,
        config: ConnectConfig,
        callbacks: Arc<dyn SessionCallbacks>,
    ) -> anyhow::Result<u64> {
        let shared = Arc::new(SessionShared::default());
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let handle = Arc::new(AndroidSessionHandle {
            cmd_tx,
            shared: shared.clone(),
            disconnected: AtomicBool::new(false),
        });
        let id = self.insert(handle);
        let table = self.clone();
        if let Err(e) = spawn_session_thread(config, shared, cmd_rx, callbacks, Some(Box::new(move || {
            table.remove(id);
        }))) {
            self.remove(id);
            return Err(e);
        }
        Ok(id)
    }
}

/// Start an Android viewer session on a background OS thread running a dedicated Tokio runtime.
pub fn start_session(
    config: ConnectConfig,
    callbacks: Arc<dyn SessionCallbacks>,
    on_exit: Option<Box<dyn FnOnce() + Send + 'static>>,
) -> anyhow::Result<Arc<AndroidSessionHandle>> {
    let shared = Arc::new(SessionShared::default());
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    spawn_session_thread(config, shared.clone(), cmd_rx, callbacks, on_exit)?;
    Ok(Arc::new(AndroidSessionHandle {
        cmd_tx,
        shared,
        disconnected: AtomicBool::new(false),
    }))
}

fn spawn_session_thread(
    config: ConnectConfig,
    shared_bg: Arc<SessionShared>,
    cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    callbacks: Arc<dyn SessionCallbacks>,
    on_exit: Option<Box<dyn FnOnce() + Send + 'static>>,
) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("velox-android-net".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("velox-android-rt")
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    callbacks.on_closed(&format!("failed to create async runtime: {e}"), true);
                    if let Some(cleanup) = on_exit {
                        cleanup();
                    }
                    return;
                }
            };

            let outcome = rt.block_on(run_session_async(config, shared_bg, cmd_rx, callbacks.clone()));
            drop(rt);
            match outcome {
                SessionEnd::LocalDisconnect => callbacks.on_closed("Disconnected", false),
                SessionEnd::RemoteNormal(reason) => callbacks.on_closed(&reason, false),
                SessionEnd::Error(reason) => callbacks.on_closed(&reason, true),
            }
            if let Some(cleanup) = on_exit {
                cleanup();
            }
        })
        .context("spawning Android viewer network thread")?;
    Ok(())
}

enum SessionEnd {
    LocalDisconnect,
    RemoteNormal(String),
    Error(String),
}

// ───────────────────────── Mic Audio Processing (Denoise -> AEC -> Opus) ─────────────────────────

const MIC_OPUS_BITRATE_KBPS: u32 = 64;
const MIC_DEFAULT_FRAME_US: u32 = 10_000;

/// Dual-channel-layout far-end speaker reference bus feeding [`AcousticEchoCanceller`].
///
/// Playback from the host is typically stereo 48 kHz while Android microphone capture can be
/// mono or stereo; maintaining both layouts avoids mismatched channel counts during NLMS.
#[derive(Clone)]
pub struct AecReferenceBus {
    mono: AecReferenceFeed,
    stereo: AecReferenceFeed,
}

impl Default for AecReferenceBus {
    fn default() -> Self {
        Self::new()
    }
}

impl AecReferenceBus {
    pub fn new() -> Self {
        Self {
            mono: AecReferenceFeed::new(9_600),
            stereo: AecReferenceFeed::new(19_200),
        }
    }

    pub fn push_playback(&self, samples: &[f32], channels: u16) {
        if samples.is_empty() {
            return;
        }
        if channels <= 1 {
            self.mono.push(samples);
            let mut stereo = Vec::with_capacity(samples.len() * 2);
            for &s in samples {
                stereo.push(s);
                stereo.push(s);
            }
            self.stereo.push(&stereo);
        } else {
            self.stereo.push(samples);
            let mut mono = Vec::with_capacity(samples.len() / 2);
            for chunk in samples.chunks_exact(2) {
                mono.push((chunk[0] + chunk[1]) * 0.5);
            }
            self.mono.push(&mono);
        }
    }

    pub fn feed_for_channels(&self, channels: u16) -> &AecReferenceFeed {
        if channels <= 1 { &self.mono } else { &self.stereo }
    }

    pub fn clear(&self) {
        self.mono.clear();
        self.stereo.clear();
    }
}

/// Android viewer microphone uplink DSP pipeline:
/// `PCM16 -> f32 -> NoiseSuppressor -> AcousticEchoCanceller -> OpusEncoder`.
pub struct MicAudioProcessor {
    echo_cancellation: bool,
    noise_suppression: bool,
    sample_rate: u32,
    channels: u16,
    frame_duration_us: u32,
    denoiser: NoiseSuppressor,
    aec: AcousticEchoCanceller,
    encoder: Option<OpusEncoder>,
    f32_scratch: Vec<f32>,
    resample_scratch: Vec<f32>,
}

impl MicAudioProcessor {
    pub fn new(sample_rate: u32, channels: u16, echo_cancellation: bool, noise_suppression: bool) -> Self {
        let sr = sample_rate.max(8_000);
        let ch = channels.clamp(1, 2);
        let denoiser = NoiseSuppressor::new(DenoiseConfig {
            sample_rate: sr,
            channels: ch,
            ..Default::default()
        });
        let aec = AcousticEchoCanceller::new(AecConfig {
            sample_rate: sr,
            channels: ch,
            ..Default::default()
        });
        Self {
            echo_cancellation,
            noise_suppression,
            sample_rate: sr,
            channels: ch,
            frame_duration_us: MIC_DEFAULT_FRAME_US,
            denoiser,
            aec,
            encoder: None,
            f32_scratch: Vec::with_capacity(1024),
            resample_scratch: Vec::with_capacity(1024),
        }
    }

    pub fn echo_cancellation(&self) -> bool {
        self.echo_cancellation
    }

    pub fn noise_suppression(&self) -> bool {
        self.noise_suppression
    }

    pub fn last_erle_db(&self) -> f32 {
        self.aec.last_erle_db()
    }

    pub fn set_processing(&mut self, echo_cancellation: bool, noise_suppression: bool) {
        if self.echo_cancellation != echo_cancellation {
            self.echo_cancellation = echo_cancellation;
            self.aec.reset();
        }
        if self.noise_suppression != noise_suppression {
            self.noise_suppression = noise_suppression;
            self.denoiser.reset();
        }
    }

    pub fn set_frame_duration_us(&mut self, frame_duration_us: u32) {
        let valid = matches!(frame_duration_us, 2_500 | 5_000 | 10_000 | 20_000);
        if valid && self.frame_duration_us != frame_duration_us {
            self.frame_duration_us = frame_duration_us;
            self.encoder = None;
        }
    }

    fn reconfigure_dsp_if_needed(&mut self, sample_rate: u32, channels: u16) {
        if self.sample_rate != sample_rate || self.channels != channels {
            self.sample_rate = sample_rate;
            self.channels = channels;
            self.denoiser = NoiseSuppressor::new(DenoiseConfig {
                sample_rate,
                channels,
                ..Default::default()
            });
            self.aec = AcousticEchoCanceller::new(AecConfig {
                sample_rate,
                channels,
                ..Default::default()
            });
            self.encoder = None;
        }
    }

    /// Run `NoiseSuppressor -> AcousticEchoCanceller` in-place on interleaved `f32` samples.
    pub fn process_f32_in_place(&mut self, samples: &mut [f32], aec_ref: Option<&AecReferenceBus>) {
        if samples.is_empty() {
            return;
        }
        if self.noise_suppression {
            self.denoiser.process(samples);
        }
        if self.echo_cancellation {
            if let Some(bus) = aec_ref {
                self.aec.process_capture_with_feed(samples, bus.feed_for_channels(self.channels));
            } else {
                self.aec.process_capture(samples);
            }
        }
    }

    /// Convert PCM16 microphone samples to `f32`, apply `Denoise -> AEC`, and encode to Opus packets.
    pub fn process_pcm16_to_opus(
        &mut self,
        pcm16: &[i16],
        sample_rate: u32,
        channels: u16,
        aec_ref: Option<&AecReferenceBus>,
    ) -> Vec<EncodedAudio> {
        if pcm16.is_empty() || sample_rate == 0 || (channels != 1 && channels != 2) {
            return Vec::new();
        }
        self.f32_scratch.clear();
        self.f32_scratch.reserve(pcm16.len());
        for &s in pcm16 {
            self.f32_scratch.push(f32::from(s) / 32768.0);
        }

        const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
        let target_rate = if OPUS_RATES.contains(&sample_rate) {
            sample_rate
        } else {
            crate::host::resample_linear_interleaved(
                &self.f32_scratch,
                sample_rate,
                48_000,
                channels as usize,
                &mut self.resample_scratch,
            );
            std::mem::swap(&mut self.f32_scratch, &mut self.resample_scratch);
            48_000
        };

        self.reconfigure_dsp_if_needed(target_rate, channels);

        let mut samples = std::mem::take(&mut self.f32_scratch);
        self.process_f32_in_place(&mut samples, aec_ref);

        let target_fmt = AudioFormat { sample_rate: target_rate, channels };
        let need_encoder = self.encoder.as_ref().is_none_or(|e| e.format() != target_fmt);
        if need_encoder {
            let cfg = AudioEncoderConfig {
                format: target_fmt,
                bitrate_kbps: MIC_OPUS_BITRATE_KBPS,
                frame_duration: Duration::from_micros(self.frame_duration_us as u64),
                low_delay: true,
            };
            match OpusEncoder::new(&cfg) {
                Ok(enc) => self.encoder = Some(enc),
                Err(e) => {
                    tracing::warn!("Android viewer mic OpusEncoder init failed: {e}");
                    self.f32_scratch = samples;
                    return Vec::new();
                }
            }
        }

        let pkts = if let Some(enc) = self.encoder.as_mut() {
            let frame = AudioFrame {
                format: target_fmt,
                samples: samples.clone(),
                capture_ts: Timestamp::now(),
            };
            match enc.encode(&frame) {
                Ok(p) => p,
                Err(e) => {
                    tracing::debug!("Android viewer mic Opus encode error: {e}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        self.f32_scratch = samples;
        pkts
    }
}

// ───────────────────────── Audio decode thread (Opus + JitterBuffer + PLC) ─────────────────────────

struct AndroidAudioThread {
    tx: Sender<EncodedAudio>,
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl AndroidAudioThread {
    fn spawn(
        cfg: Option<&AudioConfig>,
        initial_enabled: bool,
        shared: Arc<SessionShared>,
        callbacks: Arc<dyn SessionCallbacks>,
    ) -> Option<Self> {
        let fmt = cfg
            .map(|c| AudioFormat {
                sample_rate: c.sample_rate.max(8_000),
                channels: c.channels.clamp(1, 2) as u16,
            })
            .unwrap_or(AudioFormat::STEREO_48K);
        let frame_us = cfg.map(|c| c.frame_duration_us).filter(|&u| u >= 2_500).unwrap_or(5_000);
        let (tx, rx) = crossbeam_channel::bounded::<EncodedAudio>(64);
        let enabled = Arc::new(AtomicBool::new(initial_enabled));
        let stop = Arc::new(AtomicBool::new(false));
        let enabled_bg = enabled.clone();
        let stop_bg = stop.clone();
        let join = std::thread::Builder::new()
            .name("velox-android-audio".into())
            .spawn(move || audio_decode_loop(rx, fmt, frame_us, enabled_bg, stop_bg, shared, callbacks))
            .ok()?;
        Some(Self { tx, enabled, stop, join: Some(join) })
    }

    fn push(&self, pkt: EncodedAudio) {
        let _ = self.tx.try_send(pkt);
    }

    fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for AndroidAudioThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn audio_decode_loop(
    rx: Receiver<EncodedAudio>,
    fmt: AudioFormat,
    frame_us: u32,
    enabled: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    shared: Arc<SessionShared>,
    callbacks: Arc<dyn SessionCallbacks>,
) {
    let frame_dur = Duration::from_micros(frame_us as u64);
    let mut decoder = match OpusDecoder::new(fmt, frame_dur) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("OpusDecoder init failed (audio disabled): {e}");
            return;
        }
    };
    let mut jb = AudioJitterBuffer::new(JitterConfig {
        frame_duration: frame_dur,
        target_delay: (frame_dur * 3).max(Duration::from_millis(15)),
        max_delay: Duration::from_millis(80),
    });
    let mut pcm16 = Vec::with_capacity(
        (fmt.sample_rate as usize * frame_us as usize / 1_000_000) * fmt.channels as usize + 64,
    );
    let mut next_tick = Instant::now() + frame_dur;

    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if now < next_tick {
            match rx.recv_timeout(next_tick - now) {
                Ok(pkt) => {
                    jb.push(pkt);
                    while let Ok(more) = rx.try_recv() {
                        jb.push(more);
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        }
        let now = Instant::now();
        if now < next_tick {
            continue;
        }
        next_tick += frame_dur;
        if next_tick + frame_dur * 4 < now {
            next_tick = now + frame_dur;
        }
        match jb.pop_decoded(&mut decoder) {
            Ok(Some(pcm)) => {
                if enabled.load(Ordering::Relaxed) && !pcm.samples.is_empty() {
                    if shared.mic_active.load(Ordering::Relaxed)
                        && shared.echo_cancellation.load(Ordering::Relaxed)
                    {
                        shared.aec_ref.push_playback(&pcm.samples, pcm.format.channels);
                    }
                    f32_to_pcm16(&pcm.samples, &mut pcm16);
                    callbacks.on_audio_pcm(pcm.format.sample_rate as i32, pcm.format.channels as i32, &pcm16);
                }
            }
            Ok(None) => {}
            Err(e) => tracing::debug!("Opus decode error: {e}"),
        }
    }
}

// ───────────────────────── Clipboard helper ─────────────────────────

struct ClipboardHandle {
    out_tx: mpsc::UnboundedSender<String>,
    stop_tx: watch::Sender<bool>,
}

impl ClipboardHandle {
    fn spawn(ch: Arc<dyn TransportChannel>, callbacks: Arc<dyn SessionCallbacks>) -> Self {
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = watch::channel(false);
        tokio::spawn(async move {
            match ch.open_bi(StreamKind::Clipboard).await {
                Ok((send, recv)) => {
                    let cb = callbacks.clone();
                    if let Err(e) =
                        run_android_clipboard_stream(send, recv, out_rx, move |text| cb.on_clipboard_text(text), stop_rx)
                            .await
                    {
                        tracing::debug!("clipboard sync ended: {e}");
                    }
                }
                Err(e) => tracing::warn!("open Clipboard stream failed: {e}"),
            }
        });
        Self { out_tx, stop_tx }
    }

    fn send_text(&self, text: String) {
        let _ = self.out_tx.send(text);
    }

    fn stop(self) {
        let _ = self.stop_tx.send(true);
    }
}

// ───────────────────────── Hello & Handshake ─────────────────────────

fn build_hello(id: &DeviceIdentity, cfg: &ConnectConfig) -> Hello {
    let video_caps: Vec<VideoCodecCap> = decode_codec_caps_mask(
        cfg.codec_caps_mask,
        cfg.viewer_width,
        cfg.viewer_height,
        cfg.viewer_refresh_hz,
    );
    let device_name = if cfg.device_name.trim().is_empty() {
        "VeloxDesk Android".to_string()
    } else {
        cfg.device_name.trim().to_string()
    };
    Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: id.device_id(),
        public_key: id.public_key().to_vec(),
        device_name,
        os: "android".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        mode: cfg.mode,
        video_caps,
        audio_codecs: if cfg.audio_enabled { vec![AudioCodec::Opus as i32] } else { Vec::new() },
        viewer_width: cfg.viewer_width,
        viewer_height: cfg.viewer_height,
        viewer_refresh_hz: cfg.viewer_refresh_hz,
        max_bitrate_kbps: cfg.max_bitrate_kbps,
    }
}

enum HandshakeOutcome {
    Config(SessionConfig),
    Cancelled,
}

async fn recv_disconnect(cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>) {
    while let Some(cmd) = cmd_rx.recv().await {
        if matches!(cmd, SessionCommand::Disconnect | SessionCommand::CancelAuth) {
            return;
        }
    }
}

async fn wait_for_password(cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>) -> Option<SecretString> {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            SessionCommand::SubmitPassword(pw) => return Some(pw),
            SessionCommand::CancelAuth | SessionCommand::Disconnect => return None,
            _ => {}
        }
    }
    None
}

async fn respond_to_challenge(
    challenge: &AuthChallenge,
    id: &DeviceIdentity,
    tls_peer_pk: Option<[u8; 32]>,
    initial_password: &mut Option<SecretString>,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    callbacks: &Arc<dyn SessionCallbacks>,
) -> anyhow::Result<Option<AuthResponse>> {
    // Fail closed if the transport did not authenticate the peer's Ed25519 TLS certificate.
    let Some(tls_pk) = tls_peer_pk else {
        bail!("missing TLS peer public key for host identity binding");
    };

    let host_pk: [u8; 32] = if challenge.host_public_key.is_empty() {
        tls_pk
    } else {
        let pk: [u8; 32] = challenge
            .host_public_key
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid host public key length in AuthChallenge"))?;
        if pk != tls_pk {
            bail!("host public key in AuthChallenge does not match TLS certificate");
        }
        pk
    };

    if !challenge.host_device_id.is_empty() {
        let derived = veloxdesk_crypto::device_id_from_public_key(&host_pk);
        if derived != challenge.host_device_id {
            bail!("host device_id does not match host public key");
        }
    }

    let has = |m: AuthMethod| challenge.methods.contains(&(m as i32));

    // 1. Trusted Ed25519 public key (passwordless).
    if has(AuthMethod::PublicKey) {
        callbacks.on_status(constants::STATUS_AUTHENTICATING, "Authenticating with device key...");
        let sig = sign_pubkey_challenge(id, &challenge.nonce, &host_pk);
        return Ok(Some(AuthResponse {
            method: AuthMethod::PublicKey as i32,
            proof: sig.to_vec(),
        }));
    }

    // 2. Permanent password / OTP challenge (Argon2id + HMAC-SHA256; never sent in clear).
    let allow_perm = has(AuthMethod::PermanentPassword);
    let allow_otp = has(AuthMethod::OneTimePassword);
    if allow_perm || allow_otp {
        let password = match initial_password.take() {
            Some(pw) => pw,
            None => {
                let host_id = if challenge.host_device_id.is_empty() {
                    veloxdesk_crypto::device_id_from_public_key(&host_pk)
                } else {
                    challenge.host_device_id.clone()
                };
                let fp = veloxdesk_crypto::fingerprint(&host_pk);
                callbacks.on_auth_required(&host_id, &fp, allow_perm, allow_otp);
                let Some(pw) = wait_for_password(cmd_rx).await else {
                    return Ok(None);
                };
                pw
            }
        };

        callbacks.on_status(constants::STATUS_AUTHENTICATING, "Authenticating...");
        let method = if allow_perm && !challenge.salt.is_empty() {
            AuthMethod::PermanentPassword
        } else if allow_otp {
            AuthMethod::OneTimePassword
        } else {
            AuthMethod::PermanentPassword
        };

        let params = if challenge.argon2_m_cost_kib > 0 && challenge.argon2_t_cost > 0 && challenge.argon2_p_cost > 0 {
            Argon2Params {
                m_cost_kib: challenge.argon2_m_cost_kib,
                t_cost: challenge.argon2_t_cost,
                p_cost: challenge.argon2_p_cost,
            }
        } else {
            Argon2Params::default()
        };

        let salt = challenge.salt.clone();
        let nonce = challenge.nonce.clone();
        let proof = tokio::task::spawn_blocking(move || -> anyhow::Result<[u8; 32]> {
            let res = derive_password_key(password.as_str(), &salt, params)
                .map_err(|e| anyhow::anyhow!("password KDF failed: {e}"));
            drop(password); // SecretString::drop zeroizes immediately.
            let mut key = res?;
            let proof = password_proof(&key, &nonce);
            zeroize_bytes(&mut key);
            Ok(proof)
        })
        .await
        .context("password KDF task failed")??;

        return Ok(Some(AuthResponse { method: method as i32, proof: proof.to_vec() }));
    }

    // 3. Interactive approval only (no password required).
    if has(AuthMethod::None) || challenge.methods.is_empty() {
        callbacks.on_status(constants::STATUS_WAITING_FOR_ACCEPT, "Waiting for host approval...");
        return Ok(Some(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }));
    }

    bail!("no supported authentication methods offered: {:?}", challenge.methods);
}

#[allow(clippy::too_many_arguments)]
async fn handshake(
    ctrl_tx: &mut ControlSender,
    ctrl_rx: &mut ControlReceiver,
    cmd_rx: &mut mpsc::UnboundedReceiver<SessionCommand>,
    id: &DeviceIdentity,
    tls_peer_pk: Option<[u8; 32]>,
    mut initial_password: Option<SecretString>,
    callbacks: &Arc<dyn SessionCallbacks>,
) -> anyhow::Result<HandshakeOutcome> {
    let mut step_timeout = HANDSHAKE_TIMEOUT;
    loop {
        let env = tokio::select! {
            res = timeout(step_timeout, ctrl_rx.recv()) => {
                res.context("handshake timed out")?.context("reading handshake message")?
            }
            () = recv_disconnect(cmd_rx) => return Ok(HandshakeOutcome::Cancelled),
        };
        match env.msg {
            Some(Msg::AuthChallenge(challenge)) => {
                let Some(resp) = respond_to_challenge(
                    &challenge,
                    id,
                    tls_peer_pk,
                    &mut initial_password,
                    cmd_rx,
                    callbacks,
                )
                .await?
                else {
                    return Ok(HandshakeOutcome::Cancelled);
                };
                ctrl_tx
                    .send(&Msg::AuthResponse(resp).into())
                    .await
                    .context("sending AuthResponse")?;
                step_timeout = APPROVAL_TIMEOUT;
            }
            Some(Msg::AuthResult(r)) => {
                if !r.ok {
                    let reason = if r.reason.is_empty() {
                        "Authentication rejected".to_string()
                    } else {
                        r.reason
                    };
                    bail!("{reason}");
                }
                tracing::info!(session_id = r.session_id, "authenticated with host");
                step_timeout = HANDSHAKE_TIMEOUT;
            }
            Some(Msg::SessionConfig(cfg)) => {
                return Ok(HandshakeOutcome::Config(cfg));
            }
            Some(Msg::Close(c)) => {
                let reason = if c.reason.is_empty() {
                    format!("Host rejected connection (code {})", c.code)
                } else {
                    c.reason
                };
                bail!("{reason}");
            }
            Some(other) => tracing::debug!(?other, "message during handshake ignored"),
            None => {}
        }
    }
}

fn send_feedback(
    envelopes: Vec<Envelope>,
    ctrl_out: &mpsc::UnboundedSender<Envelope>,
    shared: &SessionShared,
) {
    for env in envelopes {
        if let Some(Msg::ReceiverReport(rr)) = &env.msg {
            shared.hud.lock().on_receiver_report(rr);
        }
        let _ = ctrl_out.send(env);
    }
}

fn emit_video_frame(
    rf: ReceivedFrame,
    active_codec: i32,
    active_dims: (u32, u32),
    shared: &SessionShared,
    callbacks: &Arc<dyn SessionCallbacks>,
) {
    let codec = if rf.packet.codec != CoreVideoCodec::Unspecified {
        rf.packet.codec as i32
    } else {
        active_codec
    };
    let width = if rf.packet.width > 0 { rf.packet.width } else { active_dims.0 };
    let height = if rf.packet.height > 0 { rf.packet.height } else { active_dims.1 };
    let decode_us = shared.decode_us.load(Ordering::Relaxed);
    let render_us = shared.render_us.load(Ordering::Relaxed);
    {
        let clock = shared.clock.lock();
        shared.hud.lock().on_frame_received(
            rf.packet.frame_id,
            rf.packet.capture_ts,
            rf.recv_complete_ts,
            decode_us,
            render_us,
            Some(&*clock),
        );
    }
    callbacks.on_video_frame(
        codec,
        width,
        height,
        rf.packet.frame_id,
        rf.packet.capture_ts.as_micros() as i64,
        rf.packet.keyframe,
        rf.discontinuity,
        rf.packet.data.as_ref(),
    );
}

fn handle_viewer_chat_message(
    chat: &ChatMessage,
    ctrl_out_tx: &mpsc::UnboundedSender<Envelope>,
    callbacks: &Arc<dyn SessionCallbacks>,
) {
    let _ = ctrl_out_tx.send(Msg::ChatAck(ChatAck { id: chat.id }).into());
    callbacks.on_chat_message(chat.id, &chat.sender_name, &chat.text, chat.ts_ms);
}

fn handle_viewer_chat_ack(ack: &ChatAck, callbacks: &Arc<dyn SessionCallbacks>) {
    callbacks.on_chat_ack(ack.id);
}

// ───────────────────────── Main async session loop ─────────────────────────

async fn run_session_async(
    mut cfg: ConnectConfig,
    shared: Arc<SessionShared>,
    mut cmd_rx: mpsc::UnboundedReceiver<SessionCommand>,
    callbacks: Arc<dyn SessionCallbacks>,
) -> SessionEnd {
    let id = match load_or_create_identity(&cfg.files_dir) {
        Ok(id) => id,
        Err(e) => return SessionEnd::Error(format!("identity error: {e:#}")),
    };

    let target = match parse_target(&cfg.target) {
        Ok(t) => t,
        Err(e) => return SessionEnd::Error(format!("invalid target: {e}")),
    };

    let target_label = match &target {
        ConnectTarget::Direct(addr) => addr.to_string(),
        ConnectTarget::DeviceId(dev_id) => format_device_id(dev_id),
    };
    callbacks.on_status(constants::STATUS_CONNECTING, &format!("Connecting to {target_label}..."));

    // 1. Establish QUIC transport (Direct IP or P2P/Relay via rendezvous).
    let ch: Arc<dyn TransportChannel> = match &target {
        ConnectTarget::Direct(addr) => tokio::select! {
            res = timeout(CONNECT_TIMEOUT, veloxdesk_transport::connect(*addr, &id, None)) => {
                match res {
                    Ok(Ok(ch)) => Arc::new(ch),
                    Ok(Err(e)) => return SessionEnd::Error(format!("connect failed: {e}")),
                    Err(_) => return SessionEnd::Error(format!("connect timed out ({addr})")),
                }
            }
            () = recv_disconnect(&mut cmd_rx) => return SessionEnd::LocalDisconnect,
        },
        ConnectTarget::DeviceId(target_id) => {
            if cfg.rendezvous_addr.trim().is_empty() {
                return SessionEnd::Error("Rendezvous server address is required for Device ID connections".into());
            }
            let rendezvous = match parse_rendezvous_addr(&cfg.rendezvous_addr) {
                Ok(addr) => addr,
                Err(e) => return SessionEnd::Error(format!("invalid rendezvous address: {e}")),
            };
            let mut p2p_opts = P2pConnectOptions::new(rendezvous, target_id.clone());
            p2p_opts.relay_only = cfg.relay_only;
            tokio::select! {
                res = timeout(CONNECT_TIMEOUT, veloxdesk_transport::connect_p2p(p2p_opts, &id)) => {
                    match res {
                        Ok(Ok(ch)) => Arc::new(ch),
                        Ok(Err(e)) => return SessionEnd::Error(format!("P2P connect failed: {e}")),
                        Err(_) => return SessionEnd::Error(format!("connect timed out ({target_label})")),
                    }
                }
                () = recv_disconnect(&mut cmd_rx) => return SessionEnd::LocalDisconnect,
            }
        }
    };

    let path = ch.stats().path;
    shared.hud.lock().set_path(path);

    // 2. Open Control stream and send Hello.
    let (send, recv) = match timeout(HANDSHAKE_TIMEOUT, ch.open_bi(StreamKind::Control)).await {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => {
            ch.close(1, "control stream failed");
            return SessionEnd::Error(format!("control stream failed: {e}"));
        }
        Err(_) => {
            ch.close(1, "handshake timeout");
            return SessionEnd::Error("timed out opening control stream".into());
        }
    };
    let (mut ctrl_tx, mut ctrl_rx) = framed(send, recv);
    let hello = build_hello(&id, &cfg);
    if let Err(e) = ctrl_tx.send(&Msg::Hello(hello).into()).await {
        if let Ok(Ok(Envelope { msg: Some(Msg::Close(c)) })) =
            timeout(Duration::from_millis(100), ctrl_rx.recv()).await
        {
            let reason = if c.reason.is_empty() {
                format!("Host rejected connection (code {})", c.code)
            } else {
                c.reason
            };
            ch.close(0, "host closed");
            return SessionEnd::Error(reason);
        }
        ch.close(1, "send hello failed");
        return SessionEnd::Error(format!("failed to send Hello: {e}"));
    }

    callbacks.on_status(constants::STATUS_WAITING_FOR_HOST, "Waiting for host...");

    // 3. Handshake (AuthChallenge -> AuthResponse -> AuthResult -> SessionConfig).
    let initial_pw = cfg.password.take();
    let initial_cfg = match handshake(
        &mut ctrl_tx,
        &mut ctrl_rx,
        &mut cmd_rx,
        &id,
        ch.peer_public_key(),
        initial_pw,
        &callbacks,
    )
    .await
    {
        Ok(HandshakeOutcome::Config(c)) => c,
        Ok(HandshakeOutcome::Cancelled) => {
            ch.close(0, "cancelled");
            tokio::time::sleep(Duration::from_millis(QUIC_CLOSE_FLUSH_MS)).await;
            return SessionEnd::LocalDisconnect;
        }
        Err(e) => {
            ch.close(1, "handshake failed");
            tokio::time::sleep(Duration::from_millis(QUIC_CLOSE_FLUSH_MS)).await;
            return SessionEnd::Error(format!("{e:#}"));
        }
    };

    // 4. Spawn control writer + dedicated Input stream writer.
    let (ctrl_out_tx, mut ctrl_out_rx) = mpsc::unbounded_channel::<Envelope>();
    let ctrl_writer = tokio::spawn(async move {
        while let Some(env) = ctrl_out_rx.recv().await {
            if ctrl_tx.send(&env).await.is_err() {
                break;
            }
        }
        let _ = ctrl_tx.finish().await;
    });

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<InputEvent>();
    let input_ch = ch.clone();
    let input_writer = tokio::spawn(async move {
        let Ok(stream) = input_ch.open_uni(StreamKind::Input).await else {
            tracing::warn!("could not open Input stream");
            return;
        };
        let mut sender = ControlSender::new(stream);
        while let Some(ev) = input_rx.recv().await {
            if sender.send(&Msg::Input(ev).into()).await.is_err() {
                break;
            }
        }
        let _ = sender.finish().await;
    });

    // 5. Audio decode thread, mic uplink DSP (`Denoise -> AEC -> Opus`), & optional clipboard stream.
    let audio_out = AndroidAudioThread::spawn(
        initial_cfg.audio.as_ref(),
        cfg.audio_enabled,
        shared.clone(),
        callbacks.clone(),
    );
    let mut mic_sender = AudioSender::new(ch.clone(), 1);
    let mut mic_processor = MicAudioProcessor::new(
        48_000,
        1,
        shared.echo_cancellation.load(Ordering::Relaxed),
        shared.noise_suppression.load(Ordering::Relaxed),
    );
    let mut clipboard = if cfg.clipboard_enabled {
        Some(ClipboardHandle::spawn(ch.clone(), callbacks.clone()))
    } else {
        None
    };

    // 6. Initialize VideoReceiver and deliver initial SessionConfig + CONNECTED status.
    let mut receiver = VideoReceiver::new();
    let mut active_codec = constants::CODEC_H264;
    let mut active_dims = (0u32, 0u32);
    if let Some(v) = &initial_cfg.video {
        receiver.set_dimensions(v.width, v.height);
        active_codec = v.codec;
        active_dims = (v.width, v.height);
    }
    let mut last_session_cfg = initial_cfg.clone();
    let initial_displays_json = displays_to_json(&initial_cfg.displays);
    callbacks.on_session_config(&initial_cfg, &initial_displays_json);
    callbacks.on_status(constants::STATUS_CONNECTED, &format!("Connected ({})", path.label()));

    // Immediate Ping #1 so ClockSync converges quickly.
    let mut ping_seq: u32 = 1;
    let _ = ctrl_out_tx.send(Msg::Ping(Ping { seq: ping_seq, sent_us: Timestamp::now().as_micros() }).into());

    let mut feedback_tick = interval(FEEDBACK_TICK);
    feedback_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_tick = interval(PING_TICK);
    ping_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping_tick.tick().await;

    // 7. Main session event loop.
    let end = loop {
        tokio::select! {
            dgram = ch.recv_datagram() => {
                match dgram {
                    Ok(buf) => {
                        let now = Timestamp::now();
                        match datagram_kind(&buf) {
                            Some(MediaKind::Audio) => {
                                if let Some((stream_id, pkt)) = AudioReceiver::parse_datagram(&buf)
                                    && stream_id == 0
                                    && pkt.codec == AudioCodecId::Opus
                                    && let Some(ao) = &audio_out
                                {
                                    ao.push(pkt);
                                }
                            }
                            Some(MediaKind::Video) | None => {
                                shared.hud.lock().on_bytes_received(buf.len());
                                if let Some(rf) = receiver.push_datagram(&buf, now) {
                                    emit_video_frame(rf, active_codec, active_dims, &shared, &callbacks);
                                }
                                while let Some(rf) = receiver.pop_frame() {
                                    emit_video_frame(rf, active_codec, active_dims, &shared, &callbacks);
                                }
                                let fb = receiver.poll_feedback(now);
                                send_feedback(fb, &ctrl_out_tx, &shared);
                            }
                            Some(_) => {}
                        }
                    }
                    Err(CoreError::Closed) => break SessionEnd::RemoteNormal("Host closed connection".into()),
                    Err(e) => break SessionEnd::Error(format!("transport error: {e}")),
                }
            }
            msg = ctrl_rx.recv() => {
                match msg {
                    Ok(env) => match env.msg {
                        Some(Msg::Pong(p)) => {
                            let t3 = Timestamp::now().as_micros();
                            let rtt = {
                                let mut clock = shared.clock.lock();
                                clock.add_sample(p.ping_sent_us, p.peer_recv_us, p.peer_send_us, t3);
                                clock.estimate().map(|(rtt, _)| rtt as u32)
                            };
                            receiver.set_host_time_reference(Timestamp(p.peer_send_us));
                            if let Some(rtt_us) = rtt {
                                receiver.set_rtt_us(rtt_us);
                                shared.hud.lock().on_rtt(rtt_us);
                            }
                        }
                        Some(Msg::FrameTiming(ft)) => {
                            receiver.set_host_time_reference(Timestamp(ft.capture_ts_us));
                            shared.hud.lock().on_frame_timing(ft);
                        }
                        Some(Msg::SessionConfig(sc)) => {
                            if let Some(v) = &sc.video {
                                receiver.set_dimensions(v.width, v.height);
                                active_codec = v.codec;
                                active_dims = (v.width, v.height);
                            }
                            let json = displays_to_json(&sc.displays);
                            last_session_cfg = sc.clone();
                            callbacks.on_session_config(&sc, &json);
                        }
                        Some(Msg::VideoConfig(v)) => {
                            receiver.set_dimensions(v.width, v.height);
                            active_codec = v.codec;
                            active_dims = (v.width, v.height);
                            last_session_cfg.video = Some(v);
                            let json = displays_to_json(&last_session_cfg.displays);
                            callbacks.on_session_config(&last_session_cfg, &json);
                        }
                        Some(Msg::DisplayList(dl)) => {
                            last_session_cfg.displays = dl.displays.clone();
                            let json = displays_to_json(&dl.displays);
                            callbacks.on_display_list(&json);
                        }
                        Some(Msg::Rumble(r)) => {
                            callbacks.on_rumble(r.pad_id as i32, r.low_freq as i32, r.high_freq as i32);
                        }
                        Some(Msg::CursorShape(cs)) => {
                            if is_valid_cursor_shape(cs.width, cs.height, cs.rgba.len()) {
                                callbacks.on_cursor_shape(
                                    cs.id,
                                    cs.width,
                                    cs.height,
                                    cs.hotspot_x,
                                    cs.hotspot_y,
                                    &cs.rgba,
                                );
                            } else {
                                tracing::debug!(
                                    id = cs.id,
                                    w = cs.width,
                                    h = cs.height,
                                    len = cs.rgba.len(),
                                    "ignoring invalid CursorShape"
                                );
                            }
                        }
                        Some(Msg::CursorPosition(cp)) => {
                            callbacks.on_cursor_position(cp.x, cp.y, cp.visible, cp.shape_id);
                        }
                        Some(Msg::ChatMessage(chat)) => {
                            handle_viewer_chat_message(&chat, &ctrl_out_tx, &callbacks);
                        }
                        Some(Msg::ChatAck(ack)) => {
                            handle_viewer_chat_ack(&ack, &callbacks);
                        }
                        Some(Msg::Close(c)) => {
                            let reason = if c.reason.is_empty() {
                                format!("Host closed session (code {})", c.code)
                            } else {
                                c.reason
                            };
                            if c.code == 0 {
                                break SessionEnd::RemoteNormal(reason);
                            } else {
                                break SessionEnd::Error(reason);
                            }
                        }
                        Some(other) => tracing::trace!(?other, "ignored control message"),
                        None => {}
                    },
                    Err(CoreError::Closed) => break SessionEnd::RemoteNormal("Host closed control stream".into()),
                    Err(e) => break SessionEnd::Error(format!("control stream error: {e}")),
                }
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SessionCommand::Input(ev)) => {
                        let _ = input_tx.send(ev);
                    }
                    Some(SessionCommand::Gamepad(state)) => {
                        if let Ok(dgram) = encode_gamepad_datagram(&state) {
                            let _ = ch.send_datagram(dgram);
                        }
                    }
                    Some(SessionCommand::RequestKeyframe) => {
                        receiver.request_keyframe();
                        let fb = receiver.poll_feedback(Timestamp::now());
                        send_feedback(fb, &ctrl_out_tx, &shared);
                    }
                    Some(SessionCommand::SetMaxBitrate(kbps)) => {
                        let _ = ctrl_out_tx.send(
                            Msg::BitrateUpdate(BitrateUpdate {
                                stream_id: 0,
                                bitrate_kbps: kbps,
                                fps: 0,
                                fec_parity_percent: 0,
                            })
                            .into(),
                        );
                    }
                    Some(SessionCommand::UpdateSessionMode(update)) => {
                        if update.audio_frame_duration_us > 0 {
                            mic_processor.set_frame_duration_us(update.audio_frame_duration_us);
                        }
                        let _ = ctrl_out_tx.send(Msg::SessionModeUpdate(update).into());
                    }
                    Some(SessionCommand::SwitchDisplay(index)) => {
                        let _ = ctrl_out_tx.send(Msg::SwitchDisplay(SwitchDisplay { index }).into());
                    }
                    Some(SessionCommand::SetAudioEnabled(on)) => {
                        if let Some(ao) = &audio_out {
                            ao.set_enabled(on);
                        }
                    }
                    Some(SessionCommand::SetAudioProcessing {
                        echo_cancellation,
                        noise_suppression,
                    }) => {
                        mic_processor.set_processing(echo_cancellation, noise_suppression);
                        if !echo_cancellation {
                            shared.aec_ref.clear();
                        }
                    }
                    Some(SessionCommand::PushMicPcm {
                        pcm16,
                        sample_rate,
                        channels,
                    }) => {
                        let pkts = mic_processor.process_pcm16_to_opus(
                            &pcm16,
                            sample_rate,
                            channels,
                            Some(&shared.aec_ref),
                        );
                        for pkt in pkts {
                            if let Err(e) = mic_sender.send(&pkt) {
                                tracing::debug!("Android viewer mic audio send error: {e}");
                                break;
                            }
                        }
                    }
                    Some(SessionCommand::SetClipboardEnabled(on)) => {
                        if on {
                            if clipboard.is_none() {
                                clipboard = Some(ClipboardHandle::spawn(ch.clone(), callbacks.clone()));
                            }
                        } else if let Some(clip) = clipboard.take() {
                            clip.stop();
                        }
                    }
                    Some(SessionCommand::SendClipboardText(text)) => {
                        if let Some(clip) = &clipboard {
                            clip.send_text(text);
                        }
                    }
                    Some(SessionCommand::SendChat(chat)) => {
                        let _ = ctrl_out_tx.send(Msg::ChatMessage(chat).into());
                    }
                    Some(SessionCommand::SubmitPassword(_pw)) => {
                        // Dropped immediately; SecretString::drop zeroizes the backing buffer.
                    }
                    Some(SessionCommand::CancelAuth) => {}
                    Some(SessionCommand::Disconnect) | None => {
                        let _ = ctrl_out_tx.send(
                            Msg::Close(Close { code: 0, reason: "viewer disconnected".into() }).into(),
                        );
                        break SessionEnd::LocalDisconnect;
                    }
                }
            }
            _ = feedback_tick.tick() => {
                receiver.set_decode_render_us(
                    shared.decode_us.load(Ordering::Relaxed),
                    shared.render_us.load(Ordering::Relaxed),
                );
                let stats = ch.stats();
                if stats.rtt_us > 0 && shared.clock.lock().estimate().is_none() {
                    receiver.set_rtt_us(stats.rtt_us);
                    shared.hud.lock().on_rtt(stats.rtt_us);
                }
                let now = Timestamp::now();
                let fb = receiver.poll_feedback(now);
                send_feedback(fb, &ctrl_out_tx, &shared);
                while let Some(rf) = receiver.pop_frame() {
                    emit_video_frame(rf, active_codec, active_dims, &shared, &callbacks);
                }
                let maybe_metrics = shared.hud.lock().maybe_publish(Instant::now());
                if let Some(metrics) = maybe_metrics {
                    callbacks.on_stats(&metrics);
                }
            }
            _ = ping_tick.tick() => {
                ping_seq = ping_seq.wrapping_add(1);
                let _ = ctrl_out_tx.send(
                    Msg::Ping(Ping { seq: ping_seq, sent_us: Timestamp::now().as_micros() }).into(),
                );
            }
        }
    };

    // 8. Clean teardown: disconnect virtual gamepads, stop clipboard & audio, flush Close & QUIC CONNECTION_CLOSE.
    let disconnect_pads = shared.gamepads.lock().disconnect_all();
    for state in disconnect_pads {
        if let Ok(dgram) = encode_gamepad_datagram(&state) {
            let _ = ch.send_datagram(dgram);
        }
    }
    if let Some(clip) = clipboard.take() {
        clip.stop();
    }
    if let Some(ao) = audio_out {
        ao.stop();
    }
    drop(input_tx);
    drop(ctrl_out_tx);
    let _ = timeout(Duration::from_millis(200), ctrl_writer).await;
    let _ = timeout(Duration::from_millis(200), input_writer).await;
    ch.close(0, "session ended");
    tokio::time::sleep(Duration::from_millis(QUIC_CLOSE_FLUSH_MS)).await;
    end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamepad_tracker_monotonic_seq_and_disconnect_all() {
        let mut gp = GamepadTracker::new();
        let s1 = gp.next_state(0, true, 0x1000, 300, 50, 40000, -40000, 100, -100).unwrap();
        assert_eq!(s1.pad_id, 0);
        assert!(s1.connected);
        assert_eq!(s1.seq, 1);
        assert_eq!(s1.left_trigger, 255);
        assert_eq!(s1.right_trigger, 50);
        assert_eq!(s1.left_x, i16::MAX as i32);
        assert_eq!(s1.left_y, i16::MIN as i32);

        let s2 = gp.next_state(0, true, 0x0, 0, 0, 0, 0, 0, 0).unwrap();
        assert_eq!(s2.seq, 2);

        let s3 = gp.next_state(2, true, 0x0010, 10, 20, 1, 2, 3, 4).unwrap();
        assert_eq!(s3.pad_id, 2);
        assert_eq!(s3.seq, 1);

        // Out-of-range pad id rejected safely.
        assert!(gp.next_state(4, true, 0, 0, 0, 0, 0, 0, 0).is_none());

        let disc = gp.disconnect_all();
        assert_eq!(disc.len(), 2);
        assert_eq!(disc[0].pad_id, 0);
        assert!(!disc[0].connected);
        assert_eq!(disc[0].seq, 3);
        assert_eq!(disc[1].pad_id, 2);
        assert!(!disc[1].connected);
        assert_eq!(disc[1].seq, 2);

        // Second disconnect_all is empty.
        assert!(gp.disconnect_all().is_empty());
    }

    #[test]
    fn handle_table_safety_and_double_disconnect() {
        let table = HandleTable::new();
        assert!(table.get(0).is_none());
        assert!(table.remove(0).is_none());
        assert!(table.is_empty());

        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = Arc::new(AndroidSessionHandle {
            cmd_tx: tx,
            shared: Arc::new(SessionShared::default()),
            disconnected: AtomicBool::new(false),
        });
        let id = table.insert(handle.clone());
        assert_ne!(id, 0);
        assert_eq!(table.len(), 1);
        assert!(table.get(id).is_some());

        // Double disconnect sends SessionCommand::Disconnect only once.
        let removed1 = table.remove(id).unwrap();
        removed1.disconnect();
        removed1.disconnect();
        assert!(table.remove(id).is_none());
        assert!(table.is_empty());
        assert!(matches!(rx.try_recv(), Ok(SessionCommand::Disconnect)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn displays_json_matches_kotlin_schema() {
        let displays = vec![DisplayInfo {
            index: 0,
            name: "Primary".into(),
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
            refresh_hz: 144,
            scale_factor: 1.5,
            primary: true,
            hdr: false,
        }];
        let json = displays_to_json(&displays);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["index"], 0);
        assert_eq!(arr[0]["name"], "Primary");
        assert_eq!(arr[0]["x"], 0);
        assert_eq!(arr[0]["y"], 0);
        assert_eq!(arr[0]["width"], 1920);
        assert_eq!(arr[0]["height"], 1080);
        assert_eq!(arr[0]["refreshHz"], 144);
        assert_eq!(arr[0]["primary"], true);
    }

    #[tokio::test]
    async fn auth_challenge_fails_closed_when_tls_peer_pk_missing() {
        struct NoopCallbacks;
        impl SessionCallbacks for NoopCallbacks {
            fn on_status(&self, _: i32, _: &str) {}
            fn on_auth_required(&self, _: &str, _: &str, _: bool, _: bool) {}
            fn on_session_config(&self, _: &SessionConfig, _: &str) {}
            fn on_display_list(&self, _: &str) {}
            fn on_video_frame(&self, _: i32, _: u32, _: u32, _: u32, _: i64, _: bool, _: bool, _: &[u8]) {}
            fn on_audio_pcm(&self, _: i32, _: i32, _: &[i16]) {}
            fn on_stats(&self, _: &HudMetrics) {}
            fn on_clipboard_text(&self, _: &str) {}
            fn on_rumble(&self, _: i32, _: i32, _: i32) {}
            fn on_closed(&self, _: &str, _: bool) {}
        }

        let id = DeviceIdentity::generate();
        let challenge = AuthChallenge {
            nonce: vec![1u8; 32],
            methods: vec![AuthMethod::PublicKey as i32],
            ..Default::default()
        };
        let mut pw = None;
        let (_tx, mut rx) = mpsc::unbounded_channel();
        let cb: Arc<dyn SessionCallbacks> = Arc::new(NoopCallbacks);
        let res = respond_to_challenge(&challenge, &id, None, &mut pw, &mut rx, &cb).await;
        assert!(res.is_err(), "respond_to_challenge must fail closed when tls_peer_pk is None");
    }

    #[test]
    fn cursor_shape_validation_bounds() {
        assert!(is_valid_cursor_shape(16, 16, 16 * 16 * 4));
        assert!(is_valid_cursor_shape(256, 256, 256 * 256 * 4));
        assert!(!is_valid_cursor_shape(0, 16, 0));
        assert!(!is_valid_cursor_shape(16, 0, 0));
        assert!(!is_valid_cursor_shape(257, 16, 257 * 16 * 4));
        assert!(!is_valid_cursor_shape(16, 257, 16 * 257 * 4));
        assert!(!is_valid_cursor_shape(16, 16, 16 * 16 * 4 - 1));
        assert!(!is_valid_cursor_shape(16, 16, 16 * 16 * 4 + 4));
    }

    #[test]
    fn send_chat_assigns_monotonic_ids_and_rejects_empty_or_disconnected() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = AndroidSessionHandle {
            cmd_tx: tx,
            shared: Arc::new(SessionShared::default()),
            disconnected: AtomicBool::new(false),
        };

        assert_eq!(handle.send_chat("Phone".into(), String::new()), 0);
        assert!(rx.try_recv().is_err());

        let id1 = handle.send_chat("Phone".into(), "Hello PC".into());
        let id2 = handle.send_chat("Phone".into(), "Привет".into());
        assert_eq!(id1, 1);
        assert_eq!(id2, 2);

        match rx.try_recv().unwrap() {
            SessionCommand::SendChat(m) => {
                assert_eq!(m.id, 1);
                assert_eq!(m.sender_name, "Phone");
                assert_eq!(m.text, "Hello PC");
                assert!(m.ts_ms > 0);
            }
            _ => panic!("expected SendChat"),
        }
        match rx.try_recv().unwrap() {
            SessionCommand::SendChat(m) => {
                assert_eq!(m.id, 2);
                assert_eq!(m.text, "Привет");
            }
            _ => panic!("expected SendChat"),
        }

        handle.disconnect();
        assert_eq!(handle.send_chat("Phone".into(), "After disconnect".into()), 0);
    }

    #[test]
    fn viewer_incoming_chat_sends_ack_and_invokes_callbacks() {
        #[derive(Default)]
        struct ChatCb {
            msgs: Mutex<Vec<(u64, String, String, u64)>>,
            acks: Mutex<Vec<u64>>,
        }
        impl SessionCallbacks for ChatCb {
            fn on_status(&self, _: i32, _: &str) {}
            fn on_auth_required(&self, _: &str, _: &str, _: bool, _: bool) {}
            fn on_session_config(&self, _: &SessionConfig, _: &str) {}
            fn on_display_list(&self, _: &str) {}
            fn on_video_frame(&self, _: i32, _: u32, _: u32, _: u32, _: i64, _: bool, _: bool, _: &[u8]) {}
            fn on_audio_pcm(&self, _: i32, _: i32, _: &[i16]) {}
            fn on_stats(&self, _: &HudMetrics) {}
            fn on_clipboard_text(&self, _: &str) {}
            fn on_rumble(&self, _: i32, _: i32, _: i32) {}
            fn on_chat_message(&self, id: u64, sender_name: &str, text: &str, ts_ms: u64) {
                self.msgs.lock().push((id, sender_name.to_string(), text.to_string(), ts_ms));
            }
            fn on_chat_ack(&self, id: u64) {
                self.acks.lock().push(id);
            }
            fn on_closed(&self, _: &str, _: bool) {}
        }

        let rec = Arc::new(ChatCb::default());
        let cb: Arc<dyn SessionCallbacks> = rec.clone();
        let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();

        let incoming = ChatMessage {
            id: 99,
            sender_name: "DesktopHost".into(),
            text: "Hello from PC!".into(),
            ts_ms: 1_700_000_000_000,
        };
        handle_viewer_chat_message(&incoming, &ctrl_tx, &cb);
        handle_viewer_chat_ack(&ChatAck { id: 1 }, &cb);

        let ack_env = ctrl_rx.try_recv().expect("viewer must send ChatAck");
        assert_eq!(ack_env.msg, Some(Msg::ChatAck(ChatAck { id: 99 })));
        assert_eq!(rec.msgs.lock().as_slice(), &[(99, "DesktopHost".into(), "Hello from PC!".into(), 1_700_000_000_000)]);
        assert_eq!(rec.acks.lock().as_slice(), &[1]);
    }

    #[test]
    fn update_session_mode_queues_normalized_update_and_roundtrips() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = AndroidSessionHandle {
            cmd_tx: tx,
            shared: Arc::new(SessionShared::default()),
            disconnected: AtomicBool::new(false),
        };

        handle.update_session_mode(constants::MODE_GAMING, 50_000, 120, 5_000, false);
        handle.update_session_mode(99, 20_000, 60, 10_000, true);

        match rx.try_recv().unwrap() {
            SessionCommand::UpdateSessionMode(u) => {
                assert_eq!(u.mode, constants::MODE_GAMING);
                assert_eq!(u.max_bitrate_kbps, 50_000);
                assert_eq!(u.max_fps, 120);
                assert_eq!(u.audio_frame_duration_us, 5_000);
                assert!(!u.prefer_yuv444);
                let env: Envelope = Msg::SessionModeUpdate(u.clone()).into();
                assert_eq!(env.msg, Some(Msg::SessionModeUpdate(u)));
            }
            _ => panic!("expected UpdateSessionMode"),
        }

        // Unknown mode normalizes to MODE_DESKTOP.
        match rx.try_recv().unwrap() {
            SessionCommand::UpdateSessionMode(u) => {
                assert_eq!(u.mode, constants::MODE_DESKTOP);
                assert_eq!(u.max_bitrate_kbps, 20_000);
                assert_eq!(u.max_fps, 60);
                assert_eq!(u.audio_frame_duration_us, 10_000);
                assert!(u.prefer_yuv444);
            }
            _ => panic!("expected UpdateSessionMode"),
        }
    }

    #[test]
    fn audio_processing_config_and_mic_dsp_pipeline_denoise_aec_opus() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let shared = Arc::new(SessionShared::default());
        let handle = AndroidSessionHandle {
            cmd_tx: tx,
            shared: shared.clone(),
            disconnected: AtomicBool::new(false),
        };

        // Defaults: echo cancellation & noise suppression both enabled.
        assert!(shared.echo_cancellation.load(Ordering::Relaxed));
        assert!(shared.noise_suppression.load(Ordering::Relaxed));

        handle.set_audio_processing(false, true);
        assert!(!shared.echo_cancellation.load(Ordering::Relaxed));
        assert!(shared.noise_suppression.load(Ordering::Relaxed));
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionCommand::SetAudioProcessing {
                echo_cancellation: false,
                noise_suppression: true,
            })
        ));

        // Invalid mic chunks are ignored; valid chunk sets mic_active and queues command.
        handle.push_mic_pcm(Vec::new(), 48_000, 1);
        handle.push_mic_pcm(vec![0; 480], 48_000, 3);
        assert!(rx.try_recv().is_err());

        handle.push_mic_pcm(vec![1000; 480], 48_000, 1);
        assert!(shared.mic_active.load(Ordering::Relaxed));
        assert!(matches!(rx.try_recv(), Ok(SessionCommand::PushMicPcm { sample_rate: 48_000, channels: 1, .. })));

        // Verify NoiseSuppressor + AcousticEchoCanceller + OpusEncoder end-to-end.
        let mut proc = MicAudioProcessor::new(48_000, 1, true, true);
        assert!(proc.echo_cancellation());
        assert!(proc.noise_suppression());

        let bus = AecReferenceBus::new();
        let mut x: u64 = 0xCAFE_BABE_1234_5678;
        let mut next_sample = || -> f32 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let u = (x >> 40) as f32 / ((1u32 << 24) as f32);
            (u * 2.0 - 1.0) * 0.45
        };

        let mut total_pkts = 0usize;
        let mut delay_line = std::collections::VecDeque::from(vec![0.0f32; 32]);
        let mut tail_in_pow = 0.0f64;
        let mut tail_out_pow = 0.0f64;

        // 80 x 10 ms frames = 800 ms of stereo playback echoed into mono mic.
        for frame_idx in 0..80usize {
            let mut stereo_far = Vec::with_capacity(480 * 2);
            let mut mono_mic = Vec::with_capacity(480);
            for _ in 0..480 {
                let s = next_sample();
                stereo_far.push(s);
                stereo_far.push(s);
                let delayed = delay_line.pop_front().unwrap_or(0.0);
                delay_line.push_back(s);
                mono_mic.push(delayed * 0.5);
            }
            bus.push_playback(&stereo_far, 2);

            let mut f32_copy = mono_mic.clone();
            proc.process_f32_in_place(&mut f32_copy, Some(&bus));
            assert!(f32_copy.iter().all(|v| v.is_finite()));
            if frame_idx >= 50 {
                tail_in_pow += mono_mic.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>();
                tail_out_pow += f32_copy.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>();
            }
        }
        assert!(
            tail_out_pow < tail_in_pow * 0.25,
            "AEC should strongly attenuate echo (in={tail_in_pow:.4}, out={tail_out_pow:.4}, erle={:.2}dB)",
            proc.last_erle_db()
        );

        // Verify PCM16 -> Denoise -> AEC -> Opus produces real Opus packets.
        let mut pcm_in = vec![0i16; 480];
        for (i, slot) in pcm_in.iter_mut().enumerate() {
            let t = i as f32 / 48_000.0;
            *slot = ((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 12_000.0) as i16;
        }
        for _ in 0..3 {
            let pkts = proc.process_pcm16_to_opus(&pcm_in, 48_000, 1, Some(&bus));
            for pkt in &pkts {
                assert_eq!(pkt.codec, AudioCodecId::Opus);
                assert!(!pkt.data.is_empty());
            }
            total_pkts += pkts.len();
        }
        assert!(total_pkts >= 3, "expected at least 3 encoded Opus mic packets, got {total_pkts}");

        // Toggling flags off resets state and leaves non-finite sanitization intact.
        proc.set_processing(false, false);
        assert!(!proc.echo_cancellation());
        assert!(!proc.noise_suppression());
        let mut raw = [0.25f32, -0.25f32];
        proc.process_f32_in_place(&mut raw, None);
        assert_eq!(raw, [0.25f32, -0.25f32]);
    }
}
