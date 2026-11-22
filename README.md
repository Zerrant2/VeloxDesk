# VeloxDesk

Open, fast, lightweight, cross-platform remote desktop written in pure Rust —
RustDesk-class office/admin features plus a Parsec / Sunshine+Moonlight-class
**gaming mode** (GPU zero-copy, hardware encode/decode, sub-15 ms glass-to-glass on LAN).

* Transport: QUIC (`quinn`) — unreliable datagrams + Reed-Solomon FEC for media,
  reliable streams for control/input/files/tunnels/terminal; E2EE (Ed25519 + TLS 1.3).
* Rendering/UI: `wgpu` + `egui` (single render pass, zero-copy video textures).
* Self-hosted rendezvous + relay server, no Pro limitations.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design, ADRs, protocol and milestones.

## Build

```sh
cargo build --release            # client: target/release/veloxdesk, server: veloxdesk-server
cargo test --workspace
```

## License

Dual-licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.
