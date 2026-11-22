//! M2 end-to-end integration tests: self-hosted Rendezvous + zero-knowledge Relay +
//! P2P connect-by-ID + mutual-TLS E2EE + Argon2id/HMAC password, rotating OTP,
//! trusted Ed25519 PublicKey whitelist, and interactive ApprovalHandler permissions.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with `BypassSandbox: true`.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use prost::Message;
use tokio::net::UdpSocket;
use tokio::sync::watch;

use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::transport::{PathKind, TransportChannel};
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, DecoderConfig, Display, DisplayId, Frame,
    FrameCapture, FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_core_traits::{Result, Timestamp};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{
    Argon2Params, PasswordHash, derive_password_key, password_proof, sign_pubkey_challenge, sign_register_peer,
};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthChallenge, AuthMethod, AuthResponse, AuthResult, Close, Hello, InputEvent, KeyEvent, KeyframeRequest,
    MouseMove, Permissions, SessionConfig, SessionMode, VideoCodecCap, envelope::Msg, input_event,
};
use veloxdesk_protocol::rendezvous::{NatType, PeerLookup, RendezvousMessage, rendezvous_message::Msg as RdvMsg};
use veloxdesk_server_relay::{RelayConfig, RelayServer};
use veloxdesk_server_rendezvous::{MAGIC_RENDEZVOUS, RendezvousConfig, RendezvousServer};
use veloxdesk_service::host::serve_p2p_until;
use veloxdesk_service::{
    ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostAuthState, HostBackends, HostConfig,
    TransportAbr, TransportVideoSink, TrustedPeer,
};
use veloxdesk_transport::{AbrController, P2pConnectOptions, P2pListener, VideoReceiver, VideoSender, connect_p2p, framed};

// ───────────────────────────── test helpers ─────────────────────────────

fn fast_argon2() -> Argon2Params {
    // Low cost for fast deterministic CI tests while still exercising full Argon2id.
    Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }
}

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

struct SynthCapture {
    w: u32,
    h: u32,
    n: u32,
    buf: Vec<u8>,
}

impl SynthCapture {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, n: 0, buf: vec![0x38; (w * h * 4) as usize] }
    }
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synthetic-m2", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("SYNTH0".into()),
            index: 0,
            name: "Synthetic 320x180".into(),
            x: 0,
            y: 0,
            width: self.w,
            height: self.h,
            refresh_hz: 60,
            scale_factor: 1.0,
            primary: true,
            hdr: false,
        }])
    }
    fn start(&mut self, _cfg: &CaptureConfig) -> Result<()> {
        Ok(())
    }
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        std::thread::sleep(timeout.min(Duration::from_millis(12)));
        let n = self.n;
        self.n += 1;
        let stride = (self.w * 4) as usize;
        let row = (n as usize * 5) % (self.h as usize);
        for px in self.buf[row * stride..(row + 1) * stride].chunks_exact_mut(4) {
            px[0] = (n & 0xFF) as u8;
            px[1] = 180;
            px[2] = 90;
            px[3] = 255;
        }
        let frame = Frame {
            width: self.w,
            height: self.h,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::copy_from_slice(&self.buf), stride }]),
            capture_ts: Timestamp::now(),
        };
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 80, convert_us: 0 }))
    }
    fn stop(&mut self) {}
}

struct RecordingInjector {
    events: Arc<Mutex<Vec<InputEvent>>>,
}

impl InputInjector for RecordingInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "recording-m2", keyboard: true, mouse_absolute: true, mouse_relative: true, ..Default::default() }
    }
    fn set_displays(&mut self, _displays: &[DisplayGeometry]) {}
    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        Vec::new()
    }
}

fn synth_backends(recorded_input: Arc<Mutex<Vec<InputEvent>>>) -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(SynthCapture::new(320, 180)))),
        encoder: Arc::new(|cfg, _prefer_hw| veloxdesk_codec::create_encoder(cfg, false)),
        injector: Arc::new(move || Ok(Box::new(RecordingInjector { events: recorded_input.clone() }))),
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    }
}

fn base_host_config(rdv: SocketAddr, auth: HostAuthConfig) -> HostConfig {
    HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 60,
        bitrate_kbps: 2_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        fec_parity_percent: 15,
        allow_input: true,
        max_sessions: 2,
        rendezvous: Some(rdv),
        auth,
        ..Default::default()
    }
}

fn make_hello(viewer: &DeviceIdentity, name: &str) -> Hello {
    Hello {
        protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
        device_id: viewer.device_id(),
        public_key: viewer.public_key().to_vec(),
        device_name: name.into(),
        os: std::env::consts::OS.into(),
        app_version: "0.2.0".into(),
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
    }
}

async fn send_rdv(sock: &UdpSocket, to: SocketAddr, msg: RdvMsg) {
    let env: RendezvousMessage = msg.into();
    let mut pkt = MAGIC_RENDEZVOUS.to_vec();
    pkt.extend_from_slice(&env.to_vec());
    sock.send_to(&pkt, to).await.expect("send_to rdv");
}

async fn recv_rdv(sock: &UdpSocket) -> RdvMsg {
    let mut buf = vec![0u8; 4096];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), sock.recv_from(&mut buf))
        .await
        .expect("rdv recv timeout")
        .expect("rdv recv");
    assert_eq!(&buf[..4], &MAGIC_RENDEZVOUS);
    RendezvousMessage::decode(&buf[4..n]).expect("decode rdv").msg.expect("empty rdv")
}

/// Start a self-hosted Relay + Rendezvous pair on loopback ephemeral ports.
struct TestInfra {
    rdv_addr: SocketAddr,
    shutdown_tx: watch::Sender<bool>,
    rdv_task: tokio::task::JoinHandle<Result<()>>,
    relay_task: tokio::task::JoinHandle<Result<()>>,
}

impl TestInfra {
    async fn start() -> Self {
        let relay = RelayServer::bind(RelayConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            session_ttl: Duration::from_secs(30),
        })
        .await
        .expect("bind relay");
        let relay_addr = relay.local_addr();

        let rdv = RendezvousServer::bind(RendezvousConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            relay_addr: Some(relay_addr),
            peer_ttl: Duration::from_secs(30),
            keepalive_secs: 15,
        })
        .await
        .expect("bind rdv");
        let rdv_addr = rdv.local_addr();

        let (shutdown_tx, rx) = watch::channel(false);
        let relay_task = tokio::spawn(relay.run_until(rx.clone()));
        let rdv_task = tokio::spawn(rdv.run_until(rx));
        Self { rdv_addr, shutdown_tx, rdv_task, relay_task }
    }

    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = self.rdv_task.await;
        let _ = self.relay_task.await;
    }
}

/// Wait until `target_id` is registered and visible as online on `rdv_addr`.
async fn wait_peer_online(rdv_addr: SocketAddr, target_id: &str) {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        send_rdv(&sock, rdv_addr, RdvMsg::PeerLookup(PeerLookup { target_id: target_id.into() })).await;
        if let RdvMsg::PeerInfo(info) = recv_rdv(&sock).await
            && info.online
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("peer {target_id} did not register online in time");
}

/// Receive and decode at least `min_frames` real H.264 frames from `ch`.
async fn receive_and_decode_frames(
    ch: &Arc<dyn TransportChannel>,
    ctrl_tx: &mut veloxdesk_transport::ControlSender,
    vcfg: &veloxdesk_protocol::control::VideoConfig,
    min_frames: u32,
) -> u32 {
    ctrl_tx
        .send(&Msg::KeyframeRequest(KeyframeRequest { stream_id: 0, last_good_frame_id: 0 }).into())
        .await
        .expect("send PLI");

    let mut rx = VideoReceiver::new();
    rx.set_dimensions(vcfg.width, vcfg.height);
    let mut dec = veloxdesk_codec::create_decoder(
        &DecoderConfig {
            codec: VideoCodec::H264,
            width: vcfg.width,
            height: vcfg.height,
            prefer_gpu_output: false,
            shared_device: None,
        },
        false,
    )
    .expect("create_decoder");

    let mut decoded = 0u32;
    let mut saw_keyframe = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && decoded < min_frames {
        if let Ok(Ok(d)) = tokio::time::timeout(Duration::from_millis(50), ch.recv_datagram()).await {
            let mut frames = Vec::new();
            if let Some(f) = rx.push_datagram(&d, Timestamp::now()) {
                frames.push(f);
            }
            while let Some(f) = rx.pop_frame() {
                frames.push(f);
            }
            for rf in frames {
                if rf.packet.keyframe {
                    saw_keyframe = true;
                }
                if saw_keyframe
                    && let Ok(Some(df)) = dec.decode(&rf.packet)
                {
                    assert_eq!(df.frame.width, vcfg.width);
                    assert_eq!(df.frame.height, vcfg.height);
                    decoded += 1;
                }
            }
        }
    }
    decoded
}

fn password_response(method: AuthMethod, password: &str, ch: &AuthChallenge) -> AuthResponse {
    let params = Argon2Params {
        m_cost_kib: ch.argon2_m_cost_kib,
        t_cost: ch.argon2_t_cost,
        p_cost: ch.argon2_p_cost,
    };
    let key = derive_password_key(password, &ch.salt, params).expect("derive_password_key");
    let proof = password_proof(&key, &ch.nonce);
    AuthResponse { method: method as i32, proof: proof.to_vec() }
}

async fn recv_auth_challenge(rx: &mut veloxdesk_transport::ControlReceiver) -> AuthChallenge {
    let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("challenge timeout")
        .expect("challenge recv");
    match env.msg {
        Some(Msg::AuthChallenge(c)) => c,
        other => panic!("expected AuthChallenge, got {other:?}"),
    }
}

async fn recv_auth_result(rx: &mut veloxdesk_transport::ControlReceiver) -> AuthResult {
    let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("auth result timeout")
        .expect("auth result recv");
    match env.msg {
        Some(Msg::AuthResult(r)) => r,
        other => panic!("expected AuthResult, got {other:?}"),
    }
}

async fn recv_session_config(rx: &mut veloxdesk_transport::ControlReceiver) -> SessionConfig {
    let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("session config timeout")
        .expect("session config recv");
    match env.msg {
        Some(Msg::SessionConfig(sc)) => sc,
        other => panic!("expected SessionConfig, got {other:?}"),
    }
}

// ───────────────────────────── Test 1 ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rendezvous_registration_valid_vs_forged_and_mismatched_id() {
    let infra = TestInfra::start().await;
    let host = DeviceIdentity::generate();
    let other = DeviceIdentity::generate();
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let probe = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // 1. Forged Ed25519 signature -> rejected with 401 and peer stays offline.
    let mut forged = sign_register_peer(&host, vec![sock.local_addr().unwrap().to_string()], NatType::Cone, now_unix_ms());
    forged.signature[0] ^= 0xFF;
    send_rdv(&sock, infra.rdv_addr, RdvMsg::RegisterPeer(forged)).await;
    match recv_rdv(&sock).await {
        RdvMsg::Error(e) => assert_eq!(e.code, 401, "expected 401 for forged signature, got {e:?}"),
        other => panic!("expected Error(401), got {other:?}"),
    }

    // 2. Mismatched device_id (claiming `other`'s ID while signing with `host`) -> rejected with 401.
    let mut mismatched = sign_register_peer(&host, vec![], NatType::Cone, now_unix_ms());
    mismatched.device_id = other.device_id();
    send_rdv(&sock, infra.rdv_addr, RdvMsg::RegisterPeer(mismatched)).await;
    match recv_rdv(&sock).await {
        RdvMsg::Error(e) => assert_eq!(e.code, 401, "expected 401 for mismatched device_id, got {e:?}"),
        other => panic!("expected Error(401), got {other:?}"),
    }

    // 3. Stale timestamp (> 5 min skew) -> rejected with 401.
    let stale_ts = now_unix_ms().saturating_sub(10 * 60 * 1000);
    let stale = sign_register_peer(&host, vec![], NatType::Cone, stale_ts);
    send_rdv(&sock, infra.rdv_addr, RdvMsg::RegisterPeer(stale)).await;
    match recv_rdv(&sock).await {
        RdvMsg::Error(e) => assert_eq!(e.code, 401, "expected 401 for stale timestamp, got {e:?}"),
        other => panic!("expected Error(401), got {other:?}"),
    }

    // Peer must still be offline after all rejected registrations.
    send_rdv(&probe, infra.rdv_addr, RdvMsg::PeerLookup(PeerLookup { target_id: host.device_id() })).await;
    match recv_rdv(&probe).await {
        RdvMsg::PeerInfo(info) => assert!(!info.online, "rejected peer must not appear online"),
        other => panic!("expected offline PeerInfo, got {other:?}"),
    }

    // 4. Valid signed registration -> accepted with RegisterOk & PeerLookup returns online PeerInfo.
    let valid = sign_register_peer(&host, vec![sock.local_addr().unwrap().to_string()], NatType::Cone, now_unix_ms());
    send_rdv(&sock, infra.rdv_addr, RdvMsg::RegisterPeer(valid)).await;
    match recv_rdv(&sock).await {
        RdvMsg::RegisterOk(ok) => {
            assert_eq!(ok.observed_addr, sock.local_addr().unwrap().to_string());
            assert_eq!(ok.keepalive_secs, 15);
        }
        other => panic!("expected RegisterOk, got {other:?}"),
    }

    send_rdv(&probe, infra.rdv_addr, RdvMsg::PeerLookup(PeerLookup { target_id: host.device_id() })).await;
    match recv_rdv(&probe).await {
        RdvMsg::PeerInfo(info) => {
            assert!(info.online);
            assert_eq!(info.device_id, host.device_id());
            assert_eq!(info.public_key, host.public_key());
        }
        other => panic!("expected online PeerInfo, got {other:?}"),
    }

    infra.stop().await;
}

// ───────────────────────────── Test 2 ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2p_connect_by_id_and_permanent_password_auth() {
    let infra = TestInfra::start().await;
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let pw_hash = PasswordHash::hash_with("VeloxM2!Pass", fast_argon2()).expect("hash password");
    let mut auth = HostAuthConfig::with_permanent_password(pw_hash);
    auth.host_public_key = Some(host_id.public_key());

    let listener = P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(infra.rdv_addr), &host_id)
        .await
        .expect("bind P2pListener");
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let host_task = tokio::spawn(serve_p2p_until(
        listener,
        base_host_config(infra.rdv_addr, auth),
        synth_backends(recorded),
        async move {
            let _ = stop_rx.wait_for(|&v| v).await;
        },
    ));

    wait_peer_online(infra.rdv_addr, &host_id.device_id()).await;

    // Format the 9-digit Device ID with spaces ("123 456 789") to verify normalization.
    let raw_id = host_id.device_id();
    let spaced_id = format!("{} {} {}", &raw_id[0..3], &raw_id[3..6], &raw_id[6..9]);

    // 2a. Wrong password is rejected.
    let bad_ch: Arc<dyn TransportChannel> = Arc::new(
        connect_p2p(P2pConnectOptions::new(infra.rdv_addr, spaced_id.clone()), &viewer_id)
            .await
            .expect("connect_p2p for wrong-password test"),
    );
    assert_eq!(bad_ch.stats().path, PathKind::P2p);
    assert_eq!(bad_ch.peer_public_key(), Some(host_id.public_key()));
    let (s, r) = bad_ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut tx, mut rx) = framed(s, r);
    tx.send(&Msg::Hello(make_hello(&viewer_id, "wrong-pw-viewer")).into()).await.expect("send Hello");
    let challenge = recv_auth_challenge(&mut rx).await;
    assert!(challenge.methods.contains(&(AuthMethod::PermanentPassword as i32)));
    assert_eq!(challenge.host_public_key, host_id.public_key());
    tx.send(&Msg::AuthResponse(password_response(AuthMethod::PermanentPassword, "WRONG-PASS", &challenge)).into())
        .await
        .expect("send bad AuthResponse");
    let bad_res = recv_auth_result(&mut rx).await;
    assert!(!bad_res.ok, "wrong password must be rejected");
    bad_ch.close(0, "done");

    // 2b. Correct Argon2id+HMAC password succeeds and streams real video frames over P2P.
    let good_ch: Arc<dyn TransportChannel> = Arc::new(
        connect_p2p(P2pConnectOptions::new(infra.rdv_addr, spaced_id), &viewer_id)
            .await
            .expect("connect_p2p for valid-password test"),
    );
    assert_eq!(good_ch.stats().path, PathKind::P2p);
    assert_eq!(good_ch.peer_public_key(), Some(host_id.public_key()));
    let (s, r) = good_ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut tx, mut rx) = framed(s, r);
    tx.send(&Msg::Hello(make_hello(&viewer_id, "valid-pw-viewer")).into()).await.expect("send Hello");
    let challenge = recv_auth_challenge(&mut rx).await;
    tx.send(&Msg::AuthResponse(password_response(AuthMethod::PermanentPassword, "VeloxM2!Pass", &challenge)).into())
        .await
        .expect("send good AuthResponse");
    let ok_res = recv_auth_result(&mut rx).await;
    assert!(ok_res.ok, "valid password rejected: {}", ok_res.reason);
    let sc = recv_session_config(&mut rx).await;
    let vcfg = sc.video.expect("video config");

    let decoded = receive_and_decode_frames(&good_ch, &mut tx, &vcfg, 5).await;
    assert!(decoded >= 5, "expected >= 5 decoded P2P frames, got {decoded}");

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "p2p auth ok".into() }).into()).await;
    good_ch.close(0, "done");

    let _ = stop_tx.send(true);
    host_task.await.unwrap().expect("host clean exit");
    infra.stop().await;
}

// ───────────────────────────── Test 3 ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forced_zero_knowledge_relay_fallback_and_e2ee_verification() {
    let infra = TestInfra::start().await;
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let wrong_host_id = DeviceIdentity::generate();

    let pw_hash = PasswordHash::hash_with("RelayE2EE#99", fast_argon2()).expect("hash password");
    let mut auth = HostAuthConfig::with_permanent_password(pw_hash);
    auth.host_public_key = Some(host_id.public_key());

    let listener = P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(infra.rdv_addr), &host_id)
        .await
        .expect("bind P2pListener");
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let host_task = tokio::spawn(serve_p2p_until(
        listener,
        base_host_config(infra.rdv_addr, auth),
        synth_backends(recorded),
        async move {
            let _ = stop_rx.wait_for(|&v| v).await;
        },
    ));

    wait_peer_online(infra.rdv_addr, &host_id.device_id()).await;

    // 3a. Mismatched `expected_peer` over forced relay MUST fail mutual-TLS E2EE verification.
    let mut bad_opts = P2pConnectOptions::new(infra.rdv_addr, host_id.device_id());
    bad_opts.relay_only = true;
    bad_opts.expected_peer = Some(wrong_host_id.public_key());
    let bad_conn = connect_p2p(bad_opts, &viewer_id).await;
    assert!(bad_conn.is_err(), "mismatched expected_peer must fail TLS E2EE verification over relay");

    // 3b. Valid `expected_peer` with `relay_only: true` succeeds over PathKind::Relay and streams frames.
    let mut ok_opts = P2pConnectOptions::new(infra.rdv_addr, host_id.device_id());
    ok_opts.relay_only = true;
    ok_opts.expected_peer = Some(host_id.public_key());
    let ch: Arc<dyn TransportChannel> =
        Arc::new(connect_p2p(ok_opts, &viewer_id).await.expect("connect_p2p relay_only"));
    assert_eq!(ch.stats().path, PathKind::Relay, "expected PathKind::Relay");
    assert_eq!(ch.peer_public_key(), Some(host_id.public_key()));

    let (s, r) = ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut tx, mut rx) = framed(s, r);
    tx.send(&Msg::Hello(make_hello(&viewer_id, "relay-viewer")).into()).await.expect("send Hello");
    let challenge = recv_auth_challenge(&mut rx).await;
    tx.send(&Msg::AuthResponse(password_response(AuthMethod::PermanentPassword, "RelayE2EE#99", &challenge)).into())
        .await
        .expect("send AuthResponse");
    let auth_res = recv_auth_result(&mut rx).await;
    assert!(auth_res.ok, "relay auth failed: {}", auth_res.reason);
    let sc = recv_session_config(&mut rx).await;
    let vcfg = sc.video.expect("video config");

    let decoded = receive_and_decode_frames(&ch, &mut tx, &vcfg, 5).await;
    assert!(decoded >= 5, "expected >= 5 decoded frames over relay, got {decoded}");

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "relay ok".into() }).into()).await;
    ch.close(0, "done");

    let _ = stop_tx.send(true);
    host_task.await.unwrap().expect("host clean exit");
    infra.stop().await;
}

// ───────────────────────────── Test 4 ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn otp_rotation_trusted_pubkey_and_interactive_approval_permissions() {
    let infra = TestInfra::start().await;

    // ── 4a. One-Time Password: succeeds once, rotates immediately, rejects replay, accepts rotated OTP ──
    {
        let host_id = DeviceIdentity::generate();
        let viewer_id = DeviceIdentity::generate();
        let initial_otp = "K7M9P2Q4".to_string();

        let mut auth = HostAuthConfig::with_otp(initial_otp.clone());
        auth.host_public_key = Some(host_id.public_key());
        let auth_state = HostAuthState::from_config(&auth);
        auth.shared_state = Some(auth_state.clone());

        let listener = P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(infra.rdv_addr), &host_id)
            .await
            .expect("bind P2pListener (OTP)");
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let host_task = tokio::spawn(serve_p2p_until(
            listener,
            base_host_config(infra.rdv_addr, auth),
            synth_backends(recorded),
            async move {
                let _ = stop_rx.wait_for(|&v| v).await;
            },
        ));
        wait_peer_online(infra.rdv_addr, &host_id.device_id()).await;

        // First login with initial OTP succeeds.
        let ch1: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &viewer_id)
                .await
                .expect("connect OTP 1"),
        );
        let (s, r) = ch1.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&viewer_id, "otp-1")).into()).await.unwrap();
        let chlg = recv_auth_challenge(&mut rx).await;
        assert!(chlg.methods.contains(&(AuthMethod::OneTimePassword as i32)));
        tx.send(&Msg::AuthResponse(password_response(AuthMethod::OneTimePassword, &initial_otp, &chlg)).into())
            .await
            .unwrap();
        let res1 = recv_auth_result(&mut rx).await;
        assert!(res1.ok, "initial OTP login failed: {}", res1.reason);
        let _ = recv_session_config(&mut rx).await;
        let _ = tx.send(&Msg::Close(Close { code: 0, reason: "otp 1 done".into() }).into()).await;
        ch1.close(0, "done");

        // OTP must have rotated to a fresh value.
        let rotated_otp = auth_state.current_otp().expect("rotated OTP must exist");
        assert_ne!(rotated_otp, initial_otp, "OTP did not rotate after login");

        // Replaying the consumed initial OTP MUST fail.
        let ch2: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &viewer_id)
                .await
                .expect("connect OTP replay"),
        );
        let (s, r) = ch2.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&viewer_id, "otp-replay")).into()).await.unwrap();
        let chlg = recv_auth_challenge(&mut rx).await;
        tx.send(&Msg::AuthResponse(password_response(AuthMethod::OneTimePassword, &initial_otp, &chlg)).into())
            .await
            .unwrap();
        let res2 = recv_auth_result(&mut rx).await;
        assert!(!res2.ok, "replayed OTP must be rejected");
        ch2.close(0, "done");

        // Logging in with the newly rotated OTP succeeds.
        let ch3: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &viewer_id)
                .await
                .expect("connect OTP rotated"),
        );
        let (s, r) = ch3.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&viewer_id, "otp-rotated")).into()).await.unwrap();
        let chlg = recv_auth_challenge(&mut rx).await;
        tx.send(&Msg::AuthResponse(password_response(AuthMethod::OneTimePassword, &rotated_otp, &chlg)).into())
            .await
            .unwrap();
        let res3 = recv_auth_result(&mut rx).await;
        assert!(res3.ok, "rotated OTP login failed: {}", res3.reason);
        let _ = tx.send(&Msg::Close(Close { code: 0, reason: "otp 3 done".into() }).into()).await;
        ch3.close(0, "done");

        let _ = stop_tx.send(true);
        host_task.await.unwrap().expect("host exit (OTP)");
    }

    // ── 4b. Trusted PublicKey whitelist (passwordless, bypasses approval) ──
    {
        let host_id = DeviceIdentity::generate();
        let trusted_viewer = DeviceIdentity::generate();
        let stranger_viewer = DeviceIdentity::generate();

        let approval_calls = Arc::new(AtomicU32::new(0));
        let calls_clone = approval_calls.clone();
        let handler: ApprovalHandler = Arc::new(move |_req: ApprovalRequest| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { ApprovalDecision::reject("unexpected approval prompt for trusted peer") })
        });

        let mut auth = HostAuthConfig::with_trusted_peers(vec![TrustedPeer::new(
            trusted_viewer.public_key(),
            "Trusted Admin",
        )]);
        auth.require_accept = true;
        auth.approval_handler = Some(handler);
        auth.host_public_key = Some(host_id.public_key());

        let listener = P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(infra.rdv_addr), &host_id)
            .await
            .expect("bind P2pListener (Trusted)");
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let host_task = tokio::spawn(serve_p2p_until(
            listener,
            base_host_config(infra.rdv_addr, auth),
            synth_backends(recorded),
            async move {
                let _ = stop_rx.wait_for(|&v| v).await;
            },
        ));
        wait_peer_online(infra.rdv_addr, &host_id.device_id()).await;

        // Trusted viewer authenticates passwordlessly via Ed25519 PublicKey signature.
        let ch: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &trusted_viewer)
                .await
                .expect("connect trusted"),
        );
        let (s, r) = ch.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&trusted_viewer, "trusted-laptop")).into()).await.unwrap();
        let chlg = recv_auth_challenge(&mut rx).await;
        assert!(chlg.methods.contains(&(AuthMethod::PublicKey as i32)));
        let host_pk: [u8; 32] = chlg.host_public_key.as_slice().try_into().expect("32B host pk");
        assert_eq!(host_pk, host_id.public_key());
        let sig = sign_pubkey_challenge(&trusted_viewer, &chlg.nonce, &host_pk);
        tx.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }).into())
            .await
            .unwrap();
        let res = recv_auth_result(&mut rx).await;
        assert!(res.ok, "trusted pubkey login failed: {}", res.reason);
        assert_eq!(approval_calls.load(Ordering::SeqCst), 0, "trusted peer must bypass approval handler");
        let _ = recv_session_config(&mut rx).await;
        let _ = tx.send(&Msg::Close(Close { code: 0, reason: "trusted done".into() }).into()).await;
        ch.close(0, "done");

        // Untrusted stranger is NOT offered PublicKey and cannot spoof PublicKey method.
        let ch_stranger: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &stranger_viewer)
                .await
                .expect("connect stranger"),
        );
        let (s, r) = ch_stranger.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&stranger_viewer, "stranger")).into()).await.unwrap();
        let chlg = recv_auth_challenge(&mut rx).await;
        assert!(!chlg.methods.contains(&(AuthMethod::PublicKey as i32)));
        let sig = sign_pubkey_challenge(&stranger_viewer, &chlg.nonce, &host_id.public_key());
        tx.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }).into())
            .await
            .unwrap();
        let res = recv_auth_result(&mut rx).await;
        assert!(!res.ok, "untrusted peer attempting PublicKey must be rejected");
        ch_stranger.close(0, "done");

        let _ = stop_tx.send(true);
        host_task.await.unwrap().expect("host exit (Trusted)");
    }

    // ── 4c. Interactive ApprovalHandler with granular Permissions (`input: false` blocks injection) ──
    {
        let host_id = DeviceIdentity::generate();
        let denied_viewer = DeviceIdentity::generate();
        let view_only_viewer = DeviceIdentity::generate();

        let denied_id = denied_viewer.device_id();
        let seen_requests = Arc::new(Mutex::new(Vec::<ApprovalRequest>::new()));
        let seen_clone = seen_requests.clone();
        let handler: ApprovalHandler = Arc::new(move |req: ApprovalRequest| {
            let denied = denied_id.clone();
            let seen = seen_clone.clone();
            Box::pin(async move {
                let is_denied = req.peer_device_id == denied;
                seen.lock().unwrap().push(req);
                if is_denied {
                    ApprovalDecision::reject("denied by host user")
                } else {
                    ApprovalDecision::accept(Permissions {
                        view: true,
                        input: false,
                        ..Default::default()
                    })
                }
            })
        });

        let mut auth = HostAuthConfig::require_accept_with(handler);
        auth.host_public_key = Some(host_id.public_key());

        let listener = P2pListener::bind("127.0.0.1:0".parse().unwrap(), Some(infra.rdv_addr), &host_id)
            .await
            .expect("bind P2pListener (Approval)");
        let (stop_tx, mut stop_rx) = watch::channel(false);
        let recorded_input = Arc::new(Mutex::new(Vec::new()));
        let host_task = tokio::spawn(serve_p2p_until(
            listener,
            base_host_config(infra.rdv_addr, auth),
            synth_backends(recorded_input.clone()),
            async move {
                let _ = stop_rx.wait_for(|&v| v).await;
            },
        ));
        wait_peer_online(infra.rdv_addr, &host_id.device_id()).await;

        // Viewer 1: rejected by ApprovalHandler.
        let ch_deny: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &denied_viewer)
                .await
                .expect("connect denied"),
        );
        let (s, r) = ch_deny.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&denied_viewer, "unwanted-guest")).into()).await.unwrap();
        let chlg = recv_auth_challenge(&mut rx).await;
        assert!(chlg.methods.contains(&(AuthMethod::None as i32)));
        tx.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }).into())
            .await
            .unwrap();
        let res = recv_auth_result(&mut rx).await;
        assert!(!res.ok, "rejected viewer must fail AuthResult");
        assert!(res.reason.contains("denied by host user"), "unexpected reason: {}", res.reason);
        ch_deny.close(0, "done");

        // Viewer 2: accepted with `view: true, input: false` -> frames flow, input events are blocked!
        let ch_view: Arc<dyn TransportChannel> = Arc::new(
            connect_p2p(P2pConnectOptions::new(infra.rdv_addr, host_id.device_id()), &view_only_viewer)
                .await
                .expect("connect view-only"),
        );
        let (s, r) = ch_view.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, mut rx) = framed(s, r);
        tx.send(&Msg::Hello(make_hello(&view_only_viewer, "view-only-guest")).into()).await.unwrap();
        let _chlg = recv_auth_challenge(&mut rx).await;
        tx.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::None as i32, proof: Vec::new() }).into())
            .await
            .unwrap();
        let res = recv_auth_result(&mut rx).await;
        assert!(res.ok, "view-only viewer should be accepted: {}", res.reason);
        let perms = res.permissions.expect("permissions missing");
        assert!(perms.view && !perms.input, "expected view=true input=false, got {perms:?}");

        let sc = recv_session_config(&mut rx).await;
        let vcfg = sc.video.expect("video config");

        // Attempt to send keyboard + mouse events over StreamKind::Input.
        let (in_s, in_r) = ch_view.open_bi(StreamKind::Input).await.expect("open input stream");
        let (mut in_tx, _in_rx) = framed(in_s, in_r);
        let _ = in_tx
            .send(
                &Msg::Input(InputEvent {
                    ts_us: Timestamp::now().as_micros(),
                    event: Some(input_event::Event::Key(KeyEvent { hid_usage: 0x04, down: true, modifiers: 0 })),
                })
                .into(),
            )
            .await;
        let _ = in_tx
            .send(
                &Msg::Input(InputEvent {
                    ts_us: Timestamp::now().as_micros(),
                    event: Some(input_event::Event::MouseMove(MouseMove { x: 50, y: 60, display: 0 })),
                })
                .into(),
            )
            .await;

        // Video must still stream cleanly while input is blocked.
        let decoded = receive_and_decode_frames(&ch_view, &mut tx, &vcfg, 5).await;
        assert!(decoded >= 5, "expected >= 5 decoded frames in view-only session, got {decoded}");

        let _ = tx.send(&Msg::Close(Close { code: 0, reason: "view-only done".into() }).into()).await;
        drop(in_tx);
        ch_view.close(0, "done");

        let _ = stop_tx.send(true);
        host_task.await.unwrap().expect("host exit (Approval)");

        assert!(
            recorded_input.lock().unwrap().is_empty(),
            "input: false must block all input injection, got {:?}",
            recorded_input.lock().unwrap()
        );
        let reqs = seen_requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].peer_device_id, denied_viewer.device_id());
        assert_eq!(reqs[1].peer_device_id, view_only_viewer.device_id());
        assert_eq!(reqs[1].peer_public_key, Some(view_only_viewer.public_key()));
        assert!(!reqs[1].peer_fingerprint.is_empty());
    }

    infra.stop().await;
}
