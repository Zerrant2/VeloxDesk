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
