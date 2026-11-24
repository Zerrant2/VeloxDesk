//! Host↔viewer session smoke test using `veloxdesk-service::run_session` over real
//! loopback QUIC, real openh264 encoder/decoder, real FEC `VideoSender`/`VideoReceiver`,
//! and real `AbrController`, with synthetic capture and a recording `InputInjector` so the
//! test runs headlessly in CI without disturbing the local desktop.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with network access.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, DecoderConfig, Display, DisplayId, Frame,
    FrameCapture, FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_core_traits::{Result, Timestamp};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    Close, Hello, InputEvent, KeyEvent, KeyframeRequest, MouseMove, Ping, SessionMode, VideoCodecCap,
    envelope::Msg, input_event,
};
use veloxdesk_service::{HostBackends, HostConfig, RateController, RateDecision, VideoSink, run_session};
use veloxdesk_transport::{AbrController, VideoReceiver, VideoSender, framed};

// ───────────────────────────── synthetic capture + recording injector ─────────────────────────────

struct SynthCapture {
    w: u32,
    h: u32,
    n: u32,
    buf: Vec<u8>,
}

impl SynthCapture {
    fn new(w: u32, h: u32) -> Self {
        Self { w, h, n: 0, buf: vec![0x40; (w * h * 4) as usize] }
    }
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synthetic", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("SYNTH0".into()),
            index: 0,
            name: "Synthetic 720p".into(),
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
        // Paint a moving stripe so the encoder produces real non-trivial deltas.
        let stride = (self.w * 4) as usize;
        let row = (n as usize * 7) % (self.h as usize);
        for px in self.buf[row * stride..(row + 1) * stride].chunks_exact_mut(4) {
            px[0] = (n & 0xFF) as u8;
            px[1] = 200;
            px[2] = 100;
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
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 100, convert_us: 0 }))
    }
    fn stop(&mut self) {}
}

struct RecordingInjector {
    events: Arc<Mutex<Vec<InputEvent>>>,
    released: Arc<Mutex<u32>>,
}

impl InputInjector for RecordingInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "recording", keyboard: true, mouse_absolute: true, mouse_relative: true, ..Default::default() }
    }
    fn set_displays(&mut self, _displays: &[DisplayGeometry]) {}
    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        self.events.lock().unwrap().push(event.clone());
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        *self.released.lock().unwrap() += 1;
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        Vec::new()
    }
}

struct RealSink(VideoSender);
impl VideoSink for RealSink {
    fn send_frame(&mut self, pkt: &veloxdesk_core_traits::video::EncodedPacket) -> Result<u32> {
        let t = Instant::now();
        let _ = self.0.send_frame(pkt)?;
        Ok(t.elapsed().as_micros() as u32)
    }
    fn set_parity_percent(&mut self, pct: u8) {
        self.0.set_parity_percent(pct);
    }
    fn on_nack(&mut self, nack: &veloxdesk_protocol::control::Nack) {
        self.0.on_nack(nack);
    }
}

struct RealAbr(AbrController);
impl RateController for RealAbr {
    fn on_report(&mut self, rr: &veloxdesk_protocol::control::ReceiverReport, now: Timestamp) -> Option<RateDecision> {
        self.0.on_report(rr, now).map(|d| RateDecision { bitrate_kbps: d.bitrate_kbps, parity_percent: d.parity_percent })
    }
}

// ───────────────────────────── test ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_viewer_session_end_to_end() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();

    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host_id).await.expect("listen");
    let addr = listener.local_addr();

    let (host_ch, viewer_ch) =
        tokio::join!(listener.accept(), veloxdesk_transport::connect(addr, &viewer_id, Some(host_id.public_key())));
    let host_ch: Arc<dyn TransportChannel> = Arc::new(host_ch.expect("accept"));
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(viewer_ch.expect("connect"));
    assert_eq!(viewer_ch.peer_public_key(), Some(host_id.public_key()));

    let recorded = Arc::new(Mutex::new(Vec::new()));
    let released = Arc::new(Mutex::new(0u32));
    let backends = HostBackends {
        capture: Arc::new(|_| Ok(Box::new(SynthCapture::new(640, 360)))),
        encoder: Arc::new(|cfg, _prefer_hw| veloxdesk_codec::create_encoder(cfg, false)),
        injector: {
            let events = recorded.clone();
            let rel = released.clone();
            Arc::new(move || Ok(Box::new(RecordingInjector { events: events.clone(), released: rel.clone() })))
        },
        video_sink: Arc::new(|ch, sid| Box::new(RealSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(RealAbr(AbrController::new(start, min, max)))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        mic_playback: None,
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    };

    let cfg = Arc::new(HostConfig {
        max_fps: 60,
        bitrate_kbps: 4_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        fec_parity_percent: 15,
        allow_input: true,
        ..Default::default()
    });

    let host_task = tokio::spawn(run_session(host_ch, cfg, backends));

    // Viewer: open control stream and run handshake.
    let (ctrl_s, ctrl_r) = viewer_ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut ctrl_tx, mut ctrl_rx) = framed(ctrl_s, ctrl_r);
    ctrl_tx
        .send(
            &Msg::Hello(Hello {
                protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                device_id: viewer_id.device_id(),
                public_key: viewer_id.public_key().to_vec(),
                device_name: "qa-viewer".into(),
                os: std::env::consts::OS.into(),
                app_version: "0.1.0".into(),
                mode: SessionMode::Gaming as i32,
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
                viewer_width: 1920,
                viewer_height: 1080,
                viewer_refresh_hz: 60,
                max_bitrate_kbps: 10_000,
            })
            .into(),
        )
        .await
        .expect("send Hello");

    // Expect AuthResult(ok = true).
    let auth = tokio::time::timeout(Duration::from_secs(5), ctrl_rx.recv()).await.expect("auth timeout").expect("auth recv");
    match auth.msg {
        Some(Msg::AuthResult(r)) => assert!(r.ok, "auth rejected: {}", r.reason),
        other => panic!("expected AuthResult, got {other:?}"),
    }

    // Expect SessionConfig with display + H264 video config.
    let sc_msg = tokio::time::timeout(Duration::from_secs(5), ctrl_rx.recv()).await.expect("sc timeout").expect("sc recv");
    let vcfg = match sc_msg.msg {
        Some(Msg::SessionConfig(sc)) => {
            assert_eq!(sc.displays.len(), 1);
            sc.video.expect("video config missing")
        }
        other => panic!("expected SessionConfig, got {other:?}"),
    };
    assert_eq!(vcfg.width, 640);
    assert_eq!(vcfg.height, 360);

    // Send a Ping and expect a Pong (ignoring interleaved FrameTiming messages).
    let ping_ts = Timestamp::now().as_micros();
    ctrl_tx.send(&Msg::Ping(Ping { seq: 42, sent_us: ping_ts }).into()).await.expect("send Ping");
    let mut got_pong = false;
    let mut timing_reports = 0u32;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && (!got_pong || timing_reports < 5) {
        let env = tokio::time::timeout(Duration::from_secs(2), ctrl_rx.recv()).await.expect("ctrl timeout").expect("ctrl recv");
        match env.msg {
            Some(Msg::Pong(p)) => {
                assert_eq!(p.seq, 42);
                assert_eq!(p.ping_sent_us, ping_ts);
                got_pong = true;
            }
            Some(Msg::FrameTiming(_)) => timing_reports += 1,
            _ => {}
        }
    }
    assert!(got_pong, "did not receive Pong");
    assert!(timing_reports >= 5, "expected FrameTiming reports, got {timing_reports}");

    // Open Input stream and send a key + mouse event.
    let (in_s, in_r) = viewer_ch.open_bi(StreamKind::Input).await.expect("open input");
    let (mut in_tx, _in_rx) = framed(in_s, in_r);
    in_tx
        .send(
            &Msg::Input(InputEvent {
                ts_us: Timestamp::now().as_micros(),
                event: Some(input_event::Event::Key(KeyEvent { hid_usage: 0x04, down: true, modifiers: 0 })),
            })
            .into(),
        )
        .await
        .expect("send Key");
    in_tx
        .send(
            &Msg::Input(InputEvent {
                ts_us: Timestamp::now().as_micros(),
                event: Some(input_event::Event::MouseMove(MouseMove { x: 100, y: 200, display: 0 })),
            })
            .into(),
        )
        .await
        .expect("send MouseMove");

    // Request a keyframe via PLI so we are guaranteed to decode cleanly.
    ctrl_tx
        .send(&Msg::KeyframeRequest(KeyframeRequest { stream_id: 0, last_good_frame_id: 0 }).into())
        .await
        .expect("send PLI");

    // Receive and decode ≥ 10 real frames over QUIC datagrams!
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
    let media_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < media_deadline && decoded < 10 {
        if let Ok(Ok(d)) = tokio::time::timeout(Duration::from_millis(50), viewer_ch.recv_datagram()).await {
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
                    assert_eq!(df.frame.width, 640);
                    assert_eq!(df.frame.height, 360);
                    decoded += 1;
                }
            }
            for fb in rx.poll_feedback(Timestamp::now()) {
                let _ = ctrl_tx.send(&fb).await;
            }
        }
    }
    assert!(decoded >= 10, "expected >= 10 decoded frames, got {decoded}");

    // Clean close.
    ctrl_tx.send(&Msg::Close(Close { code: 0, reason: "smoke test done".into() }).into()).await.expect("send Close");
    drop(in_tx);
    let summary = tokio::time::timeout(Duration::from_secs(5), host_task)
        .await
        .expect("host exit timeout")
        .expect("host join")
        .expect("host ok");
    assert!(summary.frames_sent >= 10, "host frames_sent={}", summary.frames_sent);
    assert!(recorded.lock().unwrap().len() >= 2, "expected >=2 input events, got {:?}", recorded.lock().unwrap().len());
    assert!(*released.lock().unwrap() >= 1, "expected release_all on teardown");
}
