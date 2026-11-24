//! v0.3.0 Host Security Integration Tests:
//! - Live `RunningHost` QUIC handshakes across all 3 `PasswordMode`s (`OneTime`, `Permanent`, `Both`)
//!   with runtime `HostAuthState` transitions.
//! - Live `HostStatusEvent::TrustedPeerAdded` on `ApprovalDecision::accept_and_trust`,
//!   `SavedTrustedPeer` JSON persistence round-trip, passwordless Ed25519 pubkey reconnect,
//!   and immediate revocation via `remove_trusted_peer`.
//! - Headless `egui` render of `settings_window_with_security` (Host Access & Security controls +
//!   Trusted devices list) and `ConnectScreen` in `OneTime`, `Permanent`, and `Both` modes across
//!   EN & RU with 100% font-glyph verification and zero forbidden missing-glyph Unicode icons.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with `BypassSandbox: true`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::time::timeout;

use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, Display, DisplayId, Frame, FrameCapture,
    FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_core_traits::{Result, Timestamp};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{
    Argon2Params, PasswordHash, derive_password_key, password_proof, sign_pubkey_challenge,
};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthChallenge, AuthMethod, AuthResponse, AuthResult, Close, Hello, InputEvent, Permissions, SessionMode,
    VideoCodecCap, envelope::Msg,
};
use veloxdesk_service::{
    ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostBackends, HostConfig, HostStatusEvent,
    RunningHost, TransportAbr, TransportVideoSink, TrustedPeer,
};
use veloxdesk_transport::{AbrController, VideoSender, framed};
use veloxdesk_ui::{
    ConnectScreen, Language, PasswordMode, SavedTrustedPeer, Settings, UpdateUiState, settings_window_with_security,
    tr,
};

// ---------------------------------------------------------------------------
// Synthetic host backends & viewer QUIC handshake helpers
// ---------------------------------------------------------------------------

struct SynthCapture {
    frame_n: u32,
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-v030", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("V030".into()),
            index: 0,
            name: "Synth 320x180".into(),
            x: 0,
            y: 0,
            width: 320,
            height: 180,
            refresh_hz: 60,
            scale_factor: 1.0,
            primary: true,
            hdr: false,
        }])
    }
    fn start(&mut self, _cfg: &CaptureConfig) -> Result<()> {
        Ok(())
    }
    fn next_frame(&mut self, timeout_dur: Duration) -> Result<Option<CapturedFrame>> {
        std::thread::sleep(timeout_dur.min(Duration::from_millis(16)));
        self.frame_n = self.frame_n.wrapping_add(1);
        let mut buf = vec![0x33u8; 320 * 180 * 4];
        buf[0] = (self.frame_n & 0xFF) as u8;
        let frame = Frame {
            width: 320,
            height: 180,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::from(buf), stride: 320 * 4 }]),
            capture_ts: Timestamp::now(),
        };
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 20, convert_us: 0 }))
    }
    fn stop(&mut self) {}
}

struct NullInjector;
impl InputInjector for NullInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "null-v030", keyboard: true, mouse_absolute: true, ..Default::default() }
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

fn synth_backends() -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(SynthCapture { frame_n: 0 }))),
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

fn fast_argon2() -> Argon2Params {
    Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }
}

fn make_hello(viewer: &DeviceIdentity, name: &str) -> Hello {
    Hello {
        protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
        device_id: viewer.device_id(),
        public_key: viewer.public_key().to_vec(),
        device_name: name.into(),
        os: std::env::consts::OS.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
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
        max_bitrate_kbps: 2_000,
    }
}

enum AuthAttempt<'a> {
    Otp(&'a str),
    Permanent(&'a str),
    PublicKey,
}

/// Perform a full QUIC connection + `Hello` + `AuthChallenge` + `AuthResponse` handshake against `host`.
/// Returns `(AuthChallenge, AuthResult)` and gracefully closes the session if authentication succeeded.
async fn attempt_auth(
    host: &RunningHost,
    host_id: &DeviceIdentity,
    viewer_id: &DeviceIdentity,
    viewer_name: &str,
    attempt: AuthAttempt<'_>,
) -> (AuthChallenge, AuthResult) {
    let ch: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(host.local_addr(), viewer_id, Some(host_id.public_key()))
            .await
            .expect("QUIC connect to RunningHost"),
    );
    let (s, r) = ch.open_bi(StreamKind::Control).await.expect("open control stream");
    let (mut tx, mut rx) = framed(s, r);

    tx.send(&Msg::Hello(make_hello(viewer_id, viewer_name)).into())
        .await
        .expect("send Hello");

    let chlg = match timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("AuthChallenge timeout")
        .expect("AuthChallenge recv")
        .msg
    {
        Some(Msg::AuthChallenge(c)) => c,
        other => panic!("expected AuthChallenge, got {other:?}"),
    };

    let params = Argon2Params {
        m_cost_kib: chlg.argon2_m_cost_kib,
        t_cost: chlg.argon2_t_cost,
        p_cost: chlg.argon2_p_cost,
    };

    let response = match attempt {
        AuthAttempt::Otp(otp) => {
            let key = derive_password_key(otp, &chlg.salt, params).expect("derive OTP key");
            let proof = password_proof(&key, &chlg.nonce);
            AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: proof.to_vec() }
        }
        AuthAttempt::Permanent(pw) => {
            let key = derive_password_key(pw, &chlg.salt, params).expect("derive permanent password key");
            let proof = password_proof(&key, &chlg.nonce);
            AuthResponse { method: AuthMethod::PermanentPassword as i32, proof: proof.to_vec() }
        }
        AuthAttempt::PublicKey => {
            let sig = sign_pubkey_challenge(viewer_id, &chlg.nonce, &host_id.public_key());
            AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }
        }
    };

    tx.send(&Msg::AuthResponse(response).into())
        .await
        .expect("send AuthResponse");

    let result = match timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("AuthResult timeout")
        .expect("AuthResult recv")
        .msg
    {
        Some(Msg::AuthResult(r)) => r,
        other => panic!("expected AuthResult, got {other:?}"),
    };

    if result.ok {
        // Wait for SessionConfig so host finishes session setup before clean Close.
        let _ = timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await {
                    Ok(env) if matches!(env.msg, Some(Msg::SessionConfig(_))) => break,
                    Ok(_) => continue,
                    Err(_) => break,
                }
            }
        })
        .await;
        let _ = tx.send(&Msg::Close(Close { code: 0, reason: "test done".into() }).into()).await;
    }
    ch.close(0, "done");
    // Brief yield so host cleans up session slot before next connection.
    tokio::time::sleep(Duration::from_millis(40)).await;
    (chlg, result)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0F) as usize] as char);
    }
    out
}

fn saved_peer_from_trusted(p: &TrustedPeer) -> SavedTrustedPeer {
    SavedTrustedPeer {
        public_key_hex: hex_encode(&p.public_key),
        device_id: p.device_id(),
        name: p.name.clone(),
        fingerprint: p.fingerprint(),
        allow_input: p.permissions.input,
        allow_clipboard: p.permissions.clipboard,
        allow_file_transfer: p.permissions.file_transfer,
        allow_audio: p.permissions.audio,
        allow_terminal: p.permissions.terminal,
        allow_tunnel: p.permissions.tunnel,
        allow_privacy_mode: p.permissions.privacy_mode,
        allow_chat: p.permissions.allow_chat,
        allow_power: p.permissions.allow_power,
    }
}

// ---------------------------------------------------------------------------
// Test 1: All 3 PasswordModes (`OneTime`, `Permanent`, `Both`) with live RunningHost
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v030_live_password_modes_onetime_permanent_and_both() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 30,
        bitrate_kbps: 2_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: false,
        ipc_port: None,
        auth: HostAuthConfig::with_auto_otp(),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id.clone(), synth_backends(), None)
        .await
        .expect("RunningHost::start");

    // --- Mode 1: OneTime (OTP-only) ---
    let otp1 = host.auth_state().current_otp().expect("auto_otp must generate initial OTP");
    let (chlg1, res1) = attempt_auth(&host, &host_id, &viewer_id, "v-otp1", AuthAttempt::Otp(&otp1)).await;
    assert!(chlg1.methods.contains(&(AuthMethod::OneTimePassword as i32)));
    assert!(!chlg1.methods.contains(&(AuthMethod::PermanentPassword as i32)));
    assert!(res1.ok, "initial OTP must authenticate: {}", res1.reason);

    let otp2 = host.auth_state().current_otp().expect("OTP must rotate after use");
    assert_ne!(otp2, otp1, "OTP must rotate immediately after successful login");

    // Reusing old OTP must fail.
    let (_chlg_old, res_old) = attempt_auth(&host, &host_id, &viewer_id, "v-otp-old", AuthAttempt::Otp(&otp1)).await;
    assert!(!res_old.ok, "reused OTP must be rejected after rotation");

    // --- Mode 2: Permanent-only (repeatable permanent password, OTP disabled & rejected) ---
    let perm_secret = "VeloxDesk-Permanent-2026!";
    let perm_hash = PasswordHash::hash_with(perm_secret, fast_argon2()).expect("hash permanent password");
    host.auth_state().set_permanent_password_hash(Some(perm_hash));
    assert!(host.auth_state().set_otp_enabled(false).is_none());
    assert!(host.auth_state().current_otp().is_none(), "OTP must be cleared in Permanent-only mode");

    // First permanent login succeeds.
    let (chlg_p1, res_p1) =
        attempt_auth(&host, &host_id, &viewer_id, "v-perm1", AuthAttempt::Permanent(perm_secret)).await;
    assert!(chlg_p1.methods.contains(&(AuthMethod::PermanentPassword as i32)));
    assert!(!chlg_p1.methods.contains(&(AuthMethod::OneTimePassword as i32)));
    assert!(res_p1.ok, "first permanent password login must succeed: {}", res_p1.reason);

    // Second permanent login with the SAME password also succeeds (does not rotate).
    let (_chlg_p2, res_p2) =
        attempt_auth(&host, &host_id, &viewer_id, "v-perm2", AuthAttempt::Permanent(perm_secret)).await;
    assert!(res_p2.ok, "permanent password must work repeatedly without rotating: {}", res_p2.reason);

    // Wrong permanent password is rejected.
    let (_chlg_bad, res_bad) =
        attempt_auth(&host, &host_id, &viewer_id, "v-perm-bad", AuthAttempt::Permanent("wrong-pass")).await;
    assert!(!res_bad.ok, "wrong permanent password must be rejected");

    // OTP is rejected in Permanent-only mode.
    let (_chlg_no_otp, res_no_otp) =
        attempt_auth(&host, &host_id, &viewer_id, "v-otp-disabled", AuthAttempt::Otp(&otp2)).await;
    assert!(!res_no_otp.ok, "OTP must be rejected when Permanent-only mode is active");

    // --- Mode 3: Both (permanent password AND active OTP both accepted) ---
    let otp_both = host
        .auth_state()
        .set_otp_enabled(true)
        .expect("enabling OTP in Both mode must generate fresh OTP");

    // Permanent login succeeds and does NOT rotate the OTP.
    let (chlg_b1, res_b1) =
        attempt_auth(&host, &host_id, &viewer_id, "v-both-perm", AuthAttempt::Permanent(perm_secret)).await;
    assert!(chlg_b1.methods.contains(&(AuthMethod::PermanentPassword as i32)));
    assert!(chlg_b1.methods.contains(&(AuthMethod::OneTimePassword as i32)));
    assert!(res_b1.ok, "permanent password must succeed in Both mode: {}", res_b1.reason);
    assert_eq!(
        host.auth_state().current_otp().as_deref(),
        Some(otp_both.as_str()),
        "permanent password login must NOT rotate the active OTP"
    );

    // OTP login succeeds in Both mode and DOES rotate the OTP.
    let (_chlg_b2, res_b2) = attempt_auth(&host, &host_id, &viewer_id, "v-both-otp", AuthAttempt::Otp(&otp_both)).await;
    assert!(res_b2.ok, "active OTP must succeed in Both mode: {}", res_b2.reason);
    let otp_after_both = host.auth_state().current_otp().expect("OTP rotates after use in Both mode");
    assert_ne!(otp_after_both, otp_both, "OTP login in Both mode must rotate the OTP");

    host.stop();
}

// ---------------------------------------------------------------------------
// Test 2: Live TrustedPeerAdded -> SavedTrustedPeer JSON -> pubkey reconnect -> live revoke
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v030_live_trusted_peer_added_persist_reconnect_and_revoke() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let custom_perms = Permissions {
        view: true,
        input: true,
        clipboard: true,
        file_transfer: false,
        audio: true,
        tunnel: true,
        terminal: false,
        privacy_mode: true,
        restart: false,
        allow_chat: true,
        allow_power: false,
    };

    let approval_calls = Arc::new(AtomicUsize::new(0));
    let approval_calls_cb = approval_calls.clone();
    let perms_for_handler = custom_perms.clone();
    let handler: ApprovalHandler = Arc::new(move |_req: ApprovalRequest| {
        approval_calls_cb.fetch_add(1, Ordering::SeqCst);
        let perms = perms_for_handler.clone();
        Box::pin(async move { ApprovalDecision::accept_and_trust(perms) })
    });

    let mut auth = HostAuthConfig::with_auto_otp();
    auth.require_accept = true;
    auth.approval_handler = Some(handler);

    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 30,
        bitrate_kbps: 2_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: false,
        ipc_port: None,
        auth,
        ..Default::default()
    };

    let events = Arc::new(Mutex::new(Vec::<HostStatusEvent>::new()));
    let events_cb = events.clone();
    let host = RunningHost::start(
        cfg,
        host_id.clone(),
        synth_backends(),
        Some(Arc::new(move |ev| events_cb.lock().unwrap().push(ev))),
    )
    .await
    .expect("RunningHost::start");

    // 1. Initial connection via OTP triggers ApprovalHandler returning `accept_and_trust`.
    let otp = host.auth_state().current_otp().expect("initial OTP");
    let (_chlg1, res1) = attempt_auth(&host, &host_id, &viewer_id, "Trusted-Laptop", AuthAttempt::Otp(&otp)).await;
    assert!(res1.ok, "initial OTP + accept_and_trust must succeed: {}", res1.reason);
    assert_eq!(approval_calls.load(Ordering::SeqCst), 1, "ApprovalHandler must be invoked once");

    // 2. Verify `HostStatusEvent::TrustedPeerAdded` fired with exact peer identity & permissions.
    let added_peer = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|ev| match ev {
            HostStatusEvent::TrustedPeerAdded { peer } => Some(peer.clone()),
            _ => None,
        })
        .expect("HostStatusEvent::TrustedPeerAdded must be emitted on accept_and_trust");
    assert_eq!(added_peer.public_key, viewer_id.public_key());
    assert_eq!(added_peer.name, "Trusted-Laptop");
    assert_eq!(added_peer.permissions, custom_perms);

    // 3. Persist through `SavedTrustedPeer` + `Settings` JSON save/load round-trip.
    let saved = saved_peer_from_trusted(&added_peer);
    let mut settings = Settings::default();
    settings.upsert_trusted_peer(saved.clone());
    assert_eq!(settings.trusted_peers.len(), 1);

    let dir = std::env::temp_dir().join(format!("veloxdesk-v030-trusted-{}", std::process::id()));
    let path = dir.join("settings.json");
    settings.save(&path).expect("save settings with trusted peer");
    let reloaded = Settings::load(&path).expect("reload settings with trusted peer");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(reloaded.trusted_peers, vec![saved.clone()]);
    assert!(reloaded.trusted_peers[0].allow_input);
    assert!(reloaded.trusted_peers[0].allow_clipboard);
    assert!(!reloaded.trusted_peers[0].allow_file_transfer);
    assert!(reloaded.trusted_peers[0].allow_audio);
    assert!(!reloaded.trusted_peers[0].allow_terminal);
    assert!(reloaded.trusted_peers[0].allow_tunnel);
    assert!(reloaded.trusted_peers[0].allow_privacy_mode);

    // 4. Reconnect passwordless via Ed25519 PublicKey — must succeed WITHOUT re-prompting ApprovalHandler!
    let (chlg_pk, res_pk) =
        attempt_auth(&host, &host_id, &viewer_id, "Trusted-Laptop", AuthAttempt::PublicKey).await;
    assert!(
        chlg_pk.methods.contains(&(AuthMethod::PublicKey as i32)),
        "host must advertise PublicKey method to trusted peer"
    );
    assert!(res_pk.ok, "trusted peer must reconnect passwordless via PublicKey: {}", res_pk.reason);
    assert_eq!(
        approval_calls.load(Ordering::SeqCst),
        1,
        "trusted peer reconnect must bypass interactive ApprovalHandler"
    );

    // 5. Revoke trusted peer live on `HostAuthState` and in `Settings` -> immediate rejection!
    assert!(
        host.auth_state().remove_trusted_peer(&viewer_id.public_key()),
        "remove_trusted_peer must return true for existing trusted peer"
    );
    assert!(
        !host.auth_state().remove_trusted_peer(&viewer_id.public_key()),
        "second remove_trusted_peer must return false"
    );
    assert!(settings.remove_trusted_peer(&saved.public_key_hex));
    assert!(settings.trusted_peers.is_empty());

    let (chlg_revoked, res_revoked) =
        attempt_auth(&host, &host_id, &viewer_id, "Trusted-Laptop", AuthAttempt::PublicKey).await;
    assert!(
        !chlg_revoked.methods.contains(&(AuthMethod::PublicKey as i32)),
        "revoked peer must no longer be offered PublicKey auth"
    );
    assert!(!res_revoked.ok, "revoked peer PublicKey login must be rejected immediately");

    host.stop();
}

// ---------------------------------------------------------------------------
// Test 3: BUG #2 — Same-peer reconnect supersedes stale session on 1st try;
//         different peer receives clean Close("host busy")
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v030_bug2_same_peer_reconnect_supersedes_stale_session_and_other_peer_gets_busy() {
    let host_id = DeviceIdentity::generate();
    let peer_a = DeviceIdentity::generate();
    let peer_b = DeviceIdentity::generate();

    let perm_secret = "Reconnect-Test-2026!";
    let perm_hash = PasswordHash::hash_with(perm_secret, fast_argon2()).expect("hash perm");

    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 30,
        bitrate_kbps: 2_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: false,
        max_sessions: 1,
        ipc_port: None,
        auth: HostAuthConfig::with_permanent_password(perm_hash),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id.clone(), synth_backends(), None)
        .await
        .expect("RunningHost::start");

    // 1. Peer A establishes an active session and intentionally keeps it open (simulating crash/hang).
    let ch_a1: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(host.local_addr(), &peer_a, Some(host_id.public_key()))
            .await
            .expect("peer A connect 1"),
    );
    let (s1, r1) = ch_a1.open_bi(StreamKind::Control).await.expect("peer A open_bi 1");
    let (mut tx1, mut rx1) = framed(s1, r1);
    tx1.send(&Msg::Hello(make_hello(&peer_a, "PeerA-old")).into())
        .await
        .expect("peer A send Hello 1");
    let chlg1 = match timeout(Duration::from_secs(5), rx1.recv()).await.unwrap().unwrap().msg {
        Some(Msg::AuthChallenge(c)) => c,
        other => panic!("expected AuthChallenge, got {other:?}"),
    };
    let key1 = derive_password_key(
        perm_secret,
        &chlg1.salt,
        Argon2Params {
            m_cost_kib: chlg1.argon2_m_cost_kib,
            t_cost: chlg1.argon2_t_cost,
            p_cost: chlg1.argon2_p_cost,
        },
    )
    .unwrap();
    tx1.send(
        &Msg::AuthResponse(AuthResponse {
            method: AuthMethod::PermanentPassword as i32,
            proof: password_proof(&key1, &chlg1.nonce).to_vec(),
        })
        .into(),
    )
    .await
    .unwrap();
    match timeout(Duration::from_secs(5), rx1.recv()).await.unwrap().unwrap().msg {
        Some(Msg::AuthResult(r)) => assert!(r.ok, "peer A initial auth failed: {}", r.reason),
        other => panic!("expected AuthResult, got {other:?}"),
    }
    // Wait for SessionConfig so Peer A's session is fully active.
    timeout(Duration::from_secs(5), async {
        loop {
            if matches!(rx1.recv().await.unwrap().msg, Some(Msg::SessionConfig(_))) {
                break;
            }
        }
    })
    .await
    .expect("peer A SessionConfig timeout");
    assert_eq!(host.active_sessions(), 1);

    // 2. Different device (Peer B) connects while Peer A holds the single slot -> gets Close("host busy").
    let ch_b: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(host.local_addr(), &peer_b, Some(host_id.public_key()))
            .await
            .expect("peer B connect"),
    );
    let (sb, rb) = ch_b.open_bi(StreamKind::Control).await.expect("peer B open_bi");
    let (mut txb, mut rxb) = framed(sb, rb);
    let _ = txb.send(&Msg::Hello(make_hello(&peer_b, "PeerB")).into()).await;
    let busy_msg = timeout(Duration::from_secs(3), rxb.recv())
        .await
        .expect("peer B Close timeout")
        .expect("peer B must receive clean Close envelope");
    match busy_msg.msg {
        Some(Msg::Close(c)) => {
            assert!(
                c.reason.to_lowercase().contains("busy"),
                "expected 'host busy' reason for Peer B, got {:?}",
                c.reason
            );
        }
        other => panic!("expected Msg::Close for busy host, got {other:?}"),
    }
    ch_b.close(0, "done");

    // 3. Same device (Peer A) reconnects while stale ch_a1 is still open -> supersedes stale session on 1st attempt!
    let (_chlg_a2, res_a2) = attempt_auth(&host, &host_id, &peer_a, "PeerA-new", AuthAttempt::Permanent(perm_secret)).await;
    assert!(
        res_a2.ok,
        "same-peer reconnect must supersede stale session and succeed on 1st attempt: {}",
        res_a2.reason
    );

    ch_a1.close(0, "cleanup");
    host.stop();
}

// ---------------------------------------------------------------------------
// Test 4: Headless egui render (EN & RU) + 100% font glyphs + zero forbidden icons
// ---------------------------------------------------------------------------

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("tests crate must live under workspace root")
        .to_path_buf()
}

fn make_input(viewport: egui::Vec2, time: f64) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
        time: Some(time),
        ..Default::default()
    }
}

fn find_text_rect(shape: &egui::epaint::Shape, needle: &str) -> Option<egui::Rect> {
    match shape {
        egui::epaint::Shape::Vec(v) => v.iter().find_map(|s| find_text_rect(s, needle)),
        egui::epaint::Shape::Text(ts) => {
            if ts.galley.text().contains(needle) {
                Some(ts.galley.rect.translate(ts.pos.to_vec2()))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn shapes_contain_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
    shapes.iter().any(|c| find_text_rect(&c.shape, needle).is_some())
}

fn verify_text_shapes_have_glyphs(ctx: &egui::Context, shape: &egui::epaint::Shape) {
    match shape {
        egui::epaint::Shape::Vec(v) => {
            for s in v {
                verify_text_shapes_have_glyphs(ctx, s);
            }
        }
        egui::epaint::Shape::Text(ts) => {
            for section in &ts.galley.job.sections {
                let range = section.byte_range.start.0..section.byte_range.end.0;
                let slice = &ts.galley.job.text[range];
                let printable: String = slice.chars().filter(|c| !c.is_control()).collect();
                let ok = ctx.fonts_mut(|f| f.has_glyphs(&section.format.font_id, &printable));
                assert!(
                    ok,
                    "missing glyph in egui bundled font: {slice:?} (font {:?})",
                    section.format.font_id
                );
            }
        }
        _ => {}
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn strip_tests_and_comments(src: &str) -> String {
    let prod = src.split("\n#[cfg(test)]").next().unwrap_or(src);
    let mut out = String::with_capacity(prod.len());
    for line in prod.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
        let mut in_str = false;
        let mut escaped = false;
        let mut chars = line.char_indices().peekable();
        let mut cut = line.len();
        while let Some((idx, ch)) = chars.next() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_str = false;
                }
            } else if ch == '"' {
                in_str = true;
            } else if ch == '/' && chars.peek().is_some_and(|(_, next)| *next == '/') {
                cut = idx;
                break;
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

#[test]
fn v030_ui_settings_and_connect_screen_render_en_and_ru_with_full_glyph_and_icon_audit() {
    // 1. Backwards-compatibility with v0.2.0 settings JSON + PasswordMode serde.
    let v020_json = r#"{
        "language": "Ru",
        "recent": ["10.0.0.5:21118"],
        "rendezvous_server": "",
        "mode": "Desktop",
        "require_accept": true
    }"#;
    let parsed: Settings = serde_json::from_str(v020_json).expect("v0.2.0 settings.json must deserialize cleanly");
    assert_eq!(parsed.password_mode, PasswordMode::OneTime);
    assert!(parsed.permanent_password_hash.is_none());
    assert!(parsed.trusted_peers.is_empty());
    assert!(parsed.effective_otp_enabled());

    // 2. Zero forbidden missing-glyph Unicode icons across all crates/ui/src files.
    const FORBIDDEN: &[char] = &[
        '\u{25CF}', '\u{25CB}', '\u{2715}', '\u{2716}', '\u{2699}', '\u{1F512}', '\u{1F513}', '\u{1F4CB}',
        '\u{1F4C1}', '\u{1F5A5}', '\u{1F3AE}', '\u{1F5D1}', '\u{26A0}', '\u{2713}', '\u{2714}', '\u{2717}',
        '\u{1F504}', '\u{26A1}', '\u{2139}', '\u{1F50A}', '\u{1F507}', '\u{1F3A4}', '\u{25B6}', '\u{23F8}',
        '\u{23F9}', '\u{26F6}', '\u{2605}', '\u{2606}', '\u{25C0}', '\u{25B2}', '\u{25BC}', '\u{25BE}',
        '\u{25B4}', '\u{2B06}', '\u{2192}',
    ];
    let mut ui_files = Vec::new();
    collect_rs_files(&workspace_root().join("crates/ui/src"), &mut ui_files);
    for path in ui_files {
        let raw = std::fs::read_to_string(&path).expect("read ui src");
        let code = strip_tests_and_comments(&raw);
        for (idx, line) in code.lines().enumerate() {
            for &ch in FORBIDDEN {
                assert!(
                    !line.contains(ch),
                    "{}:{} contains forbidden missing-glyph U+{:04X}: {}",
                    path.display(),
                    idx + 1,
                    ch as u32,
                    line.trim()
                );
            }
        }
    }

    // 3. Headless egui render of Settings ("Host Access & Security" + Trusted devices) and
    //    ConnectScreen across all 3 PasswordModes in EN and RU with 100% glyph verification.
    let viewport = egui::vec2(1280.0, 800.0);
    let fake_phc = "$argon2id$v=19$m=1024,t=1,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNo".to_string();
    let sample_peer = SavedTrustedPeer {
        public_key_hex: "aa".repeat(32),
        device_id: "987654321".into(),
        name: "Gaming-Laptop".into(),
        fingerprint: "0123456789abcdef0123456789abcdef".into(),
        ..Default::default()
    };

    for lang in Language::ALL {
        let t = tr(lang);

        // Render Settings window in all 3 PasswordModes (with and without hash & trusted peers).
        for (mode_idx, (pw_mode, has_hash, with_peers)) in [
            (PasswordMode::OneTime, false, false),
            (PasswordMode::Permanent, false, false),
            (PasswordMode::Permanent, true, true),
            (PasswordMode::Both, true, true),
        ]
        .into_iter()
        .enumerate()
        {
            let ctx = egui::Context::default();
            let mut settings = Settings {
                language: lang,
                password_mode: pw_mode,
                permanent_password_hash: has_hash.then(|| fake_phc.clone()),
                trusted_peers: if with_peers { vec![sample_peer.clone()] } else { Vec::new() },
                ..Default::default()
            };
            let mut open = true;
            let mut last_shapes = Vec::new();
            let mut win_center = egui::pos2(300.0, 300.0);
            for step in 0..4 {
                let events = if step >= 2 {
                    vec![
                        egui::Event::PointerMoved(win_center),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -600.0),
                            modifiers: egui::Modifiers::NONE,
                            phase: egui::TouchPhase::Move,
                            source: egui::MouseWheelSource::Trackpad,
                        },
                    ]
                } else {
                    Vec::new()
                };
                let mut input = make_input(viewport, 0.05 * (mode_idx * 4 + step) as f64);
                input.events = events;
                let mut out = ctx.run_ui(input, |ui| {
                    let _ = settings_window_with_security(
                        ui.ctx(),
                        &mut open,
                        &mut settings,
                        None,
                        Some("v0.3.0 (dev)"),
                        Some(&UpdateUiState::Idle),
                        Some(t.pw_saved_notice),
                    );
                });
                out.textures_delta.clear();
                for clipped in &out.shapes {
                    verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                }
                if step == 1
                    && let Some(r) = out
                        .shapes
                        .iter()
                        .find_map(|c| find_text_rect(&c.shape, t.settings_quality))
                {
                    win_center = r.center();
                }
                last_shapes = out.shapes;
            }
            assert!(
                shapes_contain_text(&last_shapes, t.settings_security_heading),
                "Host Access & Security heading missing ({lang:?}, {pw_mode:?})"
            );
            assert!(
                shapes_contain_text(&last_shapes, t.settings_trusted_devices),
                "Trusted devices heading missing ({lang:?}, {pw_mode:?})"
            );
            if with_peers {
                assert!(
                    shapes_contain_text(&last_shapes, "Gaming-Laptop"),
                    "Trusted peer name missing ({lang:?}, {pw_mode:?})"
                );
            } else {
                assert!(
                    shapes_contain_text(&last_shapes, t.trusted_devices_empty),
                    "Empty trusted devices label missing ({lang:?}, {pw_mode:?})"
                );
            }
        }

        // Render ConnectScreen in OneTime, Permanent (active), and Both modes.
        for (pw_mode, has_hash, expected_text) in [
            (PasswordMode::OneTime, false, "ABCD1234"),
            (PasswordMode::Permanent, true, t.connect_permanent_active),
            (PasswordMode::Both, true, t.connect_plus_permanent_badge),
        ] {
            let ctx = egui::Context::default();
            let mut settings = Settings {
                language: lang,
                password_mode: pw_mode,
                permanent_password_hash: has_hash.then(|| fake_phc.clone()),
                ..Default::default()
            };
            let mut screen = ConnectScreen::new(&settings);
            screen.device_id = "123456789".into();
            screen.host_running = true;
            screen.host_otp = "ABCD1234".into();
            screen.show_otp = true;
            screen.build_display = "v0.3.0 (dev)".into();

            let mut out = ctx.run_ui(make_input(viewport, 0.02), |ui| {
                let _ = screen.show(ui, &mut settings, false);
            });
            out.textures_delta.clear();
            for clipped in &out.shapes {
                verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
            }
            assert!(
                shapes_contain_text(&out.shapes, expected_text),
                "ConnectScreen missing {expected_text:?} in {pw_mode:?} ({lang:?})"
            );
        }
    }
}
