//! Pure-Rust Android Host engine speaking the standard VeloxDesk wire protocol (v0.4.0).
//!
//! Enables PC→Phone (`veloxdesk.exe` → Android) and Phone↔Phone (Android Viewer → Android Host)
//! connections over direct LAN/Tailscale (`UDP 21118`) or 9-digit Device ID (`P2pListener` via
//! Rendezvous `21116` + zero-knowledge Relay `21117`).
//!
//! Security & reliability parity with `crates/service`:
//! - Persistent Ed25519 identity (`<filesDir>/identity.key`) bound to mutual-TLS certificates.
//! - Password modes: `OneTime` (OTP only), `Permanent` (Argon2id PHC hash only), `Both`.
//!   - In `OneTime` mode, any stored permanent password hash is NOT accepted.
//!   - Safety invariant: NEVER allows `AuthMethod::None` and refuses to disable OTP if no valid
//!     permanent password hash is active.
//! - Rotating single-use 8-char OTP (`consume_and_rotate_otp` + guarded manual `rotate_otp`).
//! - Persistent trusted Ed25519 peers (`AuthMethod::PublicKey` bypass + live revocation).
//! - Interactive `AcceptPrompt` with 60 s timeout, granular permissions, and "Remember this device".
//! - Same-peer reconnect superseding (`max_sessions = 1`, authenticated `peer_public_key()` match),
//!   while a different peer receives a clean `Msg::Close("host busy")`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::timeout;
use veloxdesk_audio::OpusEncoder;
use veloxdesk_core_traits::audio::{AudioEncoder, AudioEncoderConfig, AudioFormat, AudioFrame};
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec as CoreVideoCodec};
use veloxdesk_core_traits::{CoreError, Result, Timestamp, TransportChannel};
use veloxdesk_crypto::auth::{
    Argon2Params, PasswordHash, derive_password_key, generate_nonce, generate_otp, verify_password_proof,
    verify_pubkey_challenge,
};
use veloxdesk_crypto::{DeviceIdentity, device_id_from_public_key, fingerprint};
use veloxdesk_protocol::control::{
    AudioCodec, AudioConfig, AuthChallenge, AuthMethod, AuthResponse, AuthResult, BitrateUpdate, ChromaFormat, Close,
    DisplayInfo, DisplayList, Envelope, FrameTimingReport, Hello, InputEvent, MouseButton, Permissions, Pong,
    SessionConfig, VideoCodec, VideoConfig, envelope::Msg, input_event::Event as InputEventKind,
};
use veloxdesk_protocol::{DEFAULT_DIRECT_PORT, StreamKind};
use veloxdesk_transport::{
    AbrController, AudioSender, ControlReceiver, ControlSender, Listener, P2pListener, QuicChannel, VideoSender,
    framed, listen,
};

use crate::clipboard::run_android_clipboard_stream;
use crate::session::load_or_create_identity;
use crate::target::parse_rendezvous_addr;

const DEFAULT_OTP_LEN: usize = 8;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const AUTH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
const SUPERSEDE_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_NORMAL: u32 = 0;
const CLOSE_PROTOCOL: u32 = 2;
const CLOSE_BUSY: u32 = 3;
const CLOSE_AUTH_FAILED: u32 = 4;

const MIN_BITRATE_KBPS: u32 = 500;
const DEFAULT_BITRATE_KBPS: u32 = 6_000;
const MAX_BITRATE_KBPS: u32 = 50_000;
const DEFAULT_FEC_PARITY_PERCENT: u8 = 15;
const AUDIO_FRAME_DURATION_US: u32 = 10_000;
const AUDIO_BITRATE_KBPS: u32 = 128;

/// Input event kind tags matching `VeloxHostCallback.onHostInputEvent(kind, ...)`.
pub mod input_kind {
    pub const MOUSE_MOVE: i32 = 0;
    pub const MOUSE_BUTTON: i32 = 1;
    pub const MOUSE_WHEEL: i32 = 2;
    pub const KEY: i32 = 3;
    pub const TEXT: i32 = 4;
}

/// Host password authentication mode matching the frozen JNI contract (`0=OneTime, 1=Permanent, 2=Both`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostPasswordMode {
    #[default]
    OneTime = 0,
    Permanent = 1,
    Both = 2,
}

impl HostPasswordMode {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::Permanent,
            2 => Self::Both,
            _ => Self::OneTime,
        }
    }
}

/// Persisted trusted peer JSON matching `VeloxHostCallback.onHostTrustedPeerAdded` and
/// `nativeHostStart(..., trustedPeersJson, ...)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SavedTrustedPeerJson {
    pub public_key_hex: String,
    pub device_id: String,
    pub name: String,
    pub fingerprint: String,
    pub allow_input: bool,
    pub allow_clipboard: bool,
    pub allow_file_transfer: bool,
    pub allow_audio: bool,
    pub allow_terminal: bool,
    pub allow_tunnel: bool,
    pub allow_privacy_mode: bool,
}

impl Default for SavedTrustedPeerJson {
    fn default() -> Self {
        Self {
            public_key_hex: String::new(),
            device_id: String::new(),
            name: String::new(),
            fingerprint: String::new(),
            allow_input: true,
            allow_clipboard: true,
            allow_file_transfer: false,
            allow_audio: true,
            allow_terminal: false,
            allow_tunnel: false,
            allow_privacy_mode: false,
        }
    }
}

/// Parse a 64-character hex-encoded Ed25519 public key into `[u8; 32]`.
pub fn parse_hex_pubkey_32(hex: &str) -> Option<[u8; 32]> {
    let s = hex.trim();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Encode a 32-byte Ed25519 public key as lowercase hex.
pub fn encode_hex_pubkey_32(pk: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for b in pk {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

/// Hash a permanent password into an Argon2id PHC string (`$argon2id$...`).
pub fn hash_password_phc(password: &str) -> anyhow::Result<String> {
    if password.is_empty() {
        anyhow::bail!("password must not be empty");
    }
    let ph = PasswordHash::hash(password).map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))?;
    Ok(ph.to_phc_string())
}

/// Validate an Argon2id PHC string (`$argon2id$...`).
pub fn is_valid_phc_hash(phc: &str) -> bool {
    !phc.trim().is_empty() && PasswordHash::from_phc_string(phc.trim()).is_ok()
}

/// Callback sink decoupling the pure-Rust Android host engine from JNI (also used in integration tests).
pub trait HostCallbacks: Send + Sync + 'static {
    fn on_listening(&self, device_id: &str, otp: &str, port: u16);
    fn on_otp_rotated(&self, otp: &str);
    fn on_approval_requested(
        &self,
        request_id: u64,
        peer_device_id: &str,
        peer_name: &str,
        peer_fingerprint: &str,
        peer_pub_key_hex: &str,
    );
    fn on_trusted_peer_added(&self, peer_json: &str);
    fn on_session_started(&self, peer_device_id: &str, peer_name: &str, requested_bitrate_kbps: u32, requested_fps: u32);
    fn on_session_ended(&self, peer_device_id: &str, reason: &str);
    fn on_bitrate_changed(&self, bitrate_kbps: u32);
    fn on_keyframe_requested(&self);
    #[allow(clippy::too_many_arguments)]
    fn on_input_event(
        &self,
        kind: i32,
        x: i32,
        y: i32,
        code: i32,
        down: bool,
        modifiers: i32,
        dx: i32,
        dy: i32,
        text: &str,
    );
    fn on_clipboard_text(&self, text: &str);
    fn on_error(&self, message: &str);
    fn on_stopped(&self);
}

/// Startup configuration for [`AndroidHostEngine`].
#[derive(Clone, Debug)]
pub struct AndroidHostConfig {
    pub files_dir: PathBuf,
    /// Optional explicit bind address (`None` defaults to `0.0.0.0:21118` or `[::]:21118` for IPv6 rendezvous).
    pub bind_addr: Option<SocketAddr>,
    pub rendezvous_addr: String,
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub password_mode: HostPasswordMode,
    pub permanent_hash_phc: String,
    pub require_accept: bool,
    pub trusted_peers_json: String,
}

#[derive(Debug, Clone)]
struct AndroidTrustedPeer {
    public_key: [u8; 32],
    name: String,
    permissions: Permissions,
}

impl AndroidTrustedPeer {
    fn from_saved(saved: &SavedTrustedPeerJson) -> Option<Self> {
        let pk = parse_hex_pubkey_32(&saved.public_key_hex)?;
        let perms = Permissions {
            view: true,
            input: saved.allow_input,
            clipboard: saved.allow_clipboard,
            file_transfer: saved.allow_file_transfer,
            audio: saved.allow_audio,
            tunnel: saved.allow_tunnel,
            terminal: saved.allow_terminal,
            restart: false,
            privacy_mode: saved.allow_privacy_mode,
            allow_chat: true,
            allow_power: false,
        };
        Some(Self { public_key: pk, name: saved.name.clone(), permissions: perms })
    }

    fn to_saved(&self) -> SavedTrustedPeerJson {
        SavedTrustedPeerJson {
            public_key_hex: encode_hex_pubkey_32(&self.public_key),
            device_id: device_id_from_public_key(&self.public_key),
            name: self.name.clone(),
            fingerprint: fingerprint(&self.public_key),
            allow_input: self.permissions.input,
            allow_clipboard: self.permissions.clipboard,
            allow_file_transfer: self.permissions.file_transfer,
            allow_audio: self.permissions.audio,
            allow_terminal: self.permissions.terminal,
            allow_tunnel: self.permissions.tunnel,
            allow_privacy_mode: self.permissions.privacy_mode,
        }
    }
}

struct HostAuthInner {
    password_mode: HostPasswordMode,
    saved_perm_hash: Option<PasswordHash>,
    current_otp: Option<String>,
    require_accept: bool,
    trusted_peers: Vec<AndroidTrustedPeer>,
}

struct AndroidHostAuthState {
    inner: Mutex<HostAuthInner>,
    callbacks: Arc<dyn HostCallbacks>,
}

impl AndroidHostAuthState {
    fn new(
        mode: HostPasswordMode,
        permanent_hash_phc: &str,
        require_accept: bool,
        trusted_peers_json: &str,
        callbacks: Arc<dyn HostCallbacks>,
    ) -> Self {
        let saved_perm_hash = parse_optional_phc(permanent_hash_phc);
        let trusted_peers = parse_trusted_peers_json(trusted_peers_json);
        let effective_perm = if mode == HostPasswordMode::OneTime { None } else { saved_perm_hash.as_ref() };
        // Safety invariant: NEVER leave host with neither OTP nor permanent password.
        let need_otp = mode != HostPasswordMode::Permanent || effective_perm.is_none();
        let current_otp = need_otp.then(|| generate_otp(DEFAULT_OTP_LEN));

        Self {
            inner: Mutex::new(HostAuthInner {
                password_mode: mode,
                saved_perm_hash,
                current_otp,
                require_accept,
                trusted_peers,
            }),
            callbacks,
        }
    }

    fn current_otp(&self) -> Option<String> {
        self.inner.lock().current_otp.clone()
    }

    fn effective_perm_hash(&self) -> Option<PasswordHash> {
        let g = self.inner.lock();
        if g.password_mode == HostPasswordMode::OneTime {
            None
        } else {
            g.saved_perm_hash.clone()
        }
    }

    fn require_accept(&self) -> bool {
        self.inner.lock().require_accept
    }

    fn find_trusted(&self, pk: &[u8; 32]) -> Option<AndroidTrustedPeer> {
        self.inner.lock().trusted_peers.iter().find(|p| &p.public_key == pk).cloned()
    }

    fn rotate_otp(&self) -> Option<String> {
        let next = {
            let mut g = self.inner.lock();
            // Do not silently re-enable OTP if host is in Permanent-only mode with a valid permanent password.
            let otp_allowed = g.password_mode != HostPasswordMode::Permanent || g.saved_perm_hash.is_none();
            if !otp_allowed && g.current_otp.is_none() {
                return None;
            }
            let fresh = fresh_otp(g.current_otp.as_deref());
            g.current_otp = Some(fresh.clone());
            fresh
        };
        self.callbacks.on_otp_rotated(&next);
        Some(next)
    }

    fn consume_and_rotate_otp(&self, matched: &str) -> Option<String> {
        let next = {
            let mut g = self.inner.lock();
            if g.current_otp.as_deref() != Some(matched) {
                return None;
            }
            let fresh = fresh_otp(Some(matched));
            g.current_otp = Some(fresh.clone());
            fresh
        };
        self.callbacks.on_otp_rotated(&next);
        Some(next)
    }

    fn update_auth(&self, mode: HostPasswordMode, permanent_hash_phc: &str, require_accept: bool) {
        let parsed = parse_optional_phc(permanent_hash_phc);
        let otp_notify: Option<String> = {
            let mut g = self.inner.lock();
            g.password_mode = mode;
            g.saved_perm_hash = parsed;
            g.require_accept = require_accept;

            let effective_perm_exists = mode != HostPasswordMode::OneTime && g.saved_perm_hash.is_some();
            // Safety invariant: refuse to disable OTP if no valid permanent PHC hash is active.
            let want_otp = mode != HostPasswordMode::Permanent || !effective_perm_exists;
            if want_otp {
                if g.current_otp.is_none() {
                    let fresh = fresh_otp(None);
                    g.current_otp = Some(fresh.clone());
                    Some(fresh)
                } else {
                    None
                }
            } else if g.current_otp.take().is_some() {
                // Signal UI that OTP is now disabled.
                Some(String::new())
            } else {
                None
            }
        };
        if let Some(otp) = otp_notify {
            self.callbacks.on_otp_rotated(&otp);
        }
    }

    fn add_trusted_peer(&self, peer: AndroidTrustedPeer) {
        let saved = peer.to_saved();
        {
            let mut g = self.inner.lock();
            if let Some(existing) = g.trusted_peers.iter_mut().find(|p| p.public_key == peer.public_key) {
                *existing = peer;
            } else {
                g.trusted_peers.push(peer);
            }
        }
        if let Ok(json) = serde_json::to_string(&saved) {
            self.callbacks.on_trusted_peer_added(&json);
        }
    }

    fn remove_trusted_peer(&self, pk: &[u8; 32]) -> bool {
        let mut g = self.inner.lock();
        let before = g.trusted_peers.len();
        g.trusted_peers.retain(|p| &p.public_key != pk);
        g.trusted_peers.len() != before
    }
}

fn parse_optional_phc(phc: &str) -> Option<PasswordHash> {
    let trimmed = phc.trim();
    if trimmed.is_empty() {
        return None;
    }
    match PasswordHash::from_phc_string(trimmed) {
        Ok(h) => Some(h),
        Err(e) => {
            tracing::warn!("ignoring invalid permanent_hash_phc: {e}");
            None
        }
    }
}

fn parse_trusted_peers_json(json: &str) -> Vec<AndroidTrustedPeer> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let parsed: Vec<SavedTrustedPeerJson> = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("ignoring malformed trusted_peers_json: {e}");
            return Vec::new();
        }
    };
    let mut out = Vec::with_capacity(parsed.len());
    for item in &parsed {
        if let Some(tp) = AndroidTrustedPeer::from_saved(item) {
            out.push(tp);
        } else {
            tracing::warn!(key = %item.public_key_hex, "ignoring malformed trusted peer key");
        }
    }
    out
}

fn fresh_otp(previous: Option<&str>) -> String {
    loop {
        let cand = generate_otp(DEFAULT_OTP_LEN);
        if previous != Some(cand.as_str()) {
            return cand;
        }
    }
}

#[derive(Debug, Clone)]
struct HostApprovalDecision {
    accepted: bool,
    remember_peer: bool,
    allow_input: bool,
    allow_clipboard: bool,
    allow_audio: bool,
}

struct PendingApprovals {
    next_id: AtomicU64,
    waiters: Mutex<HashMap<u64, oneshot::Sender<HostApprovalDecision>>>,
}

impl PendingApprovals {
    fn new() -> Self {
        Self { next_id: AtomicU64::new(1), waiters: Mutex::new(HashMap::new()) }
    }

    fn register(&self) -> (u64, oneshot::Receiver<HostApprovalDecision>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiters.lock().insert(id, tx);
        (id, rx)
    }

    fn cancel(&self, request_id: u64) {
        self.waiters.lock().remove(&request_id);
    }

    fn respond(&self, request_id: u64, decision: HostApprovalDecision) -> bool {
        if let Some(tx) = self.waiters.lock().remove(&request_id) {
            tx.send(decision).is_ok()
        } else {
            false
        }
    }
}

#[derive(Debug, Clone)]
struct DisplayState {
    name: String,
    width: u32,
    height: u32,
    fps: u32,
}

impl DisplayState {
    fn display_info(&self) -> DisplayInfo {
        DisplayInfo {
            index: 0,
            name: self.name.clone(),
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
            refresh_hz: self.fps,
            scale_factor: 1.0,
            primary: true,
            hdr: false,
        }
    }
}

struct HostAudioState {
    sender: AudioSender,
    encoder: Option<OpusEncoder>,
    f32_scratch: Vec<f32>,
    resample_scratch: Vec<f32>,
}

impl HostAudioState {
    fn new(ch: Arc<dyn TransportChannel>) -> Self {
        Self {
            sender: AudioSender::new(ch, 0),
            encoder: None,
            f32_scratch: Vec::with_capacity(2048),
            resample_scratch: Vec::with_capacity(2048),
        }
    }

    fn set_redundant(&mut self, enabled: bool) {
        self.sender.set_redundant(enabled);
    }

    fn push_pcm16(&mut self, pcm16: &[i16], sample_rate: u32, channels: u16) {
        if pcm16.is_empty() || sample_rate == 0 || (channels != 1 && channels != 2) {
            return;
        }
        // Convert interleaved i16 PCM to f32 (-1.0..=1.0).
        self.f32_scratch.clear();
        self.f32_scratch.reserve(pcm16.len());
        for &s in pcm16 {
            self.f32_scratch.push(f32::from(s) / 32768.0);
        }

        // Opus accepts 8/12/16/24/48 kHz natively; resample other rates (e.g. 44.1 kHz) to 48 kHz.
        const OPUS_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
        let (target_rate, samples_slice): (u32, &[f32]) = if OPUS_RATES.contains(&sample_rate) {
            (sample_rate, &self.f32_scratch)
        } else {
            resample_linear_interleaved(&self.f32_scratch, sample_rate, 48_000, channels as usize, &mut self.resample_scratch);
            (48_000, &self.resample_scratch)
        };

        let target_fmt = AudioFormat { sample_rate: target_rate, channels };
        let need_reinit = self.encoder.as_ref().is_none_or(|e| e.format() != target_fmt);
        if need_reinit {
            let cfg = AudioEncoderConfig {
                format: target_fmt,
                bitrate_kbps: AUDIO_BITRATE_KBPS,
                frame_duration: Duration::from_micros(AUDIO_FRAME_DURATION_US as u64),
                low_delay: true,
            };
            match OpusEncoder::new(&cfg) {
                Ok(enc) => self.encoder = Some(enc),
                Err(e) => {
                    tracing::warn!("AndroidHost OpusEncoder init failed: {e}");
                    return;
                }
            }
        }

        let Some(enc) = self.encoder.as_mut() else { return };
        let frame = AudioFrame {
            format: target_fmt,
            samples: samples_slice.to_vec(),
            capture_ts: Timestamp::now(),
        };
        match enc.encode(&frame) {
            Ok(pkts) => {
                for pkt in pkts {
                    if let Err(e) = self.sender.send(&pkt) {
                        tracing::debug!("AndroidHost audio send error: {e}");
                        break;
                    }
                }
            }
            Err(e) => tracing::debug!("AndroidHost Opus encode error: {e}"),
        }
    }
}

fn resample_linear_interleaved(src: &[f32], src_rate: u32, dst_rate: u32, channels: usize, dst: &mut Vec<f32>) {
    dst.clear();
    if src.is_empty() || src_rate == 0 || dst_rate == 0 || channels == 0 {
        return;
    }
    let src_frames = src.len() / channels;
    if src_frames == 0 {
        return;
    }
    let dst_frames = ((src_frames as u64 * dst_rate as u64) / src_rate as u64).max(1) as usize;
    dst.reserve(dst_frames * channels);
    let ratio = src_rate as f64 / dst_rate as f64;
    for i in 0..dst_frames {
        let pos = i as f64 * ratio;
        let idx = (pos.floor() as usize).min(src_frames - 1);
        let next = (idx + 1).min(src_frames - 1);
        let frac = (pos - idx as f64) as f32;
        for ch in 0..channels {
            let a = src[idx * channels + ch];
            let b = src[next * channels + ch];
            dst.push(a + (b - a) * frac);
        }
    }
}

/// Shared active-session bridge enabling zero-hop video/audio/display/clipboard push from Kotlin threads.
struct ActiveSessionBridge {
    session_id: u64,
    mode: i32,
    allow_audio: bool,
    bitrate_kbps: AtomicU32,
    frame_id: AtomicU32,
    video_sender: Mutex<VideoSender>,
    audio_state: Mutex<HostAudioState>,
    ctrl_out: mpsc::UnboundedSender<Envelope>,
    clip_out: Mutex<Option<mpsc::UnboundedSender<String>>>,
}

impl ActiveSessionBridge {
    fn new(
        session_id: u64,
        mode: i32,
        allow_audio: bool,
        initial_bitrate_kbps: u32,
        ch: Arc<dyn TransportChannel>,
        ctrl_out: mpsc::UnboundedSender<Envelope>,
    ) -> Self {
        let mut video_sender = VideoSender::new(ch.clone(), 0);
        video_sender.set_parity_percent(DEFAULT_FEC_PARITY_PERCENT);
        Self {
            session_id,
            mode,
            allow_audio,
            bitrate_kbps: AtomicU32::new(initial_bitrate_kbps),
            frame_id: AtomicU32::new(1),
            video_sender: Mutex::new(video_sender),
            audio_state: Mutex::new(HostAudioState::new(ch)),
            ctrl_out,
            clip_out: Mutex::new(None),
        }
    }

    fn build_session_config(&self, display: &DisplayState) -> SessionConfig {
        let parity = self.video_sender.lock().parity_percent() as u32;
        let video = VideoConfig {
            codec: VideoCodec::H264 as i32,
            width: display.width,
            height: display.height,
            fps: display.fps,
            bitrate_kbps: self.bitrate_kbps.load(Ordering::Relaxed),
            chroma: ChromaFormat::Yuv420 as i32,
            bit_depth: 8,
            encoder_name: "MediaCodec".to_string(),
            hw_encoder: true,
            zero_copy: true,
            fec_parity_percent: parity,
            stream_id: 0,
        };
        let audio = self.allow_audio.then_some(AudioConfig {
            codec: AudioCodec::Opus as i32,
            sample_rate: 48_000,
            channels: 2,
            frame_duration_us: AUDIO_FRAME_DURATION_US,
            bitrate_kbps: AUDIO_BITRATE_KBPS,
        });
        SessionConfig {
            displays: vec![display.display_info()],
            active_display: 0,
            video: Some(video),
            audio,
            host_name: display.name.clone(),
            host_os: "android".to_string(),
            mode: self.mode,
        }
    }

    fn push_video_frame(&self, data: Vec<u8>, is_keyframe: bool, pts_us: i64, width: u32, height: u32) {
        if data.is_empty() || width == 0 || height == 0 {
            return;
        }
        let frame_id = self.frame_id.fetch_add(1, Ordering::Relaxed);
        let capture_ts = if pts_us > 0 { Timestamp(pts_us as u64) } else { Timestamp::now() };
        let size_bytes = data.len() as u32;
        let pkt = EncodedPacket {
            codec: CoreVideoCodec::H264,
            data: Bytes::from(data),
            frame_id,
            keyframe: is_keyframe,
            recovery_point: is_keyframe,
            capture_ts,
            convert_us: 0,
            encode_us: 0,
            width,
            height,
        };
        let send_res = self.video_sender.lock().send_frame(&pkt);
        match send_res {
            Ok(stats) => {
                let timing = FrameTimingReport {
                    stream_id: 0,
                    frame_id,
                    capture_ts_us: capture_ts.as_micros(),
                    capture_us: 0,
                    convert_us: 0,
                    encode_us: 0,
                    packetize_us: stats.packetize_us,
                    size_bytes,
                };
                let _ = self.ctrl_out.send(Msg::FrameTiming(timing).into());
            }
            Err(CoreError::Closed) => {}
            Err(e) => tracing::debug!(frame_id, "AndroidHost send_frame failed: {e}"),
        }
    }

    fn push_audio_pcm(&self, pcm16: &[i16], sample_rate: u32, channels: u16) {
        if !self.allow_audio || pcm16.is_empty() {
            return;
        }
        self.audio_state.lock().push_pcm16(pcm16, sample_rate, channels);
    }

    fn notify_display_updated(&self, display: &DisplayState) {
        let dl = DisplayList { displays: vec![display.display_info()] };
        let _ = self.ctrl_out.send(Msg::DisplayList(dl).into());
        let _ = self.ctrl_out.send(Msg::SessionConfig(self.build_session_config(display)).into());
    }

    fn send_clipboard_text(&self, text: String) {
        if let Some(tx) = self.clip_out.lock().as_ref() {
            let _ = tx.send(text);
        }
    }
}

struct HostShared {
    auth: AndroidHostAuthState,
    approvals: PendingApprovals,
    display: RwLock<DisplayState>,
    active_session: RwLock<Option<Arc<ActiveSessionBridge>>>,
    callbacks: Arc<dyn HostCallbacks>,
}

/// Thread-safe handle to a running Android host engine.
pub struct AndroidHostEngine {
    shared: Arc<HostShared>,
    stop_tx: watch::Sender<bool>,
    stopped: AtomicBool,
    thread: Mutex<Option<(ThreadId, JoinHandle<()>)>>,
}

impl AndroidHostEngine {
    /// Start the Android host listener on a background Tokio runtime thread.
    pub fn start(cfg: AndroidHostConfig, callbacks: Arc<dyn HostCallbacks>) -> anyhow::Result<Self> {
        let id = load_or_create_identity(&cfg.files_dir)?;
        let rendezvous = if cfg.rendezvous_addr.trim().is_empty() {
            None
        } else {
            Some(
                parse_rendezvous_addr(&cfg.rendezvous_addr)
                    .map_err(|e| anyhow::anyhow!("invalid rendezvous address: {e}"))?,
            )
        };
        let bind_addr = cfg.bind_addr.unwrap_or_else(|| match rendezvous {
            Some(SocketAddr::V6(_)) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), DEFAULT_DIRECT_PORT),
            _ => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), DEFAULT_DIRECT_PORT),
        });

        let device_name = if cfg.device_name.trim().is_empty() {
            "VeloxDesk Android".to_string()
        } else {
            cfg.device_name.trim().to_string()
        };
        let width = cfg.width.max(16);
        let height = cfg.height.max(16);
        let fps = cfg.fps.clamp(15, 60);

        let shared = Arc::new(HostShared {
            auth: AndroidHostAuthState::new(
                cfg.password_mode,
                &cfg.permanent_hash_phc,
                cfg.require_accept,
                &cfg.trusted_peers_json,
                callbacks.clone(),
            ),
            approvals: PendingApprovals::new(),
            display: RwLock::new(DisplayState { name: device_name, width, height, fps }),
            active_session: RwLock::new(None),
            callbacks: callbacks.clone(),
        });

        let (stop_tx, stop_rx) = watch::channel(false);
        let shared_bg = shared.clone();
        let join = std::thread::Builder::new()
            .name("velox-android-host".into())
            .spawn(move || host_thread_main(bind_addr, rendezvous, id, shared_bg, stop_rx))
            .context("spawning Android host thread")?;
        let tid = join.thread().id();

        Ok(Self {
            shared,
            stop_tx,
            stopped: AtomicBool::new(false),
            thread: Mutex::new(Some((tid, join))),
        })
    }

    /// Push a hardware-encoded Annex-B H.264 video frame to the active viewer (no-op if idle).
    pub fn push_video_frame(&self, data: Vec<u8>, is_keyframe: bool, pts_us: i64, width: u32, height: u32) {
        let Some(session) = self.shared.active_session.read().clone() else { return };
        session.push_video_frame(data, is_keyframe, pts_us, width, height);
    }

    /// Push interleaved 16-bit PCM audio samples to be Opus-encoded and streamed to the active viewer.
    pub fn push_audio_pcm(&self, pcm16: &[i16], sample_rate: u32, channels: u16) {
        let Some(session) = self.shared.active_session.read().clone() else { return };
        session.push_audio_pcm(pcm16, sample_rate, channels);
    }

    /// Notify host & active viewer that the phone display dimensions/FPS changed (e.g. rotation).
    pub fn update_display(&self, width: u32, height: u32, fps: u32) {
        if width == 0 || height == 0 {
            return;
        }
        let updated = {
            let mut d = self.shared.display.write();
            d.width = width;
            d.height = height;
            if fps > 0 {
                d.fps = fps.clamp(15, 60);
            }
            d.clone()
        };
        if let Some(session) = self.shared.active_session.read().clone() {
            session.notify_display_updated(&updated);
        }
    }

    /// Manually rotate the one-time password (ignored when in Permanent-only mode with an active permanent hash).
    pub fn rotate_otp(&self) {
        let _ = self.shared.auth.rotate_otp();
    }

    /// Update password mode, permanent PHC hash, and interactive approval policy live.
    pub fn update_auth(&self, mode: HostPasswordMode, permanent_hash_phc: &str, require_accept: bool) {
        self.shared.auth.update_auth(mode, permanent_hash_phc, require_accept);
    }

    /// Revoke a trusted peer by its 64-char hex Ed25519 public key.
    pub fn remove_trusted_peer(&self, pub_key_hex: &str) -> bool {
        let Some(pk) = parse_hex_pubkey_32(pub_key_hex) else {
            return false;
        };
        self.shared.auth.remove_trusted_peer(&pk)
    }

    /// Respond to a pending interactive approval prompt (`onHostApprovalRequested`).
    #[allow(clippy::too_many_arguments)]
    pub fn respond_approval(
        &self,
        request_id: u64,
        accepted: bool,
        remember_peer: bool,
        allow_input: bool,
        allow_clipboard: bool,
        allow_audio: bool,
    ) -> bool {
        self.shared.approvals.respond(
            request_id,
            HostApprovalDecision { accepted, remember_peer, allow_input, allow_clipboard, allow_audio },
        )
    }

    /// Send local Android clipboard text to the active viewer over `StreamKind::Clipboard`.
    pub fn send_clipboard_text(&self, text: String) {
        if let Some(session) = self.shared.active_session.read().clone() {
            session.send_clipboard_text(text);
        }
    }

    /// Signal graceful shutdown (non-blocking & idempotent; safe to call from any thread).
    pub fn stop(&self) {
        if !self.stopped.swap(true, Ordering::AcqRel) {
            let _ = self.stop_tx.send(true);
        }
    }

    /// Signal graceful shutdown and wait for the host thread to finish (for tests / clean teardown).
    pub fn stop_and_join(&self) {
        self.stop();
        if let Some((tid, join)) = self.thread.lock().take()
            && std::thread::current().id() != tid
        {
            let _ = join.join();
        }
    }
}

impl Drop for AndroidHostEngine {
    fn drop(&mut self) {
        self.stop();
        if let Some((tid, join)) = self.thread.lock().take()
            && std::thread::current().id() != tid
        {
            let _ = join.join();
        }
    }
}

/// Opaque handle table mapping non-zero `u64` IDs to running [`AndroidHostEngine`] instances.
pub struct HostHandleTable {
    next_id: AtomicU64,
    hosts: RwLock<HashMap<u64, Arc<AndroidHostEngine>>>,
}

impl Default for HostHandleTable {
    fn default() -> Self {
        Self::new()
    }
}

impl HostHandleTable {
    pub fn new() -> Self {
        Self { next_id: AtomicU64::new(1), hosts: RwLock::new(HashMap::new()) }
    }

    pub fn spawn_host(&self, cfg: AndroidHostConfig, callbacks: Arc<dyn HostCallbacks>) -> anyhow::Result<u64> {
        let engine = Arc::new(AndroidHostEngine::start(cfg, callbacks)?);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.hosts.write().insert(id, engine);
        Ok(id)
    }

    pub fn get(&self, id: u64) -> Option<Arc<AndroidHostEngine>> {
        self.hosts.read().get(&id).cloned()
    }

    pub fn remove(&self, id: u64) -> Option<Arc<AndroidHostEngine>> {
        self.hosts.write().remove(&id)
    }

    pub fn len(&self) -> usize {
        self.hosts.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.read().is_empty()
    }
}

// ───────────────────────── Host Listener & Session Admission ─────────────────────────

enum AndroidListener {
    Direct(Listener),
    P2p(P2pListener),
}

impl AndroidListener {
    async fn bind(bind_addr: SocketAddr, rendezvous: Option<SocketAddr>, id: &DeviceIdentity) -> Result<Self> {
        match rendezvous {
            Some(rdv) => match P2pListener::bind(bind_addr, Some(rdv), id).await {
                Ok(l) => Ok(Self::P2p(l)),
                Err(CoreError::Io(e)) if e.kind() == std::io::ErrorKind::AddrInUse && bind_addr.port() != 0 => {
                    tracing::warn!(%bind_addr, "host port busy; retrying on ephemeral port");
                    let fallback = SocketAddr::new(bind_addr.ip(), 0);
                    Ok(Self::P2p(P2pListener::bind(fallback, Some(rdv), id).await?))
                }
                Err(e) => Err(e),
            },
            None => match listen(bind_addr, id).await {
                Ok(l) => Ok(Self::Direct(l)),
                Err(CoreError::Io(e)) if e.kind() == std::io::ErrorKind::AddrInUse && bind_addr.port() != 0 => {
                    tracing::warn!(%bind_addr, "host port busy; retrying on ephemeral port");
                    let fallback = SocketAddr::new(bind_addr.ip(), 0);
                    Ok(Self::Direct(listen(fallback, id).await?))
                }
                Err(e) => Err(e),
            },
        }
    }

    fn local_addr(&self) -> SocketAddr {
        match self {
            Self::Direct(l) => l.local_addr(),
            Self::P2p(l) => l.local_addr(),
        }
    }

    async fn accept(&self) -> Result<QuicChannel> {
        match self {
            Self::Direct(l) => l.accept().await,
            Self::P2p(l) => l.accept().await,
        }
    }

    fn shutdown(&self) {
        match self {
            Self::Direct(l) => l.close(),
            Self::P2p(l) => l.shutdown(),
        }
    }
}

struct ActiveSessionEntry {
    entry_id: u64,
    peer_pk: Option<[u8; 32]>,
    stop_tx: watch::Sender<bool>,
}

struct ActiveSessionGuard {
    entry_id: u64,
    registry: Arc<Mutex<Vec<ActiveSessionEntry>>>,
}

impl ActiveSessionGuard {
    fn register(
        registry: Arc<Mutex<Vec<ActiveSessionEntry>>>,
        entry_id: u64,
        peer_pk: Option<[u8; 32]>,
        stop_tx: watch::Sender<bool>,
    ) -> Self {
        registry.lock().push(ActiveSessionEntry { entry_id, peer_pk, stop_tx });
        Self { entry_id, registry }
    }
}

impl Drop for ActiveSessionGuard {
    fn drop(&mut self) {
        self.registry.lock().retain(|e| e.entry_id != self.entry_id);
    }
}

async fn wait_watch_true(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            break;
        }
    }
}

fn host_thread_main(
    bind_addr: SocketAddr,
    rendezvous: Option<SocketAddr>,
    id: DeviceIdentity,
    shared: Arc<HostShared>,
    stop_rx: watch::Receiver<bool>,
) {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .thread_name("velox-android-host-rt")
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            shared.callbacks.on_error(&format!("Failed to start host runtime: {e}"));
            shared.callbacks.on_stopped();
            return;
        }
    };

    rt.block_on(async move {
        if let Err(e) = serve_host(bind_addr, rendezvous, id, shared.clone(), stop_rx).await {
            shared.callbacks.on_error(&e.to_string());
        }
        shared.callbacks.on_stopped();
    });
}

async fn serve_host(
    bind_addr: SocketAddr,
    rendezvous: Option<SocketAddr>,
    id: DeviceIdentity,
    shared: Arc<HostShared>,
    mut host_stop: watch::Receiver<bool>,
) -> Result<()> {
    let listener = AndroidListener::bind(bind_addr, rendezvous, &id).await?;
    let local = listener.local_addr();
    let host_pk = id.public_key();
    let host_device_id = id.device_id();
    let initial_otp = shared.auth.current_otp().unwrap_or_default();

    tracing::info!(%local, device_id = %host_device_id, "AndroidHostEngine listening");
    shared.callbacks.on_listening(&host_device_id, &initial_otp, local.port());

    let permits = Arc::new(Semaphore::new(1));
    let active_registry: Arc<Mutex<Vec<ActiveSessionEntry>>> = Arc::new(Mutex::new(Vec::new()));
    let next_entry_id = Arc::new(AtomicU64::new(1));
    let mut sessions = JoinSet::new();

    loop {
        if *host_stop.borrow() {
            break;
        }
        tokio::select! {
            () = wait_watch_true(&mut host_stop) => {
                tracing::info!("AndroidHostEngine shutdown requested");
                break;
            }
            Some(res) = sessions.join_next(), if !sessions.is_empty() => {
                if let Err(e) = res {
                    tracing::warn!("AndroidHostEngine session task panicked: {e}");
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok(ch) => {
                        let ch: Arc<dyn TransportChannel> = Arc::new(ch);
                        match permits.clone().try_acquire_owned() {
                            Ok(permit) => {
                                let entry_id = next_entry_id.fetch_add(1, Ordering::Relaxed);
                                let (sess_stop_tx, sess_stop_rx) = watch::channel(false);
                                // Pre-register BEFORE spawning so rapid same-peer reconnects see this entry immediately.
                                let reg_guard = ActiveSessionGuard::register(
                                    active_registry.clone(),
                                    entry_id,
                                    ch.peer_public_key(),
                                    sess_stop_tx.clone(),
                                );
                                let shared_c = shared.clone();
                                let stop_c = host_stop.clone();
                                sessions.spawn(async move {
                                    // `_reg_guard` declared AFTER `_permit` so it drops BEFORE `_permit` is released.
                                    let _permit = permit;
                                    let _reg_guard = reg_guard;
                                    run_bound_session(ch, entry_id, host_pk, shared_c, stop_c, sess_stop_tx, sess_stop_rx).await;
                                });
                            }
                            Err(_) => {
                                let incoming_pk = ch.peer_public_key();
                                let superseded = if let Some(pk) = incoming_pk {
                                    let g = active_registry.lock();
                                    let mut matched = false;
                                    for entry in g.iter() {
                                        if entry.peer_pk == Some(pk) {
                                            let _ = entry.stop_tx.send(true);
                                            matched = true;
                                        }
                                    }
                                    matched
                                } else {
                                    false
                                };

                                if superseded {
                                    tracing::info!("same peer reconnected while busy; superseding stale session");
                                    let entry_id = next_entry_id.fetch_add(1, Ordering::Relaxed);
                                    let permits_c = permits.clone();
                                    let registry_c = active_registry.clone();
                                    let shared_c = shared.clone();
                                    let mut wait_stop = host_stop.clone();
                                    let stop_c = host_stop.clone();
                                    sessions.spawn(async move {
                                        let acquired = tokio::select! {
                                            () = wait_watch_true(&mut wait_stop) => None,
                                            res = timeout(SUPERSEDE_WAIT_TIMEOUT, permits_c.acquire_owned()) => {
                                                res.ok().and_then(std::result::Result::ok)
                                            }
                                        };
                                        if let Some(permit) = acquired {
                                            let _permit = permit;
                                            let (sess_stop_tx, sess_stop_rx) = watch::channel(false);
                                            let _reg_guard = ActiveSessionGuard::register(
                                                registry_c,
                                                entry_id,
                                                ch.peer_public_key(),
                                                sess_stop_tx.clone(),
                                            );
                                            run_bound_session(
                                                ch,
                                                entry_id,
                                                host_pk,
                                                shared_c,
                                                stop_c,
                                                sess_stop_tx,
                                                sess_stop_rx,
                                            )
                                            .await;
                                        } else if *wait_stop.borrow() {
                                            ch.close(CLOSE_BUSY, "host stopping");
                                        } else {
                                            tracing::warn!("timed out superseding stale session; rejecting with CLOSE_BUSY");
                                            reject_connection(ch, CLOSE_BUSY, "host busy").await;
                                        }
                                    });
                                } else {
                                    tracing::info!("rejecting viewer: Android host busy");
                                    tokio::spawn(async move {
                                        reject_connection(ch, CLOSE_BUSY, "host busy").await;
                                    });
                                }
                            }
                        }
                    }
                    Err(CoreError::Closed) => break,
                    Err(e) => {
                        tracing::warn!("AndroidHostEngine accept error: {e}");
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        }
    }

    listener.shutdown();
    // Signal all active sessions to close cleanly.
    {
        let g = active_registry.lock();
        for entry in g.iter() {
            let _ = entry.stop_tx.send(true);
        }
    }
    let drain = async {
        while sessions.join_next().await.is_some() {}
    };
    if timeout(Duration::from_secs(3), drain).await.is_err() {
        sessions.abort_all();
    }
    Ok(())
}

async fn run_bound_session(
    ch: Arc<dyn TransportChannel>,
    session_id: u64,
    host_pk: [u8; 32],
    shared: Arc<HostShared>,
    mut host_stop: watch::Receiver<bool>,
    sess_stop_tx: watch::Sender<bool>,
    sess_stop_rx: watch::Receiver<bool>,
) {
    let fwd = tokio::spawn(async move {
        tokio::select! {
            () = wait_watch_true(&mut host_stop) => {
                let _ = sess_stop_tx.send(true);
            }
            () = sess_stop_tx.closed() => {}
        }
    });
    if let Err(e) = run_session(ch, session_id, host_pk, shared, sess_stop_rx).await {
        tracing::debug!(session_id, "AndroidHostEngine session ended with error: {e}");
    }
    fwd.abort();
}

async fn reject_connection(ch: Arc<dyn TransportChannel>, code: u32, reason: &str) {
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

async fn reject_auth(
    writer: &mut ControlSender,
    ch: &dyn TransportChannel,
    reason: impl Into<String>,
) -> CoreError {
    let reason = reason.into();
    let _ = writer
        .send(&Msg::AuthResult(AuthResult { ok: false, reason: reason.clone(), permissions: None, session_id: 0 }).into())
        .await;
    let _ = writer.send(&Msg::Close(Close { code: CLOSE_AUTH_FAILED, reason: reason.clone() }).into()).await;
    let _ = writer.finish().await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    ch.close(CLOSE_AUTH_FAILED, &reason);
    CoreError::AccessDenied(reason.into())
}

// ───────────────────────── Session Lifecycle ─────────────────────────

struct AuthOutcome {
    peer_device_id: String,
    permissions: Permissions,
}

#[allow(clippy::too_many_arguments)]
async fn authenticate(
    sid: u64,
    hello: &Hello,
    ch: &Arc<dyn TransportChannel>,
    host_pk: [u8; 32],
    shared: &Arc<HostShared>,
    writer: &mut ControlSender,
    reader: &mut ControlReceiver,
    stop: &mut watch::Receiver<bool>,
) -> Result<AuthOutcome> {
    let ch_ref: &dyn TransportChannel = ch.as_ref();

    // 1. Bind Hello identity to mutual-TLS Ed25519 certificate whenever present.
    let tls_pk = ch_ref.peer_public_key();
    if let Some(pk) = tls_pk {
        if !hello.public_key.is_empty() && hello.public_key.as_slice() != pk {
            return Err(reject_auth(writer, ch_ref, "Hello public_key does not match TLS certificate").await);
        }
        let expected_id = device_id_from_public_key(&pk);
        if !hello.device_id.is_empty() && hello.device_id != expected_id {
            return Err(reject_auth(writer, ch_ref, "Hello device_id does not match TLS certificate").await);
        }
    }
    let peer_pk: Option<[u8; 32]> = tls_pk.or_else(|| hello.public_key.as_slice().try_into().ok());
    if tls_pk.is_none()
        && let Some(pk) = peer_pk
    {
        let expected_id = device_id_from_public_key(&pk);
        if !hello.device_id.is_empty() && hello.device_id != expected_id {
            return Err(reject_auth(writer, ch_ref, "Hello device_id does not match Hello public_key").await);
        }
    }
    let peer_device_id =
        peer_pk.as_ref().map(device_id_from_public_key).unwrap_or_else(|| hello.device_id.clone());

    // 2. Build AuthChallenge (safety invariant: NEVER offer AuthMethod::None).
    let trusted_entry = peer_pk.as_ref().and_then(|pk| shared.auth.find_trusted(pk));
    let perm_hash = shared.auth.effective_perm_hash();
    let active_otp = shared.auth.current_otp();

    let mut methods = Vec::new();
    if trusted_entry.is_some() {
        methods.push(AuthMethod::PublicKey as i32);
    }
    if perm_hash.is_some() {
        methods.push(AuthMethod::PermanentPassword as i32);
    }
    if active_otp.is_some() {
        methods.push(AuthMethod::OneTimePassword as i32);
    }
    if methods.is_empty() {
        return Err(reject_auth(writer, ch_ref, "no authentication method available for this peer").await);
    }

    let nonce = generate_nonce();
    let (salt, argon2_params) = if let Some(ph) = &perm_hash {
        (ph.salt.clone(), ph.params)
    } else if active_otp.is_some() {
        (generate_nonce()[..16].to_vec(), Argon2Params::default())
    } else {
        (Vec::new(), Argon2Params { m_cost_kib: 0, t_cost: 0, p_cost: 0 })
    };

    writer
        .send(
            &Msg::AuthChallenge(AuthChallenge {
                nonce: nonce.to_vec(),
                salt: salt.clone(),
                argon2_m_cost_kib: argon2_params.m_cost_kib,
                argon2_t_cost: argon2_params.t_cost,
                argon2_p_cost: argon2_params.p_cost,
                methods: methods.clone(),
                host_public_key: host_pk.to_vec(),
                host_device_id: device_id_from_public_key(&host_pk),
            })
            .into(),
        )
        .await?;

    // 3. Receive AuthResponse (cancellable on host stop or same-peer supersede).
    let resp: AuthResponse = tokio::select! {
        () = wait_watch_true(stop) => {
            let _ = writer.send(&Msg::Close(Close { code: CLOSE_NORMAL, reason: "host stopping".into() }).into()).await;
            let _ = writer.finish().await;
            ch_ref.close(CLOSE_NORMAL, "host stopping");
            return Err(CoreError::Closed);
        }
        res = timeout(AUTH_RESPONSE_TIMEOUT, reader.recv()) => match res {
            Ok(Ok(env)) => match env.msg {
                Some(Msg::AuthResponse(r)) => r,
                Some(Msg::Close(c)) => {
                    ch_ref.close(c.code, &c.reason);
                    return Err(CoreError::Closed);
                }
                other => {
                    return Err(reject_auth(writer, ch_ref, format!("expected AuthResponse, got {other:?}")).await);
                }
            },
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(reject_auth(writer, ch_ref, "timed out waiting for AuthResponse").await);
            }
        }
    };

    if !methods.contains(&resp.method) {
        return Err(reject_auth(writer, ch_ref, "authentication method not allowed").await);
    }

    // 4. Verify cryptographic proof.
    let mut authed_as_trusted: Option<AndroidTrustedPeer> = None;
    match AuthMethod::try_from(resp.method) {
        Ok(AuthMethod::PublicKey) => {
            let (Some(pk), Some(tp)) = (peer_pk, trusted_entry) else {
                return Err(reject_auth(writer, ch_ref, "peer is not in trusted whitelist").await);
            };
            if !verify_pubkey_challenge(&pk, &nonce, &host_pk, &resp.proof) {
                return Err(reject_auth(writer, ch_ref, "invalid public key proof").await);
            }
            authed_as_trusted = Some(tp);
        }
        Ok(AuthMethod::PermanentPassword) => {
            let mut ok = perm_hash
                .as_ref()
                .is_some_and(|ph| verify_password_proof(&ph.derived_key, &nonce, &resp.proof));
            // Also accept active OTP if Both mode offered both password methods.
            if !ok
                && let Some(otp) = active_otp.as_ref()
                && verify_otp_proof(otp, &salt, argon2_params, &nonce, &resp.proof).await?
            {
                if shared.auth.consume_and_rotate_otp(otp).is_some() {
                    ok = true;
                } else {
                    return Err(reject_auth(writer, ch_ref, "one-time password already used").await);
                }
            }
            if !ok {
                return Err(reject_auth(writer, ch_ref, "invalid password").await);
            }
        }
        Ok(AuthMethod::OneTimePassword) => {
            let Some(otp) = active_otp else {
                return Err(reject_auth(writer, ch_ref, "one-time password is not active").await);
            };
            if !verify_otp_proof(&otp, &salt, argon2_params, &nonce, &resp.proof).await? {
                return Err(reject_auth(writer, ch_ref, "invalid one-time password").await);
            }
            if shared.auth.consume_and_rotate_otp(&otp).is_none() {
                return Err(reject_auth(writer, ch_ref, "one-time password already used").await);
            }
        }
        _ => {
            return Err(reject_auth(writer, ch_ref, "authentication method not allowed").await);
        }
    }

    // 5. Grant permissions (trusted peer bypasses interactive approval).
    let perms = if let Some(tp) = authed_as_trusted {
        if !tp.permissions.view {
            return Err(reject_auth(writer, ch_ref, "trusted peer does not have view permission").await);
        }
        tp.permissions
    } else if shared.auth.require_accept() {
        let (req_id, rx) = shared.approvals.register();
        let peer_fp = peer_pk.as_ref().map(fingerprint).unwrap_or_default();
        let peer_pk_hex = peer_pk.as_ref().map(encode_hex_pubkey_32).unwrap_or_default();
        let peer_name = if hello.device_name.is_empty() { peer_device_id.clone() } else { hello.device_name.clone() };
        shared
            .callbacks
            .on_approval_requested(req_id, &peer_device_id, &peer_name, &peer_fp, &peer_pk_hex);

        let decision = tokio::select! {
            () = wait_watch_true(stop) => {
                shared.approvals.cancel(req_id);
                let _ = writer.send(&Msg::Close(Close { code: CLOSE_NORMAL, reason: "host stopping".into() }).into()).await;
                let _ = writer.finish().await;
                ch_ref.close(CLOSE_NORMAL, "host stopping");
                return Err(CoreError::Closed);
            }
            res = timeout(APPROVAL_TIMEOUT, rx) => match res {
                Ok(Ok(dec)) => dec,
                Ok(Err(_)) => {
                    shared.approvals.cancel(req_id);
                    return Err(reject_auth(writer, ch_ref, "host approval prompt closed").await);
                }
                Err(_) => {
                    shared.approvals.cancel(req_id);
                    return Err(reject_auth(writer, ch_ref, "host approval timed out").await);
                }
            }
        };

        if !decision.accepted {
            return Err(reject_auth(writer, ch_ref, "connection rejected by host user").await);
        }
        let granted = Permissions {
            view: true,
            input: decision.allow_input,
            clipboard: decision.allow_clipboard,
            file_transfer: false,
            audio: decision.allow_audio,
            tunnel: false,
            terminal: false,
            restart: false,
            privacy_mode: false,
            allow_chat: true,
            allow_power: false,
        };
        if decision.remember_peer
            && let Some(pk) = peer_pk
        {
            shared.auth.add_trusted_peer(AndroidTrustedPeer {
                public_key: pk,
                name: peer_name,
                permissions: granted.clone(),
            });
        }
        granted
    } else {
        Permissions {
            view: true,
            input: true,
            clipboard: true,
            file_transfer: false,
            audio: true,
            tunnel: false,
            terminal: false,
            restart: false,
            privacy_mode: false,
            allow_chat: true,
            allow_power: false,
        }
    };

    writer
        .send(
            &Msg::AuthResult(AuthResult {
                ok: true,
                reason: String::new(),
                permissions: Some(perms.clone()),
                session_id: sid,
            })
            .into(),
        )
        .await?;

    Ok(AuthOutcome { peer_device_id, permissions: perms })
}

async fn verify_otp_proof(
    otp: &str,
    salt: &[u8],
    params: Argon2Params,
    nonce: &[u8; 32],
    proof: &[u8],
) -> Result<bool> {
    let otp = otp.to_string();
    let salt = salt.to_vec();
    let key = tokio::task::spawn_blocking(move || derive_password_key(&otp, &salt, params))
        .await
        .map_err(|e| CoreError::platform(format!("argon2 task failed: {e}")))?
        .map_err(CoreError::from)?;
    Ok(verify_password_proof(&key, nonce, proof))
}

async fn run_session(
    ch: Arc<dyn TransportChannel>,
    sid: u64,
    host_pk: [u8; 32],
    shared: Arc<HostShared>,
    mut stop: watch::Receiver<bool>,
) -> Result<()> {
    // 1. Accept Control stream + Hello (cancellable on stop / supersede).
    let (send, recv) = loop {
        let accepted = tokio::select! {
            () = wait_watch_true(&mut stop) => {
                ch.close(CLOSE_NORMAL, "host stopping");
                return Err(CoreError::Closed);
            }
            res = timeout(HANDSHAKE_TIMEOUT, ch.accept_bi()) => res.map_err(|_| CoreError::Timeout)??,
        };
        let (kind, s, r) = accepted;
        if kind == StreamKind::Control {
            break (s, r);
        }
    };
    let (mut writer, mut reader) = framed(send, recv);
    let hello = tokio::select! {
        () = wait_watch_true(&mut stop) => {
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

    // 2. Authenticate & authorize viewer.
    let auth = authenticate(sid, &hello, &ch, host_pk, &shared, &mut writer, &mut reader, &mut stop).await?;
    let peer_name = if hello.device_name.is_empty() { auth.peer_device_id.clone() } else { hello.device_name.clone() };
    let viewer_accepts_opus =
        hello.audio_codecs.is_empty() || hello.audio_codecs.contains(&(AudioCodec::Opus as i32));
    let allow_audio = auth.permissions.audio && viewer_accepts_opus;

    // 3. Negotiate initial bitrate / FPS and send SessionConfig.
    let display = shared.display.read().clone();
    let requested_bitrate_kbps = if hello.max_bitrate_kbps > 0 {
        hello.max_bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
    } else {
        DEFAULT_BITRATE_KBPS
    };
    let cap_fps = hello.video_caps.iter().map(|c| c.max_fps).filter(|&f| f > 0).max().unwrap_or(display.fps);
    let requested_fps = display.fps.min(cap_fps).clamp(15, 60);

    let (ctrl_out, mut ctrl_out_rx) = mpsc::unbounded_channel::<Envelope>();
    let bridge = Arc::new(ActiveSessionBridge::new(
        sid,
        hello.mode,
        allow_audio,
        requested_bitrate_kbps,
        ch.clone(),
        ctrl_out.clone(),
    ));
    let initial_cfg = bridge.build_session_config(&display);
    writer.send(&Msg::SessionConfig(initial_cfg).into()).await?;

    // Register bridge BEFORE notifying Kotlin so frames pushed immediately in `onHostSessionStarted` are sent.
    *shared.active_session.write() = Some(bridge.clone());
    shared
        .callbacks
        .on_session_started(&auth.peer_device_id, &peer_name, requested_bitrate_kbps, requested_fps);

    // 4. Control writer + extra streams (Input, Clipboard).
    let writer_task = tokio::spawn(async move {
        while let Some(env) = ctrl_out_rx.recv().await {
            if writer.send(&env).await.is_err() {
                break;
            }
        }
        let _ = writer.finish().await;
    });

    let (streams_stop_tx, streams_stop_rx) = watch::channel(false);
    let streams_task = tokio::spawn(accept_extra_streams(
        ch.clone(),
        auth.permissions.clone(),
        bridge.clone(),
        shared.callbacks.clone(),
        streams_stop_rx,
    ));

    // 5. Main control loop (ABR, keyframe requests, Ping/Pong, NACK, fallback control input).
    let mut viewer_cap = requested_bitrate_kbps;
    let mut abr = AbrController::new(viewer_cap, MIN_BITRATE_KBPS.min(viewer_cap), viewer_cap);
    let mut last_bitrate_notified = viewer_cap;
    let mut last_keyframe_req = Instant::now() - Duration::from_secs(1);

    let close_reason = loop {
        if *stop.borrow() {
            let _ = ctrl_out.send(Msg::Close(Close { code: CLOSE_NORMAL, reason: "host shutting down".into() }).into());
            break "host shutting down".to_string();
        }
        tokio::select! {
            () = wait_watch_true(&mut stop) => {
                let _ = ctrl_out.send(Msg::Close(Close { code: CLOSE_NORMAL, reason: "host shutting down".into() }).into());
                break "host shutting down".to_string();
            }
            msg = reader.recv() => {
                let env = match msg {
                    Ok(m) => m,
                    Err(CoreError::Closed) => break "viewer disconnected".to_string(),
                    Err(e) => break format!("control stream error: {e}"),
                };
                match env.msg {
                    Some(Msg::Ping(p)) => {
                        let now = Timestamp::now().as_micros();
                        let _ = ctrl_out.send(
                            Msg::Pong(Pong {
                                seq: p.seq,
                                ping_sent_us: p.sent_us,
                                peer_recv_us: now,
                                peer_send_us: Timestamp::now().as_micros(),
                            })
                            .into(),
                        );
                    }
                    Some(Msg::KeyframeRequest(_)) => {
                        if last_keyframe_req.elapsed() >= Duration::from_millis(100) {
                            last_keyframe_req = Instant::now();
                            shared.callbacks.on_keyframe_requested();
                        }
                    }
                    Some(Msg::Nack(n)) => {
                        bridge.video_sender.lock().on_nack(&n);
                    }
                    Some(Msg::ReceiverReport(rr)) => {
                        let total = rr.packets_received + rr.packets_lost;
                        let loss = if total == 0 { 0.0 } else { rr.packets_lost as f64 / total as f64 };
                        if allow_audio {
                            bridge.audio_state.lock().set_redundant(loss > 0.02);
                        }
                        if let Some(d) = abr.on_report(&rr, Timestamp::now()) {
                            let target_kbps = d.bitrate_kbps.min(viewer_cap).max(MIN_BITRATE_KBPS);
                            bridge.video_sender.lock().set_parity_percent(d.parity_percent);
                            bridge.bitrate_kbps.store(target_kbps, Ordering::Relaxed);
                            if target_kbps != last_bitrate_notified {
                                last_bitrate_notified = target_kbps;
                                shared.callbacks.on_bitrate_changed(target_kbps);
                            }
                            let cur_fps = shared.display.read().fps;
                            let _ = ctrl_out.send(
                                Msg::BitrateUpdate(BitrateUpdate {
                                    stream_id: rr.stream_id,
                                    bitrate_kbps: target_kbps,
                                    fps: cur_fps,
                                    fec_parity_percent: d.parity_percent as u32,
                                })
                                .into(),
                            );
                        }
                    }
                    Some(Msg::BitrateUpdate(b)) if b.bitrate_kbps > 0 => {
                        viewer_cap = b.bitrate_kbps.clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS);
                        abr = AbrController::new(viewer_cap, MIN_BITRATE_KBPS.min(viewer_cap), viewer_cap);
                        bridge.bitrate_kbps.store(viewer_cap, Ordering::Relaxed);
                        if viewer_cap != last_bitrate_notified {
                            last_bitrate_notified = viewer_cap;
                            shared.callbacks.on_bitrate_changed(viewer_cap);
                        }
                    }
                    Some(Msg::Input(ev)) => {
                        if auth.permissions.input {
                            dispatch_input_event(&ev, &shared.callbacks);
                        }
                    }
                    Some(Msg::Close(c)) => {
                        break if c.reason.is_empty() {
                            format!("viewer closed ({})", c.code)
                        } else {
                            c.reason
                        };
                    }
                    _ => {}
                }
            }
        }
    };

    // 6. Clean teardown.
    {
        let mut active = shared.active_session.write();
        if active.as_ref().is_some_and(|b| b.session_id == sid) {
            *active = None;
        }
    }
    let _ = streams_stop_tx.send(true);
    drop(ctrl_out);
    let _ = timeout(Duration::from_millis(250), writer_task).await;
    streams_task.abort();
    ch.close(CLOSE_NORMAL, "session ended");
    shared.callbacks.on_session_ended(&auth.peer_device_id, &close_reason);
    Ok(())
}

async fn accept_extra_streams(
    ch: Arc<dyn TransportChannel>,
    perms: Permissions,
    bridge: Arc<ActiveSessionBridge>,
    callbacks: Arc<dyn HostCallbacks>,
    stop: watch::Receiver<bool>,
) {
    let mut readers = JoinSet::new();
    loop {
        let incoming = tokio::select! {
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
                if !perms.input {
                    tracing::debug!("AndroidHost: ignoring Input stream (input permission denied)");
                    continue;
                }
                let cb = callbacks.clone();
                let mut reader = ControlReceiver::new(recv);
                readers.spawn(async move {
                    while let Ok(env) = reader.recv().await {
                        if let Some(Msg::Input(ev)) = env.msg {
                            dispatch_input_event(&ev, &cb);
                        }
                    }
                });
            }
            StreamKind::Clipboard => {
                let Some(send) = send_opt else { continue };
                if !perms.clipboard {
                    readers.spawn(deny_stream_with_close(send, "clipboard permission denied"));
                    continue;
                }
                let (clip_tx, clip_rx) = mpsc::unbounded_channel::<String>();
                *bridge.clip_out.lock() = Some(clip_tx);
                let cb = callbacks.clone();
                let stop_rx = stop.clone();
                readers.spawn(async move {
                    if let Err(e) =
                        run_android_clipboard_stream(send, recv, clip_rx, move |text| cb.on_clipboard_text(text), stop_rx)
                            .await
                    {
                        tracing::debug!("AndroidHost clipboard stream ended: {e}");
                    }
                });
            }
            StreamKind::FileTransfer | StreamKind::Tunnel | StreamKind::Terminal => {
                if let Some(send) = send_opt {
                    readers.spawn(deny_stream_with_close(send, "not supported on Android host"));
                }
            }
            _ => {}
        }
    }
    readers.abort_all();
}

async fn deny_stream_with_close(send: veloxdesk_core_traits::transport::SendStream, reason: &str) {
    let mut w = ControlSender::new(send);
    let _ = w.send(&Msg::Close(Close { code: CLOSE_AUTH_FAILED, reason: reason.into() }).into()).await;
    let _ = w.finish().await;
}

/// Map wire `MouseButton` enum (0=Left, 1=Right, 2=Middle, 3=Back, 4=Forward)
/// to the frozen `VeloxHostCallback.onHostInputEvent` button code (`1=left, 2=right, 3=middle`).
pub fn map_mouse_button_code(wire_button: i32) -> i32 {
    match MouseButton::try_from(wire_button) {
        Ok(MouseButton::Left) => 1,
        Ok(MouseButton::Right) => 2,
        Ok(MouseButton::Middle) => 3,
        Ok(MouseButton::Back) => 4,
        Ok(MouseButton::Forward) => 5,
        Err(_) => wire_button.saturating_add(1),
    }
}

fn dispatch_input_event(ev: &InputEvent, callbacks: &Arc<dyn HostCallbacks>) {
    let Some(kind) = &ev.event else { return };
    match kind {
        InputEventKind::MouseMove(m) => {
            callbacks.on_input_event(input_kind::MOUSE_MOVE, m.x, m.y, 0, false, 0, 0, 0, "");
        }
        InputEventKind::MouseButton(b) => {
            let code = map_mouse_button_code(b.button);
            callbacks.on_input_event(input_kind::MOUSE_BUTTON, 0, 0, code, b.down, 0, 0, 0, "");
        }
        InputEventKind::MouseWheel(w) => {
            callbacks.on_input_event(input_kind::MOUSE_WHEEL, 0, 0, 0, false, 0, w.dx, w.dy, "");
        }
        InputEventKind::Key(k) => {
            callbacks.on_input_event(
                input_kind::KEY,
                0,
                0,
                k.hid_usage as i32,
                k.down,
                k.modifiers as i32,
                0,
                0,
                "",
            );
        }
        InputEventKind::Text(t) => {
            callbacks.on_input_event(input_kind::TEXT, 0, 0, 0, false, 0, 0, 0, &t.text);
        }
        InputEventKind::MouseRelative(_) | InputEventKind::Gamepad(_) | InputEventKind::Touch(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoopHostCallbacks;
    impl HostCallbacks for NoopHostCallbacks {
        fn on_listening(&self, _device_id: &str, _otp: &str, _port: u16) {}
        fn on_otp_rotated(&self, _otp: &str) {}
        fn on_approval_requested(&self, _: u64, _: &str, _: &str, _: &str, _: &str) {}
        fn on_trusted_peer_added(&self, _: &str) {}
        fn on_session_started(&self, _: &str, _: &str, _: u32, _: u32) {}
        fn on_session_ended(&self, _: &str, _: &str) {}
        fn on_bitrate_changed(&self, _: u32) {}
        fn on_keyframe_requested(&self) {}
        fn on_input_event(&self, _: i32, _: i32, _: i32, _: i32, _: bool, _: i32, _: i32, _: i32, _: &str) {}
        fn on_clipboard_text(&self, _: &str) {}
        fn on_error(&self, _: &str) {}
        fn on_stopped(&self) {}
    }

    #[test]
    fn phc_hash_and_hex_pubkey_helpers() {
        let phc = hash_password_phc("MyP@ssw0rd!").expect("hash");
        assert!(phc.starts_with("$argon2id$"));
        assert!(is_valid_phc_hash(&phc));
        assert!(!is_valid_phc_hash(""));
        assert!(!is_valid_phc_hash("not-a-phc"));
        assert!(hash_password_phc("").is_err());

        let pk = [0x5Au8; 32];
        let hex = encode_hex_pubkey_32(&pk);
        assert_eq!(hex.len(), 64);
        assert_eq!(parse_hex_pubkey_32(&hex), Some(pk));
        assert_eq!(parse_hex_pubkey_32(&hex.to_uppercase()), Some(pk));
        assert_eq!(parse_hex_pubkey_32("bad"), None);
    }

    #[test]
    fn auth_state_enforces_onetime_and_non_empty_invariants() {
        let phc = hash_password_phc("PermSecret#42").expect("hash");
        let cb: Arc<dyn HostCallbacks> = Arc::new(NoopHostCallbacks);

        // OneTime mode with a saved PHC hash must NOT expose effective_perm_hash and MUST keep OTP active.
        let st = AndroidHostAuthState::new(HostPasswordMode::OneTime, &phc, true, "", cb.clone());
        assert!(st.effective_perm_hash().is_none());
        assert!(st.current_otp().is_some());

        // Switching to Permanent with valid PHC disables OTP and activates permanent hash.
        st.update_auth(HostPasswordMode::Permanent, &phc, false);
        assert!(st.effective_perm_hash().is_some());
        assert!(st.current_otp().is_none());
        // Manual rotate_otp in Permanent-only mode must NOT silently re-enable OTP.
        assert!(st.rotate_otp().is_none());
        assert!(st.current_otp().is_none());

        // Switching to Permanent with corrupt/empty PHC MUST refuse to disable OTP (keeps/generates OTP).
        st.update_auth(HostPasswordMode::Permanent, "corrupt-phc", false);
        assert!(st.effective_perm_hash().is_none());
        assert!(st.current_otp().is_some());

        // Switching to Both enables both permanent hash and OTP.
        st.update_auth(HostPasswordMode::Both, &phc, true);
        assert!(st.effective_perm_hash().is_some());
        assert!(st.current_otp().is_some());
    }

    #[test]
    fn mouse_button_mapping_matches_frozen_jni_contract() {
        assert_eq!(map_mouse_button_code(MouseButton::Left as i32), 1);
        assert_eq!(map_mouse_button_code(MouseButton::Right as i32), 2);
        assert_eq!(map_mouse_button_code(MouseButton::Middle as i32), 3);
    }
}
