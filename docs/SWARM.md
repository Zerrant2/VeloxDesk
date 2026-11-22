# Swarm working agreement + M1 integration contract

Read `docs/ARCHITECTURE.md` first (contracts, ADRs, ownership). This file adds the
practical rules and the **cross-crate public APIs** each owner must expose in M1 so others
can code against them in parallel. Signatures below are binding; additions are fine,
renames/removals need the coordinator.

## Environment (Windows dev box, 12 threads, MSVC Build Tools 2022/2026, Rust 1.100)
* Cargo is not on PATH in new shells: start every command with
  `$env:PATH="$env:USERPROFILE\.cargo\bin;$env:PATH";`
* Use your own target dir: `$env:CARGO_TARGET_DIR="target\<your-name>"`.
* PowerShell turns cargo's stderr into fake errors — wrap: `cmd /c "cargo check -p X --offline 2>&1"`.
* The sandbox has **no network**. Add deps with `cargo add -p <crate> <dep>@<ver>` or
  `cargo fetch` in a command run with network access (BypassSandbox), alone; everything else
  (`check/test/clippy --offline`) runs sandboxed.
* Edit only crates you own. Never edit root `Cargo.toml`, `crates/protocol`,
  `crates/core-traits` — post a request to `coordinator` on the board instead.
* Do **not** `git commit`; the coordinator commits at checkpoints. Don't run `cargo fmt`
  on other people's crates.
* **Report on the board by mentioning `@coordinator`** when an API compiles (so dependants
  can integrate), when you are blocked, and when a deliverable is done.
* Done = `cargo clippy -p <crate> --all-targets --offline -- -D warnings` and
  `cargo test -p <crate> --offline` green, no `todo!()` on the critical path, report on the board
  with numbers (latencies, throughput) where relevant.

## M1 cross-crate APIs

### `veloxdesk-capture` (media-worker)
```rust
pub fn create_capture(prefer_gpu: bool) -> Result<Box<dyn FrameCapture>>; // best backend for this OS
pub fn list_displays() -> Result<Vec<Display>>;
```
### `veloxdesk-codec` (media-worker)
```rust
pub fn probe_encoders() -> Vec<EncoderCaps>;
pub fn probe_decoders() -> Vec<DecoderCaps>;
pub fn create_encoder(cfg: &EncoderConfig, prefer_hw: bool) -> Result<Box<dyn VideoEncoder>>; // HW → SW fallback
pub fn create_decoder(cfg: &DecoderConfig, prefer_hw: bool) -> Result<Box<dyn VideoDecoder>>;
/// CPU helpers usable by anyone (tests, fallbacks):
pub mod convert { pub fn bgra_to_nv12(..); pub fn nv12_to_rgba(..); }
```
Input to `encode` may be `Bgra8` (CPU or GPU); the encoder converts on GPU when it can and
reports `convert_us`. Decoder output: `Nv12` (GPU texture when `prefer_gpu_output`, else CPU).

### `veloxdesk-audio` (media-worker, M1 optional / M3 required)
```rust
pub fn create_loopback_capture() -> Result<Box<dyn AudioCapture>>;
pub fn create_playback() -> Result<Box<dyn AudioPlayback>>;
pub struct OpusEncoder; pub struct OpusDecoder; // impl AudioEncoder / AudioDecoder
```

### `veloxdesk-crypto` (net-worker)
```rust
pub struct DeviceIdentity { /* Ed25519 */ }
impl DeviceIdentity { pub fn generate() -> Self; pub fn load_or_create(path: &Path) -> Result<Self>;
  pub fn public_key(&self) -> [u8; 32]; pub fn device_id(&self) -> String; /* 9 digits */ }
```
### `veloxdesk-transport` (net-worker)
```rust
pub struct QuicChannel;                       // impl TransportChannel, Clone (Arc inside)
pub struct Listener; impl Listener { pub async fn accept(&self) -> Result<QuicChannel>; pub fn local_addr(&self) -> SocketAddr; }
pub async fn listen(bind: SocketAddr, id: &DeviceIdentity) -> Result<Listener>;
pub async fn connect(addr: SocketAddr, id: &DeviceIdentity, expected_peer: Option<[u8; 32]>) -> Result<QuicChannel>;
/// Framed protobuf stream (u32 LE len | Envelope) over SendStream/RecvStream.
pub struct ControlSender; pub struct ControlReceiver;
impl ControlSender { pub async fn send(&mut self, msg: &Envelope) -> Result<()>; }
impl ControlReceiver { pub async fn recv(&mut self) -> Result<Envelope>; }
pub fn framed(send: SendStream, recv: RecvStream) -> (ControlSender, ControlReceiver);
/// Media plane.
pub struct VideoSender;   // EncodedPacket → FEC shards → datagrams
impl VideoSender { pub fn new(ch: Arc<dyn TransportChannel>, stream_id: u8) -> Self;
  pub fn send_frame(&mut self, pkt: &EncodedPacket) -> Result<SendStats>;
  pub fn set_parity_percent(&mut self, pct: u8); pub fn on_nack(&mut self, nack: &Nack); }
pub struct VideoReceiver; // datagrams → complete EncodedPacket (+ loss/FEC stats)
impl VideoReceiver { pub fn new() -> Self;
  pub fn push_datagram(&mut self, dgram: &[u8], now: Timestamp) -> Option<ReceivedFrame>; // ReceivedFrame { packet: EncodedPacket, recv_complete_ts, recovered_shards }
  pub fn poll_feedback(&mut self, now: Timestamp) -> Vec<Envelope>; // ReceiverReport / Nack / KeyframeRequest
}
pub struct AbrController; // host side: consumes ReceiverReport → EncoderUpdate + parity %
```

### `veloxdesk-input` (sys-worker)
```rust
pub fn create_injector() -> Result<Box<dyn InputInjector>>;
```
### `veloxdesk-service` (sys-worker) — host pipeline
```rust
pub struct HostConfig { pub bind: SocketAddr, pub max_fps: u32, pub bitrate_kbps: u32, pub codec: VideoCodec, pub prefer_hw: bool, pub display: Option<u32>, /* .. */ }
pub async fn run_host(cfg: HostConfig, id: DeviceIdentity) -> Result<()>; // listen, accept sessions, capture→encode→VideoSender, input→injector
```
### `veloxdesk-render` (render-ui-worker)
```rust
pub struct VideoSurface; // impl VideoRenderer; owns wgpu device/surface, composes egui (UI + HUD) in the same pass
```
### `apps/client` (render-ui-worker)
`veloxdesk --host [--bind 0.0.0.0:21118]` (calls `run_host`) and `veloxdesk --connect <ip:port>`
(viewer: connect → Hello → SessionConfig → VideoReceiver → decoder thread → VideoSurface,
winit input → InputEvent on the Input stream, HUD with real per-stage ms).

---

## M2 cross-crate APIs (Self-Hosted P2P & E2EE)

All M2 wire formats use existing `veloxdesk-protocol` messages (`control::Auth*`, `control::Permissions`, `rendezvous::*`) without breaking M1 compatibility.

### Wire framing on multiplexed UDP socket (zero collision with QUIC RFC 9000 fixed bit `0x40`)
* Rendezvous packet: `[0x01, b'V', b'D', b'Z'] || protobuf(RendezvousMessage)`
* Relay packet: `[0x02, b'V', b'D', b'R'] || token[16] || raw_quic_payload` (empty payload = register/keepalive for `token`)
* NAT punch probe: `[0x03, b'V', b'D', b'P'] || punch_id(u64 LE)`

### `veloxdesk-crypto::auth` (net-worker)
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Argon2Params { pub m_cost_kib: u32, pub t_cost: u32, pub p_cost: u32 }
impl Default for Argon2Params { /* e.g. 19456 KiB, t=2, p=1 */ }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordHash { pub salt: Vec<u8>, pub params: Argon2Params, pub derived_key: [u8; 32] }
impl PasswordHash {
    pub fn hash(password: &str) -> Result<Self>;
    pub fn hash_with(password: &str, params: Argon2Params) -> Result<Self>;
    pub fn to_phc_string(&self) -> String;
    pub fn from_phc_string(s: &str) -> Result<Self>;
}

pub fn derive_password_key(password: &str, salt: &[u8], params: Argon2Params) -> Result<[u8; 32]>;
pub fn password_proof(derived_key: &[u8; 32], nonce: &[u8]) -> [u8; 32]; // HMAC-SHA256
pub fn verify_password_proof(derived_key: &[u8; 32], nonce: &[u8], proof: &[u8]) -> bool; // constant-time
pub fn generate_nonce() -> [u8; 32];
pub fn generate_otp(len: usize) -> String; // unambiguous alphanumeric (no 0/O/1/I/l)
pub fn sign_pubkey_challenge(id: &DeviceIdentity, nonce: &[u8], host_pk: &[u8; 32]) -> [u8; 64]; // Ed25519(nonce || host_pk)
pub fn verify_pubkey_challenge(peer_pk: &[u8; 32], nonce: &[u8], host_pk: &[u8; 32], proof: &[u8]) -> bool;
pub fn sign_register_peer(id: &DeviceIdentity, local_addrs: Vec<String>, nat_type: NatType, timestamp_unix_ms: u64) -> RegisterPeer;
pub fn verify_register_peer(msg: &RegisterPeer, now_unix_ms: u64, max_skew_ms: u64) -> Result<[u8; 32]>;
```

### `veloxdesk-server-rendezvous` & `veloxdesk-server-relay` & `apps/server` (net-worker)
```rust
// veloxdesk-server-rendezvous
pub struct RendezvousConfig { pub bind: SocketAddr, pub relay_addr: Option<SocketAddr>, pub peer_ttl: Duration, pub keepalive_secs: u32 }
pub struct RendezvousServer { .. }
impl RendezvousServer {
    pub async fn bind(cfg: RendezvousConfig) -> Result<Self>;
    pub fn local_addr(&self) -> SocketAddr;
    pub async fn run(self) -> Result<()>;
    pub async fn run_until(self, shutdown: watch::Receiver<bool>) -> Result<()>;
}

// veloxdesk-server-relay (zero-knowledge UDP forwarder; sees ciphertext only)
pub struct RelayConfig { pub bind: SocketAddr, pub session_ttl: Duration }
pub struct RelayServer { .. }
impl RelayServer {
    pub async fn bind(cfg: RelayConfig) -> Result<Self>;
    pub fn local_addr(&self) -> SocketAddr;
    pub async fn run(self) -> Result<()>;
    pub async fn run_until(self, shutdown: watch::Receiver<bool>) -> Result<()>;
}
```
`apps/server` (`veloxdesk-server`): `--rendezvous-bind 0.0.0.0:21116`, `--relay-bind 0.0.0.0:21117`, `--public-relay-addr <addr>`, `--peer-ttl 90`.

### `veloxdesk-transport::p2p` (net-worker)
```rust
pub struct P2pConnectOptions {
    pub rendezvous: SocketAddr,
    pub target_id: String, // 9-digit device id
    pub direct_timeout: Duration, // e.g. 2.5s before automatic relay fallback
    pub relay_only: bool, // forces zero-knowledge relay (used for tests / strict firewalls)
    pub expected_peer: Option<[u8; 32]>, // optional pinned pubkey; otherwise verified against PeerInfo.public_key -> device_id
}
/// Host listener multiplexing direct LAN QUIC, rendezvous registration + NAT punching, and relay tunneling on one UDP socket.
pub struct P2pListener { .. }
impl P2pListener {
    pub async fn bind(bind: SocketAddr, rendezvous: Option<SocketAddr>, id: &DeviceIdentity) -> Result<Self>;
    pub fn local_addr(&self) -> SocketAddr;
    pub async fn accept(&self) -> Result<QuicChannel>; // sets PathKind::Direct / P2p / Relay
    pub fn shutdown(&self);
}
/// Viewer connect by 9-digit Device ID: lookup + IPv6/LAN-first UDP hole punching -> automatic zero-knowledge relay fallback.
pub async fn connect_p2p(opts: P2pConnectOptions, id: &DeviceIdentity) -> Result<QuicChannel>;
```

### `veloxdesk-service` M2 additions (sys-worker)
* `HostConfig` adds: `rendezvous: Option<SocketAddr>`, `auth: HostAuthConfig` (`one_time_password: Option<String>`, `auto_otp: bool`, `permanent_password_hash: Option<PasswordHash>`, `trusted_peers: Vec<TrustedPeer>`, `require_accept: bool`, `allow_no_auth: bool`).
* `ApprovalRequest` / `ApprovalDecision` + `ApprovalHandler` callback (`Arc<dyn Fn(ApprovalRequest) -> Pin<Box<dyn Future<Output = ApprovalDecision> + Send>> + Send + Sync>`) for interactive accept dialogs with granular `Permissions` and "remember trusted peer".
* `run_session` enforces full `Hello -> AuthChallenge -> AuthResponse -> AuthResult` (verifies TLS peer public key == `Hello.public_key`, Argon2id+HMAC proofs, OTP rotation, trusted Ed25519 pubkeys, granular `Permissions` enforcement).
* `run_host` uses `P2pListener` when `cfg.rendezvous.is_some()` (otherwise direct `listen`), keeping M1 LAN behavior intact.

### `veloxdesk-ui` & `apps/client` M2 additions (render-ui-worker)
* `ConnectScreen` accepts either a **9-digit Device ID** (with optional spaces e.g. `123 456 789`) OR direct `ip[:port]` / `host[:port]`, plus Rendezvous server field in Settings.
* Password prompt modal (on `AuthChallenge` requiring password/OTP) and interactive incoming-session Accept modal (displaying caller ID, fingerprint, granular `Permissions` toggles, "Trust peer", Accept/Reject).
* `apps/client` CLI: `--rendezvous <addr>`, `--password <pw>`, `--connect <id-or-ip:port>` (auto-detects 9-digit ID → `connect_p2p`), HUD displays real `PathKind` (`Direct` / `P2P Direct` / `Relay`).

---

## M3 cross-crate APIs (Gaming Mode — Parsec Class)

### `veloxdesk-transport::media` (net-worker)
```rust
/// Low-latency audio datagrams (`MediaKind::Audio`, 24-byte `MediaHeader`).
/// `stream_id = 0` host system audio, `stream_id = 1` viewer microphone.
pub struct AudioSender;
impl AudioSender {
    pub fn new(ch: Arc<dyn TransportChannel>, stream_id: u8) -> Self;
    pub fn set_redundant(&mut self, enabled: bool); // optional duplicate send under >2% loss
    pub fn send(&mut self, pkt: &EncodedAudio) -> Result<()>;
}
pub struct AudioReceiver;
impl AudioReceiver {
    pub fn parse_datagram(dgram: &[u8]) -> Option<(u8 /* stream_id */, EncodedAudio)>;
}

/// Idempotent gamepad snapshots (`MediaKind::GamepadState`, 24-byte `MediaHeader` + prost `GamepadState`).
pub fn encode_gamepad_datagram(state: &GamepadState) -> Result<Bytes>;
pub fn decode_gamepad_datagram(dgram: &[u8]) -> Option<GamepadState>;
```

### `veloxdesk-render::zero_copy` (render-ui-worker)
* Windows DX12 importer: opens `GpuHandle::D3D11Texture { shared_handle, adapter_luid }` via `ID3D12Device::OpenSharedHandle` + `wgpu::hal::api::Dx12::Device::texture_from_raw` (`NV12` / `P010`), caches imported textures by `shared_handle`, verifies LUID match, falls back to CPU readback if unavailable.
* `VideoRenderer::caps().zero_copy_import` reports `["D3D11Texture"]` when DX12 zero-copy is active.
* `video.wgsl` + `VideoPipeline` support `Nv12`, `I420`, `Bgra8/Rgba8`, **`P010` (10-bit HDR10 PQ→SDR tone-map / passthrough)**, and **`Yuv444` (3-plane full chroma)**.
* `OffscreenRenderer::new_with_backends(w, h, backends)` (or `new` preferring DX12 on Windows) exposes `draw_frame(&mut self, frame: &Frame, scale: ScaleMode) -> Result<(u32, bool /* zero_copy */)>`.

### `veloxdesk-input` M3 virtual gamepads (sys-worker)
* Windows: ViGEmBus virtual X360 controllers (up to 4 simultaneous pads, direct Win32 `SetupDi` + `DeviceIoControl` — zero vcpkg) + rumble notifications drained via `InputInjector::poll_feedback() -> Vec<InputFeedback::Rumble>`. Graceful fallback when ViGEmBus driver is not installed.
* Linux (`#[cfg(target_os = "linux")]`): `/dev/uinput` virtual X360 gamepads (up to 4 pads) with `EV_FF` rumble feedback.

### `veloxdesk-service` M3 additions (sys-worker)
* `HostConfig` / `HostArgs` add: `ten_bit: bool` (`--hdr`), `chroma: ChromaFormat` (`--yuv444`), `audio_enabled: bool` (`--no-audio`), `audio_bitrate_kbps: u32`, `audio_frame_ms: f32` (2.5/5/10/20), `allow_mic: bool`.
* `HostBackends` adds optional `audio_capture` / `audio_encoder` / `audio_playback` factories (wired in `HostBackends::system()` to `veloxdesk-audio`).
* `run_session` wires: host WASAPI loopback → `OpusEncoder` → `AudioSender` (gated by `permissions.audio`), incoming `MediaKind::GamepadState` datagrams (per-pad monotonic `seq` filter) → `InputThread`, and `InputFeedback::Rumble` → `Msg::Rumble` on control stream!

### `veloxdesk-ui` & `apps/client` M3 additions (render-ui-worker)
* Settings & toolbar: FPS (`60/120/144/240`), 10-bit HDR, YUV 4:4:4, Audio / Mic toggles, Pointer Lock (`Ctrl+Alt+G`).
* Viewer requests `prefer_gpu_output = true` when `VideoSurface` supports zero-copy import, displays `zero_copy` and `audio_buf_ms` in HUD, plays received `MediaKind::Audio` through `AudioJitterBuffer` + `OpusDecoder` + `veloxdesk_audio::create_playback()`, polls local physical gamepads (multi-player) and applies incoming `Msg::Rumble`!

---

## M4 cross-crate APIs (Full RustDesk Parity)

> Temporary M4 crate assignments (disjoint, zero overlap): `net-worker` implements `crates/tunnel` + `crates/file-transfer`; `media-worker` implements `crates/clipboard` + `crates/virtual-display`; `sys-worker` implements `crates/terminal` + `crates/service`; `render-ui-worker` implements `crates/ui` + `apps/client`; `qa-worker` implements `tests/tests/m4_rustdesk_parity.rs` + final checks.

### `veloxdesk-tunnel` (net-worker)
```rust
#[derive(Debug, Clone)]
pub struct TunnelPolicy { pub allow_loopback_only: bool, pub allowed_ports: Option<Vec<u16>> }
impl Default for TunnelPolicy { /* allow_loopback_only: false, allowed_ports: None */ }
/// Host side: handles one `StreamKind::Tunnel` bidi stream (reads `TunnelOpen`, replies `TunnelResult`, bridges TCP).
pub async fn serve_tunnel_stream(send: SendStream, recv: RecvStream, policy: &TunnelPolicy) -> Result<()>;
/// Viewer side: listens on `local_bind`, opens a `StreamKind::Tunnel` stream per TCP client.
pub struct LocalForwarder { .. }
impl LocalForwarder {
    pub async fn bind(local_bind: SocketAddr, remote_host: String, remote_port: u16, ch: Arc<dyn TransportChannel>) -> Result<Self>;
    pub fn local_addr(&self) -> SocketAddr;
    pub fn shutdown(&self);
}
```

### `veloxdesk-file-transfer` (net-worker)
```rust
#[derive(Debug, Clone)]
pub struct FileTransferConfig { pub root_dir: Option<PathBuf>, pub chunk_size: usize, pub max_parallel: usize, pub compress: bool }
/// Host side: serves `FileRequest` (`ListDir/Stat/Download/Upload/Delete/Mkdir/Rename`) on one `StreamKind::FileTransfer` bidi stream with zstd + resume + SHA-256 digest.
pub async fn serve_file_stream(send: SendStream, recv: RecvStream, cfg: &FileTransferConfig) -> Result<()>;
/// Viewer side: high-level client opening `StreamKind::FileTransfer` streams (supports resume, zstd, SHA-256 verification, parallel transfers).
pub struct FileTransferClient { .. }
impl FileTransferClient {
    pub fn new(ch: Arc<dyn TransportChannel>) -> Self;
    pub async fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>>;
    pub async fn mkdir(&self, path: &str) -> Result<()>;
    pub async fn delete(&self, path: &str) -> Result<()>;
    pub async fn rename(&self, from: &str, to: &str) -> Result<()>;
    pub async fn download(&self, remote_path: &str, local_path: &Path, resume: bool, compress: bool, on_progress: Option<ProgressCb>) -> Result<TransferReport>;
    pub async fn upload(&self, local_path: &Path, remote_path: &str, resume: bool, compress: bool, on_progress: Option<ProgressCb>) -> Result<TransferReport>;
}
```

### `veloxdesk-clipboard` (media-worker)
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardItem { Text(String), Html(String), ImagePng(Bytes), FileList(Vec<String>) }
impl ClipboardItem {
    pub fn content_hash(&self) -> [u8; 32]; // SHA-256 for echo suppression
    pub fn to_wire(&self, compress: bool) -> Result<ClipboardData>;
    pub fn from_wire(data: &ClipboardData) -> Result<Self>;
}
pub trait ClipboardBackend: Send { fn read(&mut self) -> Result<Option<ClipboardItem>>; fn write(&mut self, item: &ClipboardItem) -> Result<()>; }
pub struct SystemClipboard; // impl ClipboardBackend (arboard text+PNG + Windows CF_HDROP)
pub struct MemoryClipboard; // impl ClipboardBackend (Arc-backed, for deterministic tests)
pub fn create_system_clipboard() -> Result<Box<dyn ClipboardBackend>>;
pub async fn run_clipboard_stream(send: SendStream, recv: RecvStream, backend: Box<dyn ClipboardBackend>, poll_interval: Duration, shutdown: watch::Receiver<bool>) -> Result<()>;
```

### `veloxdesk-virtual-display` (media-worker)
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDisplaySpec { pub width: u32, pub height: u32, pub refresh_hz: u32, pub name: String }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDisplayInfo { pub id: u32, pub spec: VirtualDisplaySpec }
pub trait VirtualDisplayManager: Send + Sync {
    fn backend(&self) -> &'static str;
    fn is_supported(&self) -> bool;
    fn create(&self, spec: VirtualDisplaySpec) -> Result<VirtualDisplayInfo>;
    fn remove(&self, id: u32) -> Result<()>;
    fn list(&self) -> Vec<VirtualDisplayInfo>;
}
pub fn create_virtual_display_manager() -> Box<dyn VirtualDisplayManager>;
pub trait PrivacyController: Send + Sync {
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn is_enabled(&self) -> bool;
}
pub struct PrivacyGuard { .. } // RAII: always calls disable() on Drop
pub fn create_privacy_controller() -> Arc<dyn PrivacyController>;
pub struct SimulatedPrivacyController; // impl PrivacyController (for tests)
```

### `veloxdesk-terminal` (sys-worker)
```rust
/// Host side: reads `TerminalOpen`, spawns real ConPTY (Windows) or POSIX PTY (Unix), bridges `TerminalData` + `TerminalResize`, emits `TerminalExit`.
pub async fn serve_terminal_stream(send: SendStream, recv: RecvStream) -> Result<()>;
/// Viewer side: interactive remote terminal session over `StreamKind::Terminal`.
pub struct TerminalClient { .. }
impl TerminalClient {
    pub async fn open(ch: &dyn TransportChannel, cols: u32, rows: u32, shell: String) -> Result<Self>;
    pub async fn write(&mut self, data: &[u8]) -> Result<()>;
    pub async fn resize(&mut self, cols: u32, rows: u32) -> Result<()>;
    pub async fn recv(&mut self) -> Result<TerminalEvent>; // Data(Bytes) | Exit(i32)
}
```

### `veloxdesk-service` M4 additions (sys-worker)
* Wires all 5 M4 crates into `run_session` gated by `Permissions::{clipboard, file_transfer, tunnel, terminal, privacy_mode}`, RAII `PrivacyGuard` cleanup on disconnect, live `DisplayList` + hot `SwitchDisplay`.
* Local IPC (`ipc::{IpcServer, IpcClient, IpcRequest, IpcResponse}`), daemon/service helpers (`daemon::{ServiceStatus, DesktopKind, detect_desktop, install_service, uninstall_service, service_status}`), portable mode (`--portable`) & `--config <path>`.
