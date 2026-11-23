//! Control-plane protobuf messages (ARCHITECTURE §4.3), carried on reliable QUIC streams
//! as `u32 LE length | Envelope` (see [`crate::framing`]).
//!
//! These are hand-written `prost` derives (no `protoc`). **Never renumber or reuse tags.**
//! Keyboard keys are transmitted as **USB HID usage codes (page 0x07)** — physical,
//! layout-independent scancodes. Coordinates are host-display pixels unless noted.

use bytes::Bytes;

// ───────────────────────────── enums ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum SessionMode {
    Desktop = 0,
    Gaming = 1,
}

/// Values match [`crate::media::WireCodec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum VideoCodec {
    Unspecified = 0,
    H264 = 1,
    H265 = 2,
    Av1 = 3,
    Vp9 = 4,
}

/// Values match [`crate::media::WireCodec`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum AudioCodec {
    Unspecified = 0,
    Opus = 16,
    Pcm = 17,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum ChromaFormat {
    Yuv420 = 0,
    Yuv444 = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum AuthMethod {
    None = 0,
    OneTimePassword = 1,
    PermanentPassword = 2,
    /// Peer's Ed25519 key is in the host whitelist; proof = signature over the nonce.
    PublicKey = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum MouseButton {
    Left = 0,
    Right = 1,
    Middle = 2,
    Back = 3,
    Forward = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum ClipFormat {
    Text = 0,
    Html = 1,
    Rtf = 2,
    /// PNG-encoded image.
    ImagePng = 3,
    /// Newline-separated file names; contents go through the file-transfer stream.
    FileList = 4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum FileOp {
    ListDir = 0,
    Download = 1,
    Upload = 2,
    Delete = 3,
    Mkdir = 4,
    Rename = 5,
    Stat = 6,
}

/// Remote host power management actions (v0.5.0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum SystemPowerAction {
    Reboot = 0,
    RebootSafeMode = 1,
    LockScreen = 2,
    Sleep = 3,
}

// ───────────────────────────── session ─────────────────────────────

/// Viewer decode capability for one codec.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct VideoCodecCap {
    #[prost(enumeration = "VideoCodec", tag = "1")]
    pub codec: i32,
    #[prost(bool, tag = "2")]
    pub hw_decode: bool,
    #[prost(uint32, tag = "3")]
    pub max_width: u32,
    #[prost(uint32, tag = "4")]
    pub max_height: u32,
    #[prost(uint32, tag = "5")]
    pub max_fps: u32,
    #[prost(bool, tag = "6")]
    pub yuv444: bool,
    #[prost(bool, tag = "7")]
    pub ten_bit: bool,
}

/// First message of a session (viewer → host).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Hello {
    #[prost(uint32, tag = "1")]
    pub protocol_version: u32,
    /// Human device ID of the viewer (9 digits).
    #[prost(string, tag = "2")]
    pub device_id: String,
    /// Ed25519 public key of the viewer (32 bytes).
    #[prost(bytes = "vec", tag = "3")]
    pub public_key: Vec<u8>,
    #[prost(string, tag = "4")]
    pub device_name: String,
    #[prost(string, tag = "5")]
    pub os: String,
    #[prost(string, tag = "6")]
    pub app_version: String,
    #[prost(enumeration = "SessionMode", tag = "7")]
    pub mode: i32,
    #[prost(message, repeated, tag = "8")]
    pub video_caps: Vec<VideoCodecCap>,
    #[prost(enumeration = "AudioCodec", repeated, tag = "9")]
    pub audio_codecs: Vec<i32>,
    /// Viewer's local display, used to pick resolution/refresh.
    #[prost(uint32, tag = "10")]
    pub viewer_width: u32,
    #[prost(uint32, tag = "11")]
    pub viewer_height: u32,
    #[prost(uint32, tag = "12")]
    pub viewer_refresh_hz: u32,
    /// Requested max bitrate (0 = host decides).
    #[prost(uint32, tag = "13")]
    pub max_bitrate_kbps: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AuthChallenge {
    #[prost(bytes = "vec", tag = "1")]
    pub nonce: Vec<u8>,
    /// Argon2id salt for the permanent password.
    #[prost(bytes = "vec", tag = "2")]
    pub salt: Vec<u8>,
    #[prost(uint32, tag = "3")]
    pub argon2_m_cost_kib: u32,
    #[prost(uint32, tag = "4")]
    pub argon2_t_cost: u32,
    #[prost(uint32, tag = "5")]
    pub argon2_p_cost: u32,
    #[prost(enumeration = "AuthMethod", repeated, tag = "6")]
    pub methods: Vec<i32>,
    /// Host Ed25519 public key (also bound by the TLS certificate).
    #[prost(bytes = "vec", tag = "7")]
    pub host_public_key: Vec<u8>,
    #[prost(string, tag = "8")]
    pub host_device_id: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AuthResponse {
    #[prost(enumeration = "AuthMethod", tag = "1")]
    pub method: i32,
    /// Password methods: `HMAC-SHA256(key = argon2id(password, salt), msg = nonce)`.
    /// PublicKey: Ed25519 signature over `nonce || host_public_key`.
    #[prost(bytes = "vec", tag = "2")]
    pub proof: Vec<u8>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Permissions {
    #[prost(bool, tag = "1")]
    pub view: bool,
    #[prost(bool, tag = "2")]
    pub input: bool,
    #[prost(bool, tag = "3")]
    pub clipboard: bool,
    #[prost(bool, tag = "4")]
    pub file_transfer: bool,
    #[prost(bool, tag = "5")]
    pub audio: bool,
    #[prost(bool, tag = "6")]
    pub tunnel: bool,
    #[prost(bool, tag = "7")]
    pub terminal: bool,
    #[prost(bool, tag = "8")]
    pub restart: bool,
    #[prost(bool, tag = "9")]
    pub privacy_mode: bool,
    /// Bidirectional text chat in session (v0.5.0; interactive default: true).
    #[prost(bool, tag = "10")]
    pub allow_chat: bool,
    /// Remote host power actions: reboot/lock/sleep (v0.5.0; interactive default: false).
    #[prost(bool, tag = "11")]
    pub allow_power: bool,
}

impl Permissions {
    pub fn all() -> Self {
        Self {
            view: true,
            input: true,
            clipboard: true,
            file_transfer: true,
            audio: true,
            tunnel: true,
            terminal: true,
            restart: true,
            privacy_mode: true,
            allow_chat: true,
            allow_power: true,
        }
    }

    /// Standard permissions for interactive sessions (chat allowed, power actions disabled by default).
    pub fn interactive_default() -> Self {
        Self {
            view: true,
            input: true,
            clipboard: true,
            file_transfer: true,
            audio: true,
            tunnel: true,
            terminal: true,
            restart: false,
            privacy_mode: true,
            allow_chat: true,
            allow_power: false,
        }
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AuthResult {
    #[prost(bool, tag = "1")]
    pub ok: bool,
    #[prost(string, tag = "2")]
    pub reason: String,
    #[prost(message, optional, tag = "3")]
    pub permissions: Option<Permissions>,
    #[prost(uint64, tag = "4")]
    pub session_id: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DisplayInfo {
    #[prost(uint32, tag = "1")]
    pub index: u32,
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(sint32, tag = "3")]
    pub x: i32,
    #[prost(sint32, tag = "4")]
    pub y: i32,
    #[prost(uint32, tag = "5")]
    pub width: u32,
    #[prost(uint32, tag = "6")]
    pub height: u32,
    #[prost(uint32, tag = "7")]
    pub refresh_hz: u32,
    #[prost(float, tag = "8")]
    pub scale_factor: f32,
    #[prost(bool, tag = "9")]
    pub primary: bool,
    #[prost(bool, tag = "10")]
    pub hdr: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct VideoConfig {
    #[prost(enumeration = "VideoCodec", tag = "1")]
    pub codec: i32,
    #[prost(uint32, tag = "2")]
    pub width: u32,
    #[prost(uint32, tag = "3")]
    pub height: u32,
    #[prost(uint32, tag = "4")]
    pub fps: u32,
    #[prost(uint32, tag = "5")]
    pub bitrate_kbps: u32,
    #[prost(enumeration = "ChromaFormat", tag = "6")]
    pub chroma: i32,
    #[prost(uint32, tag = "7")]
    pub bit_depth: u32,
    /// e.g. "NVENC", "AMF", "QSV", "VAAPI", "VideoToolbox", "openh264".
    #[prost(string, tag = "8")]
    pub encoder_name: String,
    #[prost(bool, tag = "9")]
    pub hw_encoder: bool,
    #[prost(bool, tag = "10")]
    pub zero_copy: bool,
    #[prost(uint32, tag = "11")]
    pub fec_parity_percent: u32,
    #[prost(uint32, tag = "12")]
    pub stream_id: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct AudioConfig {
    #[prost(enumeration = "AudioCodec", tag = "1")]
    pub codec: i32,
    #[prost(uint32, tag = "2")]
    pub sample_rate: u32,
    #[prost(uint32, tag = "3")]
    pub channels: u32,
    /// Opus frame duration: 2500, 5000, 10000, 20000 µs.
    #[prost(uint32, tag = "4")]
    pub frame_duration_us: u32,
    #[prost(uint32, tag = "5")]
    pub bitrate_kbps: u32,
}

/// Host → viewer after successful auth, and again whenever it changes.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SessionConfig {
    #[prost(message, repeated, tag = "1")]
    pub displays: Vec<DisplayInfo>,
    #[prost(uint32, tag = "2")]
    pub active_display: u32,
    #[prost(message, optional, tag = "3")]
    pub video: Option<VideoConfig>,
    #[prost(message, optional, tag = "4")]
    pub audio: Option<AudioConfig>,
    #[prost(string, tag = "5")]
    pub host_name: String,
    #[prost(string, tag = "6")]
    pub host_os: String,
    #[prost(enumeration = "SessionMode", tag = "7")]
    pub mode: i32,
}

/// NTP-style clock sync / RTT probe. Times are sender-monotonic µs.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Ping {
    #[prost(uint32, tag = "1")]
    pub seq: u32,
    #[prost(uint64, tag = "2")]
    pub sent_us: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Pong {
    #[prost(uint32, tag = "1")]
    pub seq: u32,
    #[prost(uint64, tag = "2")]
    pub ping_sent_us: u64,
    #[prost(uint64, tag = "3")]
    pub peer_recv_us: u64,
    #[prost(uint64, tag = "4")]
    pub peer_send_us: u64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Close {
    #[prost(uint32, tag = "1")]
    pub code: u32,
    #[prost(string, tag = "2")]
    pub reason: String,
}

// ───────────────────────────── video / ABR ─────────────────────────────

/// Picture Loss Indication: viewer asks for an IDR / intra-refresh.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct KeyframeRequest {
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    #[prost(uint32, tag = "2")]
    pub last_good_frame_id: u32,
}

/// Request retransmission of specific shards (used only when RTT allows).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Nack {
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    #[prost(uint32, tag = "2")]
    pub frame_id: u32,
    #[prost(uint32, tag = "3")]
    pub block_index: u32,
    #[prost(uint32, repeated, tag = "4")]
    pub shard_indices: Vec<u32>,
}

/// Periodic viewer feedback (every 50–100 ms) driving ABR and FEC ratio.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ReceiverReport {
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    #[prost(uint32, tag = "2")]
    pub highest_frame_id: u32,
    #[prost(uint32, tag = "3")]
    pub packets_received: u32,
    #[prost(uint32, tag = "4")]
    pub packets_lost: u32,
    #[prost(uint32, tag = "5")]
    pub packets_recovered_fec: u32,
    #[prost(uint32, tag = "6")]
    pub frames_unrecoverable: u32,
    #[prost(uint32, tag = "7")]
    pub recv_kbps: u32,
    /// Trendline slope of one-way delay variation (µs per frame); > 0 = queue building.
    #[prost(float, tag = "8")]
    pub delay_gradient: f32,
    #[prost(uint32, tag = "9")]
    pub jitter_us: u32,
    #[prost(uint32, tag = "10")]
    pub decode_us: u32,
    #[prost(uint32, tag = "11")]
    pub render_us: u32,
    #[prost(uint32, tag = "12")]
    pub rtt_us: u32,
    /// Interval covered by this report.
    #[prost(uint32, tag = "13")]
    pub interval_ms: u32,
}

/// Host announces new encoder targets (informational for the HUD).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct BitrateUpdate {
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    #[prost(uint32, tag = "2")]
    pub bitrate_kbps: u32,
    #[prost(uint32, tag = "3")]
    pub fps: u32,
    #[prost(uint32, tag = "4")]
    pub fec_parity_percent: u32,
}

/// Host-side timings of one frame (sent on the control stream, sampled or every frame).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FrameTimingReport {
    #[prost(uint32, tag = "1")]
    pub stream_id: u32,
    #[prost(uint32, tag = "2")]
    pub frame_id: u32,
    /// Full 64-bit host capture timestamp (µs, host monotonic clock).
    #[prost(uint64, tag = "3")]
    pub capture_ts_us: u64,
    #[prost(uint32, tag = "4")]
    pub capture_us: u32,
    #[prost(uint32, tag = "5")]
    pub convert_us: u32,
    #[prost(uint32, tag = "6")]
    pub encode_us: u32,
    #[prost(uint32, tag = "7")]
    pub packetize_us: u32,
    #[prost(uint32, tag = "8")]
    pub size_bytes: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SwitchDisplay {
    #[prost(uint32, tag = "1")]
    pub index: u32,
}

// ───────────────────────────── input ─────────────────────────────

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MouseMove {
    /// Absolute position in host display pixels.
    #[prost(sint32, tag = "1")]
    pub x: i32,
    #[prost(sint32, tag = "2")]
    pub y: i32,
    #[prost(uint32, tag = "3")]
    pub display: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MouseRelative {
    /// Raw device deltas (coalesced on the viewer between sends).
    #[prost(sint32, tag = "1")]
    pub dx: i32,
    #[prost(sint32, tag = "2")]
    pub dy: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MouseButtonEvent {
    #[prost(enumeration = "MouseButton", tag = "1")]
    pub button: i32,
    #[prost(bool, tag = "2")]
    pub down: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct MouseWheel {
    /// In 1/120 notch units (Windows WHEEL_DELTA convention); positive = up/right.
    #[prost(sint32, tag = "1")]
    pub dx: i32,
    #[prost(sint32, tag = "2")]
    pub dy: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct KeyEvent {
    /// USB HID usage code (keyboard page 0x07).
    #[prost(uint32, tag = "1")]
    pub hid_usage: u32,
    #[prost(bool, tag = "2")]
    pub down: bool,
    /// Bitmask: 1 Shift, 2 Ctrl, 4 Alt, 8 Meta, 16 CapsLock, 32 NumLock.
    #[prost(uint32, tag = "3")]
    pub modifiers: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TextInput {
    #[prost(string, tag = "1")]
    pub text: String,
}

/// Full gamepad state, XInput layout. Also sent as `MediaKind::GamepadState` datagram.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GamepadState {
    #[prost(uint32, tag = "1")]
    pub pad_id: u32,
    #[prost(bool, tag = "2")]
    pub connected: bool,
    /// XInput `wButtons` bitmask.
    #[prost(uint32, tag = "3")]
    pub buttons: u32,
    /// 0..=255
    #[prost(uint32, tag = "4")]
    pub left_trigger: u32,
    #[prost(uint32, tag = "5")]
    pub right_trigger: u32,
    /// -32768..=32767
    #[prost(sint32, tag = "6")]
    pub left_x: i32,
    #[prost(sint32, tag = "7")]
    pub left_y: i32,
    #[prost(sint32, tag = "8")]
    pub right_x: i32,
    #[prost(sint32, tag = "9")]
    pub right_y: i32,
    /// Monotonic per-pad sequence so stale datagrams are dropped.
    #[prost(uint32, tag = "10")]
    pub seq: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TouchEvent {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    /// 0 down, 1 move, 2 up, 3 cancel
    #[prost(uint32, tag = "2")]
    pub phase: u32,
    #[prost(sint32, tag = "3")]
    pub x: i32,
    #[prost(sint32, tag = "4")]
    pub y: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InputEvent {
    /// Viewer monotonic µs when the event was generated (input latency metric).
    #[prost(uint64, tag = "1")]
    pub ts_us: u64,
    #[prost(oneof = "input_event::Event", tags = "2, 3, 4, 5, 6, 7, 8, 9")]
    pub event: Option<input_event::Event>,
}

pub mod input_event {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Event {
        #[prost(message, tag = "2")]
        MouseMove(super::MouseMove),
        #[prost(message, tag = "3")]
        MouseRelative(super::MouseRelative),
        #[prost(message, tag = "4")]
        MouseButton(super::MouseButtonEvent),
        #[prost(message, tag = "5")]
        MouseWheel(super::MouseWheel),
        #[prost(message, tag = "6")]
        Key(super::KeyEvent),
        #[prost(message, tag = "7")]
        Text(super::TextInput),
        #[prost(message, tag = "8")]
        Gamepad(super::GamepadState),
        #[prost(message, tag = "9")]
        Touch(super::TouchEvent),
    }
}

/// Host → viewer force feedback.
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Rumble {
    #[prost(uint32, tag = "1")]
    pub pad_id: u32,
    /// 0..=65535
    #[prost(uint32, tag = "2")]
    pub low_freq: u32,
    #[prost(uint32, tag = "3")]
    pub high_freq: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CursorShape {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(uint32, tag = "2")]
    pub width: u32,
    #[prost(uint32, tag = "3")]
    pub height: u32,
    #[prost(uint32, tag = "4")]
    pub hotspot_x: u32,
    #[prost(uint32, tag = "5")]
    pub hotspot_y: u32,
    /// Straight-alpha RGBA8, `width*height*4` bytes.
    #[prost(bytes = "bytes", tag = "6")]
    pub rgba: Bytes,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct CursorPosition {
    #[prost(sint32, tag = "1")]
    pub x: i32,
    #[prost(sint32, tag = "2")]
    pub y: i32,
    #[prost(bool, tag = "3")]
    pub visible: bool,
    #[prost(uint64, tag = "4")]
    pub shape_id: u64,
}

// ───────────────────────────── features ─────────────────────────────

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ClipboardData {
    #[prost(enumeration = "ClipFormat", tag = "1")]
    pub format: i32,
    #[prost(bytes = "bytes", tag = "2")]
    pub data: Bytes,
    /// zstd-compressed `data`.
    #[prost(bool, tag = "3")]
    pub compressed: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileRequest {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(enumeration = "FileOp", tag = "2")]
    pub op: i32,
    #[prost(string, tag = "3")]
    pub path: String,
    #[prost(string, tag = "4")]
    pub dest_path: String,
    /// Resume offset for Download/Upload.
    #[prost(uint64, tag = "5")]
    pub offset: u64,
    #[prost(uint64, tag = "6")]
    pub size: u64,
    #[prost(bool, tag = "7")]
    pub compress: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileEntry {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(bool, tag = "2")]
    pub is_dir: bool,
    #[prost(uint64, tag = "3")]
    pub size: u64,
    #[prost(int64, tag = "4")]
    pub modified_unix: i64,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileListing {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(string, tag = "2")]
    pub path: String,
    #[prost(message, repeated, tag = "3")]
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileChunk {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(uint64, tag = "2")]
    pub offset: u64,
    #[prost(bytes = "bytes", tag = "3")]
    pub data: Bytes,
    #[prost(bool, tag = "4")]
    pub compressed: bool,
    #[prost(bool, tag = "5")]
    pub last: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct FileAck {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(uint64, tag = "2")]
    pub offset: u64,
    /// Empty on success.
    #[prost(string, tag = "3")]
    pub error: String,
    /// BLAKE3 or SHA-256 digest of the full file when transfer completes.
    #[prost(bytes = "vec", tag = "4")]
    pub digest: Vec<u8>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TunnelOpen {
    #[prost(uint32, tag = "1")]
    pub tunnel_id: u32,
    #[prost(string, tag = "2")]
    pub remote_host: String,
    #[prost(uint32, tag = "3")]
    pub remote_port: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TunnelResult {
    #[prost(uint32, tag = "1")]
    pub tunnel_id: u32,
    #[prost(bool, tag = "2")]
    pub ok: bool,
    #[prost(string, tag = "3")]
    pub error: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TerminalOpen {
    #[prost(uint32, tag = "1")]
    pub term_id: u32,
    #[prost(uint32, tag = "2")]
    pub cols: u32,
    #[prost(uint32, tag = "3")]
    pub rows: u32,
    /// Empty = platform default shell.
    #[prost(string, tag = "4")]
    pub shell: String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TerminalData {
    #[prost(uint32, tag = "1")]
    pub term_id: u32,
    #[prost(bytes = "bytes", tag = "2")]
    pub data: Bytes,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TerminalResize {
    #[prost(uint32, tag = "1")]
    pub term_id: u32,
    #[prost(uint32, tag = "2")]
    pub cols: u32,
    #[prost(uint32, tag = "3")]
    pub rows: u32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TerminalExit {
    #[prost(uint32, tag = "1")]
    pub term_id: u32,
    #[prost(sint32, tag = "2")]
    pub code: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PrivacyMode {
    #[prost(bool, tag = "1")]
    pub enable: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PrivacyModeResult {
    #[prost(bool, tag = "1")]
    pub enabled: bool,
    #[prost(string, tag = "2")]
    pub error: String,
}

/// Bidirectional in-session chat message (v0.5.0, Envelope tag 46).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ChatMessage {
    #[prost(uint64, tag = "1")]
    pub id: u64,
    #[prost(string, tag = "2")]
    pub sender_name: String,
    #[prost(string, tag = "3")]
    pub text: String,
    /// Sender wall-clock timestamp in milliseconds since UNIX epoch.
    #[prost(uint64, tag = "4")]
    pub ts_ms: u64,
}

/// Delivery acknowledgement for a [`ChatMessage`] (v0.5.0, Envelope tag 47).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct ChatAck {
    #[prost(uint64, tag = "1")]
    pub id: u64,
}

/// Viewer → host remote power management request (v0.5.0, Envelope tag 48).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SystemPowerRequest {
    #[prost(enumeration = "SystemPowerAction", tag = "1")]
    pub action: i32,
    /// Optional reason string (logged by OS shutdown/power manager).
    #[prost(string, tag = "2")]
    pub reason: String,
    /// Optional grace delay in seconds before executing reboot.
    #[prost(uint32, tag = "3")]
    pub delay_secs: u32,
}

/// Host → viewer acknowledgement/result for [`SystemPowerRequest`] (v0.5.0, Envelope tag 49).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct SystemPowerResult {
    #[prost(enumeration = "SystemPowerAction", tag = "1")]
    pub action: i32,
    #[prost(bool, tag = "2")]
    pub ok: bool,
    #[prost(string, tag = "3")]
    pub error: String,
}

// ───────────────────────────── envelope ─────────────────────────────

/// Top-level control message. Unknown oneof tags decode as `msg: None` (ignore them).
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Envelope {
    #[prost(
        oneof = "envelope::Msg",
        tags = "1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13, 14, 15, 16, 20, 21, 22, 23, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49"
    )]
    pub msg: Option<envelope::Msg>,
}

pub mod envelope {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Msg {
        // session 1..9
        #[prost(message, tag = "1")]
        Hello(super::Hello),
        #[prost(message, tag = "2")]
        AuthChallenge(super::AuthChallenge),
        #[prost(message, tag = "3")]
        AuthResponse(super::AuthResponse),
        #[prost(message, tag = "4")]
        AuthResult(super::AuthResult),
        #[prost(message, tag = "5")]
        SessionConfig(super::SessionConfig),
        #[prost(message, tag = "6")]
        Ping(super::Ping),
        #[prost(message, tag = "7")]
        Pong(super::Pong),
        #[prost(message, tag = "8")]
        Close(super::Close),
        // video / ABR 10..19
        #[prost(message, tag = "10")]
        KeyframeRequest(super::KeyframeRequest),
        #[prost(message, tag = "11")]
        Nack(super::Nack),
        #[prost(message, tag = "12")]
        ReceiverReport(super::ReceiverReport),
        #[prost(message, tag = "13")]
        BitrateUpdate(super::BitrateUpdate),
        #[prost(message, tag = "14")]
        FrameTiming(super::FrameTimingReport),
        #[prost(message, tag = "15")]
        SwitchDisplay(super::SwitchDisplay),
        #[prost(message, tag = "16")]
        VideoConfig(super::VideoConfig),
        // input 20..29
        #[prost(message, tag = "20")]
        Input(super::InputEvent),
        #[prost(message, tag = "21")]
        Rumble(super::Rumble),
        #[prost(message, tag = "22")]
        CursorShape(super::CursorShape),
        #[prost(message, tag = "23")]
        CursorPosition(super::CursorPosition),
        // features 30..
        #[prost(message, tag = "30")]
        Clipboard(super::ClipboardData),
        #[prost(message, tag = "31")]
        FileRequest(super::FileRequest),
        #[prost(message, tag = "32")]
        FileListing(super::FileListing),
        #[prost(message, tag = "33")]
        FileChunk(super::FileChunk),
        #[prost(message, tag = "34")]
        FileAck(super::FileAck),
        #[prost(message, tag = "35")]
        TunnelOpen(super::TunnelOpen),
        #[prost(message, tag = "36")]
        TunnelResult(super::TunnelResult),
        #[prost(message, tag = "37")]
        TerminalOpen(super::TerminalOpen),
        #[prost(message, tag = "38")]
        TerminalData(super::TerminalData),
        #[prost(message, tag = "39")]
        TerminalResize(super::TerminalResize),
        #[prost(message, tag = "40")]
        TerminalExit(super::TerminalExit),
        #[prost(message, tag = "41")]
        PrivacyMode(super::PrivacyMode),
        #[prost(message, tag = "42")]
        PrivacyModeResult(super::PrivacyModeResult),
        #[prost(message, tag = "43")]
        Permissions(super::Permissions),
        #[prost(message, tag = "44")]
        AudioConfig(super::AudioConfig),
        #[prost(message, tag = "45")]
        DisplayList(super::DisplayList),
        // v0.5.0 features 46..
        #[prost(message, tag = "46")]
        ChatMessage(super::ChatMessage),
        #[prost(message, tag = "47")]
        ChatAck(super::ChatAck),
        #[prost(message, tag = "48")]
        SystemPowerRequest(super::SystemPowerRequest),
        #[prost(message, tag = "49")]
        SystemPowerResult(super::SystemPowerResult),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct DisplayList {
    #[prost(message, repeated, tag = "1")]
    pub displays: Vec<DisplayInfo>,
}

impl From<envelope::Msg> for Envelope {
    fn from(m: envelope::Msg) -> Self {
        Envelope { msg: Some(m) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn envelope_roundtrip() {
        let ev = InputEvent {
            ts_us: 99,
            event: Some(input_event::Event::Key(KeyEvent { hid_usage: 0x04, down: true, modifiers: 1 })),
        };
        let env: Envelope = envelope::Msg::Input(ev).into();
        let bytes = env.encode_to_vec();
        assert_eq!(Envelope::decode(bytes.as_slice()).unwrap(), env);
    }

    #[test]
    fn unknown_tag_is_ignored() {
        // field 99, varint 1  → unknown to this version, must decode as msg: None
        let bytes = [0x98, 0x06, 0x01];
        let env = Envelope::decode(&bytes[..]).unwrap();
        assert!(env.msg.is_none());
    }

    #[test]
    fn v050_chat_and_power_roundtrip() {
        let msgs: Vec<envelope::Msg> = vec![
            envelope::Msg::ChatMessage(ChatMessage {
                id: 42,
                sender_name: "Alice".into(),
                text: "Привет / Hello!".into(),
                ts_ms: 1_700_000_000_000,
            }),
            envelope::Msg::ChatAck(ChatAck { id: 42 }),
            envelope::Msg::SystemPowerRequest(SystemPowerRequest {
                action: SystemPowerAction::Reboot as i32,
                reason: "Windows Update".into(),
                delay_secs: 5,
            }),
            envelope::Msg::SystemPowerResult(SystemPowerResult {
                action: SystemPowerAction::Reboot as i32,
                ok: true,
                error: String::new(),
            }),
        ];
        for msg in msgs {
            let env: Envelope = msg.into();
            let bytes = env.encode_to_vec();
            assert_eq!(Envelope::decode(bytes.as_slice()).unwrap(), env);
        }
    }

    #[test]
    fn permissions_v040_compat_roundtrip() {
        let full = Permissions::all();
        assert!(full.allow_chat && full.allow_power);
        let inter = Permissions::interactive_default();
        assert!(inter.allow_chat && !inter.allow_power);
        let encoded = inter.encode_to_vec();
        assert_eq!(Permissions::decode(encoded.as_slice()).unwrap(), inter);
    }
}
