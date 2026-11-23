<div align="center">

# VeloxDesk

**Fast, lightweight, open-source remote desktop written in pure Rust.**  
RustDesk-class desktop administration + Parsec-class low-latency gaming mode — with zero Pro paywalls and zero Electron/Flutter/C++ runtime bloat.

[![GitHub Release](https://img.shields.io/github/v/release/Zerrant2/VeloxDesk?style=flat-square)](https://github.com/Zerrant2/VeloxDesk/releases)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue?style=flat-square)](LICENSE-MIT)
[![Rust 2024](https://img.shields.io/badge/rust-2024%20edition-orange?style=flat-square)](Cargo.toml)

**English** | **[Русский](README.ru.md)**

</div>

---

## Highlights

### Unified Host + Viewer App (`veloxdesk`)
- **All-in-one GUI**: Ready to accept incoming connections ("This Desk") and connect to remote machines from one lightweight `wgpu` + `egui` window.
- **System Tray & Service Mode**: Minimizes cleanly to the Windows system tray, supports `--portable` mode, local IPC, and native Windows SCM / Linux systemd service installation (`--service-install`).
- **Multilingual UI**: Full **English** and **Russian** interface (including HUD, settings, modals, hover tooltips, and system tray), persisted across restarts.
- **Built-in Auto-Updater**: Checks [GitHub Releases](https://github.com/Zerrant2/VeloxDesk/releases) over HTTPS (`rustls`), verifies PE headers & GitHub SHA-256 digests, atomically replaces the running executable with rollback on failure, and relaunches cleanly.

### Parsec-Class Gaming Mode & Deep Desktop / Gaming Split
- **Deeply Differentiated Profiles**: Separate persisted streaming profiles for **Desktop** (`Balanced` 20 Mbps, 60 FPS, VSync `Fifo`, YUV 4:4:4 crisp text, 10 ms Opus, static idle refresh, cursor composited) and **Gaming** (`High` 50 Mbps, 120 FPS, `Mailbox` low-latency present, HUD on, relative mouse lock, YUV 4:2:0 fast HW encode, 5 ms Opus `RestrictedLowDelay`, elevated >=15% FEC parity, instant cursor hiding) — with 1-click **Reset profile defaults** and **live mid-session `Desktop ↔ Gaming` renegotiation** (`SessionModeUpdate`).
- **GPU Zero-Copy Pipeline**: Windows DXGI Desktop Duplication + DX12 GPU Zero-Copy (`~3.03 ms` p50 @ 1080p120; `~3.23 ms` p50 @ 1080p240 in local pipeline benchmarks).
- **Hardware Video & High Fidelity**: NVENC / D3D11VA / Media Foundation hardware encode & decode (with `openh264` software fallback), **10-bit HDR** (`P010` / HEVC Main10 / AV1 10-bit), and **YUV 4:4:4** full chroma.
- **Low-Latency Audio, Echo Cancellation, Noise Suppression & Gamepads**: Pure-Rust Opus (`rusty-opus`, configurable `Auto / 2.5 / 5 / 10 / 20 ms` frames), Windows 10 2004+/11 **Process Loopback Exclusion** + pure-Rust **NLMS Acoustic Echo Canceller (AEC)**, pure-Rust **5-band Microphone Noise Suppression + smooth VAD gate** (`>23 dB` stationary noise reduction), microphone passthrough, and up to **4 gamepads** (XInput → ViGEmBus / uinput) with rumble feedback.
- **Zero Re-Encode Session Recording**: Client-side pure-Rust Matroska (`.mkv`) muxer on a dedicated IO thread (H.264/HEVC/AV1 + Opus) that starts cleanly on an IDR keyframe without adding render latency, complete with a live `REC` toolbar indicator, timer, file size readout, configurable folder (default `Videos\VeloxDesk`), and 1-click **Open folder** button in Settings.

### RustDesk-Class Remote Admin, Android & Networking
- **Flexible Connectivity**:
  - Direct **LAN / IP** connections (`UDP 21118`).
  - **Native Tailscale Integration**: Auto-detects installed `tailscale` CLI, copies your Tailscale IP in one click, and lists online Tailscale peers for 1-click connect.
  - **Connect by 9-Digit Device ID**: Self-hosted `veloxdesk-server` with UDP NAT hole punching (`UDP 21116`) and zero-knowledge UDP relay fallback (`UDP 21117`).
- **Android Companion (Viewer & Host)**: Native Jetpack Compose Android app (`apps/android`, v0.6.0) backed by the Rust JNI bridge (`crates/android-bridge`) with bidirectional PC ↔ Android session chat, differentiated Desktop/Gaming profiles, AEC & microphone noise suppression settings, configurable Android Host permissions, remote control, and screen streaming.
- **Resilient Media Transport**: QUIC (`quinn`) with unreliable media datagrams + Reed-Solomon FEC + adaptive bitrate (ABR), plus reliable streams for control, input, files, tunnels, and terminal.
- **Admin Tools & Live Host Settings**:
  - **Full Settings Control (PC + Android)**: Live-synced **Host Access & Security** toggles for granular default permissions (`Input`, `Clipboard`, `File Transfer`, `Audio & Mic`, `Chat`, `Terminal & Tunnels`, `Remote Power / Reboot`), Host Loopback Echo Exclusion, Windows Privacy strategy (`Curtain Overlay` vs `VDD + Monitor Off`), and Auto-Reconnect toggle.
  - **Bidirectional Session Chat**: Built-in chat panel on Desktop (both Viewer and embedded Host GUI) and Android with unread badges and delivery acknowledgements (`allow_chat` permission).
  - **Remote Power & Auto-Reconnect**: Confirmation-gated `Reboot`, `Reboot (Safe Mode)`, `Lock Screen`, and `Sleep` (`allow_power` permission, configurable in Host Settings) with automatic exponential-backoff reconnect for up to 5 minutes after host reboot.
  - **Reliable Windows Privacy Mode**: Two-tier `WindowsPrivacyController` using a black fullscreen topmost curtain overlay per physical monitor with `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` (works without VDD or admin privileges while DXGI/WGC captures cleanly underneath), optional `BlockInput` / VDD `SC_MONITORPOWER`, and strict RAII `PrivacyGuard` cleanup.
  - Bidirectional clipboard sync (text, HTML, PNG, file lists with `zstd` compression & echo suppression).
  - Resumable file manager with `zstd` compression and SHA-256 verification.
  - Built-in remote terminal (**ConPTY** on Windows / PTY on Unix).
  - TCP port-forwarding tunnels (`--tunnel local:host:remote`).
  - Multi-monitor switching and virtual display support.

### End-to-End Security & Flexible Access Control
- **E2EE by Default**: QUIC + TLS 1.3 pinned directly to persistent Ed25519 device identities (`identity.key`). Neither the rendezvous nor the relay server can decrypt traffic.
- **Zero-Plaintext Password Auth**: Challenge-response authentication (`Argon2id` + `HMAC-SHA256`) — passwords are never transmitted or stored in plaintext.
- **Three Host Password Modes**: Choose **One-time password (OTP)** (auto-rotates after every login), **Permanent password** (Argon2id hash for unattended access), or **Both**.
- **Interactive Approval & Persistent Trusted Devices**: Pop-up `AcceptPrompt` with per-session granular permissions (input, clipboard, files, audio, terminal, tunnels) and *"Remember this device"* saved to disk with instant revocation from Settings.

---

## Quick Start

### 1. Download Prebuilt Windows Binaries
Grab the latest `veloxdesk.exe` and `veloxdesk-server.exe` from [**GitHub Releases**](https://github.com/Zerrant2/VeloxDesk/releases/latest).

### 2. Direct Connection (LAN or Tailscale)
1. Run `veloxdesk.exe` on both machines.
2. On the host machine, note its **LAN IP** or **Tailscale IP** (`100.x.y.z`) and its password (OTP or Permanent password configured in **Settings → Host Access & Security**).
3. On the viewer machine, enter the IP (or click a Tailscale peer) and click **Connect**.

### 3. Self-Hosted ID & Relay Server (`veloxdesk-server`)
To connect across the internet using 9-digit Device IDs:

```sh
veloxdesk-server \
  --rendezvous-bind 0.0.0.0:21116 \
  --relay-bind 0.0.0.0:21117 \
  --public-relay-addr <YOUR_PUBLIC_IP>:21117
```

Open UDP ports **`21116`** (rendezvous / NAT punch) and **`21117`** (zero-knowledge relay) on your server firewall, then enter `<YOUR_PUBLIC_IP>:21116` in **Settings → Rendezvous server** on both clients.

### 4. CLI & Unattended Service Usage

```sh
# Launch unified GUI (Host + Viewer)
veloxdesk

# Portable mode (stores settings & identity.key next to veloxdesk.exe)
veloxdesk --portable

# Connect directly via CLI in Gaming mode with HUD and a TCP tunnel
veloxdesk --connect 100.64.0.10:21118 --gaming --hud --tunnel 8080:127.0.0.1:80

# Run headless host daemon
veloxdesk --host --config veloxdesk.toml

# Install / inspect / uninstall OS service (Windows SCM / Linux systemd)
veloxdesk --service-install
veloxdesk --service-status
veloxdesk --service-uninstall
```

---

## Configuration & Storage

| Platform / Mode | Path | Contents |
|---|---|---|
| **Windows** | `%APPDATA%\veloxdesk\` | `settings.json` (profiles, language, recent list, Argon2id permanent hash, trusted devices), `identity.key` (Ed25519 key / 9-digit ID) |
| **Linux** | `$XDG_CONFIG_HOME/veloxdesk/` or `~/.config/veloxdesk/` | `settings.json`, `identity.key` |
| **macOS** | `~/Library/Application Support/veloxdesk/` | `settings.json`, `identity.key` |
| **Portable (`--portable`)** | Directory next to `veloxdesk.exe` | `veloxdesk-settings.json`, `identity.key`, optional `veloxdesk.toml` |

*Tailscale tokens and peer lists are never stored by VeloxDesk — they are queried live from the local `tailscale status --json` CLI.*

---

## Architecture

VeloxDesk is organized as a modular 24-crate Cargo workspace (+ Android Jetpack Compose app):

```text
apps/
  android/             # Android Viewer + Host Jetpack Compose app (Kotlin + JNI)
  client/              # Unified GUI (Host + Viewer + Win32 tray + Tailscale + Auto-updater)
  server/              # Self-hosted rendezvous + zero-knowledge relay binary
crates/
  protocol/            # Protobuf control messages & wire framing
  core-traits/         # Shared media, input, transport, and permission traits
  crypto/              # Ed25519 identities, TLS 1.3 pinning, Argon2id & HMAC-SHA256 auth
  transport/           # QUIC (quinn), unreliable datagrams, Reed-Solomon FEC, ABR, NAT punch
  server-rendezvous/   # 9-digit ID registry & hole-punch coordinator (UDP 21116)
  server-relay/        # Zero-knowledge UDP relay (UDP 21117)
  capture/             # DXGI Desktop Duplication & DX12 GPU Zero-Copy capture
  codec/               # Hardware NVENC/MF/D3D11VA + openh264 software codecs
  audio/               # Pure-Rust Opus, WASAPI process-exclusion loopback, NLMS AEC, jitter buffer
  recorder/            # Zero re-encode pure-Rust Matroska (.mkv) muxer (H.264/HEVC/AV1 + Opus)
  render/              # Single-pass wgpu + egui video renderer & performance HUD
  ui/                  # Connect screen, Settings, chat, power modal, EN/RU i18n, tools UI
  input/               # Keyboard/mouse injection & XInput/ViGEmBus/uinput gamepads
  clipboard/           # Cross-platform clipboard sync (text/HTML/PNG/files + zstd)
  file-transfer/       # Resumable zstd file transfer with SHA-256 verification
  tunnel/              # Multiplexed TCP port forwarding over QUIC
  terminal/            # Native Windows ConPTY & Unix PTY remote shell
  virtual-display/     # Virtual display management & WDA_EXCLUDEFROMCAPTURE privacy curtain
  service/             # Host session manager, chat/power dispatch, auth state, IPC, SCM / systemd
  android-bridge/      # Rust JNI bridge powering the Android Viewer & Host app
tests/                 # End-to-end integration, protocol compat & headless UI glyph tests
benches/               # Pipeline latency & network impairment simulator benchmarks
```

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for detailed ADRs, protocol diagrams, and latency budgets.

---

## Building from Source

Requires **Rust 1.85+** (Rust 2024 edition).

```sh
# Clone the repository
git clone https://github.com/Zerrant2/VeloxDesk.git
cd VeloxDesk

# Build optimized release binaries
cargo build --release -p veloxdesk -p veloxdesk-server

# Run the full test suite & clippy lint gate
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## Keyboard Shortcuts (In Session)

| Shortcut | Action |
|---|---|
| `Ctrl+Shift+F` | Toggle fullscreen |
| `Ctrl+Alt+G` | Toggle Gaming relative mouse lock |
| `Ctrl+Shift+H` | Toggle Performance HUD |

---

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
