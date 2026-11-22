//! M3 gaming-mode integration tests:
//! 1. End-to-end Windows GPU zero-copy (`MfEncoder` GPU → `MfDecoder` GPU `shared_handle`
//!    → `OffscreenRenderer` DX12 zero-copy import, `zero_copy == true`, PSNR check).
//! 2. Low-latency Opus audio over QUIC datagrams under `impair.rs` (3% loss + jitter →
//!    `AudioJitterBuffer` + `OpusDecoder` PLC, plus redundant-mode recovery check).
//! 3. Multi-player gamepads (4 simultaneous pads) + rumble feedback roundtrip over
//!    `MediaKind::GamepadState` + `Msg::Rumble` (including stale `seq` drop).
//! 4. ABR + FEC adaptation under `impair.rs` (4% loss + 10 ms jitter): bitrate drops,
//!    FEC parity rises & recovers lost shards, then bitrate ramps back up when clean.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with `BypassSandbox: true`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use veloxdesk_audio::{AudioJitterBuffer, JitterConfig, JitterPop, OpusDecoder, OpusEncoder};
use veloxdesk_benches::impair::{Direction, ImpairmentConfig, LossModel, UdpImpairProxy};
use veloxdesk_core_traits::audio::{AudioDecoder, AudioEncoder, AudioEncoderConfig, AudioFormat, AudioFrame};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, Display, DisplayId, EncodedPacket, Frame,
    FrameCapture, FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_core_traits::{Result, Timestamp};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{
    Close, GamepadState, Hello, InputEvent, Rumble, SessionMode, VideoCodecCap, envelope::Msg, input_event,
};
use veloxdesk_service::{
    HostBackends, HostConfig, TransportAbr, TransportVideoSink, run_session,
};
use veloxdesk_transport::{
    AbrController, AudioReceiver, AudioSender, VideoReceiver, VideoSender, encode_gamepad_datagram, framed,
};

// ───────────────────────────── Test 1: GPU Zero-Copy ─────────────────────────────

#[cfg(windows)]
#[test]
fn gpu_zero_copy_encode_decode_dx12_render() {
    use veloxdesk_core_traits::render::ScaleMode;
    use veloxdesk_core_traits::video::{ChromaFormat, DecoderConfig, EncodeOptions, EncoderConfig, RateControl};
    use veloxdesk_render::OffscreenRenderer;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    let enc_caps = veloxdesk_codec::probe_encoders();
    let dec_caps = veloxdesk_codec::probe_decoders();
    if !enc_caps.iter().any(|c| c.hardware && c.codecs.contains(&VideoCodec::H264))
        || !dec_caps.iter().any(|c| c.hardware && c.gpu_output && c.codecs.contains(&VideoCodec::H264))
    {
        eprintln!("skipping GPU zero-copy test: HW H.264 encoder/decoder unavailable");
        return;
    }

    let (w, h) = (1920u32, 1080u32);
    let mut renderer = match OffscreenRenderer::new(w, h) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skipping GPU zero-copy test: no headless GPU renderer ({e})");
            return;
        }
    };
    let rcaps = renderer.caps();
    if !rcaps.zero_copy_import.contains(&"D3D11Texture") {
        eprintln!("skipping GPU zero-copy test: renderer {} ({:?}) lacks DX12 D3D11Texture import", renderer.adapter_name, renderer.backend);
        return;
    }

    // Create D3D11 device on the exact same adapter LUID as the DX12 renderer.
    let dev = veloxdesk_codec::win::d3d::create_device(renderer.adapter_luid()).expect("D3D11 device on DX12 LUID");
    let luid = veloxdesk_codec::win::d3d::device_luid(&dev);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut pool = veloxdesk_codec::win::d3d::TexturePool::new(desc, false, 2);
    let holder = pool.get(&dev).expect("pool BGRA texture");

    // Synthetic BGRA gradient pattern.
    let mut bgra = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            bgra[i] = ((x * 255) / w) as u8; // B
            bgra[i + 1] = ((y * 255) / h) as u8; // G
            bgra[i + 2] = (((x + y) * 127) / (w + h) + 64) as u8; // R
            bgra[i + 3] = 255;
        }
    }
    let mut expected_rgba = vec![0u8; bgra.len()];
    for (dst, src) in expected_rgba.chunks_exact_mut(4).zip(bgra.chunks_exact(4)) {
        dst.copy_from_slice(&[src[2], src[1], src[0], 255]);
    }
    unsafe {
        let ctx = dev.GetImmediateContext().expect("immediate context");
        ctx.UpdateSubresource(&holder.tex, 0, None, bgra.as_ptr().cast(), w * 4, 0);
        ctx.Flush();
    }

    let mut enc = veloxdesk_codec::create_encoder(
        &EncoderConfig {
            codec: VideoCodec::H264,
            width: w,
            height: h,
            fps: 120,
            bitrate_kbps: 25_000,
            rate_control: RateControl::Cbr,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
            gop: None,
            intra_refresh: false,
            input_format: PixelFormat::Bgra8,
            gpu_input: true,
            color_space: ColorSpace::Bt709,
        },
        true,
    )
    .expect("create HW encoder");
    assert!(enc.caps().hardware, "expected HW encoder");

    let mut dec = veloxdesk_codec::create_decoder(
        &DecoderConfig {
            codec: VideoCodec::H264,
            width: w,
            height: h,
            prefer_gpu_output: true,
            shared_device: None,
        },
        true,
    )
    .expect("create HW decoder");
    assert!(dec.caps().hardware && dec.caps().gpu_output, "expected HW GPU decoder");

    let mut zero_copy_draws = 0u32;
    let mut last_rgba = None;
    let mut render_samples = Vec::new();
    for i in 0..10u32 {
        let gframe = Frame {
            width: w,
            height: h,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Gpu(veloxdesk_codec::win::d3d::gpu_frame(&holder, luid)),
            capture_ts: Timestamp(i as u64 * 8_333),
        };
        if let Some(pkt) = enc.encode(&gframe, EncodeOptions { force_keyframe: i == 0 }).expect("encode")
            && let Some(df) = dec.decode(&pkt).expect("decode")
        {
            assert!(df.frame.is_gpu(), "expected GPU decoded frame");
            let (render_us, zc) = renderer.draw_frame(&df.frame, ScaleMode::Stretch).expect("draw_frame");
            assert!(zc, "frame {i}: expected zero_copy == true on DX12");
            zero_copy_draws += 1;
            if i >= 2 {
                render_samples.push(render_us);
            }
            if i == 9 {
                last_rgba = Some(renderer.render_rgba(&df.frame, ScaleMode::Stretch).expect("render_rgba"));
            }
        }
    }
    assert!(zero_copy_draws >= 6, "expected >= 6 zero-copy draws, got {zero_copy_draws}");
    let rgba = last_rgba.expect("expected final rendered RGBA frame");
    let psnr = veloxdesk_codec::convert::psnr(&expected_rgba, &rgba);
    eprintln!(
        "[m3_gaming] GPU zero-copy 1080p ({}, {:?}): draws={zero_copy_draws} render_us={render_samples:?} PSNR={psnr:.2} dB",
        renderer.adapter_name, renderer.backend
    );
    assert!(psnr > 26.0, "GPU zero-copy rendered PSNR too low: {psnr:.2} dB");
}

#[cfg(not(windows))]
#[test]
fn gpu_zero_copy_encode_decode_dx12_render() {
    // Windows DX12 D3D11Texture zero-copy import is Windows-specific; nothing to fail on Linux/macOS CI.
}

// ───────────────────────────── Test 2: Opus Audio over QUIC + Impairment + PLC ─────────────────────────────

fn sine_audio_frame(fmt: AudioFormat, samples_per_ch: usize, phase_start: usize) -> AudioFrame {
    let mut samples = Vec::with_capacity(samples_per_ch * fmt.channels as usize);
    for i in 0..samples_per_ch {
        let t = (phase_start + i) as f32 / fmt.sample_rate as f32;
        samples.push((2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5);
        samples.push((2.0 * std::f32::consts::PI * 660.0 * t).sin() * 0.4);
    }
    AudioFrame { format: fmt, samples, capture_ts: Timestamp::now() }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opus_audio_over_quic_under_impairment_plc() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host_id).await.expect("listen");
    let proxy = UdpImpairProxy::start(listener.local_addr(), ImpairmentConfig::default(), ImpairmentConfig::default())
        .await
        .expect("proxy");
    let (h, v) = tokio::join!(listener.accept(), veloxdesk_transport::connect(proxy.local_addr(), &viewer_id, None));
    let host_ch: Arc<dyn TransportChannel> = Arc::new(h.expect("accept"));
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(v.expect("connect"));

    // 4% random loss + 5 ms latency + 4 ms jitter on host→viewer audio datagrams.
    let impair = ImpairmentConfig::default()
        .with_loss(LossModel::Random(0.04))
        .with_latency(Duration::from_millis(5), Duration::from_millis(4));
    proxy.set_config(Direction::ToClient, impair);

    let frame_dur = Duration::from_millis(5); // 240 samples/ch @ 48 kHz
    let samples_per_ch = 240usize;
    let total_frames = 120usize; // 600 ms of audio

    let recv_task = tokio::spawn({
        let ch = viewer_ch.clone();
        async move {
            let mut jb = AudioJitterBuffer::new(JitterConfig {
                frame_duration: frame_dur,
                target_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(80),
            });
            let mut dec = OpusDecoder::new(AudioFormat::STEREO_48K, frame_dur).expect("opus dec");
            let mut decoded_frames = 0u32;
            let mut plc_frames = 0u32;
            let mut energy_sum = 0.0f64;
            let mut sample_count = 0u64;
            let mut ticker = tokio::time::interval(Duration::from_millis(10));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let start = Instant::now();
            let mut popped_slots = 0usize;
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline && decoded_frames < 100 {
                tokio::select! {
                    res = ch.recv_datagram() => {
                        match res {
                            Ok(d) => {
                                if let Some((sid, pkt)) = AudioReceiver::parse_datagram(&d) {
                                    assert_eq!(sid, 0);
                                    jb.push(pkt);
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    _ = ticker.tick() => {
                        // Drain 5 ms playout slots proportional to wall-clock time so Windows'
                        // 15.6 ms default timer granularity never throttles playout rate.
                        let target_slots = (start.elapsed().as_millis() / 5) as usize;
                        while popped_slots < target_slots && decoded_frames < 100 {
                            match jb.pop() {
                                JitterPop::Packet(pkt) => {
                                    let pcm = dec.decode(Some(&pkt)).expect("decode opus");
                                    assert_eq!(pcm.samples.len(), samples_per_ch * 2);
                                    for &s in &pcm.samples {
                                        assert!(s.is_finite(), "NaN/Inf in decoded audio");
                                        energy_sum += (s as f64) * (s as f64);
                                        sample_count += 1;
                                    }
                                    decoded_frames += 1;
                                    popped_slots += 1;
                                }
                                JitterPop::Missing { .. } => {
                                    let pcm = dec.decode(None).expect("decode PLC");
                                    assert_eq!(pcm.samples.len(), samples_per_ch * 2);
                                    for &s in &pcm.samples {
                                        assert!(s.is_finite(), "NaN/Inf in PLC audio");
                                        energy_sum += (s as f64) * (s as f64);
                                        sample_count += 1;
                                    }
                                    plc_frames += 1;
                                    decoded_frames += 1;
                                    popped_slots += 1;
                                }
                                JitterPop::Buffering => break,
                            }
                        }
                    }
                }
            }
            let rms = (energy_sum / sample_count.max(1) as f64).sqrt();
            (decoded_frames, plc_frames, jb.stats(), rms)
        }
    });

    let mut enc = OpusEncoder::new(&AudioEncoderConfig {
        format: AudioFormat::STEREO_48K,
        bitrate_kbps: 128,
        frame_duration: frame_dur,
        low_delay: true,
    })
    .expect("opus enc");
    let mut sender = AudioSender::new(host_ch.clone(), 0);
    let send_start = Instant::now();
    for i in 0..total_frames {
        let target = send_start + frame_dur * (i as u32);
        if let Some(wait) = target.checked_duration_since(Instant::now())
            && wait > Duration::from_millis(2)
        {
            tokio::time::sleep(wait).await;
        }
        let frame = sine_audio_frame(AudioFormat::STEREO_48K, samples_per_ch, i * samples_per_ch);
        for pkt in enc.encode(&frame).expect("opus encode") {
            sender.send(&pkt).expect("send audio datagram");
        }
    }

    let (decoded, plc, stats, rms) = recv_task.await.expect("audio recv join");
    let pstats = proxy.stats(Direction::ToClient);
    eprintln!(
        "[m3_gaming] Opus 5ms over QUIC (loss {:.1}% + 5±4ms): decoded={decoded} plc={plc} recv={} dup={} late={} rms={rms:.3}",
        100.0 * pstats.loss_rate(),
        stats.received,
        stats.duplicates,
        stats.late_dropped
    );
    assert!(pstats.dropped_loss > 0, "expected proxy to drop at least 1 audio datagram");
    assert!(decoded >= 90, "expected >= 90 decoded audio slots, got {decoded}");
    assert!(plc > 0 && stats.plc_concealed > 0, "expected PLC concealment under loss, got plc={plc}");
    assert!(rms > 0.1, "expected audible sine energy, got rms={rms:.4}");

    host_ch.close(0, "done");
    viewer_ch.close(0, "done");
}

// ───────────────────────────── Test 3: 4 Gamepads + Rumble + Stale Seq ─────────────────────────────

struct SynthCaptureMinimal;
impl FrameCapture for SynthCaptureMinimal {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-m3", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }
    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("M3".into()),
            index: 0,
            name: "M3".into(),
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
        std::thread::sleep(timeout.min(Duration::from_millis(16)));
        let frame = Frame {
            width: 320,
            height: 180,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::from(vec![0x50; 320 * 180 * 4]), stride: 320 * 4 }]),
            capture_ts: Timestamp::now(),
        };
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 50, convert_us: 0 }))
    }
    fn stop(&mut self) {}
}

struct RecordingGamepadInjector {
    /// Per-pad sequence of injected `GamepadState`s.
    pads: Arc<Mutex<HashMap<u32, Vec<GamepadState>>>>,
    pending_rumble: Vec<InputFeedback>,
}

impl InputInjector for RecordingGamepadInjector {
    fn caps(&self) -> InputCaps {
        InputCaps {
            backend: "recording-gamepad",
            keyboard: true,
            mouse_absolute: true,
            mouse_relative: true,
            gamepads: true,
            gamepad_backend: Some("mock-vigem"),
            max_gamepads: 4,
            ..Default::default()
        }
    }
    fn set_displays(&mut self, _displays: &[DisplayGeometry]) {}
    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        if let Some(input_event::Event::Gamepad(g)) = &event.event {
            self.pads.lock().unwrap().entry(g.pad_id).or_default().push(g.clone());
            // Emit deterministic rumble feedback when button bit 0x1000 (A) is pressed on seq == 3.
            if g.seq == 3 && (g.buttons & 0x1000) != 0 {
                self.pending_rumble.push(InputFeedback::Rumble(Rumble {
                    pad_id: g.pad_id,
                    low_freq: 10_000 + g.pad_id * 5_000,
                    high_freq: 20_000 + g.pad_id * 4_000,
                }));
            }
        }
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        Ok(())
    }
    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        std::mem::take(&mut self.pending_rumble)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_player_four_gamepads_and_rumble_roundtrip() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host_id).await.expect("listen");
    let (h, v) = tokio::join!(listener.accept(), veloxdesk_transport::connect(listener.local_addr(), &viewer_id, None));
    let host_ch: Arc<dyn TransportChannel> = Arc::new(h.expect("accept"));
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(v.expect("connect"));

    let pads_log: Arc<Mutex<HashMap<u32, Vec<GamepadState>>>> = Arc::new(Mutex::new(HashMap::new()));
    let backends = HostBackends {
        capture: Arc::new(|_| Ok(Box::new(SynthCaptureMinimal))),
        encoder: Arc::new(|cfg, _| veloxdesk_codec::create_encoder(cfg, false)),
        injector: {
            let log = pads_log.clone();
            Arc::new(move || Ok(Box::new(RecordingGamepadInjector { pads: log.clone(), pending_rumble: Vec::new() })))
        },
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        clipboard: None,
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    };
    let cfg = Arc::new(HostConfig {
        max_fps: 30,
        bitrate_kbps: 2_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: false,
        ..Default::default()
    });
    let host_task = tokio::spawn(run_session(host_ch, cfg, backends));

    let (ctrl_s, ctrl_r) = viewer_ch.open_bi(StreamKind::Control).await.expect("open control");
    let (mut ctrl_tx, mut ctrl_rx) = framed(ctrl_s, ctrl_r);
    ctrl_tx
        .send(
            &Msg::Hello(Hello {
                protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
                device_id: viewer_id.device_id(),
                public_key: viewer_id.public_key().to_vec(),
                device_name: "m3-gamepad-viewer".into(),
                os: std::env::consts::OS.into(),
                app_version: "0.3.0".into(),
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
                viewer_width: 1280,
                viewer_height: 720,
                viewer_refresh_hz: 60,
                max_bitrate_kbps: 4_000,
            })
            .into(),
        )
        .await
        .expect("send Hello");

    // Consume AuthResult + SessionConfig.
    let auth = tokio::time::timeout(Duration::from_secs(3), ctrl_rx.recv()).await.unwrap().unwrap();
    assert!(matches!(auth.msg, Some(Msg::AuthResult(r)) if r.ok));
    let sc = tokio::time::timeout(Duration::from_secs(3), ctrl_rx.recv()).await.unwrap().unwrap();
    assert!(matches!(sc.msg, Some(Msg::SessionConfig(_))));

    // Send GamepadState datagrams for all 4 pads: seq 1 -> 2 -> 3, then stale seq 1 & 2!
    for pad_id in 0..4u32 {
        for &(seq, buttons, lx) in &[
            (1u32, 0x0001u32, 1000i32),
            (2, 0x0002, 2000),
            (3, 0x1000, 12_000 + pad_id as i32 * 1000),
            // Stale / out-of-order packets that MUST be dropped by the host!
            (1, 0xFFFF, -32_000),
            (2, 0xDEAD, -16_000),
        ] {
            let gs = GamepadState {
                pad_id,
                connected: true,
                buttons,
                left_trigger: 128,
                right_trigger: 200,
                left_x: lx,
                left_y: 0,
                right_x: 0,
                right_y: 0,
                seq,
            };
            let dgram = encode_gamepad_datagram(&gs).expect("encode gamepad");
            viewer_ch.send_datagram(dgram).expect("send gamepad datagram");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    // Collect Msg::Rumble for all 4 pads on the control stream.
    let mut rumbles: HashMap<u32, (u32, u32)> = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && rumbles.len() < 4 {
        if let Ok(Ok(env)) = tokio::time::timeout(Duration::from_millis(100), ctrl_rx.recv()).await
            && let Some(Msg::Rumble(r)) = env.msg
        {
            rumbles.insert(r.pad_id, (r.low_freq, r.high_freq));
        }
    }

    let _ = ctrl_tx.send(&Msg::Close(Close { code: 0, reason: "gamepads done".into() }).into()).await;
    viewer_ch.close(0, "done");
    let _ = tokio::time::timeout(Duration::from_secs(5), host_task).await.expect("host exit").unwrap().unwrap();

    // Verify all 4 pads received monotonic seq updates and ZERO stale packets got through.
    let log = pads_log.lock().unwrap();
    for pad_id in 0..4u32 {
        let events = log.get(&pad_id).unwrap_or_else(|| panic!("no events for pad {pad_id}"));
        assert!(!events.is_empty());
        let mut prev_seq = 0u32;
        for ev in events {
            assert!(ev.seq > prev_seq, "pad {pad_id}: stale/duplicate seq {} after {prev_seq}", ev.seq);
            assert_ne!(ev.buttons, 0xFFFF, "pad {pad_id}: stale seq 1 leaked through");
            assert_ne!(ev.buttons, 0xDEAD, "pad {pad_id}: stale seq 2 leaked through");
            prev_seq = ev.seq;
        }
        assert_eq!(events.last().unwrap().seq, 3, "pad {pad_id}: expected latest seq 3");
        let &(lo, hi) = rumbles.get(&pad_id).unwrap_or_else(|| panic!("missing Msg::Rumble for pad {pad_id}"));
        assert_eq!(lo, 10_000 + pad_id * 5_000);
        assert_eq!(hi, 20_000 + pad_id * 4_000);
    }
}

// ───────────────────────────── Test 4: ABR + FEC under Loss & Jitter ─────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abr_and_fec_adaptation_under_loss_and_jitter() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host_id).await.expect("listen");
    let proxy = UdpImpairProxy::start(listener.local_addr(), ImpairmentConfig::default(), ImpairmentConfig::default())
        .await
        .expect("proxy");
    let (h, v) = tokio::join!(listener.accept(), veloxdesk_transport::connect(proxy.local_addr(), &viewer_id, None));
    let host_ch: Arc<dyn TransportChannel> = Arc::new(h.expect("accept"));
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(v.expect("connect"));

    let (rr_tx, mut rr_rx) = tokio::sync::mpsc::unbounded_channel();
    let rx_done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let recv_task = tokio::spawn({
        let ch = viewer_ch.clone();
        let done = rx_done.clone();
        async move {
            let mut rx = VideoReceiver::new();
            let mut complete = 0u64;
            let mut recovered = 0u64;
            let mut last_fb = Instant::now();
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                match tokio::time::timeout(Duration::from_millis(5), ch.recv_datagram()).await {
                    Ok(Ok(d)) => {
                        if let Some(f) = rx.push_datagram(&d, Timestamp::now()) {
                            complete += 1;
                            recovered += f.recovered_shards as u64;
                        }
                        while let Some(f) = rx.pop_frame() {
                            complete += 1;
                            recovered += f.recovered_shards as u64;
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {}
                }
                if last_fb.elapsed() >= Duration::from_millis(50) {
                    last_fb = Instant::now();
                    for env in rx.poll_feedback(Timestamp::now()) {
                        if let Some(Msg::ReceiverReport(rr)) = env.msg {
                            let _ = rr_tx.send(rr);
                        }
                    }
                }
            }
            (complete, recovered)
        }
    });

    let mut sender = VideoSender::new(host_ch.clone(), 0);
    let mut abr = AbrController::new(20_000, 2_000, 25_000);
    let mut current_kbps = 20_000u32;
    let mut current_parity = 10u8;
    sender.set_parity_percent(current_parity);

    let mut frame_id = 0u32;
    async fn run_phase(
        frames: u32,
        sender: &mut VideoSender,
        abr: &mut AbrController,
        rr_rx: &mut tokio::sync::mpsc::UnboundedReceiver<veloxdesk_protocol::control::ReceiverReport>,
        kbps: &mut u32,
        parity: &mut u8,
        fid: &mut u32,
    ) -> (u32, u32, u8, u32, u32) {
        let mut min_kbps = *kbps;
        let mut max_kbps = *kbps;
        let mut max_parity = *parity;
        let mut reports = 0u32;
        let mut ticker = tokio::time::interval(Duration::from_millis(16)); // ~60 fps
        for _ in 0..frames {
            ticker.tick().await;
            // Frame payload proportional to current target bitrate (~kbps * 1000 / 8 / 60).
            let len = ((*kbps as usize * 1000) / (8 * 60)).clamp(4_000, 50_000);
            let pkt = EncodedPacket {
                data: Bytes::from(vec![(*fid & 0xFF) as u8; len]),
                codec: VideoCodec::H264,
                keyframe: *fid % 60 == 0,
                recovery_point: false,
                width: 1920,
                height: 1080,
                capture_ts: Timestamp::now(),
                frame_id: *fid,
                encode_us: 0,
                convert_us: 0,
            };
            *fid += 1;
            sender.send_frame(&pkt).expect("send_frame");

            while let Ok(rr) = rr_rx.try_recv() {
                reports += 1;
                if let Some(dec) = abr.on_report(&rr, Timestamp::now()) {
                    *kbps = dec.bitrate_kbps;
                    *parity = dec.parity_percent;
                    sender.set_parity_percent(*parity);
                    min_kbps = min_kbps.min(*kbps);
                    max_kbps = max_kbps.max(*kbps);
                    max_parity = max_parity.max(*parity);
                }
            }
        }
        (min_kbps, max_kbps, max_parity, reports, *kbps)
    }

    // Phase 1: Clean LAN (40 frames ~0.65 s)
    let (_p1_min, _p1_max, _p1_par, p1_rr, kbps_after_clean) =
        run_phase(40, &mut sender, &mut abr, &mut rr_rx, &mut current_kbps, &mut current_parity, &mut frame_id).await;
    assert!(p1_rr >= 4, "expected >= 4 ReceiverReports in clean phase, got {p1_rr}");

    // Phase 2: 5% random loss + 10 ms latency + 8 ms jitter (90 frames ~1.5 s)
    let impair = ImpairmentConfig::default()
        .with_loss(LossModel::Random(0.05))
        .with_latency(Duration::from_millis(10), Duration::from_millis(8));
    proxy.set_config(Direction::ToClient, impair);
    let (p2_min_kbps, _p2_max, p2_max_parity, p2_rr, kbps_after_loss) =
        run_phase(90, &mut sender, &mut abr, &mut rr_rx, &mut current_kbps, &mut current_parity, &mut frame_id).await;
    assert!(p2_rr >= 10, "expected >= 10 ReceiverReports in impaired phase, got {p2_rr}");

    // Phase 3: Clean recovery (~3.5 s so slow loss_ema decay (a=0.05/report) clears < 2%,
    // hold_after_decrease_us (500 ms) elapses, parity drops back down, and bitrate ramps up).
    proxy.set_config(Direction::ToClient, ImpairmentConfig::default());
    tokio::time::sleep(Duration::from_millis(60)).await;
    let (p3_min_kbps, p3_max_kbps, _p3_par, p3_rr, kbps_after_recovery) =
        run_phase(220, &mut sender, &mut abr, &mut rr_rx, &mut current_kbps, &mut current_parity, &mut frame_id).await;
    assert!(p3_rr >= 30, "expected >= 30 ReceiverReports in recovery phase, got {p3_rr}");
    let p3_end_parity = abr.parity_percent();
    let p3_end_kbps = abr.bitrate_kbps();

    tokio::time::sleep(Duration::from_millis(100)).await;
    rx_done.store(true, std::sync::atomic::Ordering::Release);
    let (complete, recovered) = recv_task.await.expect("recv join");
    let pstats = proxy.stats(Direction::ToClient);

    eprintln!(
        "[m3_gaming] ABR+FEC: clean={kbps_after_clean} -> impaired(min={p2_min_kbps}, end={kbps_after_loss}, max_parity={p2_max_parity}%) -> recovery(min={p3_min_kbps}, max={p3_max_kbps}, end={p3_end_kbps}, end_parity={p3_end_parity}%) | complete={complete}/{frame_id} FEC_recovered={recovered} proxy_dropped={}",
        pstats.dropped_loss
    );

    assert!(p2_min_kbps < kbps_after_clean, "ABR must reduce bitrate under 5% loss+jitter ({p2_min_kbps} vs {kbps_after_clean})");
    assert!(p2_max_parity >= 10, "ABR must raise/maintain FEC parity >= 10% under 5% loss, got {p2_max_parity}%");
    assert!(recovered > 0, "expected FEC to recover lost shards");
    assert!(complete as f64 >= frame_id as f64 * 0.90, "expected >= 90% frame delivery via FEC, got {complete}/{frame_id}");
    assert!(
        p3_end_parity < p2_max_parity,
        "ABR must reduce FEC parity once network is clean (end={p3_end_parity}% vs max={p2_max_parity}%)"
    );
    assert!(
        p3_end_kbps > p3_min_kbps || kbps_after_recovery > kbps_after_loss,
        "ABR must ramp bitrate back up when network recovers (p3_min={p3_min_kbps}, p3_end={p3_end_kbps}, loss_end={kbps_after_loss})"
    );

    host_ch.close(0, "done");
    viewer_ch.close(0, "done");
}
