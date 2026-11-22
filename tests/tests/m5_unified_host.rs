//! M5 unified embedded host (`RunningHost`) + live local IPC + rotating OTP +
//! interactive `ApprovalHandler` + status event lifecycle end-to-end integration test.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with `BypassSandbox: true`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use veloxdesk_crypto::auth::{Argon2Params, derive_password_key, password_proof};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    AuthMethod, AuthResponse, Close, Hello, InputEvent, Permissions, SessionMode, VideoCodecCap, envelope::Msg,
};
use veloxdesk_service::{
    ApprovalDecision, ApprovalHandler, ApprovalRequest, HostAuthConfig, HostBackends, HostConfig, HostStatusEvent,
    IpcClient, IpcRequest, IpcResponse, RunningHost, TransportAbr, TransportVideoSink,
};
use veloxdesk_transport::{AbrController, VideoSender, framed};

struct SynthCapture {
    frame_n: u32,
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-m5", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("M5".into()),
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
        let mut buf = vec![0x42u8; 320 * 180 * 4];
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
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 30, convert_us: 0 }))
    }
    fn stop(&mut self) {}
}

struct NullInjector;
impl InputInjector for NullInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "null-m5", keyboard: true, mouse_absolute: true, ..Default::default() }
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
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_host_live_ipc_otp_rotation_approval_and_clean_stop() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let approvals = Arc::new(Mutex::new(Vec::<ApprovalRequest>::new()));
    let approvals_cb = approvals.clone();
    let handler: ApprovalHandler = Arc::new(move |req: ApprovalRequest| {
        let log = approvals_cb.clone();
        Box::pin(async move {
            log.lock().unwrap().push(req);
            ApprovalDecision::accept(Permissions::all())
        })
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
        ipc_port: Some(0), // ephemeral live IPC port
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

    let ipc_addr = host.ipc_addr().expect("live IPC server must be bound");
    assert_eq!(host.device_id(), host_id.device_id());
    assert_eq!(host.active_sessions(), 0);

    // 1. Live IPC Status returns real device_id, initial active_otp, active_sessions == 0.
    let initial_otp = match IpcClient::request(ipc_addr, &IpcRequest::Status).await.expect("IPC Status") {
        IpcResponse::Status { device_id, active_otp, active_sessions, desktop } => {
            assert_eq!(device_id, host_id.device_id());
            assert_eq!(active_sessions, 0);
            assert!(!desktop.is_empty());
            active_otp.expect("auto_otp must populate active_otp")
        }
        other => panic!("expected Status, got {other:?}"),
    };
    assert_eq!(host.auth_state().current_otp().as_deref(), Some(initial_otp.as_str()));

    // 2. Live IPC RotateOtp updates OTP immediately.
    let rotated_otp = match IpcClient::request(ipc_addr, &IpcRequest::RotateOtp).await.expect("IPC RotateOtp") {
        IpcResponse::Otp { otp } => otp,
        other => panic!("expected Otp, got {other:?}"),
    };
    assert_ne!(rotated_otp, initial_otp, "RotateOtp must produce a fresh OTP");
    assert_eq!(host.auth_state().current_otp().as_deref(), Some(rotated_otp.as_str()));

    // 3. Viewer connects over real QUIC with the rotated OTP and triggers interactive ApprovalHandler.
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(host.local_addr(), &viewer_id, Some(host_id.public_key()))
            .await
            .expect("QUIC connect to RunningHost"),
    );
    let (s, r) = viewer_ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut ctrl_tx, mut ctrl_rx) = framed(s, r);
    ctrl_tx
        .send(
            &Msg::Hello(Hello {
                protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                device_id: viewer_id.device_id(),
                public_key: viewer_id.public_key().to_vec(),
                device_name: "m5-viewer".into(),
                os: std::env::consts::OS.into(),
                app_version: "1.0.0".into(),
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

    let chlg = match timeout(Duration::from_secs(5), ctrl_rx.recv()).await.unwrap().unwrap().msg {
        Some(Msg::AuthChallenge(c)) => c,
        other => panic!("expected AuthChallenge, got {other:?}"),
    };
    assert!(chlg.methods.contains(&(AuthMethod::OneTimePassword as i32)));
    let params = Argon2Params {
        m_cost_kib: chlg.argon2_m_cost_kib,
        t_cost: chlg.argon2_t_cost,
        p_cost: chlg.argon2_p_cost,
    };
    let key = derive_password_key(&rotated_otp, &chlg.salt, params).expect("derive OTP key");
    let proof = password_proof(&key, &chlg.nonce);
    ctrl_tx
        .send(&Msg::AuthResponse(AuthResponse { method: AuthMethod::OneTimePassword as i32, proof: proof.to_vec() }).into())
        .await
        .expect("send AuthResponse");

    match timeout(Duration::from_secs(5), ctrl_rx.recv()).await.unwrap().unwrap().msg {
        Some(Msg::AuthResult(r)) => assert!(r.ok, "auth failed: {}", r.reason),
        other => panic!("expected AuthResult, got {other:?}"),
    }
    // Wait for SessionConfig so the session is fully active.
    let _sc = timeout(Duration::from_secs(5), async {
        loop {
            let env = ctrl_rx.recv().await.unwrap();
            if let Some(Msg::SessionConfig(sc)) = env.msg {
                return sc;
            }
        }
    })
    .await
    .expect("SessionConfig timeout");

    // 4. Verify ApprovalHandler was invoked and IPC Status shows active_sessions == 1 + post-login OTP rotation.
    {
        let reqs = approvals.lock().unwrap();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].peer_device_id, viewer_id.device_id());
        assert_eq!(reqs[0].peer_name, "m5-viewer");
    }
    assert_eq!(host.active_sessions(), 1);
    match IpcClient::request(ipc_addr, &IpcRequest::Status).await.expect("IPC Status while connected") {
        IpcResponse::Status { device_id, active_otp, active_sessions, .. } => {
            assert_eq!(device_id, host_id.device_id());
            assert_eq!(active_sessions, 1, "expected active_sessions == 1 while viewer connected");
            assert_ne!(
                active_otp.as_deref(),
                Some(rotated_otp.as_str()),
                "OTP must auto-rotate after single-use login"
            );
        }
        other => panic!("expected Status, got {other:?}"),
    }

    // 5. Close viewer session -> active_sessions drops back to 0.
    let _ = ctrl_tx.send(&Msg::Close(Close { code: 0, reason: "m5 done".into() }).into()).await;
    viewer_ch.close(0, "done");

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && host.active_sessions() != 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(host.active_sessions(), 0, "active_sessions must return to 0 after disconnect");

    // 6. Cleanly stop RunningHost and verify status events.
    host.stop();
    timeout(Duration::from_secs(5), host.join()).await.expect("RunningHost::join timeout").expect("host clean exit");

    // IPC server must be down after stop.
    assert!(IpcClient::request(ipc_addr, &IpcRequest::Status).await.is_err());

    let evs = events.lock().unwrap();
    assert!(evs.iter().any(|e| matches!(e, HostStatusEvent::Listening { .. })), "missing Listening event: {evs:?}");
    assert!(
        evs.iter().any(|e| matches!(e, HostStatusEvent::OtpRotated { otp } if otp == &rotated_otp)),
        "missing OtpRotated event: {evs:?}"
    );
    assert!(
        evs.iter().any(|e| matches!(e, HostStatusEvent::SessionStarted { peer_device_id } if peer_device_id == &viewer_id.device_id())),
        "missing SessionStarted event: {evs:?}"
    );
    assert!(
        evs.iter().any(|e| matches!(e, HostStatusEvent::SessionEnded { peer_device_id, .. } if peer_device_id == &viewer_id.device_id())),
        "missing SessionEnded event: {evs:?}"
    );
}
