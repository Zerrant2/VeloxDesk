//! Real QUIC loopback host↔viewer test: `veloxdesk_transport::listen` + `serve_until`
//! + `VideoSender` (RS FEC datagrams) + `AbrController` → `veloxdesk_transport::connect`
//! + `VideoReceiver`, plus `max_sessions` rejection and clean host shutdown (`release_all`).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::oneshot;
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputEvent, InputEventKind, InputFeedback};
use veloxdesk_core_traits::video::*;
use veloxdesk_core_traits::{FrameCapture, InputInjector, Result, Timestamp, TransportChannel, VideoEncoder};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{Argon2Params, PasswordHash, derive_password_key, password_proof};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{AuthMethod, AuthResponse, Envelope, Hello, KeyEvent, Ping, envelope::Msg};
use veloxdesk_service::session::CLOSE_BUSY;
use veloxdesk_service::{
    HostAuthConfig, HostBackends, HostConfig, TransportAbr, TransportVideoSink, serve_p2p_until, serve_until,
};
use veloxdesk_transport::{
    AbrController, ControlReceiver, P2pListener, VideoReceiver, VideoSender, connect, framed, listen,
};

struct StaticCapture {
    sent: bool,
}

impl FrameCapture for StaticCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "static-synth", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("D0".into()),
            index: 0,
            name: "Synth".into(),
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
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        if !self.sent {
            self.sent = true;
            return Ok(Some(CapturedFrame {
                frame: Frame {
                    width: 320,
                    height: 180,
                    format: PixelFormat::Bgra8,
                    color_space: ColorSpace::Srgb,
                    color_range: ColorRange::Full,
                    data: FrameData::Cpu(vec![Plane { data: Bytes::from(vec![0x55u8; 320 * 180 * 4]), stride: 320 * 4 }]),
                    capture_ts: Timestamp::now(),
                },
                dirty_rects: vec![],
                cursor: None,
                capture_us: 200,
                convert_us: 0,
            }));
        }
        // Static screen after first frame → exercises idle_fps re-encoding!
        std::thread::sleep(timeout.min(Duration::from_millis(10)));
        Ok(None)
    }
    fn stop(&mut self) {}
}

struct FakeEncoder {
    cfg: EncoderConfig,
    next_id: u32,
}

impl VideoEncoder for FakeEncoder {
    fn caps(&self) -> EncoderCaps {
        EncoderCaps { name: "fake-enc", codecs: vec![VideoCodec::H264], ..Default::default() }
    }
    fn config(&self) -> &EncoderConfig {
        &self.cfg
    }
    fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
        self.next_id += 1;
        // Multi-shard payload (~8 KB for keyframe, ~2 KB for P-frame) to exercise RS FEC.
        let len = if opts.force_keyframe { 8192 } else { 2048 };
        let mut payload = vec![0xABu8; len];
        payload[0..4].copy_from_slice(&self.next_id.to_le_bytes());
        Ok(Some(EncodedPacket {
            data: Bytes::from(payload),
            codec: self.cfg.codec,
            keyframe: opts.force_keyframe,
            recovery_point: false,
            width: frame.width,
            height: frame.height,
            capture_ts: frame.capture_ts,
            frame_id: self.next_id,
            encode_us: 300,
            convert_us: 20,
        }))
    }
    fn update(&mut self, _u: EncoderUpdate) -> Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct InjLog {
    events: usize,
    releases: usize,
}

struct RecInjector(Arc<Mutex<InjLog>>);

impl InputInjector for RecInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "rec", keyboard: true, ..Default::default() }
    }
    fn set_displays(&mut self, _d: &[DisplayGeometry]) {}
    fn inject(&mut self, _e: &InputEvent) -> Result<()> {
        self.0.lock().unwrap().events += 1;
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        self.0.lock().unwrap().releases += 1;
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        Vec::new()
    }
}

async fn recv_until<F: FnMut(&Msg) -> bool>(r: &mut ControlReceiver, mut f: F) -> Msg {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let env: Envelope = r.recv().await.expect("control recv");
            if let Some(m) = env.msg {
                if f(&m) {
                    return m;
                }
            }
        }
    })
    .await
    .expect("timed out waiting for control message")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_quic_loopback_fec_busy_and_shutdown() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = listen(bind, &host_id).await.expect("listen");
    let addr = listener.local_addr();

    let inj_log = Arc::new(Mutex::new(InjLog::default()));
    let backends = {
        let inj_log = inj_log.clone();
        HostBackends {
            capture: Arc::new(|_| Ok(Box::new(StaticCapture { sent: false }) as Box<dyn FrameCapture>)),
            encoder: Arc::new(|cfg, _| {
                Ok(Box::new(FakeEncoder { cfg: cfg.clone(), next_id: 0 }) as Box<dyn VideoEncoder>)
            }),
            injector: Arc::new(move || Ok(Box::new(RecInjector(inj_log.clone())) as Box<dyn InputInjector>)),
            video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
            rate: Arc::new(|s, mn, mx| Box::new(TransportAbr(AbrController::new(s, mn, mx)))),
        }
    };
    let cfg = HostConfig {
        bind: addr,
        max_fps: 60,
        idle_fps: 20, // fast idle refresh so we quickly get multiple frames on static screen
        bitrate_kbps: 15_000,
        max_sessions: 1,
        ..Default::default()
    };
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let host_task = tokio::spawn(serve_until(listener, cfg, backends, async {
        let _ = stop_rx.await;
    }));

    // Viewer 1 connects with pinned host public key (E2EE identity verification).
    let ch = connect(addr, &viewer_id, Some(host_id.public_key())).await.expect("connect");
    let (send, recv) = ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut w, mut r) = framed(send, recv);

    w.send(&Msg::Hello(Hello { device_id: viewer_id.device_id(), protocol_version: 1, ..Default::default() }).into())
        .await
        .unwrap();
    let Msg::AuthResult(a) = recv_until(&mut r, |_| true).await else { panic!("expected AuthResult") };
    assert!(a.ok);
    let Msg::SessionConfig(sc) = recv_until(&mut r, |_| true).await else { panic!("expected SessionConfig") };
    assert_eq!(sc.video.unwrap().width, 320);

    // Receive real FEC datagrams and reassemble at least 2 complete frames (1 initial + 1 idle refresh).
    let mut vr = VideoReceiver::new();
    let mut frames = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while frames.len() < 2 {
            let dgram = ch.recv_datagram().await.expect("recv_datagram");
            if let Some(rf) = vr.push_datagram(&dgram, Timestamp::now()) {
                frames.push(rf);
            }
        }
    })
    .await
    .expect("timed out receiving FEC frames over QUIC");
    assert!(frames[0].packet.keyframe, "first frame must be a keyframe");
    assert_eq!(frames[0].packet.data.len(), 8192);
    assert_eq!(frames[1].packet.data.len(), 2048);

    // Ping → Pong over QUIC control stream.
    w.send(&Msg::Ping(Ping { seq: 11, sent_us: 999 }).into()).await.unwrap();
    let Msg::Pong(p) = recv_until(&mut r, |m| matches!(m, Msg::Pong(_))).await else { unreachable!() };
    assert_eq!((p.seq, p.ping_sent_us), (11, 999));

    // Input over dedicated QUIC StreamKind::Input stream.
    let (isend, _irecv) = ch.open_bi(StreamKind::Input).await.expect("open input");
    let mut iw = veloxdesk_transport::ControlSender::new(isend);
    iw.send(
        &Msg::Input(InputEvent {
            ts_us: 1,
            event: Some(InputEventKind::Key(KeyEvent { hid_usage: 0x04, down: true, modifiers: 0 })),
        })
        .into(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while inj_log.lock().unwrap().events < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("input not delivered");

    // Second viewer is rejected with CLOSE_BUSY while session 1 is active.
    let viewer2_id = DeviceIdentity::generate();
    let ch2 = connect(addr, &viewer2_id, Some(host_id.public_key())).await.expect("connect viewer 2");
    let (_s2, r2) = ch2.open_bi(StreamKind::Control).await.expect("open control 2");
    let mut r2 = ControlReceiver::new(r2);
    let Msg::Close(c) = recv_until(&mut r2, |m| matches!(m, Msg::Close(_))).await else { unreachable!() };
    assert_eq!(c.code, CLOSE_BUSY);

    // Graceful host shutdown sends Close to viewer 1 and runs release_all().
    let _ = stop_tx.send(());
    let Msg::Close(c) = recv_until(&mut r, |m| matches!(m, Msg::Close(_))).await else { unreachable!() };
    assert_eq!(c.code, 0);
    tokio::time::timeout(Duration::from_secs(5), host_task).await.unwrap().unwrap().unwrap();
    assert!(inj_log.lock().unwrap().releases >= 1, "release_all must run on shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_quic_loopback_with_system_codec() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let listener = listen("127.0.0.1:0".parse().unwrap(), &host_id).await.expect("listen");
    let addr = listener.local_addr();

    let sys = HostBackends::system();
    let inj_log = Arc::new(Mutex::new(InjLog::default()));
    let backends = HostBackends {
        capture: Arc::new(|_| Ok(Box::new(StaticCapture { sent: false }) as Box<dyn FrameCapture>)),
        encoder: sys.encoder,
        injector: Arc::new(move || Ok(Box::new(RecInjector(inj_log.clone())) as Box<dyn InputInjector>)),
        video_sink: sys.video_sink,
        rate: sys.rate,
    };
    let cfg = HostConfig {
        bind: addr,
        max_fps: 60,
        idle_fps: 30,
        bitrate_kbps: 4_000,
        codec: VideoCodec::H264,
        prefer_hw: true,
        prefer_gpu: false,
        max_sessions: 1,
        ..Default::default()
    };
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let host_task = tokio::spawn(serve_until(listener, cfg, backends, async {
        let _ = stop_rx.await;
    }));

    let ch = connect(addr, &viewer_id, Some(host_id.public_key())).await.expect("connect");
    let (send, recv) = ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut w, mut r) = framed(send, recv);
    w.send(&Msg::Hello(Hello { device_id: viewer_id.device_id(), protocol_version: 1, ..Default::default() }).into())
        .await
        .unwrap();
    let Msg::AuthResult(a) = recv_until(&mut r, |_| true).await else { panic!("expected AuthResult") };
    assert!(a.ok);
    let Msg::SessionConfig(sc) = recv_until(&mut r, |_| true).await else { panic!("expected SessionConfig") };
    let vcfg = sc.video.expect("video config");
    assert_eq!((vcfg.width, vcfg.height), (320, 180));

    let mut vr = VideoReceiver::new();
    vr.set_dimensions(vcfg.width, vcfg.height);
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
    let mut saw_kf = false;
    tokio::time::timeout(Duration::from_secs(15), async {
        while decoded < 3 {
            let dgram = ch.recv_datagram().await.expect("recv_datagram");
            let mut batch = Vec::new();
            if let Some(rf) = vr.push_datagram(&dgram, Timestamp::now()) {
                batch.push(rf);
            }
            while let Some(rf) = vr.pop_frame() {
                batch.push(rf);
            }
            for rf in batch {
                if rf.packet.keyframe {
                    saw_kf = true;
                }
                if saw_kf
                    && let Ok(Some(df)) = dec.decode(&rf.packet)
                {
                    assert_eq!((df.frame.width, df.frame.height), (320, 180));
                    decoded += 1;
                }
            }
        }
    })
    .await
    .expect("timed out decoding real H.264 frames over QUIC");
    assert!(decoded >= 3);

    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), host_task).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_p2p_listener_direct_with_password_auth() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let listener = P2pListener::bind("127.0.0.1:0".parse().unwrap(), None, &host_id)
        .await
        .expect("p2p bind");
    let addr = listener.local_addr();

    let ph = PasswordHash::hash_with("m2-quic-pw", Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }).unwrap();
    let mut auth = HostAuthConfig::with_permanent_password(ph);
    auth.host_public_key = Some(host_id.public_key());

    let inj_log = Arc::new(Mutex::new(InjLog::default()));
    let backends = HostBackends {
        capture: Arc::new(|_| Ok(Box::new(StaticCapture { sent: false }) as Box<dyn FrameCapture>)),
        encoder: Arc::new(|cfg, _| Ok(Box::new(FakeEncoder { cfg: cfg.clone(), next_id: 0 }) as Box<dyn VideoEncoder>)),
        injector: Arc::new(move || Ok(Box::new(RecInjector(inj_log.clone())) as Box<dyn InputInjector>)),
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|s, mn, mx| Box::new(TransportAbr(AbrController::new(s, mn, mx)))),
    };
    let cfg = HostConfig { bind: addr, max_fps: 60, idle_fps: 20, auth, ..Default::default() };
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let host_task = tokio::spawn(serve_p2p_until(listener, cfg, backends, async {
        let _ = stop_rx.await;
    }));

    // Real mutual-TLS QUIC connect directly to the multiplexed P2pListener socket.
    let ch = connect(addr, &viewer_id, Some(host_id.public_key())).await.expect("connect");
    let (send, recv) = ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut w, mut r) = framed(send, recv);

    w.send(
        &Msg::Hello(Hello {
            device_id: viewer_id.device_id(),
            public_key: viewer_id.public_key().to_vec(),
            protocol_version: 1,
            ..Default::default()
        })
        .into(),
    )
    .await
    .unwrap();

    let Msg::AuthChallenge(challenge) = recv_until(&mut r, |_| true).await else { panic!("expected AuthChallenge") };
    assert_eq!(challenge.host_public_key, host_id.public_key());
    let params = Argon2Params {
        m_cost_kib: challenge.argon2_m_cost_kib,
        t_cost: challenge.argon2_t_cost,
        p_cost: challenge.argon2_p_cost,
    };
    let key = derive_password_key("m2-quic-pw", &challenge.salt, params).unwrap();
    let proof = password_proof(&key, &challenge.nonce);
    w.send(
        &Msg::AuthResponse(AuthResponse {
            method: AuthMethod::PermanentPassword as i32,
            proof: proof.to_vec(),
        })
        .into(),
    )
    .await
    .unwrap();

    let Msg::AuthResult(a) = recv_until(&mut r, |_| true).await else { panic!("expected AuthResult") };
    assert!(a.ok, "auth failed: {}", a.reason);
    let Msg::SessionConfig(sc) = recv_until(&mut r, |_| true).await else { panic!("expected SessionConfig") };
    assert_eq!(sc.video.unwrap().width, 320);

    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), host_task).await.unwrap().unwrap().unwrap();
}
