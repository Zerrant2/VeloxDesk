//! VeloxDesk v0.5.0 Integration Tests:
//! - Bidirectional in-session chat (`ChatMessage` + `ChatAck`, `HostStatusEvent::ChatReceived` /
//!   `ChatAcked`, `RunningHost::send_chat`, and `allow_chat = false` permission denial).
//! - Zero re-encode Matroska (`.mkv`) session recording (`SessionRecorder` leading P-frame
//!   keyframe gating, H.264 Annex-B + Opus muxing, `validate_mkv_file` structural checks).
//! - Remote host power management + `allow_power` permission gating (`SystemPowerRequest` /
//!   `SystemPowerResult`).
//! - `PrivacyGuard` RAII teardown on normal drop, explicit disable, panic unwind, and abrupt
//!   session disconnect.
//! - Pure-Rust NLMS Acoustic Echo Canceller (`AcousticEchoCanceller` + `AecReferenceFeed`)
//!   ERLE >= 12 dB attenuation and double-talk near-end speech preservation.
//!
//! NOTE: QUIC loopback tests require `BypassSandbox: true` on Windows because the agent sandbox
//! drops loopback UDP packets. Non-network tests (recorder, PrivacyGuard RAII, NLMS AEC) run
//! inside the sandbox cleanly.

use std::f32::consts::TAU;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::time::timeout;

use veloxdesk_audio::{AcousticEchoCanceller, AecConfig, AecReferenceFeed};
use veloxdesk_core_traits::audio::{AudioCodecId, EncodedAudio};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, Display, DisplayId, EncodedPacket, Frame,
    FrameCapture, FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_core_traits::{Result, Timestamp};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{Argon2Params, derive_password_key, password_proof};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthMethod, AuthResponse, AuthResult, ChatAck, ChatMessage, Close, Hello, InputEvent, Permissions, Ping,
    PrivacyMode, SessionMode, SystemPowerAction, SystemPowerRequest, SystemPowerResult, VideoCodecCap, envelope::Msg,
};
use veloxdesk_recorder::{
    RecorderConfig, SessionRecorder, default_recordings_dir, generate_recording_path, validate_mkv_file,
};
use veloxdesk_service::{
    ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostBackends, HostConfig, HostStatusEvent,
    PowerExecutor, RecordedPowerAction, RunningHost, SessionHooks, SimulatedPowerExecutor, TransportAbr,
    TransportVideoSink, run_session_with_hooks,
};
use veloxdesk_transport::{AbrController, ControlReceiver, ControlSender, VideoSender, framed};
use veloxdesk_virtual_display::{PrivacyController, PrivacyGuard, SimulatedPrivacyController};

// ---------------------------------------------------------------------------
// Synthetic host backends & viewer QUIC helpers
// ---------------------------------------------------------------------------

struct SynthCapture {
    frame_n: u32,
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-v050", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("V050".into()),
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
        let mut buf = vec![0x2Au8; 320 * 180 * 4];
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
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 15, convert_us: 0 }))
    }

    fn stop(&mut self) {}
}

struct NullInjector;

impl InputInjector for NullInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "null-v050", keyboard: true, mouse_absolute: true, ..Default::default() }
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

fn synth_backends(privacy: Option<Arc<dyn PrivacyController>>) -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(SynthCapture { frame_n: 0 }))),
        encoder: Arc::new(|cfg, _| veloxdesk_codec::create_encoder(cfg, false)),
        injector: Arc::new(|| Ok(Box::new(NullInjector))),
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        clipboard: None,
        privacy,
        file_transfer: None,
        tunnel_policy: None,
    }
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

fn host_config_with_perms(perms: Permissions) -> HostConfig {
    let handler: ApprovalHandler = Arc::new(move |_req: ApprovalRequest| {
        let p = perms.clone();
        Box::pin(async move { ApprovalDecision::accept(p) })
    });
    let mut auth = HostAuthConfig::with_auto_otp();
    auth.require_accept = true;
    auth.approval_handler = Some(handler);

    HostConfig {
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
    }
}

async fn connect_and_auth_viewer(
    host: &RunningHost,
    host_id: &DeviceIdentity,
    viewer_id: &DeviceIdentity,
    viewer_name: &str,
) -> (Arc<dyn TransportChannel>, ControlSender, ControlReceiver, AuthResult) {
    let otp = host.auth_state().current_otp().expect("host must have active OTP");
    let ch: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(host.local_addr(), viewer_id, Some(host_id.public_key()))
            .await
            .expect("QUIC connect to RunningHost"),
    );
    let (s, r) = ch.open_bi(StreamKind::Control).await.expect("open control stream");
    let (mut tx, mut rx) = framed(s, r);

    tx.send(&Msg::Hello(make_hello(viewer_id, viewer_name)).into()).await.expect("send Hello");

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
    let key = derive_password_key(&otp, &chlg.salt, params).expect("derive OTP key");
    let proof = password_proof(&key, &chlg.nonce);
    tx.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: proof.to_vec() }).into())
        .await
        .expect("send AuthResponse");

    let auth_res = match timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("AuthResult timeout")
        .expect("AuthResult recv")
        .msg
    {
        Some(Msg::AuthResult(r)) => r,
        other => panic!("expected AuthResult, got {other:?}"),
    };
    assert!(auth_res.ok, "viewer auth failed: {}", auth_res.reason);

    // Wait for initial SessionConfig so the session event loop is live.
    timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await.expect("control recv waiting for SessionConfig").msg {
                Some(Msg::SessionConfig(_)) => break,
                _ => continue,
            }
        }
    })
    .await
    .expect("SessionConfig timeout");

    (ch, tx, rx, auth_res)
}

async fn recv_matching<T>(
    rx: &mut ControlReceiver,
    limit: Duration,
    mut pred: impl FnMut(Msg) -> Option<T>,
) -> T {
    timeout(limit, async {
        loop {
            let env = rx.recv().await.expect("control stream closed unexpectedly");
            if let Some(m) = env.msg
                && let Some(found) = pred(m)
            {
                return found;
            }
        }
    })
    .await
    .expect("timed out waiting for expected control message")
}

async fn wait_for_host_event<T>(
    events: &Arc<Mutex<Vec<HostStatusEvent>>>,
    limit: Duration,
    mut pred: impl FnMut(&HostStatusEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(found) = events.lock().unwrap().iter().find_map(&mut pred) {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for expected HostStatusEvent");
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
}

// ---------------------------------------------------------------------------
// 1. Bidirectional Chat + allow_chat permission gating
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v050_bidirectional_chat_host_and_viewer() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let events = Arc::new(Mutex::new(Vec::<HostStatusEvent>::new()));
    let events_cb = events.clone();

    let host = RunningHost::start(
        host_config_with_perms(Permissions::interactive_default()),
        host_id.clone(),
        synth_backends(None),
        Some(Arc::new(move |ev| events_cb.lock().unwrap().push(ev))),
    )
    .await
    .expect("RunningHost::start");

    let (ch, mut tx, mut rx, auth_res) = connect_and_auth_viewer(&host, &host_id, &viewer_id, "Alice-PC").await;
    assert!(
        auth_res.permissions.as_ref().is_some_and(|p| p.allow_chat),
        "interactive_default must grant allow_chat"
    );

    // 1. Viewer -> Host chat message -> Host emits ChatReceived & replies ChatAck.
    let outgoing = ChatMessage {
        id: 101,
        sender_name: "Alice".into(),
        text: "Hello host / Привет хост!".into(),
        ts_ms: 1_735_689_600_101,
    };
    tx.send(&Msg::ChatMessage(outgoing.clone()).into()).await.expect("send viewer ChatMessage");

    let ack = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::ChatAck(a) if a.id == 101 => Some(a),
        _ => None,
    })
    .await;
    assert_eq!(ack.id, 101);

    let (peer_id, received_on_host) = wait_for_host_event(&events, Duration::from_secs(3), |ev| match ev {
        HostStatusEvent::ChatReceived { peer_device_id, msg } if msg.id == 101 => {
            Some((peer_device_id.clone(), msg.clone()))
        }
        _ => None,
    })
    .await;
    assert_eq!(peer_id, viewer_id.device_id());
    assert_eq!(received_on_host, outgoing);

    // 2. Host -> Viewer chat message via RunningHost::send_chat -> Viewer receives & acks -> Host emits ChatAcked.
    let host_msg_id = host
        .send_chat("Reply from local host / Ответ!".into(), "Host-Admin".into())
        .expect("RunningHost::send_chat");
    assert!(host_msg_id >= 1, "send_chat must assign a monotonic non-zero message id");

    let incoming_on_viewer = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::ChatMessage(c) if c.id == host_msg_id => Some(c),
        _ => None,
    })
    .await;
    assert_eq!(incoming_on_viewer.sender_name, "Host-Admin");
    assert_eq!(incoming_on_viewer.text, "Reply from local host / Ответ!");
    assert!(incoming_on_viewer.ts_ms > 0, "host chat message must carry a UNIX timestamp");

    tx.send(&Msg::ChatAck(ChatAck { id: host_msg_id }).into()).await.expect("send viewer ChatAck");

    let acked_peer = wait_for_host_event(&events, Duration::from_secs(3), |ev| match ev {
        HostStatusEvent::ChatAcked { peer_device_id, id } if *id == host_msg_id => Some(peer_device_id.clone()),
        _ => None,
    })
    .await;
    assert_eq!(acked_peer, viewer_id.device_id());

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "chat test done".into() }).into()).await;
    ch.close(0, "done");
    host.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v050_chat_denied_when_allow_chat_false() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let events = Arc::new(Mutex::new(Vec::<HostStatusEvent>::new()));
    let events_cb = events.clone();

    let perms = Permissions { allow_chat: false, ..Permissions::interactive_default() };
    let host = RunningHost::start(
        host_config_with_perms(perms),
        host_id.clone(),
        synth_backends(None),
        Some(Arc::new(move |ev| events_cb.lock().unwrap().push(ev))),
    )
    .await
    .expect("RunningHost::start");

    let (ch, mut tx, mut rx, auth_res) = connect_and_auth_viewer(&host, &host_id, &viewer_id, "Muted-Viewer").await;
    assert!(
        auth_res.permissions.as_ref().is_some_and(|p| !p.allow_chat),
        "session must reflect allow_chat == false"
    );

    // Send a blocked ChatMessage followed immediately by a Ping barrier on the ordered control stream.
    tx.send(
        &Msg::ChatMessage(ChatMessage {
            id: 202,
            sender_name: "Muted".into(),
            text: "This must be ignored".into(),
            ts_ms: 1_735_689_600_202,
        })
        .into(),
    )
    .await
    .expect("send ChatMessage");
    tx.send(&Msg::Ping(Ping { seq: 77, sent_us: 1234 }).into()).await.expect("send Ping barrier");

    let mut saw_chat_ack = false;
    timeout(Duration::from_secs(5), async {
        loop {
            let env = rx.recv().await.expect("control stream closed");
            match env.msg {
                Some(Msg::ChatAck(_)) => saw_chat_ack = true,
                Some(Msg::Pong(p)) if p.seq == 77 => break,
                _ => {}
            }
        }
    })
    .await
    .expect("Pong barrier timeout");

    assert!(!saw_chat_ack, "host must NOT send ChatAck when allow_chat == false");
    let has_chat_event = events
        .lock()
        .unwrap()
        .iter()
        .any(|ev| matches!(ev, HostStatusEvent::ChatReceived { .. }));
    assert!(!has_chat_event, "host must NOT emit HostStatusEvent::ChatReceived when allow_chat == false");

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "chat deny done".into() }).into()).await;
    ch.close(0, "done");
    host.stop();
}

// ---------------------------------------------------------------------------
// 2. Zero Re-Encode Matroska (.mkv) Session Recorder
// ---------------------------------------------------------------------------

/// Synthetic H.264 Annex-B IDR access unit with valid Baseline SPS (NAL 7), PPS (NAL 8), and IDR slice (NAL 5).
fn synthetic_h264_idr() -> Bytes {
    let mut au = Vec::new();
    // SPS: Baseline profile (0x42), compat 0xC0, level 3.0 (0x1E)
    au.extend_from_slice(&[
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0xC0, 0x1E, 0xDA, 0x01, 0x40, 0x16, 0xEC, 0x04, 0x40, 0x00, 0x00, 0x03,
        0x00, 0x40, 0x00, 0x00, 0x0F, 0x03, 0xC5, 0x8B, 0xB8,
    ]);
    // PPS
    au.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80]);
    // IDR slice payload
    au.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00, 0x33, 0xFF, 0xFE, 0xF5, 0x01, 0x02, 0x03]);
    Bytes::from(au)
}

/// Synthetic H.264 Annex-B non-IDR P-frame slice (NAL 1).
fn synthetic_h264_pframe(seed: u8) -> Bytes {
    Bytes::from(vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, 0x01, seed, 0x22, 0x33, 0x44, 0x55])
}

/// Synthetic 20 ms Opus packet (TOC 0xF8 = CELT-only FB 20 ms stereo + payload).
fn synthetic_opus_packet(seq: u32, ts_us: u64) -> EncodedAudio {
    EncodedAudio {
        data: Bytes::from(vec![0xF8, (seq & 0xFF) as u8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]),
        codec: AudioCodecId::Opus,
        seq,
        capture_ts: Timestamp(ts_us),
        duration_us: 20_000,
    }
}

#[test]
fn v050_recorder_zero_reencode_h264_opus_mkv_validation() {
    assert!(!default_recordings_dir().as_os_str().is_empty());

    let temp_dir = std::env::temp_dir().join(format!("veloxdesk-v050-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_dir);
    let out_path = generate_recording_path(&temp_dir, "Peer:123/456\\test");
    let fname = out_path.file_name().and_then(|s| s.to_str()).expect("utf8 filename");
    assert!(fname.ends_with(".mkv"), "recording path must end with .mkv: {fname}");
    assert!(!fname.contains(':') && !fname.contains('/') && !fname.contains('\\'), "path label must be sanitized");

    let cfg = RecorderConfig::new(out_path.clone(), VideoCodec::H264, 640, 360, 30);
    let recorder = SessionRecorder::start(cfg).expect("SessionRecorder::start");

    let base_us: u64 = 10_000_000;
    let make_video = |frame_id: u32, keyframe: bool, offset_ms: u64| EncodedPacket {
        data: if keyframe { synthetic_h264_idr() } else { synthetic_h264_pframe(frame_id as u8) },
        codec: VideoCodec::H264,
        keyframe,
        recovery_point: false,
        width: 640,
        height: 360,
        capture_ts: Timestamp(base_us + offset_ms * 1_000),
        frame_id,
        encode_us: 800,
        convert_us: 100,
    };

    // 1. Feed 3 leading P-frames BEFORE the first IDR — recorder MUST drop these!
    for i in 0..3u32 {
        recorder.push_video(&make_video(i, false, u64::from(i) * 33));
    }

    // 2. Feed 1st IDR keyframe at +100ms, followed by chronologically interleaved video + Opus audio,
    //    then a 2nd IDR keyframe at +2100ms (>2s later, forcing new Cluster & Cue point) + 2 P-frames.
    enum MediaEvent {
        Video(u32, bool),
        Audio(u32),
    }
    let mut timeline: Vec<(u64, MediaEvent)> = vec![(100, MediaEvent::Video(3, true))];
    for i in 0..5u32 {
        timeline.push((133 + u64::from(i) * 33, MediaEvent::Video(4 + i, false)));
    }
    for seq in 0..10u32 {
        timeline.push((105 + u64::from(seq) * 20, MediaEvent::Audio(seq)));
    }
    timeline.push((2_100, MediaEvent::Video(9, true)));
    timeline.push((2_133, MediaEvent::Video(10, false)));
    timeline.push((2_166, MediaEvent::Video(11, false)));
    timeline.sort_by_key(|(ts_ms, _)| *ts_ms);

    for (ts_ms, ev) in timeline {
        match ev {
            MediaEvent::Video(frame_id, keyframe) => {
                recorder.push_video(&make_video(frame_id, keyframe, ts_ms));
            }
            MediaEvent::Audio(seq) => {
                recorder.push_audio(&synthetic_opus_packet(seq, base_us + ts_ms * 1_000));
            }
        }
    }
    let expected_video_frames: usize = 1 + 5 + 1 + 2; // 9 frames after leading 3 P-frames dropped

    let final_stats = recorder.stop().expect("SessionRecorder::stop");
    assert!(!final_stats.active, "recorder must be inactive after stop()");
    assert!(!final_stats.waiting_for_keyframe, "waiting_for_keyframe must clear after IDR");
    assert_eq!(
        final_stats.video_frames as usize, expected_video_frames,
        "leading P-frames must be dropped until first IDR"
    );
    assert_eq!(final_stats.audio_packets, 10);
    assert!(final_stats.bytes_written > 0, "written MKV bytes must be non-zero");

    let summary = validate_mkv_file(&out_path).expect("validate_mkv_file");
    assert!(summary.has_ebml_header, "MKV must have EBML header");
    assert_eq!(summary.doc_type, "matroska", "MKV DocType must be 'matroska'");
    assert!(summary.has_segment, "MKV must have Segment");
    assert!(summary.has_tracks, "MKV must have Tracks");
    assert_eq!(summary.video_codec_id.as_deref(), Some("V_MPEG4/ISO/AVC"), "video track must be H.264 AVCC");
    assert_eq!(summary.audio_codec_id.as_deref(), Some("A_OPUS"), "audio track must be Opus");
    assert!(summary.cluster_count >= 2, "expected >=2 clusters across 2s+ keyframe span, got {}", summary.cluster_count);
    assert_eq!(summary.video_block_count, expected_video_frames);
    assert_eq!(summary.audio_block_count, 10);
    assert!(summary.first_video_is_keyframe, "first video SimpleBlock must be a keyframe (IDR)");
    assert!(summary.timestamps_monotonic, "MKV block timestamps must be monotonic");
    assert!(summary.has_cues, "MKV must contain Cues for seeking");
    assert!(summary.file_size > 0, "MKV file on disk must be non-empty");

    let _ = std::fs::remove_dir_all(&temp_dir);
}

// ---------------------------------------------------------------------------
// 3. Remote Power Management + Permissions
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v050_remote_power_denied_when_allow_power_false() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    // interactive_default() has allow_power = false.
    let host = RunningHost::start(
        host_config_with_perms(Permissions::interactive_default()),
        host_id.clone(),
        synth_backends(None),
        None,
    )
    .await
    .expect("RunningHost::start");

    let (ch, mut tx, mut rx, auth_res) = connect_and_auth_viewer(&host, &host_id, &viewer_id, "NoPower-Viewer").await;
    assert!(
        auth_res.permissions.as_ref().is_some_and(|p| !p.allow_power),
        "interactive_default must have allow_power == false"
    );

    for action in [
        SystemPowerAction::Reboot,
        SystemPowerAction::RebootSafeMode,
        SystemPowerAction::LockScreen,
        SystemPowerAction::Sleep,
    ] {
        tx.send(
            &Msg::SystemPowerRequest(SystemPowerRequest {
                action: action as i32,
                reason: "unauthorized test".into(),
                delay_secs: 5,
            })
            .into(),
        )
        .await
        .expect("send SystemPowerRequest");

        let res: SystemPowerResult = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
            Msg::SystemPowerResult(r) => Some(r),
            _ => None,
        })
        .await;
        assert_eq!(res.action, action as i32);
        assert!(!res.ok, "action {action:?} must be denied when allow_power == false");
        assert!(!res.error.is_empty(), "denied SystemPowerResult must include a clear error message");
    }

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "power deny done".into() }).into()).await;
    ch.close(0, "done");
    host.stop();
}

struct TimedPowerExecutor {
    inner: SimulatedPowerExecutor,
    timestamps: Mutex<Vec<Instant>>,
}

impl TimedPowerExecutor {
    fn new() -> Self {
        Self { inner: SimulatedPowerExecutor::new(), timestamps: Mutex::new(Vec::new()) }
    }

    fn calls(&self) -> Vec<RecordedPowerAction> {
        self.inner.calls()
    }

    fn timestamps(&self) -> Vec<Instant> {
        self.timestamps.lock().unwrap().clone()
    }
}

impl PowerExecutor for TimedPowerExecutor {
    fn execute(&self, action: SystemPowerAction, reason: &str, delay_secs: u32) -> Result<()> {
        self.timestamps.lock().unwrap().push(Instant::now());
        self.inner.execute(action, reason, delay_secs)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v050_remote_power_allowed_executes_after_ok_result() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let power_exec = Arc::new(TimedPowerExecutor::new());

    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host_id)
        .await
        .expect("listen");
    let bind_addr = listener.local_addr();

    let mut cfg_val = host_config_with_perms(Permissions::all());
    cfg_val.auth.host_public_key = Some(host_id.public_key());
    let auth_state = veloxdesk_service::HostAuthState::from_config(&cfg_val.auth);
    let active_otp = auth_state.current_otp().expect("auto_otp must exist");
    cfg_val.auth.shared_state = Some(auth_state);
    let cfg = Arc::new(cfg_val);

    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let hooks = SessionHooks { power_executor: Some(power_exec.clone()), ..Default::default() };
    let host_task = tokio::spawn(async move {
        let ch: Arc<dyn TransportChannel> = Arc::new(listener.accept().await.expect("accept"));
        run_session_with_hooks(ch, cfg, synth_backends(None), stop_rx, hooks).await
    });

    // Connect and authenticate viewer.
    let ch: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(bind_addr, &viewer_id, Some(host_id.public_key()))
            .await
            .expect("QUIC connect"),
    );
    let (s, r) = ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut tx, mut rx) = framed(s, r);
    tx.send(&Msg::Hello(make_hello(&viewer_id, "PowerAdmin")).into()).await.unwrap();

    let chlg = match timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap().msg {
        Some(Msg::AuthChallenge(c)) => c,
        other => panic!("expected AuthChallenge, got {other:?}"),
    };
    let params = Argon2Params {
        m_cost_kib: chlg.argon2_m_cost_kib,
        t_cost: chlg.argon2_t_cost,
        p_cost: chlg.argon2_p_cost,
    };
    let key = derive_password_key(&active_otp, &chlg.salt, params).unwrap();
    let proof = password_proof(&key, &chlg.nonce);
    tx.send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: proof.to_vec() }).into())
        .await
        .unwrap();
    let auth_res = match timeout(Duration::from_secs(5), rx.recv()).await.unwrap().unwrap().msg {
        Some(Msg::AuthResult(r)) => r,
        other => panic!("expected AuthResult, got {other:?}"),
    };
    assert!(auth_res.ok, "auth failed: {}", auth_res.reason);
    assert!(auth_res.permissions.as_ref().is_some_and(|p| p.allow_power), "Permissions::all must grant allow_power");

    // Wait for SessionConfig.
    recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::SessionConfig(_) => Some(()),
        _ => None,
    })
    .await;

    // Issue Reboot request and verify `ok: true` arrives BEFORE SimulatedPowerExecutor::execute runs.
    tx.send(
        &Msg::SystemPowerRequest(SystemPowerRequest {
            action: SystemPowerAction::Reboot as i32,
            reason: "VeloxDesk maintenance".into(),
            delay_secs: 5,
        })
        .into(),
    )
    .await
    .expect("send SystemPowerRequest(Reboot)");

    let res = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::SystemPowerResult(r) => Some(r),
        _ => None,
    })
    .await;
    let ack_received_at = Instant::now();
    assert_eq!(res.action, SystemPowerAction::Reboot as i32);
    assert!(res.ok, "authorized power request must return ok=true, got error: {}", res.error);
    assert!(res.error.is_empty());

    // Wait for the host's ~50ms post-ack delay + spawn_blocking execution to complete.
    let deadline = Instant::now() + Duration::from_secs(3);
    while power_exec.calls().is_empty() {
        assert!(Instant::now() < deadline, "SimulatedPowerExecutor::execute was never called");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let calls = power_exec.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        RecordedPowerAction {
            action: SystemPowerAction::Reboot,
            reason: "VeloxDesk maintenance".into(),
            delay_secs: 5,
        }
    );
    let exec_at = power_exec.timestamps()[0];
    assert!(
        exec_at >= ack_received_at,
        "SystemPowerResult {{ ok: true }} must reach the viewer BEFORE PowerExecutor::execute runs"
    );

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "power allow done".into() }).into()).await;
    ch.close(0, "done");
    let _ = timeout(Duration::from_secs(5), host_task).await;
}

// ---------------------------------------------------------------------------
// 4. PrivacyGuard RAII (normal drop, panic unwind, abrupt session disconnect)
// ---------------------------------------------------------------------------

#[test]
fn v050_privacy_guard_raii_normal_drop_and_panic_unwind() {
    let ctrl = Arc::new(SimulatedPrivacyController::new());
    assert!(!ctrl.is_enabled());

    // 1. Normal scope drop always invokes disable().
    {
        let guard = PrivacyGuard::activate(ctrl.clone()).expect("activate privacy guard");
        assert!(guard.is_active());
        assert!(ctrl.is_enabled());
        assert_eq!(ctrl.enable_count(), 1);
        assert_eq!(ctrl.disable_count(), 0);
    }
    assert!(!ctrl.is_enabled(), "normal drop must disable privacy mode");
    assert_eq!(ctrl.disable_count(), 1);

    // 2. Explicit disable() does not double-disable on subsequent drop.
    let guard = PrivacyGuard::activate(ctrl.clone()).expect("activate privacy guard 2");
    guard.disable().expect("explicit disable");
    assert!(!ctrl.is_enabled());
    assert_eq!(ctrl.enable_count(), 2);
    assert_eq!(ctrl.disable_count(), 2);

    // 3. Panic unwind MUST invoke disable() via Drop.
    let ctrl_for_panic = ctrl.clone();
    let unwind = catch_unwind(AssertUnwindSafe(move || {
        let _guard = PrivacyGuard::activate(ctrl_for_panic).expect("activate privacy guard 3");
        panic!("simulated host session panic");
    }));
    assert!(unwind.is_err(), "closure must panic to test unwind");
    assert!(!ctrl.is_enabled(), "panic unwind must restore privacy mode via PrivacyGuard::drop");
    assert_eq!(ctrl.enable_count(), 3);
    assert_eq!(ctrl.disable_count(), 3);

    // 4. Failed enable returns Err and leaves controller disabled.
    ctrl.set_fail_enable(true);
    assert!(PrivacyGuard::activate(ctrl.clone()).is_err());
    assert!(!ctrl.is_enabled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v050_privacy_mode_restored_on_abrupt_session_disconnect() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let privacy = Arc::new(SimulatedPrivacyController::new());

    let host = RunningHost::start(
        host_config_with_perms(Permissions::interactive_default()),
        host_id.clone(),
        synth_backends(Some(privacy.clone())),
        None,
    )
    .await
    .expect("RunningHost::start");

    let (ch, mut tx, mut rx, _auth) = connect_and_auth_viewer(&host, &host_id, &viewer_id, "Privacy-Viewer").await;

    tx.send(&Msg::PrivacyMode(PrivacyMode { enable: true }).into()).await.expect("send PrivacyMode(true)");
    let res = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::PrivacyModeResult(r) => Some(r),
        _ => None,
    })
    .await;
    assert!(res.enabled, "privacy mode enable failed: {}", res.error);
    assert!(privacy.is_enabled());

    // Abruptly drop QUIC connection without sending PrivacyMode(false).
    drop(tx);
    drop(rx);
    ch.close(1, "abrupt disconnect");

    let deadline = Instant::now() + Duration::from_secs(5);
    while privacy.is_enabled() {
        assert!(Instant::now() < deadline, "PrivacyGuard failed to disable privacy mode after session disconnect");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(privacy.disable_count() >= 1);
    host.stop();
}

// ---------------------------------------------------------------------------
// 5. Pure-Rust NLMS Acoustic Echo Canceller (ERLE >= 12 dB + Double-Talk)
// ---------------------------------------------------------------------------

struct Xorshift64(u64);

impl Xorshift64 {
    fn next_f32_bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        // Map top 24 bits to [-1.0, 1.0].
        let unit = ((x >> 40) as f32) / ((1u32 << 24) as f32);
        unit * 2.0 - 1.0
    }
}

#[test]
fn v050_nlms_aec_erle_and_double_talk_preservation() {
    let sample_rate = 48_000u32;
    let cfg = AecConfig {
        sample_rate,
        channels: 1,
        filter_len: 256,
        step_size: 0.4,
        epsilon: 1e-4,
        double_talk_threshold: 0.65,
    };
    let mut aec = AcousticEchoCanceller::new(cfg);

    let chunk_len = 480usize; // 10 ms at 48 kHz mono
    let total_chunks = 250usize; // 2.5 seconds total: 0..200 far-end only, 200..250 double-talk
    let total_samples = chunk_len * total_chunks;

    // Deterministic speech-like far-end excitation (lightly low-pass filtered noise + modulation).
    let mut rng = Xorshift64(0x5EED_CAFE_1234_5678);
    let mut far = vec![0.0f32; total_samples];
    let mut lp = 0.0f32;
    for (i, slot) in far.iter_mut().enumerate() {
        let white = rng.next_f32_bipolar();
        lp = 0.35 * lp + 0.65 * white;
        let env = 0.75 + 0.25 * (TAU * 3.0 * (i as f32) / (sample_rate as f32)).sin();
        *slot = (lp * 0.40 * env).clamp(-0.9, 0.9);
    }

    // Multi-tap room impulse response (delay 64 samples + secondary reflection at 84 samples).
    let echo_sample = |n: usize| -> f32 {
        let e1 = if n >= 64 { 0.50 * far[n - 64] } else { 0.0 };
        let e2 = if n >= 84 { 0.18 * far[n - 84] } else { 0.0 };
        e1 + e2
    };

    // Phase A: Far-end echo only (near-end silence) -> converge and measure ERLE over last 0.5s.
    let mut steady_in_power = 0.0f64;
    let mut steady_out_power = 0.0f64;

    for c in 0..200 {
        let start = c * chunk_len;
        let far_chunk = &far[start..start + chunk_len];
        let mut mic_chunk: Vec<f32> = (start..start + chunk_len).map(&echo_sample).collect();
        let chunk_in_pow: f64 = mic_chunk.iter().map(|&s| f64::from(s) * f64::from(s)).sum();

        aec.process_aligned(&mut mic_chunk, far_chunk);
        assert!(mic_chunk.iter().all(|s| s.is_finite()), "AEC output must remain finite");

        if c >= 150 {
            let chunk_out_pow: f64 = mic_chunk.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            steady_in_power += chunk_in_pow;
            steady_out_power += chunk_out_pow;
        }
    }

    let measured_erle_db = 10.0 * (steady_in_power / steady_out_power.max(1e-12)).log10();
    let reported_erle_db = aec.last_erle_db();
    assert!(
        measured_erle_db >= 12.0,
        "expected NLMS AEC measured ERLE >= 12 dB, got {measured_erle_db:.2} dB (reported: {reported_erle_db:.2} dB)"
    );
    assert!(
        reported_erle_db >= 12.0,
        "expected aec.last_erle_db() >= 12 dB, got {reported_erle_db:.2} dB (measured: {measured_erle_db:.2} dB)"
    );

    // Phase B: Double-talk (near-end speech + far-end echo simultaneously) -> near-end speech must survive!
    let mut dot = 0.0f64;
    let mut near_energy = 0.0f64;
    let mut out_energy = 0.0f64;

    for c in 200..250 {
        let start = c * chunk_len;
        let far_chunk = &far[start..start + chunk_len];
        let mut near_chunk = Vec::with_capacity(chunk_len);
        let mut mic_chunk = Vec::with_capacity(chunk_len);
        for n in start..start + chunk_len {
            let t = (n as f32) / (sample_rate as f32);
            let near = 0.35 * (TAU * 440.0 * t).sin() + 0.18 * (TAU * 880.0 * t).sin();
            near_chunk.push(near);
            mic_chunk.push(near + echo_sample(n));
        }

        aec.process_aligned(&mut mic_chunk, far_chunk);
        assert!(mic_chunk.iter().all(|s| s.is_finite()), "AEC double-talk output must remain finite");

        for (&n_s, &o_s) in near_chunk.iter().zip(mic_chunk.iter()) {
            dot += f64::from(n_s) * f64::from(o_s);
            near_energy += f64::from(n_s) * f64::from(n_s);
            out_energy += f64::from(o_s) * f64::from(o_s);
        }
    }

    let corr = dot / (near_energy.sqrt() * out_energy.sqrt()).max(1e-12);
    let rms_ratio = (out_energy / near_energy.max(1e-12)).sqrt();
    assert!(
        corr >= 0.70,
        "double-talk must preserve near-end speech waveform (corr={corr:.3}, rms_ratio={rms_ratio:.3})"
    );
    assert!(
        (0.50..=1.80).contains(&rms_ratio),
        "double-talk must neither crush nor blow up near-end speech (rms_ratio={rms_ratio:.3})"
    );
}

#[test]
fn v050_aec_reference_feed_stereo_path() {
    let cfg = AecConfig::default(); // 48 kHz stereo, 1024 taps
    let mut aec = AcousticEchoCanceller::new(cfg);
    let feed = AecReferenceFeed::new(48_000 * 2);

    let chunk_frames = 480usize;
    let chunk_samples = chunk_frames * usize::from(cfg.channels);
    let total_chunks = 200usize;
    let mut rng = Xorshift64(0xABCD_1234_9999_0001);
    let mut far = vec![0.0f32; chunk_samples * total_chunks];
    for s in &mut far {
        *s = rng.next_f32_bipolar() * 0.35;
    }

    let delay_samples = 40 * usize::from(cfg.channels);
    let mut in_pow = 0.0f64;
    let mut out_pow = 0.0f64;
    for c in 0..total_chunks {
        let start = c * chunk_samples;
        let far_chunk = &far[start..start + chunk_samples];
        feed.push(far_chunk);

        let mut mic: Vec<f32> = (start..start + chunk_samples)
            .map(|i| if i >= delay_samples { 0.45 * far[i - delay_samples] } else { 0.0 })
            .collect();
        let c_in: f64 = mic.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        aec.process_capture_with_feed(&mut mic, &feed);
        assert!(mic.iter().all(|x| x.is_finite()));

        if c >= 150 {
            in_pow += c_in;
            out_pow += mic.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>();
        }
    }

    let erle_db = 10.0 * (in_pow / out_pow.max(1e-12)).log10();
    assert!(
        erle_db >= 12.0,
        "stereo AecReferenceFeed path must achieve >= 12 dB ERLE, got {erle_db:.2} dB"
    );
}
