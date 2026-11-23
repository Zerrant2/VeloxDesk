//! Live end-to-end integration tests for `veloxdesk-android-bridge` against a real
//! `veloxdesk_service::RunningHost` (QUIC + E2EE auth + real H.264 encoding + FEC +
//! input/gamepad injection + bidirectional zstd clipboard + instant 1st-attempt reconnect).

#![cfg(not(target_os = "android"))]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use veloxdesk_clipboard::{ClipboardItem, MemoryClipboard};
use veloxdesk_core_traits::Result;
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::audio::{AudioCapture, AudioCaptureConfig, AudioFormat, AudioFrame};
use veloxdesk_core_traits::input::{DisplayGeometry, InputCaps, InputFeedback, InputInjector};
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, CursorImage, CursorState, Display, DisplayId,
    Frame, FrameCapture, FrameData, PixelFormat, Plane, VideoCodec,
};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_crypto::auth::{Argon2Params, PasswordHash};
use veloxdesk_protocol::control::{InputEvent, Permissions, SessionConfig, input_event};
use veloxdesk_service::{
    HostAuthConfig, HostBackends, HostConfig, RunningHost, TransportAbr, TransportVideoSink, TrustedPeer,
};
use veloxdesk_transport::{AbrController, VideoSender};

use veloxdesk_android::SecretString;
use veloxdesk_android::constants;
use veloxdesk_android::session::{ConnectConfig, HandleTable, SessionCallbacks, load_or_create_identity};
use veloxdesk_android::stats::HudMetrics;

// ───────────────────────── Synthetic Host Backends ─────────────────────────

struct SynthCapture {
    frame_n: u32,
}

impl FrameCapture for SynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-android-test", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("ANDROID-TEST".into()),
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
        let mut buf = vec![0x44u8; 320 * 180 * 4];
        // Vary pixels per frame so the H.264 encoder emits real frames continuously.
        let v = (self.frame_n.wrapping_mul(17) & 0xFF) as u8;
        for chunk in buf.chunks_exact_mut(4).take(320 * 20) {
            chunk[0] = v;
            chunk[1] = v.wrapping_add(40);
            chunk[2] = v.wrapping_add(80);
            chunk[3] = 255;
        }
        let frame = Frame {
            width: 320,
            height: 180,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::from(buf), stride: 320 * 4 }]),
            capture_ts: Timestamp::now(),
        };
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 30, convert_us: 10 }))
    }

    fn stop(&mut self) {}
}

struct RecordingInjector {
    events: Arc<Mutex<Vec<InputEvent>>>,
}

impl InputInjector for RecordingInjector {
    fn caps(&self) -> InputCaps {
        InputCaps {
            backend: "recording-android-test",
            keyboard: true,
            mouse_absolute: true,
            mouse_relative: true,
            gamepads: true,
            ..Default::default()
        }
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

fn test_backends(
    injected: Arc<Mutex<Vec<InputEvent>>>,
    host_clipboard: Option<MemoryClipboard>,
) -> HostBackends {
    HostBackends {
        capture: Arc::new(|_| Ok(Box::new(SynthCapture { frame_n: 0 }))),
        encoder: Arc::new(|cfg, _| veloxdesk_codec::create_encoder(cfg, false)),
        injector: Arc::new(move || Ok(Box::new(RecordingInjector { events: injected.clone() }))),
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
        audio_capture: None,
        audio_encoder: None,
        audio_playback: None,
        clipboard: host_clipboard.map(|mem| {
            Arc::new(move || -> Result<Box<dyn veloxdesk_clipboard::ClipboardBackend>> {
                Ok(Box::new(mem.clone()))
            }) as Arc<veloxdesk_service::ClipboardFactory>
        }),
        privacy: None,
        file_transfer: None,
        tunnel_policy: None,
    }
}

// ───────────────────────── Callback Recorder ─────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedCursorShape {
    shape_id: u64,
    width: u32,
    height: u32,
    hotspot_x: u32,
    hotspot_y: u32,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct RecordedCallbacks {
    statuses: Mutex<Vec<(i32, String)>>,
    auth_prompts: Mutex<Vec<(String, String, bool, bool)>>,
    configs: Mutex<Vec<(SessionConfig, String)>>,
    video_frames: AtomicUsize,
    keyframes: AtomicUsize,
    last_frame_bytes: Mutex<Vec<u8>>,
    audio_callbacks: AtomicUsize,
    audio_nonzero_samples: AtomicUsize,
    stats_events: Mutex<Vec<HudMetrics>>,
    clipboard_texts: Mutex<Vec<String>>,
    cursor_shapes: Mutex<Vec<RecordedCursorShape>>,
    cursor_positions: Mutex<Vec<(i32, i32, bool, u64)>>,
    closed: Mutex<Option<(String, bool)>>,
    closed_flag: AtomicBool,
}

impl SessionCallbacks for RecordedCallbacks {
    fn on_status(&self, status_code: i32, detail: &str) {
        self.statuses.lock().unwrap().push((status_code, detail.to_string()));
    }

    fn on_auth_required(&self, host_device_id: &str, host_fingerprint: &str, allow_perm: bool, allow_otp: bool) {
        self.auth_prompts.lock().unwrap().push((
            host_device_id.to_string(),
            host_fingerprint.to_string(),
            allow_perm,
            allow_otp,
        ));
    }

    fn on_session_config(&self, cfg: &SessionConfig, displays_json: &str) {
        self.configs.lock().unwrap().push((cfg.clone(), displays_json.to_string()));
    }

    fn on_display_list(&self, _displays_json: &str) {}

    fn on_video_frame(
        &self,
        _codec: i32,
        _width: u32,
        _height: u32,
        _frame_id: u32,
        _capture_ts_us: i64,
        keyframe: bool,
        _discontinuity: bool,
        data: &[u8],
    ) {
        self.video_frames.fetch_add(1, Ordering::Relaxed);
        if keyframe {
            self.keyframes.fetch_add(1, Ordering::Relaxed);
        }
        if !data.is_empty() {
            *self.last_frame_bytes.lock().unwrap() = data.to_vec();
        }
    }

    fn on_audio_pcm(&self, sample_rate: i32, channels: i32, pcm16: &[i16]) {
        assert_eq!(sample_rate, 48_000);
        assert_eq!(channels, 2);
        self.audio_callbacks.fetch_add(1, Ordering::Relaxed);
        let nz = pcm16.iter().filter(|&&s| s.abs() > 100).count();
        self.audio_nonzero_samples.fetch_add(nz, Ordering::Relaxed);
    }

    fn on_stats(&self, stats: &HudMetrics) {
        self.stats_events.lock().unwrap().push(*stats);
    }

    fn on_clipboard_text(&self, text: &str) {
        self.clipboard_texts.lock().unwrap().push(text.to_string());
    }

    fn on_rumble(&self, _pad_id: i32, _low_freq: i32, _high_freq: i32) {}

    fn on_cursor_shape(&self, shape_id: u64, width: u32, height: u32, hotspot_x: u32, hotspot_y: u32, rgba: &[u8]) {
        self.cursor_shapes.lock().unwrap().push(RecordedCursorShape {
            shape_id,
            width,
            height,
            hotspot_x,
            hotspot_y,
            rgba: rgba.to_vec(),
        });
    }

    fn on_cursor_position(&self, x: i32, y: i32, visible: bool, shape_id: u64) {
        self.cursor_positions.lock().unwrap().push((x, y, visible, shape_id));
    }

    fn on_closed(&self, reason: &str, is_error: bool) {
        *self.closed.lock().unwrap() = Some((reason.to_string(), is_error));
        self.closed_flag.store(true, Ordering::Release);
    }
}

fn temp_files_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "veloxdesk-android-bridge-{tag}-{}-{}",
        std::process::id(),
        Timestamp::now().as_micros()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn make_connect_config(
    files_dir: PathBuf,
    target: String,
    password: Option<String>,
    clipboard_enabled: bool,
) -> ConnectConfig {
    ConnectConfig {
        files_dir,
        target,
        rendezvous_addr: String::new(),
        relay_only: false,
        password: password.map(SecretString::from),
        mode: constants::MODE_DESKTOP,
        max_bitrate_kbps: 2_000,
        audio_enabled: false,
        clipboard_enabled,
        viewer_width: 1280,
        viewer_height: 720,
        viewer_refresh_hz: 60,
        device_name: "Android-Test-Phone".into(),
        codec_caps_mask: constants::CAP_H264_HW | constants::CAP_H264_SW,
    }
}

async fn wait_until<F: Fn() -> bool>(timeout: Duration, cond: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    cond()
}

fn fast_argon2() -> Argon2Params {
    Argon2Params { m_cost_kib: 1024, t_cost: 1, p_cost: 1 }
}

// ───────────────────────── Test 1: Auth Modes (OTP, Permanent, Interactive, Trusted PubKey) ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_auth_otp_permanent_interactive_and_trusted_pubkey() {
    let host_id = DeviceIdentity::generate();
    let files_dir = temp_files_dir("auth");
    let viewer_id = load_or_create_identity(&files_dir).expect("create viewer identity");
    assert_eq!(viewer_id.device_id().len(), 9);

    let injected = Arc::new(Mutex::new(Vec::new()));
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
        auth: HostAuthConfig::with_auto_otp(),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id.clone(), test_backends(injected, None), None)
        .await
        .expect("RunningHost::start");
    let target = host.local_addr().to_string();
    let table = Arc::new(HandleTable::new());

    // 1. Pre-supplied OTP login.
    let otp = host.auth_state().current_otp().expect("auto OTP");
    let cb1 = Arc::new(RecordedCallbacks::default());
    let h1 = table
        .spawn_session(make_connect_config(files_dir.clone(), target.clone(), Some(otp), false), cb1.clone())
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(6), || !cb1.configs.lock().unwrap().is_empty()).await,
        "OTP login should receive SessionConfig; closed={:?}",
        *cb1.closed.lock().unwrap()
    );
    table.remove(h1).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb1.closed_flag.load(Ordering::Acquire)).await);
    assert_eq!(cb1.closed.lock().unwrap().as_ref().map(|(_, err)| *err), Some(false));

    // 2. Permanent password with interactive `onAuthRequired` -> `submit_password`.
    let perm_secret = "Android-Perm-2026!";
    let perm_hash = PasswordHash::hash_with(perm_secret, fast_argon2()).unwrap();
    host.auth_state().set_permanent_password_hash(Some(perm_hash));
    host.auth_state().set_otp_enabled(false);

    let cb2 = Arc::new(RecordedCallbacks::default());
    let h2 = table
        .spawn_session(make_connect_config(files_dir.clone(), target.clone(), None, false), cb2.clone())
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || !cb2.auth_prompts.lock().unwrap().is_empty()).await,
        "expected onAuthRequired callback"
    );
    {
        let prompts = cb2.auth_prompts.lock().unwrap();
        assert_eq!(prompts[0].0, host_id.device_id());
        assert_eq!(prompts[0].1, host_id.fingerprint());
        assert!(prompts[0].2, "allow_perm should be true");
        assert!(!prompts[0].3, "allow_otp should be false");
    }
    table.get(h2).unwrap().submit_password(perm_secret);
    assert!(
        wait_until(Duration::from_secs(6), || !cb2.configs.lock().unwrap().is_empty()).await,
        "interactive permanent password login must succeed; closed={:?}",
        *cb2.closed.lock().unwrap()
    );
    table.remove(h2).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb2.closed_flag.load(Ordering::Acquire)).await);

    // 3. Trusted Ed25519 passwordless PublicKey auth.
    host.auth_state().add_trusted_peer(TrustedPeer {
        public_key: viewer_id.public_key(),
        name: "Android-Test-Phone".into(),
        permissions: Permissions::all(),
    });
    let cb3 = Arc::new(RecordedCallbacks::default());
    let h3 = table
        .spawn_session(make_connect_config(files_dir.clone(), target.clone(), None, false), cb3.clone())
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(6), || !cb3.configs.lock().unwrap().is_empty()).await,
        "trusted Ed25519 peer must connect passwordless; closed={:?}",
        *cb3.closed.lock().unwrap()
    );
    assert!(cb3.auth_prompts.lock().unwrap().is_empty(), "trusted peer must not prompt for password");
    table.remove(h3).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb3.closed_flag.load(Ordering::Acquire)).await);

    host.stop();
    let _ = std::fs::remove_dir_all(&files_dir);
}

// ───────────────────────── Test 2, 3, 4: H.264 Video + HUD Stats + Input/Gamepad + Instant Reconnect ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_video_stats_input_gamepad_and_instant_reconnect() {
    let host_id = DeviceIdentity::generate();
    let files_dir = temp_files_dir("media-input");
    let injected = Arc::new(Mutex::new(Vec::new()));

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
        auth: HostAuthConfig::open(),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id, test_backends(injected.clone(), None), None)
        .await
        .expect("RunningHost::start");
    let target = host.local_addr().to_string();
    let table = Arc::new(HandleTable::new());

    // Session #1: verify SessionConfig, real H.264 frames, HUD stats, mouse/key/gamepad input.
    let cb1 = Arc::new(RecordedCallbacks::default());
    let h1 = table
        .spawn_session(make_connect_config(files_dir.clone(), target.clone(), None, false), cb1.clone())
        .unwrap();

    assert!(wait_until(Duration::from_secs(6), || !cb1.configs.lock().unwrap().is_empty()).await);
    {
        let cfgs = cb1.configs.lock().unwrap();
        let (sc, json) = &cfgs[0];
        let v = sc.video.as_ref().expect("video config");
        assert_eq!(v.codec, constants::CODEC_H264);
        assert_eq!(v.width, 320);
        assert_eq!(v.height, 180);
        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
        assert_eq!(parsed[0]["width"], 320);
    }

    assert!(
        wait_until(Duration::from_secs(6), || cb1.video_frames.load(Ordering::Relaxed) >= 5).await,
        "expected >= 5 real H.264 frames, got {}",
        cb1.video_frames.load(Ordering::Relaxed)
    );
    assert!(cb1.keyframes.load(Ordering::Relaxed) >= 1, "expected at least 1 H.264 keyframe");
    assert!(!cb1.last_frame_bytes.lock().unwrap().is_empty(), "H.264 bitstream must be non-empty");

    let handle = table.get(h1).unwrap();
    handle.report_decode_time_us(2_200, 800);
    handle.request_keyframe();

    // Wait for HUD stats publish (~250ms cadence).
    assert!(
        wait_until(Duration::from_secs(3), || !cb1.stats_events.lock().unwrap().is_empty()).await,
        "expected HUD onStats callbacks"
    );
    {
        let stats = cb1.stats_events.lock().unwrap();
        let last = stats.last().unwrap();
        assert_eq!(last.path_kind, constants::PATH_DIRECT);
        assert!(last.decode_ms >= 2.0, "reported decode+render time should reflect in HUD: {last:?}");
    }

    // Send mouse, keyboard, and gamepad events and verify host InputInjector receives them!
    handle.send_mouse_move(120, 85, 0);
    handle.send_mouse_button(0, true);
    handle.send_mouse_button(0, false);
    handle.send_key(0x04, true, 0); // HID 'a'
    handle.send_key(0x04, false, 0);
    handle.send_gamepad(0, true, 0x1000, 128, 0, 12_000, -8_000, 0, 0);

    assert!(
        wait_until(Duration::from_secs(4), || {
            let evs = injected.lock().unwrap();
            let has_mouse = evs.iter().any(|e| matches!(e.event, Some(input_event::Event::MouseMove(_))));
            let has_key = evs.iter().any(|e| matches!(e.event, Some(input_event::Event::Key(_))));
            let has_gp = evs.iter().any(|e| matches!(e.event, Some(input_event::Event::Gamepad(_))));
            has_mouse && has_key && has_gp
        })
        .await,
        "host must receive mouse, key, and gamepad events; got {:?}",
        injected.lock().unwrap().len()
    );

    // Graceful Disconnect -> QUIC CONNECTION_CLOSE flush -> immediate 1st-attempt reconnect!
    table.remove(h1).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb1.closed_flag.load(Ordering::Acquire)).await);
    assert_eq!(cb1.closed.lock().unwrap().as_ref().map(|(_, err)| *err), Some(false));

    // Reconnect immediately on the 1st attempt (host max_sessions = 1!).
    let cb2 = Arc::new(RecordedCallbacks::default());
    let h2 = table
        .spawn_session(make_connect_config(files_dir.clone(), target.clone(), None, false), cb2.clone())
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(6), || {
            !cb2.configs.lock().unwrap().is_empty() && cb2.video_frames.load(Ordering::Relaxed) >= 2
        })
        .await,
        "1st-attempt reconnect after graceful disconnect must succeed; closed={:?}",
        *cb2.closed.lock().unwrap()
    );

    table.remove(h2).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb2.closed_flag.load(Ordering::Acquire)).await);
    assert!(table.is_empty());

    host.stop();
    let _ = std::fs::remove_dir_all(&files_dir);
}

// ───────────────────────── Test 5: Bidirectional Clipboard Sync (incl. Host zstd -> Viewer) ─────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_bidirectional_clipboard_sync_with_host_zstd() {
    let host_id = DeviceIdentity::generate();
    let files_dir = temp_files_dir("clipboard");
    let injected = Arc::new(Mutex::new(Vec::new()));
    let host_clip = MemoryClipboard::new();

    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 20,
        bitrate_kbps: 1_500,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: false,
        max_sessions: 1,
        ipc_port: None,
        auth: HostAuthConfig::open(),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id, test_backends(injected, Some(host_clip.clone())), None)
        .await
        .expect("RunningHost::start");
    let target = host.local_addr().to_string();
    let table = Arc::new(HandleTable::new());

    let cb = Arc::new(RecordedCallbacks::default());
    let h = table
        .spawn_session(make_connect_config(files_dir.clone(), target, None, true), cb.clone())
        .unwrap();
    assert!(wait_until(Duration::from_secs(6), || !cb.configs.lock().unwrap().is_empty()).await);

    // Give the Clipboard stream a brief moment to open on host.
    tokio::time::sleep(Duration::from_millis(120)).await;

    // 1. Host copies repetitive text -> `veloxdesk-clipboard` sends zstd-compressed ClipboardData -> Android decompresses via `ruzstd`.
    let from_host = "Host zstd clipboard payload! ".repeat(20);
    host_clip.set(Some(ClipboardItem::Text(from_host.clone())));
    assert!(
        wait_until(Duration::from_secs(4), || {
            cb.clipboard_texts.lock().unwrap().iter().any(|t| t == &from_host)
        })
        .await,
        "viewer must receive zstd-decompressed host clipboard text; got {:?}",
        *cb.clipboard_texts.lock().unwrap()
    );

    // 2. Android viewer sends text -> Host MemoryClipboard receives it without echo loop.
    let from_android = "Copied from Android phone 📱";
    table.get(h).unwrap().send_clipboard_text(from_android.into());
    assert!(
        wait_until(Duration::from_secs(4), || {
            host_clip.get() == Some(ClipboardItem::Text(from_android.into()))
        })
        .await,
        "host must receive Android clipboard text; got {:?}",
        host_clip.get()
    );

    // Confirm no echo back to Android.
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        cb.clipboard_texts.lock().unwrap().iter().filter(|t| t.as_str() == from_android).count(),
        0,
        "clipboard must suppress echo loops"
    );

    table.remove(h).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb.closed_flag.load(Ordering::Acquire)).await);
    host.stop();
    let _ = std::fs::remove_dir_all(&files_dir);
}

// ───────────────────────── Test 6: Live Opus Audio Decode -> PCM16 Callback ─────────────────────────

struct SynthSineAudio {
    phase: usize,
}

impl AudioCapture for SynthSineAudio {
    fn backend(&self) -> &'static str {
        "synth-sine-android-test"
    }

    fn start(&mut self, _cfg: &AudioCaptureConfig) -> Result<AudioFormat> {
        Ok(AudioFormat::STEREO_48K)
    }

    fn read(&mut self, timeout: Duration) -> Result<Option<AudioFrame>> {
        std::thread::sleep(timeout.min(Duration::from_millis(5)));
        let spc = 240usize; // 5 ms @ 48 kHz
        let mut samples = Vec::with_capacity(spc * 2);
        for i in 0..spc {
            let t = (self.phase + i) as f32 / 48_000.0;
            let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            samples.push(v);
            samples.push(v);
        }
        self.phase = self.phase.wrapping_add(spc);
        Ok(Some(AudioFrame {
            format: AudioFormat::STEREO_48K,
            samples,
            capture_ts: Timestamp::now(),
        }))
    }

    fn stop(&mut self) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_opus_audio_pcm16_stream() {
    let host_id = DeviceIdentity::generate();
    let files_dir = temp_files_dir("audio");
    let injected = Arc::new(Mutex::new(Vec::new()));

    let mut backends = test_backends(injected, None);
    backends.audio_capture = Some(Arc::new(|| Ok(Box::new(SynthSineAudio { phase: 0 }))));
    backends.audio_encoder = Some(Arc::new(|cfg| Ok(Box::new(veloxdesk_audio::OpusEncoder::new(cfg)?))));

    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 20,
        bitrate_kbps: 1_500,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: true,
        audio_bitrate_kbps: 96,
        audio_frame_ms: 5.0,
        max_sessions: 1,
        ipc_port: None,
        auth: HostAuthConfig::open(),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id, backends, None)
        .await
        .expect("RunningHost::start");
    let target = host.local_addr().to_string();
    let table = Arc::new(HandleTable::new());

    let mut connect_cfg = make_connect_config(files_dir.clone(), target, None, false);
    connect_cfg.audio_enabled = true;

    let cb = Arc::new(RecordedCallbacks::default());
    let h = table.spawn_session(connect_cfg, cb.clone()).unwrap();

    assert!(
        wait_until(Duration::from_secs(5), || {
            cb.audio_callbacks.load(Ordering::Relaxed) >= 5
                && cb.audio_nonzero_samples.load(Ordering::Relaxed) > 100
        })
        .await,
        "expected decoded non-zero PCM16 audio callbacks; callbacks={}, nz={}",
        cb.audio_callbacks.load(Ordering::Relaxed),
        cb.audio_nonzero_samples.load(Ordering::Relaxed)
    );

    table.remove(h).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb.closed_flag.load(Ordering::Acquire)).await);
    host.stop();
    let _ = std::fs::remove_dir_all(&files_dir);
}

// ───────────────────────── Test 7: Live CursorShape & CursorPosition Delivery ─────────────────────────

struct CursorSynthCapture {
    frame_n: u32,
}

impl FrameCapture for CursorSynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps {
            backend: "synth-cursor-android-test",
            formats: vec![PixelFormat::Bgra8],
            separate_cursor: true,
            ..Default::default()
        }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("ANDROID-CURSOR-TEST".into()),
            index: 0,
            name: "Synth Cursor 320x180".into(),
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
        let buf = vec![0x22u8; 320 * 180 * 4];
        let frame = Frame {
            width: 320,
            height: 180,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::from(buf), stride: 320 * 4 }]),
            capture_ts: Timestamp::now(),
        };
        let shape = if self.frame_n <= 2 {
            // 4x4 white RGBA cursor shape with hotspot (1, 2)
            Some(CursorImage {
                id: 77,
                width: 4,
                height: 4,
                hotspot_x: 1,
                hotspot_y: 2,
                rgba: Bytes::from(vec![0xFFu8; 4 * 4 * 4]),
            })
        } else {
            None
        };
        let cursor = Some(CursorState {
            x: 100 + (self.frame_n as i32),
            y: 50 + (self.frame_n as i32),
            visible: true,
            shape,
        });
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor, capture_us: 20, convert_us: 5 }))
    }

    fn stop(&mut self) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_cursor_shape_and_position_delivery() {
    let host_id = DeviceIdentity::generate();
    let files_dir = temp_files_dir("cursor");
    let injected = Arc::new(Mutex::new(Vec::new()));

    let mut backends = test_backends(injected, None);
    backends.capture = Arc::new(|_| Ok(Box::new(CursorSynthCapture { frame_n: 0 })));

    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 30,
        bitrate_kbps: 1_500,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        allow_input: true,
        audio_enabled: false,
        max_sessions: 1,
        ipc_port: None,
        auth: HostAuthConfig::open(),
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id, backends, None)
        .await
        .expect("RunningHost::start");
    let target = host.local_addr().to_string();
    let table = Arc::new(HandleTable::new());

    let connect_cfg = make_connect_config(files_dir.clone(), target, None, false);
    let cb = Arc::new(RecordedCallbacks::default());
    let h = table.spawn_session(connect_cfg, cb.clone()).unwrap();

    assert!(
        wait_until(Duration::from_secs(5), || {
            !cb.cursor_shapes.lock().unwrap().is_empty() && !cb.cursor_positions.lock().unwrap().is_empty()
        })
        .await,
        "expected CursorShape and CursorPosition callbacks from live RunningHost; shapes={}, pos={}",
        cb.cursor_shapes.lock().unwrap().len(),
        cb.cursor_positions.lock().unwrap().len()
    );

    let shape = cb.cursor_shapes.lock().unwrap()[0].clone();
    assert_eq!(shape.shape_id, 77);
    assert_eq!((shape.width, shape.height, shape.hotspot_x, shape.hotspot_y), (4, 4, 1, 2));
    assert_eq!(shape.rgba, vec![0xFFu8; 4 * 4 * 4]);

    let (cx, cy, visible, pos_shape_id) = cb.cursor_positions.lock().unwrap()[0];
    assert!(visible);
    assert!(cx >= 101 && cy >= 51);
    assert_eq!(pos_shape_id, 77);

    table.remove(h).unwrap().disconnect();
    assert!(wait_until(Duration::from_secs(3), || cb.closed_flag.load(Ordering::Acquire)).await);
    host.stop();
    let _ = std::fs::remove_dir_all(&files_dir);
}
