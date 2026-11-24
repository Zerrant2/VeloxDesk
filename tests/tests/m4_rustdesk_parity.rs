//! M4 RustDesk-parity end-to-end integration tests over real QUIC `veloxdesk-service::run_session`:
//! 1. Clipboard sync (text, PNG, file list, zstd, echo-loop suppression + `permissions.clipboard = false` denial).
//! 2. File transfer (`ListDir`, `Mkdir`, zstd `Upload`/`Download`, mid-transfer abort & resume from offset,
//!    SHA-256 verification, `Rename`, `Delete`, traversal protection + `permissions.file_transfer = false` denial).
//! 3. TCP tunnel (`LocalForwarder` + `StreamKind::Tunnel` + loopback TCP echo + `permissions.tunnel = false` denial).
//! 4. Remote terminal (real ConPTY / POSIX PTY shell echo, resize, `TerminalExit` + `permissions.terminal = false` denial).
//! 5. Privacy mode RAII auto-restore on abrupt disconnect, Virtual Display Manager, multi-monitor
//!    `SwitchDisplay`, Local IPC (`Status` / `RotateOtp` / `Shutdown`), and `--portable` mode.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with `BypassSandbox: true`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::time::timeout;

use veloxdesk_clipboard::{ClipboardItem, MemoryClipboard, run_clipboard_stream};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, Display, DisplayId, Frame, FrameCapture,
    FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_core_traits::{Result, Timestamp};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_file_transfer::{FileTransferClient, FileTransferConfig};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthMethod, AuthResponse, Close, Hello, InputEvent, Permissions, PrivacyMode, SessionConfig, SessionMode,
    SwitchDisplay, VideoCodecCap, envelope::Msg,
};
use veloxdesk_service::{
    ApprovalDecision, ApprovalHandler, ApprovalRequest, HostArgs, HostAuthConfig, HostAuthState, HostBackends,
    HostConfig, IpcClient, IpcRequest, IpcResponse, IpcServer, TransportAbr, TransportVideoSink, detect_desktop,
    resolve_portable_paths, run_session,
};
use veloxdesk_terminal::{TerminalClient, TerminalEvent};
use veloxdesk_transport::{AbrController, ControlReceiver, ControlSender, VideoSender, framed};
use veloxdesk_tunnel::{LocalForwarder, TunnelPolicy};
use veloxdesk_virtual_display::{
    PrivacyController, SimulatedPrivacyController, SimulatedVirtualDisplayManager, VirtualDisplayManager,
    VirtualDisplaySpec, create_virtual_display_manager,
};

// ───────────────────────────── shared test helpers ─────────────────────────────

struct DualDisplayCapture {
    active_idx: usize,
    displays: Vec<Display>,
    frame_n: u32,
}

impl DualDisplayCapture {
    fn new() -> Self {
        Self {
            active_idx: 0,
            displays: vec![
                Display {
                    id: DisplayId("DISP0".into()),
                    index: 0,
                    name: "Primary 320x180".into(),
                    x: 0,
                    y: 0,
                    width: 320,
                    height: 180,
                    refresh_hz: 60,
                    scale_factor: 1.0,
                    primary: true,
                    hdr: false,
                },
                Display {
                    id: DisplayId("DISP1".into()),
                    index: 1,
                    name: "Secondary 480x270".into(),
                    x: 320,
                    y: 0,
                    width: 480,
                    height: 270,
                    refresh_hz: 60,
                    scale_factor: 1.0,
                    primary: false,
                    hdr: false,
                },
            ],
            frame_n: 0,
        }
    }
}

impl FrameCapture for DualDisplayCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-m4-dual", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(self.displays.clone())
    }
    fn start(&mut self, cfg: &CaptureConfig) -> Result<()> {
        if let Some(pos) = self.displays.iter().position(|d| d.id == cfg.display) {
            self.active_idx = pos;
        }
        Ok(())
    }
    fn next_frame(&mut self, timeout_dur: Duration) -> Result<Option<CapturedFrame>> {
        std::thread::sleep(timeout_dur.min(Duration::from_millis(16)));
        self.frame_n = self.frame_n.wrapping_add(1);
        let d = &self.displays[self.active_idx];
        let mut buf = vec![0x40u8; (d.width * d.height * 4) as usize];
        buf[0] = (self.frame_n & 0xFF) as u8;
        let frame = Frame {
            width: d.width,
            height: d.height,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::from(buf), stride: (d.width * 4) as usize }]),
            capture_ts: Timestamp::now(),
        };
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 40, convert_us: 0 }))
    }
    fn stop(&mut self) {}
}

struct NullInjector;
impl InputInjector for NullInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "null-m4", keyboard: true, mouse_absolute: true, ..Default::default() }
    }
    fn set_displays(&mut self, _displays: &[DisplayGeometry]) {}
    fn inject(&mut self, _event: &InputEvent) -> Result<()> {
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        Vec::new()
    }
}

fn base_m4_backends() -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(DualDisplayCapture::new()))),
        encoder: Arc::new(|cfg, _| veloxdesk_codec::create_encoder(cfg, false)),
        injector: Arc::new(|| Ok(Box::new(NullInjector))),
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        mic_playback: None,
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    }
}

struct TestSession {
    viewer_ch: Arc<dyn TransportChannel>,
    ctrl_tx: ControlSender,
    ctrl_rx: ControlReceiver,
    session_cfg: SessionConfig,
    host_task: tokio::task::JoinHandle<Result<veloxdesk_service::SessionSummary>>,
}

impl TestSession {
    async fn start(backends: HostBackends, perms: Permissions) -> Self {
        let host_id = DeviceIdentity::generate();
        let viewer_id = DeviceIdentity::generate();
        let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host_id).await.expect("listen");
        let (h, v) =
            tokio::join!(listener.accept(), veloxdesk_transport::connect(listener.local_addr(), &viewer_id, None));
        let host_ch: Arc<dyn TransportChannel> = Arc::new(h.expect("accept"));
        let viewer_ch: Arc<dyn TransportChannel> = Arc::new(v.expect("connect"));

        let granted = perms.clone();
        let handler: ApprovalHandler = Arc::new(move |_req: ApprovalRequest| {
            let p = granted.clone();
            Box::pin(async move { ApprovalDecision::accept(p) })
        });
        let mut auth = HostAuthConfig::require_accept_with(handler);
        auth.host_public_key = Some(host_id.public_key());

        let cfg = Arc::new(HostConfig {
            max_fps: 30,
            bitrate_kbps: 2_000,
            codec: VideoCodec::H264,
            prefer_hw: false,
            prefer_gpu: false,
            allow_input: perms.input,
            audio_enabled: false,
            auth,
            ..Default::default()
        });

        let host_task = tokio::spawn(run_session(host_ch, cfg, backends));

        let (s, r) = viewer_ch.open_bi(StreamKind::Control).await.expect("open control");
        let (mut ctrl_tx, mut ctrl_rx) = framed(s, r);
        ctrl_tx
            .send(
                &Msg::Hello(Hello {
                    protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                    device_id: viewer_id.device_id(),
                    public_key: viewer_id.public_key().to_vec(),
                    device_name: "m4-qa-viewer".into(),
                    os: std::env::consts::OS.into(),
                    app_version: "0.4.0".into(),
                    mode: SessionMode::Desktop as i32,
                    video_caps: vec![VideoCodecCap {
                        codec: VideoCodec::H264 as i32,
                        hw_decode: false,
                        max_width: 1920,
                        max_height: 1080,
                        max_fps: 60,
                        yuv444: false,
                        ten_bit: false,
                    }],
                    audio_codecs: vec![],
                    viewer_width: 1280,
                    viewer_height: 720,
                    viewer_refresh_hz: 60,
                    max_bitrate_kbps: 4_000,
                })
                .into(),
            )
            .await
            .expect("send Hello");

        // AuthChallenge -> AuthResponse(None) -> AuthResult(ok) -> SessionConfig.
        let chlg = timeout(Duration::from_secs(5), ctrl_rx.recv()).await.unwrap().unwrap();
        assert!(matches!(chlg.msg, Some(Msg::AuthChallenge(_))), "expected AuthChallenge, got {:?}", chlg.msg);
        ctrl_tx
            .send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }).into())
            .await
            .expect("send AuthResponse");
        let auth_res = timeout(Duration::from_secs(5), ctrl_rx.recv()).await.unwrap().unwrap();
        match auth_res.msg {
            Some(Msg::AuthResult(r)) => assert!(r.ok, "auth rejected: {}", r.reason),
            other => panic!("expected AuthResult, got {other:?}"),
        }
        let session_cfg = loop {
            let env = timeout(Duration::from_secs(5), ctrl_rx.recv()).await.unwrap().unwrap();
            if let Some(Msg::SessionConfig(sc)) = env.msg {
                break sc;
            }
        };

        Self { viewer_ch, ctrl_tx, ctrl_rx, session_cfg, host_task }
    }

    async fn close(mut self) {
        let _ = self.ctrl_tx.send(&Msg::Close(Close { code: 0, reason: "m4 test done".into() }).into()).await;
        self.viewer_ch.close(0, "done");
        let _ = timeout(Duration::from_secs(5), self.host_task).await.expect("host exit timeout").unwrap();
    }
}

fn unique_temp_dir(tag: &str) -> PathBuf {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("veloxdesk-m4-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

// ───────────────────────────── Test 1: Clipboard ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clipboard_sync_all_formats_zstd_echo_suppression_and_permission_denial() {
    // 1a. Bidirectional sync (Text, PNG, FileList) + zstd roundtrip + zero echo-loop.
    let host_clip = MemoryClipboard::new();
    let viewer_clip = MemoryClipboard::new();

    let backends = base_m4_backends().with_clipboard({
        let c = host_clip.clone();
        Arc::new(move || Ok(Box::new(c.clone())))
    });
    let sess = TestSession::start(backends, Permissions::all()).await;

    let (clip_s, clip_r) = sess.viewer_ch.open_bi(StreamKind::Clipboard).await.expect("open clipboard");
    let (stop_tx, stop_rx) = watch::channel(false);
    let viewer_task = tokio::spawn(run_clipboard_stream(
        clip_s,
        clip_r,
        Box::new(viewer_clip.clone()),
        Duration::from_millis(20),
        stop_rx,
    ));

    // Helper: wait until `target` equals `expected`.
    async fn wait_clip(target: &MemoryClipboard, expected: &ClipboardItem) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if target.get().as_ref() == Some(expected) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        panic!("clipboard did not sync {:?}; got {:?}", expected, target.get());
    }

    // Text: host -> viewer (verify zstd wire compression works too).
    let text_item = ClipboardItem::Text("VeloxDesk M4 Clipboard — привет мир! ".repeat(20));
    let wire_zstd = text_item.to_wire(true).expect("to_wire zstd");
    assert!(wire_zstd.compressed && wire_zstd.data.len() < 128);
    assert_eq!(ClipboardItem::from_wire(&wire_zstd).expect("from_wire"), text_item);

    host_clip.set(Some(text_item.clone()));
    wait_clip(&viewer_clip, &text_item).await;
    let host_writes_before = host_clip.write_count();
    let viewer_writes_after_text = viewer_clip.write_count();
    // Wait several poll intervals: echo-suppression must prevent any bounce-back writes!
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(host_clip.write_count(), host_writes_before, "echo loop bounced text back to host!");
    assert_eq!(viewer_clip.write_count(), viewer_writes_after_text, "duplicate write on viewer!");

    // PNG image: viewer -> host.
    let png_bytes = Bytes::from_static(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4]);
    let png_item = ClipboardItem::ImagePng(png_bytes);
    viewer_clip.set(Some(png_item.clone()));
    wait_clip(&host_clip, &png_item).await;

    // File list: host -> viewer.
    let files_item = ClipboardItem::FileList(vec!["/tmp/a.txt".into(), "/tmp/b.png".into()]);
    host_clip.set(Some(files_item.clone()));
    wait_clip(&viewer_clip, &files_item).await;

    // Verify no echo loop after all 3 items.
    let h_w = host_clip.write_count();
    let v_w = viewer_clip.write_count();
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert_eq!(host_clip.write_count(), h_w, "echo loop detected on host");
    assert_eq!(viewer_clip.write_count(), v_w, "echo loop detected on viewer");

    let _ = stop_tx.send(true);
    let _ = timeout(Duration::from_secs(3), viewer_task).await;
    sess.close().await;

    // 1b. Permission denial: `permissions.clipboard = false` must NOT write to host clipboard.
    let host_clip_denied = MemoryClipboard::new();
    let backends = base_m4_backends().with_clipboard({
        let c = host_clip_denied.clone();
        Arc::new(move || Ok(Box::new(c.clone())))
    });
    let mut perms = Permissions::all();
    perms.clipboard = false;
    let sess = TestSession::start(backends, perms).await;

    let (cs, cr) = sess.viewer_ch.open_bi(StreamKind::Clipboard).await.expect("open denied clipboard");
    let (mut c_tx, _c_rx) = framed(cs, cr);
    let wire = ClipboardItem::Text("blocked-by-permissions".into()).to_wire(false).unwrap();
    let _ = c_tx.send(&Msg::Clipboard(wire).into()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(host_clip_denied.get(), None, "clipboard write must be blocked when permissions.clipboard = false");
    assert_eq!(host_clip_denied.write_count(), 0);
    sess.close().await;
}

// ───────────────────────────── Test 2: File Transfer ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn file_transfer_crud_zstd_abort_resume_sha256_and_permission_denial() {
    let host_root = unique_temp_dir("ft-host");
    let local_dir = unique_temp_dir("ft-local");

    let ft_cfg = FileTransferConfig {
        root_dir: Some(host_root.clone()),
        chunk_size: 32 * 1024,
        max_parallel: 4,
        compress: true,
    };

    // 2a. Full CRUD + zstd + mid-transfer abort & resume (both Download and Upload) + SHA-256.
    let backends = base_m4_backends().with_file_transfer(ft_cfg.clone());
    let sess = TestSession::start(backends, Permissions::all()).await;
    let client = FileTransferClient::new(sess.viewer_ch.clone()).with_chunk_size(32 * 1024);

    // Mkdir & ListDir.
    client.mkdir("sub").await.expect("mkdir sub");
    let root_entries = client.list_dir("").await.expect("list root");
    assert!(root_entries.iter().any(|e| e.name == "sub" && e.is_dir));

    // Path traversal blocked by root_dir jail.
    assert!(client.list_dir("../..").await.is_err(), "path traversal must be rejected");

    // Create a 192 KiB compressible local file.
    let mut payload = Vec::with_capacity(192 * 1024);
    for i in 0..(192 * 1024 / 16) {
        payload.extend_from_slice(&format!("VELOX-{:08x}\r\n", i % 64).into_bytes()[..16]);
    }
    let src_local = local_dir.join("source.bin");
    std::fs::write(&src_local, &payload).unwrap();

    // Upload with zstd + progress callback.
    let progress = Arc::new(Mutex::new(Vec::<(u64, u64)>::new()));
    let prog_cb = {
        let p = progress.clone();
        Arc::new(move |done: u64, total: u64| p.lock().unwrap().push((done, total)))
    };
    let up_rep = client.upload(&src_local, "sub/data.bin", false, true, Some(prog_cb)).await.expect("upload");
    assert_eq!(up_rep.total_bytes, payload.len() as u64);
    assert_eq!(up_rep.resumed_from, 0);
    assert!(up_rep.wire_bytes < up_rep.total_bytes / 4, "expected zstd compression, wire={}", up_rep.wire_bytes);
    assert_eq!(progress.lock().unwrap().last().copied(), Some((payload.len() as u64, payload.len() as u64)));

    // Mid-transfer abort + resume (Download): seed local file with first 64 KiB, then resume!
    let dl_local = local_dir.join("downloaded.bin");
    std::fs::write(&dl_local, &payload[..64 * 1024]).unwrap();
    let dl_rep = client.download("sub/data.bin", &dl_local, true, true, None).await.expect("resumed download");
    assert_eq!(dl_rep.resumed_from, 64 * 1024);
    assert_eq!(dl_rep.bytes_transferred, (192 - 64) * 1024);
    assert_eq!(dl_rep.total_bytes, payload.len() as u64);
    assert_eq!(dl_rep.sha256, up_rep.sha256);
    assert_eq!(std::fs::read(&dl_local).unwrap(), payload);

    // Mid-transfer abort + resume (Upload): seed host file with first 64 KiB, then resume!
    std::fs::write(host_root.join("sub/partial_up.bin"), &payload[..64 * 1024]).unwrap();
    let up_resumed = client.upload(&src_local, "sub/partial_up.bin", true, true, None).await.expect("resumed upload");
    assert_eq!(up_resumed.resumed_from, 64 * 1024);
    assert_eq!(up_resumed.bytes_transferred, (192 - 64) * 1024);
    assert_eq!(up_resumed.sha256, up_rep.sha256);
    assert_eq!(std::fs::read(host_root.join("sub/partial_up.bin")).unwrap(), payload);

    // Rename & Delete.
    client.rename("sub/data.bin", "sub/renamed.bin").await.expect("rename");
    let sub_entries = client.list_dir("sub").await.expect("list sub");
    assert!(sub_entries.iter().any(|e| e.name == "renamed.bin"));
    assert!(!sub_entries.iter().any(|e| e.name == "data.bin"));
    client.delete("sub/renamed.bin").await.expect("delete");
    let sub_after = client.list_dir("sub").await.expect("list sub after delete");
    assert!(!sub_after.iter().any(|e| e.name == "renamed.bin"));

    sess.close().await;

    // 2b. Permission denial: `permissions.file_transfer = false`.
    let backends = base_m4_backends().with_file_transfer(ft_cfg);
    let mut perms = Permissions::all();
    perms.file_transfer = false;
    let sess = TestSession::start(backends, perms).await;
    let denied_client = FileTransferClient::new(sess.viewer_ch.clone());
    assert!(
        denied_client.list_dir("").await.is_err(),
        "list_dir must fail when permissions.file_transfer = false"
    );
    sess.close().await;

    let _ = std::fs::remove_dir_all(&host_root);
    let _ = std::fs::remove_dir_all(&local_dir);
}

// ───────────────────────────── Test 3: TCP Tunnel ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tcp_tunnel_forwarder_and_permission_denial() {
    // Local TCP echo server on an ephemeral loopback port.
    let echo_listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo server");
    let echo_port = echo_listener.local_addr().unwrap().port();
    let echo_task = tokio::spawn(async move {
        while let Ok((mut sock, _)) = echo_listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    let policy = TunnelPolicy { allow_loopback_only: true, allowed_ports: Some(vec![echo_port]) };

    // 3a. Permitted tunnel forwards 64 KiB cleanly through `LocalForwarder` + `StreamKind::Tunnel`.
    let backends = base_m4_backends().with_tunnel_policy(policy.clone());
    let sess = TestSession::start(backends, Permissions::all()).await;
    let fwd = LocalForwarder::bind(
        "127.0.0.1:0".parse().unwrap(),
        "127.0.0.1".into(),
        echo_port,
        sess.viewer_ch.clone(),
    )
    .await
    .expect("bind LocalForwarder");

    let mut client = TcpStream::connect(fwd.local_addr()).await.expect("connect forwarder");
    let sent: Vec<u8> = (0..65_536usize).map(|i| (i * 31 + 7) as u8).collect();
    let sent_clone = sent.clone();
    let writer = tokio::spawn(async move {
        let mut s = sent_clone;
        s.truncate(65_536);
        s
    });
    let expected = writer.await.unwrap();
    let (mut rd, mut wr) = client.split();
    let write_fut = async {
        wr.write_all(&sent).await.expect("tcp write");
        wr.shutdown().await.expect("tcp shutdown");
    };
    let read_fut = async {
        let mut got = Vec::with_capacity(expected.len());
        rd.read_to_end(&mut got).await.expect("tcp read");
        got
    };
    let ((), got) =
        timeout(Duration::from_secs(5), async { tokio::join!(write_fut, read_fut) }).await.expect("echo timeout");
    assert_eq!(got, expected, "TCP tunnel echo payload mismatch");
    fwd.shutdown();
    sess.close().await;

    // 3b. Permission denial: `permissions.tunnel = false` rejects TunnelOpen with `TunnelResult { ok: false }`.
    let backends = base_m4_backends().with_tunnel_policy(policy);
    let mut perms = Permissions::all();
    perms.tunnel = false;
    let sess = TestSession::start(backends, perms).await;
    let (ts, tr) = sess.viewer_ch.open_bi(StreamKind::Tunnel).await.expect("open denied tunnel");
    let (mut t_tx, mut t_rx) = framed(ts, tr);
    t_tx.send(
        &Msg::TunnelOpen(veloxdesk_protocol::control::TunnelOpen {
            tunnel_id: 1,
            remote_host: "127.0.0.1".into(),
            remote_port: echo_port as u32,
        })
        .into(),
    )
    .await
    .expect("send TunnelOpen");
    let reply = timeout(Duration::from_secs(3), t_rx.recv()).await.expect("reply timeout").expect("reply recv");
    match reply.msg {
        Some(Msg::TunnelResult(res)) => {
            assert!(!res.ok, "tunnel must be rejected when permissions.tunnel = false");
            assert!(!res.error.is_empty());
        }
        other => panic!("expected TunnelResult(ok=false), got {other:?}"),
    }
    sess.close().await;
    echo_task.abort();
}

// ───────────────────────────── Test 4: Remote Terminal (ConPTY / PTY) ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_terminal_conpty_echo_resize_exit_and_permission_denial() {
    // 4a. Real PTY shell runs `echo VELOXDESK_M4_OK`, accepts resize, and emits TerminalExit(0).
    let sess = TestSession::start(base_m4_backends(), Permissions::all()).await;
    let shell = if cfg!(windows) {
        "cmd.exe /C echo VELOXDESK_M4_OK".to_string()
    } else {
        "/bin/sh -c 'echo VELOXDESK_M4_OK'".to_string()
    };
    let mut term = TerminalClient::open(sess.viewer_ch.as_ref(), 80, 24, shell).await.expect("open terminal");
    let _ = term.resize(120, 40).await;

    let mut output = Vec::new();
    let mut exit_code = None;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        match timeout(Duration::from_secs(2), term.recv()).await {
            Ok(Ok(TerminalEvent::Data(b))) => output.extend_from_slice(&b),
            Ok(Ok(TerminalEvent::Exit(code))) => {
                exit_code = Some(code);
                break;
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&output);
    assert!(text.contains("VELOXDESK_M4_OK"), "terminal output missing marker; got: {text:?}");
    assert_eq!(exit_code, Some(0), "expected TerminalExit(0), got {exit_code:?}");
    sess.close().await;

    // 4b. Permission denial: `permissions.terminal = false` blocks terminal execution.
    let mut perms = Permissions::all();
    perms.terminal = false;
    let sess = TestSession::start(base_m4_backends(), perms).await;
    let denied_shell = if cfg!(windows) {
        "cmd.exe /C echo SHOULD_NOT_RUN".to_string()
    } else {
        "/bin/sh -c 'echo SHOULD_NOT_RUN'".to_string()
    };
    let mut denied_term =
        TerminalClient::open(sess.viewer_ch.as_ref(), 80, 24, denied_shell).await.expect("open stream");
    let mut denied_out = Vec::new();
    let mut denied_code = None;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match timeout(Duration::from_millis(500), denied_term.recv()).await {
            Ok(Ok(TerminalEvent::Data(b))) => denied_out.extend_from_slice(&b),
            Ok(Ok(TerminalEvent::Exit(code))) => {
                denied_code = Some(code);
                break;
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let denied_text = String::from_utf8_lossy(&denied_out);
    assert!(!denied_text.contains("SHOULD_NOT_RUN"), "denied terminal must not execute shell");
    assert!(denied_code != Some(0), "denied terminal must not exit with 0");
    sess.close().await;
}

// ───────────────────────────── Test 5: Privacy, Virtual Display, SwitchDisplay, IPC, Portable ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn privacy_mode_raii_virtual_display_switch_display_ipc_and_portable() {
    // 5a. Multi-monitor SwitchDisplay + PrivacyMode enable + RAII auto-restore on abrupt disconnect.
    let privacy = Arc::new(SimulatedPrivacyController::new());
    let backends = base_m4_backends().with_privacy(privacy.clone());
    let mut sess = TestSession::start(backends, Permissions::all()).await;

    assert_eq!(sess.session_cfg.displays.len(), 2, "expected 2 displays in SessionConfig");
    assert_eq!(sess.session_cfg.active_display, 0);
    assert_eq!(sess.session_cfg.video.as_ref().unwrap().width, 320);

    // Switch to display 1 (480x270) and verify host emits DisplayList + reconfigured SessionConfig.
    sess.ctrl_tx.send(&Msg::SwitchDisplay(SwitchDisplay { index: 1 }).into()).await.expect("send SwitchDisplay");
    let mut saw_display_list = false;
    let switched = timeout(Duration::from_secs(5), async {
        loop {
            let env = sess.ctrl_rx.recv().await.expect("recv");
            match env.msg {
                Some(Msg::DisplayList(dl)) if dl.displays.len() == 2 => saw_display_list = true,
                Some(Msg::SessionConfig(sc))
                    if sc.active_display == 1
                        && sc.video.as_ref().is_some_and(|v| v.width == 480 && v.height == 270) =>
                {
                    return sc;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("SwitchDisplay reconfigured SessionConfig timeout");
    assert!(saw_display_list, "expected Msg::DisplayList on SwitchDisplay");
    assert_eq!(switched.video.as_ref().unwrap().width, 480);
    assert_eq!(switched.video.as_ref().unwrap().height, 270);

    // Enable Privacy Mode over control stream.
    sess.ctrl_tx.send(&Msg::PrivacyMode(PrivacyMode { enable: true }).into()).await.expect("send PrivacyMode");
    let p_res = timeout(Duration::from_secs(3), async {
        loop {
            let env = sess.ctrl_rx.recv().await.expect("recv");
            if let Some(Msg::PrivacyModeResult(r)) = env.msg {
                return r;
            }
        }
    })
    .await
    .expect("PrivacyModeResult timeout");
    assert!(p_res.enabled && p_res.error.is_empty(), "expected privacy enabled, got {p_res:?}");
    assert!(privacy.is_enabled());
    assert_eq!(privacy.enable_count(), 1);

    // Abruptly disconnect WITHOUT sending PrivacyMode { enable: false }: RAII PrivacyGuard MUST restore!
    sess.viewer_ch.close(1, "abrupt disconnect");
    let _ = timeout(Duration::from_secs(5), sess.host_task).await.expect("host teardown timeout").unwrap();
    assert!(!privacy.is_enabled(), "RAII PrivacyGuard must restore display/input on disconnect!");
    assert_eq!(privacy.disable_count(), 1);

    // 5b. Privacy permission denial (`permissions.privacy_mode = false`).
    let privacy_denied = Arc::new(SimulatedPrivacyController::new());
    let backends = base_m4_backends().with_privacy(privacy_denied.clone());
    let mut perms = Permissions::all();
    perms.privacy_mode = false;
    let mut sess = TestSession::start(backends, perms).await;
    sess.ctrl_tx.send(&Msg::PrivacyMode(PrivacyMode { enable: true }).into()).await.expect("send denied PrivacyMode");
    let p_res = timeout(Duration::from_secs(3), async {
        loop {
            let env = sess.ctrl_rx.recv().await.expect("recv");
            if let Some(Msg::PrivacyModeResult(r)) = env.msg {
                return r;
            }
        }
    })
    .await
    .expect("denied PrivacyModeResult timeout");
    assert!(!p_res.enabled && !p_res.error.is_empty(), "privacy must be denied when permissions.privacy_mode = false");
    assert!(!privacy_denied.is_enabled());
    sess.close().await;

    // 5c. Virtual Display Manager (simulated lifecycle + live platform probe).
    let sim_vdm = SimulatedVirtualDisplayManager::new();
    assert!(sim_vdm.is_supported());
    let vd = sim_vdm
        .create(VirtualDisplaySpec { width: 1920, height: 1080, refresh_hz: 120, name: "Headless".into() })
        .expect("create virtual display");
    assert_eq!(sim_vdm.list().len(), 1);
    sim_vdm.remove(vd.id).expect("remove virtual display");
    assert!(sim_vdm.list().is_empty());
    let platform_vdm = create_virtual_display_manager();
    assert!(!platform_vdm.backend().is_empty());

    // 5d. Local IPC (`IpcServer` + `IpcClient`: Status, RotateOtp, token auth, Shutdown).
    let host_id = DeviceIdentity::generate();
    let auth_state = HostAuthState::from_config(&HostAuthConfig::with_auto_otp());
    let initial_otp = auth_state.current_otp().expect("auto otp");
    let (ipc_stop_tx, ipc_stop_rx) = watch::channel(false);
    let stop_from_handler = ipc_stop_tx.clone();
    let device_id_for_ipc = host_id.device_id();
    let auth_for_ipc = auth_state.clone();
    let server = IpcServer::bind_with_token(
        "127.0.0.1:0".parse().unwrap(),
        Some("qa-ipc-secret".into()),
        Arc::new(move |req| match req {
            IpcRequest::Status => IpcResponse::Status {
                device_id: device_id_for_ipc.clone(),
                active_otp: auth_for_ipc.current_otp(),
                active_sessions: 1,
                desktop: detect_desktop().to_string(),
            },
            IpcRequest::RotateOtp => IpcResponse::Otp { otp: auth_for_ipc.rotate_otp() },
            IpcRequest::DisconnectAll => IpcResponse::Ok { message: "disconnected".into() },
            IpcRequest::Shutdown => {
                let _ = stop_from_handler.send(true);
                IpcResponse::Ok { message: "shutting down".into() }
            }
        }),
    )
    .await
    .expect("bind IpcServer");
    let ipc_addr = server.local_addr();
    let ipc_task = tokio::spawn(server.run_until(ipc_stop_rx));

    // Wrong / missing token rejected.
    let unauth = IpcClient::request_with_token(ipc_addr, Some("wrong"), &IpcRequest::Status).await.unwrap();
    assert!(matches!(unauth, IpcResponse::Error { .. }), "wrong IPC token must fail");

    // Status returns device ID + active OTP.
    match IpcClient::request_with_token(ipc_addr, Some("qa-ipc-secret"), &IpcRequest::Status).await.unwrap() {
        IpcResponse::Status { device_id, active_otp, active_sessions, desktop } => {
            assert_eq!(device_id, host_id.device_id());
            assert_eq!(active_otp.as_deref(), Some(initial_otp.as_str()));
            assert_eq!(active_sessions, 1);
            assert!(!desktop.is_empty());
        }
        other => panic!("expected Status, got {other:?}"),
    }

    // RotateOtp changes active OTP.
    match IpcClient::request_with_token(ipc_addr, Some("qa-ipc-secret"), &IpcRequest::RotateOtp).await.unwrap() {
        IpcResponse::Otp { otp } => {
            assert_ne!(otp, initial_otp);
            assert_eq!(auth_state.current_otp().as_deref(), Some(otp.as_str()));
        }
        other => panic!("expected Otp, got {other:?}"),
    }

    // Shutdown cleanly stops IpcServer.
    let shut = IpcClient::request_with_token(ipc_addr, Some("qa-ipc-secret"), &IpcRequest::Shutdown).await.unwrap();
    assert!(matches!(shut, IpcResponse::Ok { .. }));
    timeout(Duration::from_secs(3), ipc_task).await.expect("ipc exit timeout").unwrap().unwrap();

    // 5e. Portable mode (`resolve_portable_paths` + `HostArgs::resolve_with_exe`).
    let portable_dir = unique_temp_dir("portable");
    let fake_exe = portable_dir.join("veloxdesk.exe");
    let paths = resolve_portable_paths(Some(&fake_exe));
    assert_eq!(paths.dir, portable_dir);
    assert_eq!(paths.config_path.parent(), Some(portable_dir.as_path()));
    assert_eq!(paths.identity_path.parent(), Some(portable_dir.as_path()));
    std::fs::write(&paths.config_path, "max_fps = 144\nbitrate_kbps = 35000\n").unwrap();

    let args = HostArgs { portable: true, ..Default::default() };
    let resolved = args.resolve_with_exe(Some(&fake_exe)).expect("resolve portable HostArgs");
    assert!(resolved.portable);
    assert_eq!(resolved.max_fps, 144);
    assert_eq!(resolved.bitrate_kbps, 35_000);
    assert_eq!(resolved.identity_path, Some(paths.identity_path));

    let _ = std::fs::remove_dir_all(&portable_dir);
}
