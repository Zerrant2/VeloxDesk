//! Live bidirectional integration tests for `AndroidHostEngine` (v0.4.0):
//! 1. PC / generic viewer → `AndroidHostEngine` (OTP, Permanent, Both, AcceptPrompt + Trusted Peer +
//!    revoke, Annex-B video ingress, Opus audio ingress, portrait↔landscape rotation, InputEvent
//!    dispatch, ABR/Keyframe callbacks, bidirectional clipboard).
//! 2. Same-peer reconnect superseding vs different-peer clean `"host busy"`.
//! 3. Phone ↔ Phone (`session.rs` Android Viewer → `AndroidHostEngine`) over live direct QUIC.
//! 4. Phone ↔ Phone via 9-digit Device ID through real Rendezvous + Relay servers.

#![cfg(not(target_os = "android"))]

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use veloxdesk_core_traits::TransportChannel;
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{Argon2Params, derive_password_key, password_proof, sign_pubkey_challenge};
use veloxdesk_protocol::control::{
    AudioCodec, AuthMethod, AuthResponse, BitrateUpdate, Hello, InputEvent, KeyEvent, KeyframeRequest, MouseButton,
    MouseButtonEvent, MouseMove, MouseWheel, SessionConfig, SessionMode, TextInput, VideoCodec, VideoCodecCap,
    envelope::Msg, input_event::Event as InputEventKind,
};
use veloxdesk_protocol::{PROTOCOL_VERSION, StreamKind};
use veloxdesk_server_relay::{RelayConfig, RelayServer};
use veloxdesk_server_rendezvous::{RendezvousConfig, RendezvousServer};
use veloxdesk_transport::{
    AudioReceiver, ControlReceiver, ControlSender, VideoReceiver, connect, datagram_kind, framed,
};

use veloxdesk_android::SecretString;
use veloxdesk_android::constants;
use veloxdesk_android::host::{
    AndroidHostConfig, AndroidHostEngine, HostCallbacks, HostPasswordMode, SavedTrustedPeerJson, hash_password_phc,
    input_kind,
};
use veloxdesk_android::session::{ConnectConfig, HandleTable, SessionCallbacks, load_or_create_identity};
use veloxdesk_android::stats::HudMetrics;

// ───────────────────────── Test Helpers ─────────────────────────

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "veloxdesk-android-host-{tag}-{}-{}",
        std::process::id(),
        veloxdesk_core_traits::Timestamp::now().as_micros()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedInput {
    kind: i32,
    x: i32,
    y: i32,
    code: i32,
    down: bool,
    modifiers: i32,
    dx: i32,
    dy: i32,
    text: String,
}

#[derive(Debug, Clone)]
struct RecordedApproval {
    request_id: u64,
    peer_device_id: String,
    peer_name: String,
    peer_pub_key_hex: String,
}

#[derive(Default)]
struct HostRecorder {
    listening: Mutex<Option<(String, String, u16)>>,
    otp_history: Mutex<Vec<String>>,
    approvals: Mutex<Vec<RecordedApproval>>,
    trusted_added: Mutex<Vec<SavedTrustedPeerJson>>,
    sessions_started: Mutex<Vec<(String, String, u32, u32)>>,
    sessions_ended: Mutex<Vec<(String, String)>>,
    bitrates: Mutex<Vec<u32>>,
    keyframe_requests: AtomicUsize,
    inputs: Mutex<Vec<RecordedInput>>,
    clipboard: Mutex<Vec<String>>,
    errors: Mutex<Vec<String>>,
    stopped: AtomicBool,
}

impl HostCallbacks for HostRecorder {
    fn on_listening(&self, device_id: &str, otp: &str, port: u16) {
        *self.listening.lock().unwrap() = Some((device_id.to_string(), otp.to_string(), port));
    }

    fn on_otp_rotated(&self, otp: &str) {
        self.otp_history.lock().unwrap().push(otp.to_string());
    }

    fn on_approval_requested(
        &self,
        request_id: u64,
        peer_device_id: &str,
        peer_name: &str,
        _peer_fingerprint: &str,
        peer_pub_key_hex: &str,
    ) {
        self.approvals.lock().unwrap().push(RecordedApproval {
            request_id,
            peer_device_id: peer_device_id.to_string(),
            peer_name: peer_name.to_string(),
            peer_pub_key_hex: peer_pub_key_hex.to_string(),
        });
    }

    fn on_trusted_peer_added(&self, peer_json: &str) {
        let parsed: SavedTrustedPeerJson = serde_json::from_str(peer_json).expect("valid trusted peer json");
        self.trusted_added.lock().unwrap().push(parsed);
    }

    fn on_session_started(&self, peer_device_id: &str, peer_name: &str, requested_bitrate_kbps: u32, requested_fps: u32) {
        self.sessions_started.lock().unwrap().push((
            peer_device_id.to_string(),
            peer_name.to_string(),
            requested_bitrate_kbps,
            requested_fps,
        ));
    }

    fn on_session_ended(&self, peer_device_id: &str, reason: &str) {
        self.sessions_ended.lock().unwrap().push((peer_device_id.to_string(), reason.to_string()));
    }

    fn on_bitrate_changed(&self, bitrate_kbps: u32) {
        self.bitrates.lock().unwrap().push(bitrate_kbps);
    }

    fn on_keyframe_requested(&self) {
        self.keyframe_requests.fetch_add(1, Ordering::Relaxed);
    }

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
    ) {
        self.inputs.lock().unwrap().push(RecordedInput {
            kind,
            x,
            y,
            code,
            down,
            modifiers,
            dx,
            dy,
            text: text.to_string(),
        });
    }

    fn on_clipboard_text(&self, text: &str) {
        self.clipboard.lock().unwrap().push(text.to_string());
    }

    fn on_error(&self, message: &str) {
        self.errors.lock().unwrap().push(message.to_string());
    }

    fn on_stopped(&self) {
        self.stopped.store(true, Ordering::Release);
    }
}

fn wait_until<F: Fn() -> bool>(timeout: Duration, cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    cond()
}

fn make_hello(id: &DeviceIdentity, name: &str) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        device_id: id.device_id(),
        public_key: id.public_key().to_vec(),
        device_name: name.to_string(),
        os: "windows".to_string(),
        app_version: "0.4.0".to_string(),
        mode: SessionMode::Desktop as i32,
        video_caps: vec![VideoCodecCap {
            codec: VideoCodec::H264 as i32,
            hw_decode: true,
            max_width: 3840,
            max_height: 2160,
            max_fps: 60,
            yuv444: false,
            ten_bit: false,
        }],
        audio_codecs: vec![AudioCodec::Opus as i32],
        viewer_width: 1920,
        viewer_height: 1080,
        viewer_refresh_hz: 60,
        max_bitrate_kbps: 8_000,
    }
}

async fn pc_viewer_connect_and_auth(
    addr: SocketAddr,
    viewer_id: &DeviceIdentity,
    name: &str,
    password: Option<&str>,
    prefer_otp_method: bool,
) -> anyhow::Result<(
    Arc<dyn TransportChannel>,
    ControlSender,
    ControlReceiver,
    Vec<i32>,
    SessionConfig,
)> {
    let ch: Arc<dyn TransportChannel> = Arc::new(connect(addr, viewer_id, None).await?);
    let (send, recv) = ch.open_bi(StreamKind::Control).await?;
    let (mut tx, mut rx) = framed(send, recv);
    tx.send(&Msg::Hello(make_hello(viewer_id, name)).into()).await?;

    let challenge = match rx.recv().await?.msg {
        Some(Msg::AuthChallenge(c)) => c,
        Some(Msg::Close(c)) => anyhow::bail!("host closed before AuthChallenge: {} ({})", c.reason, c.code),
        other => anyhow::bail!("expected AuthChallenge, got {other:?}"),
    };
    let offered = challenge.methods.clone();

    let resp = if offered.contains(&(AuthMethod::PublicKey as i32)) && password.is_none() {
        let host_pk: [u8; 32] = challenge.host_public_key.as_slice().try_into()?;
        let sig = sign_pubkey_challenge(viewer_id, &challenge.nonce, &host_pk);
        AuthResponse { method: AuthMethod::PublicKey as i32, proof: sig.to_vec() }
    } else if let Some(pw) = password {
        let params = Argon2Params {
            m_cost_kib: challenge.argon2_m_cost_kib,
            t_cost: challenge.argon2_t_cost,
            p_cost: challenge.argon2_p_cost,
        };
        let key = derive_password_key(pw, &challenge.salt, params)?;
        let proof = password_proof(&key, &challenge.nonce);
        let method = if prefer_otp_method && offered.contains(&(AuthMethod::OneTimePassword as i32)) {
            AuthMethod::OneTimePassword
        } else if offered.contains(&(AuthMethod::PermanentPassword as i32)) {
            AuthMethod::PermanentPassword
        } else {
            AuthMethod::OneTimePassword
        };
        AuthResponse { method: method as i32, proof: proof.to_vec() }
    } else {
        anyhow::bail!("no password supplied and PublicKey not offered: {offered:?}");
    };
    tx.send(&Msg::AuthResponse(resp).into()).await?;

    match rx.recv().await?.msg {
        Some(Msg::AuthResult(r)) if r.ok => {}
        Some(Msg::AuthResult(r)) => anyhow::bail!("auth rejected: {}", r.reason),
        Some(Msg::Close(c)) => anyhow::bail!("host closed: {}", c.reason),
        other => anyhow::bail!("expected AuthResult, got {other:?}"),
    }

    let cfg = match rx.recv().await?.msg {
        Some(Msg::SessionConfig(c)) => c,
        other => anyhow::bail!("expected SessionConfig, got {other:?}"),
    };
    Ok((ch, tx, rx, offered, cfg))
}

// ───────────────────────── Test 1: PC Viewer → AndroidHostEngine ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pc_viewer_to_android_host_auth_modes_video_rotation_input_and_clipboard() {
    let host_dir = temp_dir("pc-to-phone");
    let perm_pw = "AndroidPerm#2026";
    let perm_phc = hash_password_phc(perm_pw).expect("hash perm");
    let rec = Arc::new(HostRecorder::default());

    let host = AndroidHostEngine::start(
        AndroidHostConfig {
            files_dir: host_dir.clone(),
            bind_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
            rendezvous_addr: String::new(),
            device_name: "Pixel-Test".into(),
            width: 1080,
            height: 2400,
            fps: 60,
            password_mode: HostPasswordMode::OneTime,
            // Supply a saved permanent PHC even in OneTime mode to prove BLOCKER #1 invariant holds.
            permanent_hash_phc: perm_phc.clone(),
            require_accept: false,
            trusted_peers_json: String::new(),
        },
        rec.clone(),
    )
    .expect("start AndroidHostEngine");

    assert!(wait_until(Duration::from_secs(5), || rec.listening.lock().unwrap().is_some()));
    let (_host_id, otp1, port) = rec.listening.lock().unwrap().clone().unwrap();
    assert_eq!(otp1.len(), 8);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    let pc_id = DeviceIdentity::generate();

    // 1. In OneTime mode, permanent password MUST be rejected (and AuthMethod::None NEVER offered).
    let res = pc_viewer_connect_and_auth(addr, &pc_id, "Win11-PC", Some(perm_pw), false).await;
    assert!(res.is_err(), "permanent password must be rejected in OneTime mode");

    // 2. OTP succeeds, rotates OTP immediately, and streams video/audio/rotation/input/clipboard!
    let (ch, mut tx, mut rx, offered, cfg) = pc_viewer_connect_and_auth(addr, &pc_id, "Win11-PC", Some(&otp1), true)
        .await
        .expect("OTP login");
    assert_eq!(offered, vec![AuthMethod::OneTimePassword as i32]);
    assert_eq!(cfg.host_os, "android");
    assert_eq!(cfg.host_name, "Pixel-Test");
    assert_eq!(cfg.video.as_ref().unwrap().width, 1080);
    assert_eq!(cfg.video.as_ref().unwrap().height, 2400);
    assert!(wait_until(Duration::from_secs(2), || !rec.otp_history.lock().unwrap().is_empty()));
    let otp2 = rec.otp_history.lock().unwrap().last().cloned().unwrap();
    assert_ne!(otp1, otp2);

    // Push a synthetic Annex-B H.264 IDR frame and verify intact FEC reassembly on the PC viewer.
    let annex_b = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xE0, 0x1F, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0xAA, 0xBB];
    let mut vrx = VideoReceiver::new();
    let ch_vid = ch.clone();
    let vid_task = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(dgram)) = tokio::time::timeout(Duration::from_millis(200), ch_vid.recv_datagram()).await {
                if let Some(veloxdesk_protocol::media::MediaKind::Video) = datagram_kind(&dgram) {
                    if let Some(rf) = vrx.push_datagram(&dgram, veloxdesk_core_traits::Timestamp::now()) {
                        return Some(rf.packet.data.to_vec());
                    }
                }
            }
        }
        None
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    host.push_video_frame(annex_b.clone(), true, 123_456, 1080, 2400);
    let received_frame = vid_task.await.unwrap().expect("PC viewer must receive pushed Annex-B video frame");
    assert_eq!(received_frame, annex_b);

    // Push 48kHz stereo PCM audio (20ms = 960 frames = 1920 samples) and verify Audio datagrams arrive.
    let ch_aud = ch.clone();
    let aud_task = tokio::spawn(async move {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if let Ok(Ok(dgram)) = tokio::time::timeout(Duration::from_millis(200), ch_aud.recv_datagram()).await {
                if let Some((0, pkt)) = AudioReceiver::parse_datagram(&dgram) {
                    return Some(pkt);
                }
            }
        }
        None
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let pcm: Vec<i16> = (0..1920).map(|i| ((i as f32 * 0.1).sin() * 12000.0) as i16).collect();
    host.push_audio_pcm(&pcm, 48_000, 2);
    let aud_pkt = aud_task.await.unwrap().expect("PC viewer must receive Opus audio datagram");
    assert!(!aud_pkt.data.is_empty());

    // Simulate portrait -> landscape phone rotation via update_display and verify DisplayList + SessionConfig.
    host.update_display(2400, 1080, 60);
    let mut got_dl = false;
    let mut got_rotated_cfg = false;
    let rot_deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while (!got_dl || !got_rotated_cfg) && tokio::time::Instant::now() < rot_deadline {
        if let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await {
            match env.msg {
                Some(Msg::DisplayList(dl)) if dl.displays.first().is_some_and(|d| d.width == 2400 && d.height == 1080) => {
                    got_dl = true;
                }
                Some(Msg::SessionConfig(sc))
                    if sc.host_os == "android"
                        && sc.video.as_ref().is_some_and(|v| v.width == 2400 && v.height == 1080) =>
                {
                    got_rotated_cfg = true;
                }
                _ => {}
            }
        }
    }
    assert!(got_dl && got_rotated_cfg, "rotation must emit DisplayList and SessionConfig (2400x1080)");

    // Test KeyframeRequest & BitrateUpdate callbacks.
    tx.send(&Msg::KeyframeRequest(KeyframeRequest { stream_id: 0, last_good_frame_id: 1 }).into())
        .await
        .unwrap();
    tx.send(&Msg::BitrateUpdate(BitrateUpdate { stream_id: 0, bitrate_kbps: 12_000, fps: 60, fec_parity_percent: 15 }).into())
        .await
        .unwrap();
    assert!(wait_until(Duration::from_secs(2), || rec.keyframe_requests.load(Ordering::Relaxed) >= 1));
    assert!(wait_until(Duration::from_secs(2), || rec.bitrates.lock().unwrap().contains(&12_000)));

    // Open StreamKind::Input and verify MouseMove, MouseButton (1=left, 2=right), Wheel, Key, Text mapping.
    let inp_send = ch.open_uni(StreamKind::Input).await.expect("open input stream");
    let mut inp_tx = ControlSender::new(inp_send);
    let events = vec![
        InputEvent { ts_us: 1, event: Some(InputEventKind::MouseMove(MouseMove { x: 420, y: 880, display: 0 })) },
        InputEvent { ts_us: 2, event: Some(InputEventKind::MouseButton(MouseButtonEvent { button: MouseButton::Left as i32, down: true })) },
        InputEvent { ts_us: 3, event: Some(InputEventKind::MouseButton(MouseButtonEvent { button: MouseButton::Right as i32, down: true })) },
        InputEvent { ts_us: 4, event: Some(InputEventKind::MouseWheel(MouseWheel { dx: 0, dy: -120 })) },
        InputEvent { ts_us: 5, event: Some(InputEventKind::Key(KeyEvent { hid_usage: 0x29, down: true, modifiers: 0 })) },
        InputEvent { ts_us: 6, event: Some(InputEventKind::Text(TextInput { text: "hello".into() })) },
    ];
    for ev in events {
        inp_tx.send(&Msg::Input(ev).into()).await.unwrap();
    }
    let _ = inp_tx.finish().await;
    assert!(wait_until(Duration::from_secs(2), || rec.inputs.lock().unwrap().len() >= 6));
    let got_inputs = rec.inputs.lock().unwrap().clone();
    assert_eq!(got_inputs[0], RecordedInput { kind: input_kind::MOUSE_MOVE, x: 420, y: 880, code: 0, down: false, modifiers: 0, dx: 0, dy: 0, text: String::new() });
    assert_eq!(got_inputs[1], RecordedInput { kind: input_kind::MOUSE_BUTTON, x: 0, y: 0, code: 1, down: true, modifiers: 0, dx: 0, dy: 0, text: String::new() });
    assert_eq!(got_inputs[2], RecordedInput { kind: input_kind::MOUSE_BUTTON, x: 0, y: 0, code: 2, down: true, modifiers: 0, dx: 0, dy: 0, text: String::new() });
    assert_eq!(got_inputs[3], RecordedInput { kind: input_kind::MOUSE_WHEEL, x: 0, y: 0, code: 0, down: false, modifiers: 0, dx: 0, dy: -120, text: String::new() });
    assert_eq!(got_inputs[4], RecordedInput { kind: input_kind::KEY, x: 0, y: 0, code: 0x29, down: true, modifiers: 0, dx: 0, dy: 0, text: String::new() });
    assert_eq!(got_inputs[5], RecordedInput { kind: input_kind::TEXT, x: 0, y: 0, code: 0, down: false, modifiers: 0, dx: 0, dy: 0, text: "hello".into() });

    // Open StreamKind::Clipboard and test bidirectional text sync.
    let (cs, cr) = ch.open_bi(StreamKind::Clipboard).await.expect("open clipboard stream");
    let (mut clip_tx, mut clip_rx) = framed(cs, cr);
    clip_tx
        .send(&Msg::Clipboard(veloxdesk_android::clipboard::encode_clipboard_text("from-pc").unwrap()).into())
        .await
        .unwrap();
    assert!(wait_until(Duration::from_secs(2), || rec.clipboard.lock().unwrap().iter().any(|s| s == "from-pc")));
    host.send_clipboard_text("from-phone".into());
    let clip_env = tokio::time::timeout(Duration::from_secs(2), clip_rx.recv()).await.unwrap().unwrap();
    let Some(Msg::Clipboard(wire)) = clip_env.msg else { panic!("expected Clipboard") };
    assert_eq!(veloxdesk_android::clipboard::decode_clipboard_text(&wire).unwrap().as_deref(), Some("from-phone"));

    // Close session 1 cleanly.
    ch.close(0, "done");
    assert!(wait_until(Duration::from_secs(3), || !rec.sessions_ended.lock().unwrap().is_empty()));

    // 3. Switch to Permanent mode with require_accept = true -> Approve & Remember -> PublicKey bypass -> Revoke!
    host.update_auth(HostPasswordMode::Permanent, &perm_phc, true);
    assert!(wait_until(Duration::from_secs(2), || rec.otp_history.lock().unwrap().last().is_some_and(|s| s.is_empty())));

    let pc_id_c = pc_id.clone();
    let connect_fut = tokio::spawn(async move {
        pc_viewer_connect_and_auth(addr, &pc_id_c, "Win11-Trusted", Some(perm_pw), false).await
    });
    assert!(wait_until(Duration::from_secs(5), || !rec.approvals.lock().unwrap().is_empty()));
    let req = rec.approvals.lock().unwrap()[0].clone();
    assert_eq!(req.peer_device_id, pc_id.device_id());
    assert_eq!(req.peer_name, "Win11-Trusted");
    assert_eq!(req.peer_pub_key_hex.len(), 64);
    assert!(host.respond_approval(req.request_id, true, true, true, true, true));

    let (ch2, _tx2, _rx2, offered2, _) = connect_fut.await.unwrap().expect("Permanent + Accept login");
    assert_eq!(offered2, vec![AuthMethod::PermanentPassword as i32]);
    assert!(wait_until(Duration::from_secs(2), || !rec.trusted_added.lock().unwrap().is_empty()));
    let saved = rec.trusted_added.lock().unwrap()[0].clone();
    assert_eq!(saved.device_id, pc_id.device_id());
    ch2.close(0, "done");
    assert!(wait_until(Duration::from_secs(3), || rec.sessions_ended.lock().unwrap().len() >= 2));

    // Reconnect with NO password -> trusted PublicKey bypasses password AND AcceptPrompt!
    let approvals_before = rec.approvals.lock().unwrap().len();
    let (ch3, _tx3, _rx3, offered3, _) = pc_viewer_connect_and_auth(addr, &pc_id, "Win11-Trusted", None, false)
        .await
        .expect("trusted PublicKey reconnect");
    assert!(offered3.contains(&(AuthMethod::PublicKey as i32)));
    assert_eq!(rec.approvals.lock().unwrap().len(), approvals_before, "trusted peer must bypass AcceptPrompt");
    ch3.close(0, "done");
    assert!(wait_until(Duration::from_secs(3), || rec.sessions_ended.lock().unwrap().len() >= 3));

    // Revoke trusted peer live -> PublicKey no longer offered!
    assert!(host.remove_trusted_peer(&saved.public_key_hex));
    let revoked_res = pc_viewer_connect_and_auth(addr, &pc_id, "Win11-Trusted", None, false).await;
    assert!(revoked_res.is_err(), "revoked peer must not be offered PublicKey");

    // 4. Switch to Both mode -> OTP works cleanly alongside Permanent password.
    host.update_auth(HostPasswordMode::Both, &perm_phc, false);
    assert!(wait_until(Duration::from_secs(2), || rec.otp_history.lock().unwrap().last().is_some_and(|s| s.len() == 8)));
    let both_otp = rec.otp_history.lock().unwrap().last().cloned().unwrap();
    let (ch4, _tx4, _rx4, offered4, _) = pc_viewer_connect_and_auth(addr, &pc_id, "Win11-Both", Some(&both_otp), false)
        .await
        .expect("OTP accepted in Both mode even via PermanentPassword prompt method");
    assert!(offered4.contains(&(AuthMethod::PermanentPassword as i32)));
    assert!(offered4.contains(&(AuthMethod::OneTimePassword as i32)));
    ch4.close(0, "done");

    host.stop_and_join();
    assert!(rec.stopped.load(Ordering::Acquire));
    let _ = std::fs::remove_dir_all(&host_dir);
}

// ───────────────────────── Test 2: Same-Peer Supersede & Host Busy ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn android_host_supersedes_same_peer_and_rejects_other_peer_busy() {
    let host_dir = temp_dir("supersede");
    let perm_pw = "Supersede#99";
    let perm_phc = hash_password_phc(perm_pw).unwrap();
    let rec = Arc::new(HostRecorder::default());

    let host = AndroidHostEngine::start(
        AndroidHostConfig {
            files_dir: host_dir.clone(),
            bind_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
            rendezvous_addr: String::new(),
            device_name: "Pixel-Busy".into(),
            width: 720,
            height: 1600,
            fps: 30,
            password_mode: HostPasswordMode::Permanent,
            permanent_hash_phc: perm_phc,
            require_accept: false,
            trusted_peers_json: String::new(),
        },
        rec.clone(),
    )
    .unwrap();

    assert!(wait_until(Duration::from_secs(5), || rec.listening.lock().unwrap().is_some()));
    let (_, _, port) = rec.listening.lock().unwrap().clone().unwrap();
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));

    let peer_a = DeviceIdentity::generate();
    let peer_b = DeviceIdentity::generate();

    // Peer A connects and holds the active session.
    let (_ch_a1, _tx_a1, _rx_a1, _, _) = pc_viewer_connect_and_auth(addr, &peer_a, "Peer-A", Some(perm_pw), false)
        .await
        .expect("Peer A session 1");

    // Different Peer B MUST receive clean Msg::Close("host busy").
    let ch_b: Arc<dyn TransportChannel> = Arc::new(connect(addr, &peer_b, None).await.unwrap());
    let (bs, br) = ch_b.open_bi(StreamKind::Control).await.unwrap();
    let (mut b_tx, mut b_rx) = framed(bs, br);
    let _ = b_tx.send(&Msg::Hello(make_hello(&peer_b, "Peer-B")).into()).await;
    let busy_msg = tokio::time::timeout(Duration::from_secs(2), b_rx.recv()).await.unwrap().unwrap();
    match busy_msg.msg {
        Some(Msg::Close(c)) => assert_eq!(c.reason, "host busy"),
        other => panic!("expected Close('host busy') for Peer B, got {other:?}"),
    }

    // Same Peer A reconnects WITHOUT closing session 1 -> supersedes immediately on 1st attempt!
    let (ch_a2, _tx_a2, _rx_a2, _, cfg_a2) = pc_viewer_connect_and_auth(addr, &peer_a, "Peer-A-Reconn", Some(perm_pw), false)
        .await
        .expect("Peer A reconnect must supersede stale session on 1st try");
    assert_eq!(cfg_a2.host_os, "android");
    ch_a2.close(0, "done");

    host.stop_and_join();
    let _ = std::fs::remove_dir_all(&host_dir);
}

// ───────────────────────── Test 3: Phone ↔ Phone (Direct QUIC) ─────────────────────────

#[derive(Default)]
struct ViewerRecorder {
    auth_required: Mutex<Option<(String, bool, bool)>>,
    configs: Mutex<Vec<SessionConfig>>,
    display_lists: Mutex<Vec<String>>,
    video_frames: Mutex<Vec<(u32, u32, Vec<u8>)>>,
    clipboard: Mutex<Vec<String>>,
    closed: Mutex<Option<(String, bool)>>,
}

impl SessionCallbacks for ViewerRecorder {
    fn on_status(&self, _code: i32, _detail: &str) {}
    fn on_auth_required(&self, host_device_id: &str, _fp: &str, allow_perm: bool, allow_otp: bool) {
        *self.auth_required.lock().unwrap() = Some((host_device_id.to_string(), allow_perm, allow_otp));
    }
    fn on_session_config(&self, cfg: &SessionConfig, _displays_json: &str) {
        self.configs.lock().unwrap().push(cfg.clone());
    }
    fn on_display_list(&self, displays_json: &str) {
        self.display_lists.lock().unwrap().push(displays_json.to_string());
    }
    fn on_video_frame(
        &self,
        _codec: i32,
        width: u32,
        height: u32,
        _frame_id: u32,
        _capture_ts_us: i64,
        _keyframe: bool,
        _discontinuity: bool,
        data: &[u8],
    ) {
        self.video_frames.lock().unwrap().push((width, height, data.to_vec()));
    }
    fn on_audio_pcm(&self, _sample_rate: i32, _channels: i32, _pcm16: &[i16]) {}
    fn on_stats(&self, _stats: &HudMetrics) {}
    fn on_clipboard_text(&self, text: &str) {
        self.clipboard.lock().unwrap().push(text.to_string());
    }
    fn on_rumble(&self, _pad_id: i32, _low_freq: i32, _high_freq: i32) {}
    fn on_closed(&self, reason: &str, is_error: bool) {
        *self.closed.lock().unwrap() = Some((reason.to_string(), is_error));
    }
}

#[test]
fn phone_to_phone_live_quic_loopback() {
    let host_dir = temp_dir("p2p-host");
    let viewer_dir = temp_dir("p2p-viewer");
    let host_rec = Arc::new(HostRecorder::default());
    let viewer_rec = Arc::new(ViewerRecorder::default());

    let host = AndroidHostEngine::start(
        AndroidHostConfig {
            files_dir: host_dir.clone(),
            bind_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
            rendezvous_addr: String::new(),
            device_name: "Phone-Host".into(),
            width: 1080,
            height: 2340,
            fps: 60,
            password_mode: HostPasswordMode::OneTime,
            permanent_hash_phc: String::new(),
            require_accept: true,
            trusted_peers_json: String::new(),
        },
        host_rec.clone(),
    )
    .expect("start phone host");

    assert!(wait_until(Duration::from_secs(5), || host_rec.listening.lock().unwrap().is_some()));
    let (host_id, otp, port) = host_rec.listening.lock().unwrap().clone().unwrap();

    let viewer_table = Arc::new(HandleTable::new());
    let sess_id = viewer_table
        .spawn_session(
            ConnectConfig {
                files_dir: viewer_dir.clone(),
                target: format!("127.0.0.1:{port}"),
                rendezvous_addr: String::new(),
                relay_only: false,
                password: None,
                mode: constants::MODE_DESKTOP,
                max_bitrate_kbps: 6_000,
                audio_enabled: true,
                clipboard_enabled: true,
                viewer_width: 1080,
                viewer_height: 2400,
                viewer_refresh_hz: 60,
                device_name: "Phone-Viewer".into(),
                codec_caps_mask: constants::CAP_H264_HW,
            },
            viewer_rec.clone(),
        )
        .expect("spawn phone viewer");
    let viewer = viewer_table.get(sess_id).unwrap();

    // Viewer gets onAuthRequired(allow_perm=false, allow_otp=true) and submits OTP.
    assert!(wait_until(Duration::from_secs(5), || viewer_rec.auth_required.lock().unwrap().is_some()));
    let (got_host_id, allow_perm, allow_otp) = viewer_rec.auth_required.lock().unwrap().clone().unwrap();
    assert_eq!(got_host_id, host_id);
    assert!(!allow_perm);
    assert!(allow_otp);
    viewer.submit_password(SecretString::from(otp));

    // Host gets onHostApprovalRequested and accepts.
    assert!(wait_until(Duration::from_secs(5), || !host_rec.approvals.lock().unwrap().is_empty()));
    let req = host_rec.approvals.lock().unwrap()[0].clone();
    assert!(host.respond_approval(req.request_id, true, false, true, true, true));

    // Viewer receives SessionConfig with host_os == "android".
    assert!(wait_until(Duration::from_secs(5), || !viewer_rec.configs.lock().unwrap().is_empty()));
    assert_eq!(viewer_rec.configs.lock().unwrap()[0].host_os, "android");

    // Push video frame from Phone Host -> verify Phone Viewer receives exact Annex-B bytes!
    let idr = vec![0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0x00, 0x00, 0x00, 0x01, 0x65, 0x11, 0x22, 0x33];
    host.push_video_frame(idr.clone(), true, 42_000, 1080, 2340);
    assert!(wait_until(Duration::from_secs(3), || !viewer_rec.video_frames.lock().unwrap().is_empty()));
    assert_eq!(viewer_rec.video_frames.lock().unwrap()[0].2, idr);

    // Rotate host display -> Phone Viewer receives updated DisplayList & SessionConfig.
    host.update_display(2340, 1080, 60);
    assert!(wait_until(Duration::from_secs(3), || viewer_rec.configs.lock().unwrap().len() >= 2));
    assert_eq!(viewer_rec.configs.lock().unwrap().last().unwrap().video.as_ref().unwrap().width, 2340);

    // Phone Viewer sends mouse/key/text -> Phone Host receives onHostInputEvent.
    viewer.send_mouse_move(123, 456, 0);
    viewer.send_mouse_button(MouseButton::Left as i32, true);
    viewer.send_key(0x29, true, 0);
    viewer.send_text("hi-phone".into());
    assert!(wait_until(Duration::from_secs(3), || host_rec.inputs.lock().unwrap().len() >= 4));
    assert!(host_rec.inputs.lock().unwrap().iter().any(|e| e.kind == input_kind::TEXT && e.text == "hi-phone"));

    // Bidirectional clipboard Phone Viewer <-> Phone Host.
    viewer.send_clipboard_text("viewer-clip".into());
    assert!(wait_until(Duration::from_secs(3), || host_rec.clipboard.lock().unwrap().iter().any(|s| s == "viewer-clip")));
    host.send_clipboard_text("host-clip".into());
    assert!(wait_until(Duration::from_secs(3), || viewer_rec.clipboard.lock().unwrap().iter().any(|s| s == "host-clip")));

    viewer.disconnect();
    assert!(wait_until(Duration::from_secs(3), || viewer_rec.closed.lock().unwrap().is_some()));
    host.stop_and_join();
    let _ = std::fs::remove_dir_all(&host_dir);
    let _ = std::fs::remove_dir_all(&viewer_dir);
}

// ───────────────────────── Test 4: Phone ↔ Phone via 9-Digit ID (Rendezvous + Relay) ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phone_to_phone_via_rendezvous_device_id() {
    let host_dir = temp_dir("rdv-host");
    let viewer_dir = temp_dir("rdv-viewer");

    // Start real Rendezvous + Relay servers on ephemeral loopback ports.
    let relay_srv = RelayServer::bind(RelayConfig {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        ..Default::default()
    })
    .await
    .expect("bind relay");
    let relay_addr = relay_srv.local_addr();
    let rdv_srv = RendezvousServer::bind(RendezvousConfig {
        bind: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        relay_addr: Some(relay_addr),
        ..Default::default()
    })
    .await
    .expect("bind rendezvous");
    let rdv_addr = rdv_srv.local_addr();

    let relay_task = tokio::spawn(async move {
        let _ = relay_srv.run().await;
    });
    let rdv_task = tokio::spawn(async move {
        let _ = rdv_srv.run().await;
    });

    let expected_host_id = load_or_create_identity(&host_dir).unwrap().device_id();
    let host_rec = Arc::new(HostRecorder::default());
    let viewer_rec = Arc::new(ViewerRecorder::default());

    let host = AndroidHostEngine::start(
        AndroidHostConfig {
            files_dir: host_dir.clone(),
            bind_addr: Some(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))),
            rendezvous_addr: rdv_addr.to_string(),
            device_name: "Phone-Rdv-Host".into(),
            width: 1080,
            height: 1920,
            fps: 30,
            password_mode: HostPasswordMode::OneTime,
            permanent_hash_phc: String::new(),
            require_accept: false,
            trusted_peers_json: String::new(),
        },
        host_rec.clone(),
    )
    .expect("start host with rendezvous");

    assert!(wait_until(Duration::from_secs(5), || host_rec.listening.lock().unwrap().is_some()));
    let (host_id, otp, _port) = host_rec.listening.lock().unwrap().clone().unwrap();
    assert_eq!(host_id, expected_host_id);

    let viewer_table = Arc::new(HandleTable::new());
    let sess_id = viewer_table
        .spawn_session(
            ConnectConfig {
                files_dir: viewer_dir.clone(),
                target: host_id,
                rendezvous_addr: rdv_addr.to_string(),
                relay_only: true,
                password: Some(SecretString::from(otp)),
                mode: constants::MODE_DESKTOP,
                max_bitrate_kbps: 4_000,
                audio_enabled: false,
                clipboard_enabled: false,
                viewer_width: 1080,
                viewer_height: 1920,
                viewer_refresh_hz: 60,
                device_name: "Phone-Rdv-Viewer".into(),
                codec_caps_mask: constants::CAP_H264_HW,
            },
            viewer_rec.clone(),
        )
        .expect("spawn viewer by 9-digit ID");
    let viewer = viewer_table.get(sess_id).unwrap();

    assert!(
        wait_until(Duration::from_secs(8), || !viewer_rec.configs.lock().unwrap().is_empty()),
        "Phone viewer failed to connect to Phone host via 9-digit Device ID; closed={:?}",
        viewer_rec.closed.lock().unwrap()
    );
    assert_eq!(viewer_rec.configs.lock().unwrap()[0].host_os, "android");

    let sample = vec![0x00, 0x00, 0x00, 0x01, 0x65, 0xDE, 0xAD, 0xBE, 0xEF];
    host.push_video_frame(sample.clone(), true, 99_000, 1080, 1920);
    assert!(wait_until(Duration::from_secs(4), || !viewer_rec.video_frames.lock().unwrap().is_empty()));
    assert_eq!(viewer_rec.video_frames.lock().unwrap()[0].2, sample);

    viewer.disconnect();
    host.stop_and_join();
    relay_task.abort();
    rdv_task.abort();
    let _ = std::fs::remove_dir_all(&host_dir);
    let _ = std::fs::remove_dir_all(&viewer_dir);
}
