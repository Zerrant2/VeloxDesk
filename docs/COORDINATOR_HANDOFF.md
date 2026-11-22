# Coordinator handoff (written 2026-11-22 19:02 by Opus coordinator)

Read in this order: `docs/ARCHITECTURE.md` (contract + ADRs), `docs/SWARM.md` (env rules +
binding cross-crate M1 APIs), then the swarm board (`read_board`).

## State
* **M0 DONE**, commit `32cd27c`: workspace (22 crates), `crates/protocol`, `crates/core-traits`,
  `cargo test --workspace` green (8 tests), clippy clean on contracts. Everything after that is
  uncommitted worker output — coordinator commits at checkpoints (`git add -A; git -c user.name=coordinator -c user.email=coordinator@veloxdesk.local commit -m ...`).
* **M1 in progress** (swarm title "VeloxDesk — M1 Low-Latency LAN MVP").

## Key decisions (frozen, see ARCHITECTURE ADRs)
* Name `veloxdesk`, license MIT OR Apache-2.0, Rust 2024 edition, resolver 3.
* UI: **egui + egui-wgpu + winit** (ADR-002), video + UI + HUD in one wgpu pass.
* Transport: quinn QUIC, media = datagrams + Reed-Solomon FEC, everything else = reliable
  streams with 1-byte `StreamKind` preamble. ALPN `veloxdesk/1`. Ports: rendezvous 21116, relay 21117, direct 21118.
* Serialization: control = hand-written prost derives (`protocol/src/control.rs`, never reuse tags);
  media = fixed 24-byte LE header (`protocol/src/media.rs`). Stream framing `u32 LE len | Envelope`.
* Keys on the wire = USB HID usage codes. Timestamps = host monotonic µs, `ClockSync` (Ping/Pong).
* Media path = dedicated threads + sync traits; network = tokio.
* Codecs: Windows HW via Media Foundation MFTs (+ optional direct NVENC), SW fallback openh264.
* M1 has no auth (auto-accept); real auth/E2EE/rendezvous/relay = M2.
* Agreed API extras (board #8/#9): `AbrController::new(start,min,max)`,
  `on_report(&ReceiverReport, now) -> Option<AbrDecision{bitrate_kbps, parity_percent}>`.
  Viewer↔host wire behaviour: board #11 (render-ui-worker). Crypto: `tls::{server_config, client_config, PeerPolicy}`.

## Swarm roster / ownership
| worker | owns | status at handoff |
|---|---|---|
| net-worker | crypto, transport, server-rendezvous, server-relay, apps/server | crypto green; doing transport/FEC |
| media-worker | codec, audio (capture delegated) | doing codecs |
| capture-dev (helper of media-worker) | crates/capture | doing DXGI capture |
| render-ui-worker | render, ui, apps/client | doing renderer + viewer |
| sys-worker | input, clipboard, file-transfer, tunnel, terminal, virtual-display, service | doing input + host pipeline |
| qa-worker | tests/, benches/, .github/workflows | impair proxy (11 tests), protocol compat (10 tests), CI written; pipeline/transport_loss gated behind features until APIs land |

## USER DIRECTIVE on models
Coordinator: Opus (if available). **All workers: `gemini-4-neon-high`** — existing ones switched
via `swarm model` when `waiting` (never kill), new ones spawned with `model: "gemini-4-neon-high"`.
At handoff: qa-worker, sys-worker already on Neon; net-worker, media-worker, capture-dev,
render-ui-worker still Opus → switch them as soon as they show `waiting`/`stopped`.
A cron (task "Model migration check", every minute) was doing this; re-create if lost.

## Environment gotchas
* Cargo not on PATH: prefix `$env:PATH="$env:USERPROFILE\.cargo\bin;$env:PATH";`
* Wrap cargo in `cmd /c "... 2>&1"` (PowerShell stderr noise); check `$LASTEXITCODE`.
* Sandbox: no network → `cargo fetch/add` need BypassSandbox; build with `--offline`.
* Sandbox drops loopback UDP → QUIC/UDP tests must run with BypassSandbox (board #7).
* `.ps1` scripts need `powershell -ExecutionPolicy Bypass -File`.
* Per-worker target dirs `target\<name>`.

## Next steps for coordinator
1. Finish model migration (above).
2. Wait for workers' `@coordinator` reports; integrate; when transport+codec+capture+service+client
   compile, run `cargo test --workspace` + `cargo clippy --workspace --all-targets -- -D warnings`,
   have qa-worker flip `pipeline`/`net` features and run the pipeline bench (p50/p95/p99 per stage).
3. Commit "M1", report to the user (in Russian) with real latency numbers vs budget (ARCHITECTURE §1).
4. Then M2 (net-worker: rendezvous/relay/NAT punching/E2EE auth; sys-worker: auth in service;
   render-ui: connect-by-ID UI), M3, M4 per ARCHITECTURE §9.
