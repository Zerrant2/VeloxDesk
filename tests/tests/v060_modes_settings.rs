//! VeloxDesk v0.6.0 Integration Tests:
//! - Deep Desktop vs Gaming profile defaults, `reset_profile_defaults`, host permission mapping,
//!   and v0.5.0 legacy JSON backward-compatible migration.
//! - Pure-Rust microphone background noise suppressor (`NoiseSuppressor`) achieving >= 12 dB SNR
//!   improvement on stationary noise while preserving speech, plus combined
//!   `NoiseSuppressor -> AcousticEchoCanceller` chain.
//! - Host Desktop vs Gaming differentiation (`10 ms` vs `5 ms` Opus, base vs `>= 15%` FEC parity,
//!   `draw_cursor = true` vs `false`) and live mid-session `Msg::SessionModeUpdate` renegotiation
//!   without disconnecting.
//! - Live host controls: `RunningHost::set_default_permissions` (including dynamically enabling
//!   `allow_power = true`), `set_echo_cancellation`, and `set_privacy_mode_kind`
//!   (`CurtainOverlay` vs `VddMonitorOff`).
//! - Headless `egui` Settings window rendering audit across EN & RU and Dark & Light themes
//!   verifying all new v0.6.0 sections render cleanly with zero missing/forbidden glyphs.
//!
//! NOTE: QUIC loopback tests (`v060_host_live_session_mode_update_desktop_and_gaming` and
//! `v060_running_host_live_permissions_aec_and_privacy_mode`) require unsandboxed loopback UDP on
//! Windows. All non-UDP tests run inside the sandbox cleanly.

use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::time::timeout;

use veloxdesk_audio::{AcousticEchoCanceller, AecConfig, AecReferenceFeed, DenoiseConfig, NoiseSuppressor, OpusEncoder};
use veloxdesk_core_traits::audio::{
    AudioCapture, AudioCaptureConfig, AudioCodecId, AudioEncoder, AudioFormat, AudioFrame,
};
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
    AudioCodec, AuthMethod, AuthResponse, AuthResult, Close, Hello, InputEvent, Permissions, Ping, SessionConfig,
    SessionMode, SessionModeUpdate, VideoCodecCap, envelope::Msg,
};
use veloxdesk_service::audio::{negotiate_audio_frame_us, normalize_opus_frame_us};
use veloxdesk_service::session::negotiate_params;
use veloxdesk_service::{
    HostAuthConfig, HostAuthState, HostBackends, HostConfig, RunningHost, TransportAbr, TransportVideoSink,
};
use veloxdesk_transport::{AbrController, ControlReceiver, ControlSender, VideoSender, framed};
use veloxdesk_ui::{
    AudioFrameLatency, FpsCap, HostPrivacyMode, Language, Mode, PasswordMode, Present, Quality, SavedTrustedPeer,
    Settings, StreamSettings, ThemePreference, UpdateUiState, settings_window_with_security, tr,
};
use veloxdesk_virtual_display::{PrivacyController, PrivacyModeKind, SimulatedPrivacyController};

// ---------------------------------------------------------------------------
// 1. Desktop vs Gaming Profile Defaults, Reset & v0.5.0 Legacy JSON Migration
// ---------------------------------------------------------------------------

#[test]
fn v060_desktop_vs_gaming_profile_defaults_reset_and_v050_migration() {
    // A. Desktop defaults: office clarity, 60 FPS, VSync/Fifo, YUV 4:4:4, HUD off, AEC+NS on, Auto (10 ms).
    let desk = StreamSettings::default_for(Mode::Desktop);
    assert_eq!(desk.quality, Quality::Balanced);
    assert_eq!(desk.fps_cap, FpsCap::Fps60);
    assert_eq!(desk.present, Present::Fifo);
    assert!(!desk.show_hud);
    assert!(!desk.relative_mouse_in_gaming);
    assert!(desk.yuv444);
    assert!(desk.audio_enabled);
    assert!(!desk.mic_enabled);
    assert!(desk.clipboard_sync);
    assert!(desk.echo_cancellation);
    assert!(desk.noise_suppression);
    assert_eq!(desk.audio_frame_latency, AudioFrameLatency::Auto);
    assert_eq!(desk.audio_frame_latency.to_wire_us(), 0);
    assert_eq!(desk.audio_frame_latency.effective_us(Mode::Desktop), 10_000);

    // B. Gaming defaults: High bitrate, 120 FPS, Mailbox, HUD on, relative mouse on, YUV 4:2:0, Auto (5 ms).
    let game = StreamSettings::default_for(Mode::Gaming);
    assert_eq!(game.quality, Quality::High);
    assert_eq!(game.fps_cap, FpsCap::Fps120);
    assert_eq!(game.present, Present::Mailbox);
    assert!(game.show_hud);
    assert!(game.relative_mouse_in_gaming);
    assert!(!game.yuv444);
    assert!(game.audio_enabled);
    assert!(!game.mic_enabled);
    assert!(game.clipboard_sync);
    assert!(game.echo_cancellation);
    assert!(game.noise_suppression);
    assert_eq!(game.audio_frame_latency, AudioFrameLatency::Auto);
    assert_eq!(game.audio_frame_latency.effective_us(Mode::Gaming), 5_000);

    // AudioFrameLatency explicit wire/effective values.
    assert_eq!(AudioFrameLatency::Ms2_5.to_wire_us(), 2_500);
    assert_eq!(AudioFrameLatency::Ms5.to_wire_us(), 5_000);
    assert_eq!(AudioFrameLatency::Ms10.to_wire_us(), 10_000);
    assert_eq!(AudioFrameLatency::Ms20.to_wire_us(), 20_000);
    assert_eq!(AudioFrameLatency::Ms2_5.effective_us(Mode::Desktop), 2_500);
    assert_eq!(AudioFrameLatency::Ms20.effective_us(Mode::Gaming), 20_000);

    // C. reset_profile_defaults resets ONLY the requested profile without disturbing the other.
    let mut settings = Settings::default();
    assert!(settings.auto_reconnect);
    assert!(settings.host_allow_input);
    assert!(settings.host_allow_clipboard);
    assert!(settings.host_allow_file_transfer);
    assert!(settings.host_allow_audio);
    assert!(settings.host_allow_chat);
    assert!(settings.host_allow_terminal_tunnel);
    assert!(!settings.host_allow_power);
    assert!(settings.host_echo_exclusion);
    assert_eq!(settings.host_privacy_mode, HostPrivacyMode::CurtainOverlay);

    settings.desktop.quality = Quality::Ultra;
    settings.desktop.fps_cap = FpsCap::Fps240;
    settings.desktop.show_hud = true;
    settings.desktop.yuv444 = false;
    settings.desktop.echo_cancellation = false;
    settings.desktop.noise_suppression = false;
    settings.desktop.audio_frame_latency = AudioFrameLatency::Ms20;

    settings.gaming.quality = Quality::Low;
    settings.gaming.fps_cap = FpsCap::Fps60;
    settings.gaming.present = Present::Fifo;
    settings.gaming.show_hud = false;
    settings.gaming.echo_cancellation = false;
    settings.gaming.audio_frame_latency = AudioFrameLatency::Ms2_5;
    let modified_gaming = settings.gaming;

    settings.reset_profile_defaults(Mode::Desktop);
    assert_eq!(settings.desktop, StreamSettings::default_for(Mode::Desktop));
    assert_eq!(settings.gaming, modified_gaming, "resetting Desktop must not touch Gaming");

    settings.reset_profile_defaults(Mode::Gaming);
    assert_eq!(settings.gaming, StreamSettings::default_for(Mode::Gaming));

    // D. host_default_permissions maps host toggles cleanly (including allow_power).
    let default_perms = settings.host_default_permissions();
    assert!(default_perms.view);
    assert!(default_perms.input);
    assert!(default_perms.clipboard);
    assert!(default_perms.file_transfer);
    assert!(default_perms.audio);
    assert!(default_perms.allow_chat);
    assert!(default_perms.terminal && default_perms.tunnel);
    assert!(!default_perms.allow_power);

    settings.host_allow_power = true;
    settings.host_allow_clipboard = false;
    settings.host_allow_terminal_tunnel = false;
    let custom_perms = settings.host_default_permissions();
    assert!(custom_perms.allow_power);
    assert!(custom_perms.restart);
    assert!(!custom_perms.clipboard);
    assert!(!custom_perms.terminal && !custom_perms.tunnel);

    // E. v0.5.0 legacy settings.json migration: top-level `echo_cancellation: false` migrates
    //    to both profiles and `host_echo_exclusion`, while new v0.6.0 fields take clean defaults.
    let v050_json = r#"{
        "language": "Ru",
        "theme": "Dark",
        "recent": ["192.168.1.10:21118"],
        "rendezvous_server": "rdv.example.com:21116",
        "mode": "Gaming",
        "desktop": {
            "quality": "Balanced",
            "fps_cap": "Fps60",
            "present": "Fifo",
            "scale": "Fit",
            "show_hud": false,
            "prefer_hw_decode": true,
            "relative_mouse_in_gaming": false,
            "hdr_10bit": false,
            "yuv444": true,
            "audio_enabled": true,
            "mic_enabled": false,
            "clipboard_sync": true
        },
        "gaming": {
            "quality": "High",
            "fps_cap": "Fps120",
            "present": "Mailbox",
            "scale": "Fit",
            "show_hud": true,
            "prefer_hw_decode": true,
            "relative_mouse_in_gaming": true,
            "hdr_10bit": false,
            "yuv444": false,
            "audio_enabled": true,
            "mic_enabled": true,
            "clipboard_sync": true
        },
        "require_accept": true,
        "password_mode": "Both",
        "permanent_password_hash": null,
        "trusted_peers": [],
        "echo_cancellation": false,
        "recordings_dir": "C:\\Captures"
    }"#;
    let migrated: Settings = serde_json::from_str(v050_json).expect("v0.5.0 JSON must deserialize");
    assert_eq!(migrated.language, Language::Ru);
    assert_eq!(migrated.theme, ThemePreference::Dark);
    assert_eq!(migrated.mode, Mode::Gaming);
    assert!(!migrated.desktop.echo_cancellation, "legacy top-level echo_cancellation=false must migrate to Desktop");
    assert!(!migrated.gaming.echo_cancellation, "v0.6.0 must NOT force-lock echo_cancellation in Gaming");
    assert!(!migrated.effective_echo_cancellation());
    assert!(!migrated.host_echo_exclusion, "legacy top-level echo_cancellation=false must migrate to host_echo_exclusion");
    assert!(migrated.desktop.noise_suppression);
    assert!(migrated.gaming.noise_suppression);
    assert_eq!(migrated.desktop.audio_frame_latency, AudioFrameLatency::Auto);
    assert_eq!(migrated.gaming.audio_frame_latency, AudioFrameLatency::Auto);
    assert!(migrated.auto_reconnect);
    assert!(migrated.host_allow_input);
    assert!(!migrated.host_allow_power);
    assert_eq!(migrated.host_privacy_mode, HostPrivacyMode::CurtainOverlay);
    assert_eq!(migrated.recordings_dir, r"C:\Captures");

    // F. Disk save/load round-trip preserves every v0.6.0 field.
    let dir = std::env::temp_dir().join(format!("veloxdesk-v060-settings-{}", std::process::id()));
    let path = dir.join("settings.json");
    let mut saved = migrated;
    saved.auto_reconnect = false;
    saved.host_allow_power = true;
    saved.host_privacy_mode = HostPrivacyMode::VddMonitorOff;
    saved.gaming.noise_suppression = false;
    saved.gaming.audio_frame_latency = AudioFrameLatency::Ms5;
    saved.save(&path).expect("save v0.6.0 settings");
    let reloaded = Settings::load(&path).expect("load v0.6.0 settings");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(reloaded, saved);
}

// ---------------------------------------------------------------------------
// 2. Pure-Rust Noise Suppression (>= 12 dB SNR) + AEC Chain
// ---------------------------------------------------------------------------

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

struct XorShift64(u64);
impl XorShift64 {
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let u = (self.0 >> 40) as f32 / ((1u32 << 24) as f32);
        u * 2.0 - 1.0
    }
}

#[test]
fn v060_noise_suppressor_snr_improvement_speech_preservation_and_aec_chain() {
    const SR: u32 = 48_000;
    const FRAME: usize = 480; // 10 ms mono

    let mut ns = NoiseSuppressor::new(DenoiseConfig {
        sample_rate: SR,
        channels: 1,
        attenuation_db: -24.0,
        vad_threshold_db: 6.0,
        attack_ms: 3.0,
        release_ms: 120.0,
    });
    let mut rng = XorShift64(0x9E37_79B9_7F4A_7C15);

    // Phase 1: 400 ms stationary white noise warm-up (noise floor converges).
    let mut noise_in_rms_acc = Vec::new();
    let mut noise_out_rms_acc = Vec::new();
    for frame_idx in 0..40usize {
        let mut buf: Vec<f32> = (0..FRAME).map(|_| rng.next_f32() * 0.04).collect();
        let in_r = rms(&buf);
        ns.process(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()), "NS output must be finite");
        if frame_idx >= 25 {
            noise_in_rms_acc.push(in_r);
            noise_out_rms_acc.push(rms(&buf));
        }
    }
    assert!(ns.noise_floor_rms() > 0.005, "noise floor estimator must track stationary noise");
    assert!(!ns.is_speech_active(), "stationary noise alone must not keep VAD active");

    // Phase 2: 400 ms speech-like multi-harmonic signal + same stationary noise.
    let mut speech_clean_acc = Vec::new();
    let mut speech_in_acc = Vec::new();
    let mut speech_out_acc = Vec::new();
    let mut saw_speech_active = false;
    for frame_idx in 0..40usize {
        let mut clean = Vec::with_capacity(FRAME);
        let mut buf = Vec::with_capacity(FRAME);
        for i in 0..FRAME {
            let t = (frame_idx * FRAME + i) as f32 / SR as f32;
            let voice = 0.25 * (TAU * 320.0 * t).sin()
                + 0.16 * (TAU * 640.0 * t).sin()
                + 0.09 * (TAU * 1280.0 * t).sin();
            let noise = rng.next_f32() * 0.04;
            clean.push(voice);
            buf.push(voice + noise);
        }
        let in_r = rms(&buf);
        let clean_r = rms(&clean);
        ns.process(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite()), "NS output during speech must be finite");
        if ns.is_speech_active() {
            saw_speech_active = true;
        }
        if frame_idx >= 5 {
            speech_clean_acc.push(clean_r);
            speech_in_acc.push(in_r);
            speech_out_acc.push(rms(&buf));
        }
    }
    assert!(saw_speech_active, "VAD must activate during speech burst");

    // Phase 3: 400 ms stationary noise tail (after release, noise must drop back down cleanly).
    let mut tail_in_acc = Vec::new();
    let mut tail_out_acc = Vec::new();
    for frame_idx in 0..40usize {
        let mut buf: Vec<f32> = (0..FRAME).map(|_| rng.next_f32() * 0.04).collect();
        let in_r = rms(&buf);
        ns.process(&mut buf);
        if frame_idx >= 25 {
            tail_in_acc.push(in_r);
            tail_out_acc.push(rms(&buf));
        }
    }
    assert!(!ns.is_speech_active(), "VAD must release after speech ends");

    let mean = |xs: &[f32]| xs.iter().copied().sum::<f32>() / xs.len() as f32;
    let speech_clean_rms = mean(&speech_clean_acc);
    let speech_in_rms = mean(&speech_in_acc);
    let speech_out_rms = mean(&speech_out_acc);
    let noise_in_rms = (mean(&noise_in_rms_acc) + mean(&tail_in_acc)) * 0.5;
    let noise_out_rms = (mean(&noise_out_rms_acc) + mean(&tail_out_acc)) * 0.5;

    let snr_in_db = 20.0 * (speech_in_rms / noise_in_rms.max(1e-7)).log10();
    let snr_out_db = 20.0 * (speech_out_rms / noise_out_rms.max(1e-7)).log10();
    let snr_improvement_db = snr_out_db - snr_in_db;

    assert!(
        snr_improvement_db >= 12.0,
        "NoiseSuppressor SNR improvement must be >= 12 dB (got {snr_improvement_db:.2} dB: in {snr_in_db:.2} -> out {snr_out_db:.2})"
    );
    assert!(
        speech_out_rms / speech_clean_rms >= 0.75,
        "speech must be preserved without heavy attenuation (ratio {:.3})",
        speech_out_rms / speech_clean_rms
    );
    assert!(
        ns.last_suppression_db() >= 12.0,
        "last_suppression_db must report >= 12 dB in noise tail (got {:.2} dB)",
        ns.last_suppression_db()
    );

    // Sanitization check for NaN / Inf / overload.
    let mut bad = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 5.0, -5.0, 0.0];
    ns.process(&mut bad);
    assert!(bad.iter().all(|s| s.is_finite() && s.abs() <= 1.0), "NS must sanitize NaN/Inf and clamp overload");

    // Combined chain: NoiseSuppressor -> AcousticEchoCanceller (with AecReferenceFeed).
    let mut chain_ns = NoiseSuppressor::new(DenoiseConfig::default());
    let mut chain_aec = AcousticEchoCanceller::new(AecConfig {
        sample_rate: SR,
        channels: 1,
        filter_len: 128,
        step_size: 0.45,
        ..AecConfig::default()
    });
    let feed = AecReferenceFeed::new(SR as usize / 2);
    let mut delay_line = [0.0f32; 24];
    let mut delay_pos = 0usize;

    let mut tail_mic_rms = Vec::new();
    let mut tail_chain_rms = Vec::new();
    for frame_idx in 0..80usize {
        let mut far = Vec::with_capacity(FRAME);
        let mut mic = Vec::with_capacity(FRAME);
        let speech_on = (25..45).contains(&frame_idx);
        for i in 0..FRAME {
            let n = frame_idx * FRAME + i;
            let t = n as f32 / SR as f32;
            // Far-end playback (present during frames 0..60, silent during 60..80).
            let ref_s = if frame_idx < 60 {
                0.35 * (TAU * 410.0 * t).sin() + 0.15 * rng.next_f32()
            } else {
                0.0
            };
            let delayed = delay_line[delay_pos];
            delay_line[delay_pos] = ref_s;
            delay_pos = (delay_pos + 1) % delay_line.len();
            let echo = 0.45 * delayed;
            let near = if speech_on { 0.25 * (TAU * 750.0 * t).sin() } else { 0.0 };
            let noise = rng.next_f32() * 0.025;
            far.push(ref_s);
            mic.push(echo + near + noise);
        }
        let in_r = rms(&mic);
        feed.push(&far);
        // Exact v0.6.0 viewer mic DSP chain: Capture -> NoiseSuppressor -> AcousticEchoCanceller.
        chain_ns.process(&mut mic);
        chain_aec.process_capture_with_feed(&mut mic, &feed);

        assert!(mic.iter().all(|s| s.is_finite()), "NS -> AEC output must stay finite");
        assert!(mic.iter().all(|s| s.abs() <= 1.05), "NS -> AEC output must stay bounded");
        if frame_idx >= 68 {
            tail_mic_rms.push(in_r);
            tail_chain_rms.push(rms(&mic));
        }
    }
    let tail_attenuation_db = 20.0 * (mean(&tail_mic_rms) / mean(&tail_chain_rms).max(1e-7)).log10();
    assert!(
        tail_attenuation_db >= 12.0,
        "NS -> AEC chain must strongly suppress tail noise (got {tail_attenuation_db:.2} dB)"
    );
}

// ---------------------------------------------------------------------------
// 3. Host Desktop vs Gaming Differentiation (Sandbox-safe unit check)
// ---------------------------------------------------------------------------

#[test]
fn v060_host_mode_negotiation_desktop_vs_gaming() {
    let cfg = HostConfig {
        max_fps: 120,
        bitrate_kbps: 50_000,
        idle_fps: 0,
        fec_parity_percent: 10,
        audio_frame_ms: 5.0,
        audio_frame_ms_explicit: false,
        ..Default::default()
    };

    let mut hello_desktop = Hello {
        protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
        mode: SessionMode::Desktop as i32,
        viewer_refresh_hz: 60,
        max_bitrate_kbps: 20_000,
        ..Default::default()
    };
    let desk_v = negotiate_params(&cfg, &hello_desktop);
    assert!(desk_v.draw_cursor, "Desktop mode must draw/capture cursor by default");
    assert_eq!(desk_v.parity_percent, 10, "Desktop mode must keep base FEC parity");
    assert!(desk_v.idle_fps >= 1, "Desktop mode must keep idle_fps >= 1");
    assert_eq!(negotiate_audio_frame_us(cfg.audio_frame_ms, cfg.audio_frame_ms_explicit, SessionMode::Desktop), 10_000);

    hello_desktop.mode = SessionMode::Gaming as i32;
    hello_desktop.viewer_refresh_hz = 120;
    hello_desktop.max_bitrate_kbps = 50_000;
    let game_v = negotiate_params(&cfg, &hello_desktop);
    assert!(!game_v.draw_cursor, "Gaming mode must hide captured cursor (draw_cursor = false)");
    assert!(game_v.parity_percent >= 15, "Gaming mode must raise initial FEC parity to >= 15%");
    assert_eq!(negotiate_audio_frame_us(cfg.audio_frame_ms, cfg.audio_frame_ms_explicit, SessionMode::Gaming), 5_000);

    // Explicit override beats Auto in both modes; normalize_opus_frame_us handles 0 (Auto) & explicit durations.
    assert_eq!(negotiate_audio_frame_us(2.5, true, SessionMode::Desktop), 2_500);
    assert_eq!(negotiate_audio_frame_us(20.0, true, SessionMode::Gaming), 20_000);
    assert_eq!(normalize_opus_frame_us(0, SessionMode::Desktop), 10_000);
    assert_eq!(normalize_opus_frame_us(0, SessionMode::Gaming), 5_000);
    assert_eq!(normalize_opus_frame_us(2_500, SessionMode::Desktop), 2_500);
    assert_eq!(normalize_opus_frame_us(20_000, SessionMode::Gaming), 20_000);
}

// ---------------------------------------------------------------------------
// 4. Privacy Mode Strategy & Auth State Live Default Permissions (Sandbox-safe)
// ---------------------------------------------------------------------------

#[test]
fn v060_privacy_mode_strategy_and_auth_state_default_permissions() {
    let sim = SimulatedPrivacyController::new();
    assert_eq!(sim.mode(), PrivacyModeKind::CurtainOverlay);
    sim.set_mode(PrivacyModeKind::VddMonitorOff);
    assert_eq!(sim.mode(), PrivacyModeKind::VddMonitorOff);
    sim.set_mode(PrivacyModeKind::CurtainOverlay);
    assert_eq!(sim.mode(), PrivacyModeKind::CurtainOverlay);

    let auth_state = HostAuthState::from_config(&HostAuthConfig::with_auto_otp());
    assert!(auth_state.default_permissions_override().is_none());
    let elevated = Permissions { allow_power: true, ..Permissions::interactive_default() };
    auth_state.set_default_permissions(elevated.clone());
    assert_eq!(auth_state.default_permissions_override(), Some(elevated));
}

// ---------------------------------------------------------------------------
// 5. Headless UI Settings EN/RU + Dark/Light Glyph & Section Audit
// ---------------------------------------------------------------------------

fn make_ui_input(viewport: egui::Vec2, time: f64, events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport)),
        time: Some(time),
        events,
        ..Default::default()
    }
}

fn collect_all_texts(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
    match shape {
        egui::epaint::Shape::Vec(v) => {
            for s in v {
                collect_all_texts(s, out);
            }
        }
        egui::epaint::Shape::Text(ts) => {
            out.push(ts.galley.text().to_owned());
        }
        _ => {}
    }
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
                    "rendered v0.6.0 Settings text contains missing glyph in egui bundled font: {slice:?}"
                );
            }
        }
        _ => {}
    }
}

#[test]
fn v060_headless_settings_en_ru_dark_light_sections_and_glyph_audit() {
    let viewport = egui::vec2(1280.0, 900.0);
    let forbidden_chars: [char; 13] = [
        '\u{2699}', '\u{1F4BB}', '\u{1F3AE}', '\u{1F5A5}', '\u{1F512}', '\u{1F50A}', '\u{1F3A4}',
        '\u{1F4CB}', '\u{1F4C1}', '\u{26A1}', '\u{1F319}', '\u{2600}', '\u{1F511}',
    ];

    for theme in [ThemePreference::Dark, ThemePreference::Light] {
        for lang in Language::ALL {
            let t = tr(lang);
            let ctx = egui::Context::default();
            ctx.set_theme(theme.to_egui());

            // Verify every i18n string in Strings::all_fields() has zero forbidden icons and 100% glyph coverage.
            let mut warm = ctx.run_ui(make_ui_input(viewport, 0.0, vec![]), |_| {});
            warm.textures_delta.clear();
            let body_font = egui::FontId::proportional(14.0);
            for (field, val) in t.all_fields() {
                for bad in forbidden_chars {
                    assert!(
                        !val.contains(bad),
                        "forbidden missing-glyph icon U+{:04X} found in {lang:?} i18n field `{field}`: {val:?}",
                        bad as u32
                    );
                }
                let printable: String = val.chars().filter(|c| !c.is_control()).collect();
                assert!(
                    ctx.fonts_mut(|f| f.has_glyphs(&body_font, &printable)),
                    "{lang:?} i18n field `{field}` contains glyph missing from egui font: {val:?}"
                );
            }

            let mut settings = Settings {
                language: lang,
                theme,
                mode: Mode::Desktop,
                rendezvous_server: "rdv.example.com:21116".into(),
                password_mode: PasswordMode::Both,
                trusted_peers: vec![SavedTrustedPeer {
                    public_key_hex: "aa".repeat(32),
                    device_id: "123456789".into(),
                    name: "Workstation".into(),
                    fingerprint: "0123456789abcdef0123456789abcdef".into(),
                    ..Default::default()
                }],
                ..Default::default()
            };

            let mut all_rendered_texts = Vec::new();
            let mut open = true;

            // Render top, middle, and scrolled-to-bottom frames so every section inside the ScrollArea paints.
            for step in 0..6u32 {
                let events = if step >= 2 {
                    vec![
                        egui::Event::PointerMoved(egui::pos2(300.0, 300.0)),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -400.0),
                            modifiers: egui::Modifiers::NONE,
                            phase: egui::TouchPhase::Move,
                            source: egui::MouseWheelSource::Trackpad,
                        },
                    ]
                } else {
                    vec![]
                };
                let mut out = ctx.run_ui(make_ui_input(viewport, 0.1 * step as f64, events), |ui| {
                    let _ = settings_window_with_security(
                        ui.ctx(),
                        &mut open,
                        &mut settings,
                        None,
                        Some("v0.6.0 (test)"),
                        Some(&UpdateUiState::Idle),
                        None,
                    );
                });
                assert!(open, "Settings window should remain open during headless render");
                for cs in &out.shapes {
                    verify_text_shapes_have_glyphs(&ctx, &cs.shape);
                    collect_all_texts(&cs.shape, &mut all_rendered_texts);
                }
                out.textures_delta.clear();
            }

            let joined = all_rendered_texts.join("\n");
            for expected in [
                t.settings_reset_profile_btn,
                t.settings_audio_heading,
                t.settings_noise_suppression,
                t.settings_audio_frame_latency,
                t.settings_recording_heading,
                t.settings_open_folder_btn,
                t.settings_auto_reconnect_cb,
                t.settings_security_heading,
                t.settings_host_permissions,
                t.host_perm_power,
                t.settings_host_echo_exclusion_cb,
                t.settings_host_privacy_mode,
            ] {
                assert!(
                    joined.contains(expected),
                    "v0.6.0 Settings screen ({lang:?}, {theme:?}) missing expected label: {expected:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 6. QUIC Loopback Integration Tests: Live SessionModeUpdate & Live Host Policy
// ---------------------------------------------------------------------------

struct RecordingSynthCapture {
    frame_n: u32,
    recorded_draw_cursor: Arc<Mutex<Vec<bool>>>,
}

impl FrameCapture for RecordingSynthCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps { backend: "synth-v060", formats: vec![PixelFormat::Bgra8], ..Default::default() }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        Ok(vec![Display {
            id: DisplayId("V060".into()),
            index: 0,
            name: "Synth 320x180".into(),
            x: 0,
            y: 0,
            width: 320,
            height: 180,
            refresh_hz: 120,
            scale_factor: 1.0,
            primary: true,
            hdr: false,
        }])
    }

    fn start(&mut self, cfg: &CaptureConfig) -> Result<()> {
        self.recorded_draw_cursor.lock().unwrap().push(cfg.draw_cursor);
        Ok(())
    }

    fn next_frame(&mut self, timeout_dur: Duration) -> Result<Option<CapturedFrame>> {
        std::thread::sleep(timeout_dur.min(Duration::from_millis(12)));
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
        Ok(Some(CapturedFrame { frame, dirty_rects: Vec::new(), cursor: None, capture_us: 10, convert_us: 0 }))
    }

    fn stop(&mut self) {}
}

struct SilentAudioCapture {
    fmt: AudioFormat,
    chunk: Duration,
}

impl AudioCapture for SilentAudioCapture {
    fn backend(&self) -> &'static str {
        "synth-audio-v060"
    }

    fn start(&mut self, cfg: &AudioCaptureConfig) -> Result<AudioFormat> {
        self.fmt = cfg.format;
        self.chunk = cfg.chunk;
        Ok(self.fmt)
    }

    fn read(&mut self, timeout_dur: Duration) -> Result<Option<AudioFrame>> {
        let wait = self.chunk.min(timeout_dur).max(Duration::from_millis(2));
        std::thread::sleep(wait);
        let frames = ((self.fmt.sample_rate as u64 * self.chunk.as_micros() as u64) / 1_000_000).max(120) as usize;
        Ok(Some(AudioFrame {
            format: self.fmt,
            samples: vec![0.0; frames * self.fmt.channels as usize],
            capture_ts: Timestamp::now(),
        }))
    }

    fn stop(&mut self) {}
}

struct NullInjector;
impl InputInjector for NullInjector {
    fn caps(&self) -> InputCaps {
        InputCaps { backend: "null-v060", keyboard: true, mouse_absolute: true, ..Default::default() }
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

fn v060_backends(
    recorded_draw_cursor: Arc<Mutex<Vec<bool>>>,
    privacy: Option<Arc<dyn PrivacyController>>,
    with_audio: bool,
) -> HostBackends {
    HostBackends {
        capture: Arc::new(move |_| {
            Ok(Box::new(RecordingSynthCapture { frame_n: 0, recorded_draw_cursor: recorded_draw_cursor.clone() }))
        }),
        encoder: Arc::new(|cfg, _| veloxdesk_codec::create_encoder(cfg, false)),
        injector: Arc::new(|| Ok(Box::new(NullInjector))),
        video_sink: Arc::new(|ch, sid| Box::new(TransportVideoSink(VideoSender::new(ch, sid)))),
        rate: Arc::new(|start, min, max| Box::new(TransportAbr(AbrController::new(start, min, max)))),
        audio_capture: with_audio.then(|| {
            Arc::new(|| {
                Ok(Box::new(SilentAudioCapture {
                    fmt: AudioFormat::STEREO_48K,
                    chunk: Duration::from_millis(10),
                }) as Box<dyn AudioCapture>)
            }) as Arc<veloxdesk_service::AudioCaptureFactory>
        }),
        audio_encoder: with_audio.then(|| {
            Arc::new(|cfg: &veloxdesk_core_traits::audio::AudioEncoderConfig| {
                Ok(Box::new(OpusEncoder::new(cfg)?) as Box<dyn AudioEncoder>)
            }) as Arc<veloxdesk_service::AudioEncoderFactory>
        }),
        audio_playback: None,
        clipboard: None,
        privacy,
        file_transfer: None,
        tunnel_policy: None,
    }
}

async fn connect_and_auth_viewer(
    host: &RunningHost,
    host_id: &DeviceIdentity,
    viewer_id: &DeviceIdentity,
    mode: SessionMode,
    want_audio: bool,
) -> (Arc<dyn TransportChannel>, ControlSender, ControlReceiver, AuthResult, SessionConfig) {
    let otp = host.auth_state().current_otp().expect("host must have active OTP");
    let ch: Arc<dyn TransportChannel> = Arc::new(
        veloxdesk_transport::connect(host.local_addr(), viewer_id, Some(host_id.public_key()))
            .await
            .expect("QUIC connect to RunningHost"),
    );
    let (s, r) = ch.open_bi(StreamKind::Control).await.expect("open control stream");
    let (mut tx, mut rx) = framed(s, r);

    let hello = Hello {
        protocol_version: veloxdesk_protocol::PROTOCOL_VERSION,
        device_id: viewer_id.device_id(),
        public_key: viewer_id.public_key().to_vec(),
        device_name: "v060-viewer".into(),
        os: std::env::consts::OS.into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        mode: mode as i32,
        video_caps: vec![VideoCodecCap {
            codec: VideoCodec::H264 as i32,
            hw_decode: false,
            max_width: 1920,
            max_height: 1080,
            max_fps: 120,
            yuv444: true,
            ten_bit: false,
        }],
        audio_codecs: if want_audio { vec![AudioCodec::Opus as i32] } else { vec![] },
        viewer_width: 1920,
        viewer_height: 1080,
        viewer_refresh_hz: 120,
        max_bitrate_kbps: 50_000,
    };
    tx.send(&Msg::Hello(hello).into()).await.expect("send Hello");

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

    let sc = timeout(Duration::from_secs(5), async {
        loop {
            match rx.recv().await.expect("control recv waiting for SessionConfig").msg {
                Some(Msg::SessionConfig(sc)) => return sc,
                _ => continue,
            }
        }
    })
    .await
    .expect("initial SessionConfig timeout");

    (ch, tx, rx, auth_res, sc)
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v060_host_live_session_mode_update_desktop_and_gaming() {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let recorded_draw_cursor = Arc::new(Mutex::new(Vec::<bool>::new()));

    let mut auth = HostAuthConfig::with_auto_otp();
    auth.require_accept = false;
    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 120,
        bitrate_kbps: 50_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        fec_parity_percent: 10,
        audio_enabled: true,
        audio_frame_ms: 5.0,
        audio_frame_ms_explicit: false,
        ipc_port: None,
        auth,
        ..Default::default()
    };

    let host = RunningHost::start(cfg, host_id.clone(), v060_backends(recorded_draw_cursor.clone(), None, true), None)
        .await
        .expect("RunningHost::start");

    // 1. Connect in Desktop mode: expect 10 ms Opus, base FEC (10%), draw_cursor = true.
    let (ch, mut tx, mut rx, _auth, initial_sc) =
        connect_and_auth_viewer(&host, &host_id, &viewer_id, SessionMode::Desktop, true).await;
    assert_eq!(initial_sc.mode, SessionMode::Desktop as i32);
    let init_audio = initial_sc.audio.expect("audio config must be advertised in Desktop");
    assert_eq!(init_audio.codec, AudioCodecId::Opus as i32);
    assert_eq!(init_audio.frame_duration_us, 10_000, "Desktop Auto audio must be 10 ms");
    let init_video = initial_sc.video.expect("video config must be advertised");
    assert_eq!(init_video.fec_parity_percent, 10, "Desktop mode must use base FEC parity");
    assert!(
        recorded_draw_cursor.lock().unwrap().first().copied() == Some(true),
        "Desktop mode must capture with draw_cursor = true"
    );

    // 2. Live mid-session switch Desktop -> Gaming (Auto audio = 5 ms, FEC >= 15%, draw_cursor = false) WITHOUT disconnecting!
    tx.send(
        &Msg::SessionModeUpdate(SessionModeUpdate {
            mode: SessionMode::Gaming as i32,
            max_bitrate_kbps: 50_000,
            max_fps: 120,
            audio_frame_duration_us: 0,
            prefer_yuv444: false,
        })
        .into(),
    )
    .await
    .expect("send SessionModeUpdate(Gaming)");

    let gaming_sc = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::SessionConfig(sc) if sc.mode == SessionMode::Gaming as i32 => Some(sc),
        _ => None,
    })
    .await;
    assert_eq!(gaming_sc.audio.as_ref().map(|a| a.frame_duration_us), Some(5_000), "Gaming Auto audio must switch to 5 ms");
    assert!(
        gaming_sc.video.as_ref().is_some_and(|v| v.fec_parity_percent >= 15),
        "Gaming mode must elevate FEC parity >= 15% (got {:?})",
        gaming_sc.video.as_ref().map(|v| v.fec_parity_percent)
    );
    assert_eq!(host.active_sessions(), 1, "session must remain connected after SessionModeUpdate");

    // 3. Live switch Gaming -> Desktop with explicit 20 ms Opus and confirm control stream stays live via Ping/Pong.
    tx.send(
        &Msg::SessionModeUpdate(SessionModeUpdate {
            mode: SessionMode::Desktop as i32,
            max_bitrate_kbps: 20_000,
            max_fps: 60,
            audio_frame_duration_us: 20_000,
            prefer_yuv444: true,
        })
        .into(),
    )
    .await
    .expect("send SessionModeUpdate(Desktop)");

    let desktop_sc = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::SessionConfig(sc)
            if sc.mode == SessionMode::Desktop as i32
                && sc.audio.as_ref().is_some_and(|a| a.frame_duration_us == 20_000) =>
        {
            Some(sc)
        }
        _ => None,
    })
    .await;
    assert_eq!(desktop_sc.video.as_ref().map(|v| v.fec_parity_percent), Some(10));

    tx.send(&Msg::Ping(Ping { seq: 60, sent_us: 999 }).into()).await.expect("send Ping after mode switches");
    let pong = recv_matching(&mut rx, Duration::from_secs(5), |m| match m {
        Msg::Pong(p) if p.seq == 60 => Some(p),
        _ => None,
    })
    .await;
    assert_eq!(pong.seq, 60);

    let _ = tx.send(&Msg::Close(Close { code: 0, reason: "v060 mode test done".into() }).into()).await;
    ch.close(0, "done");
    host.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v060_running_host_live_permissions_aec_and_privacy_mode() {
    let host_id = DeviceIdentity::generate();
    let viewer1_id = DeviceIdentity::generate();
    let viewer2_id = DeviceIdentity::generate();
    let sim_privacy = Arc::new(SimulatedPrivacyController::new());
    let draw_cursor_log = Arc::new(Mutex::new(Vec::new()));

    let mut auth = HostAuthConfig::with_auto_otp();
    auth.require_accept = false;
    let cfg = HostConfig {
        bind: "127.0.0.1:0".parse().unwrap(),
        max_fps: 30,
        bitrate_kbps: 2_000,
        codec: VideoCodec::H264,
        prefer_hw: false,
        prefer_gpu: false,
        audio_enabled: false,
        echo_cancellation: true,
        default_permissions: Permissions::interactive_default(),
        privacy_mode_kind: PrivacyModeKind::CurtainOverlay,
        ipc_port: None,
        auth,
        ..Default::default()
    };

    let host = RunningHost::start(
        cfg,
        host_id.clone(),
        v060_backends(draw_cursor_log, Some(sim_privacy.clone()), false),
        None,
    )
    .await
    .expect("RunningHost::start");

    // 1. Live echo cancellation & privacy mode strategy toggles.
    assert!(host.echo_cancellation());
    host.set_echo_cancellation(false);
    assert!(!host.echo_cancellation());
    host.set_echo_cancellation(true);
    assert!(host.echo_cancellation());

    assert_eq!(host.privacy_mode_kind(), PrivacyModeKind::CurtainOverlay);
    assert_eq!(sim_privacy.mode(), PrivacyModeKind::CurtainOverlay);
    host.set_privacy_mode_kind(PrivacyModeKind::VddMonitorOff);
    assert_eq!(host.privacy_mode_kind(), PrivacyModeKind::VddMonitorOff);
    assert_eq!(sim_privacy.mode(), PrivacyModeKind::VddMonitorOff);
    host.set_privacy_mode_kind(PrivacyModeKind::CurtainOverlay);
    assert_eq!(host.privacy_mode_kind(), PrivacyModeKind::CurtainOverlay);
    assert_eq!(sim_privacy.mode(), PrivacyModeKind::CurtainOverlay);

    // 2. Viewer 1 connects under initial default permissions (allow_power == false, allow_chat == true).
    let (ch1, mut tx1, _rx1, auth1, _sc1) =
        connect_and_auth_viewer(&host, &host_id, &viewer1_id, SessionMode::Desktop, false).await;
    let p1 = auth1.permissions.expect("AuthResult must include permissions");
    assert!(!p1.allow_power, "initial default permissions must have allow_power == false");
    assert!(p1.allow_chat, "initial default permissions must have allow_chat == true");

    let _ = tx1.send(&Msg::Close(Close { code: 0, reason: "viewer1 done".into() }).into()).await;
    ch1.close(0, "done");
    timeout(Duration::from_secs(5), async {
        while host.active_sessions() > 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("viewer1 disconnect timeout");

    // 3. Dynamically enable allow_power = true & disable allow_chat = false on live RunningHost!
    host.set_default_permissions(Permissions {
        allow_power: true,
        allow_chat: false,
        ..Permissions::interactive_default()
    });

    // 4. Viewer 2 connects and immediately receives the updated live permissions!
    let (ch2, mut tx2, _rx2, auth2, _sc2) =
        connect_and_auth_viewer(&host, &host_id, &viewer2_id, SessionMode::Desktop, false).await;
    let p2 = auth2.permissions.expect("AuthResult must include updated permissions");
    assert!(p2.allow_power, "RunningHost::set_default_permissions must dynamically enable allow_power");
    assert!(!p2.allow_chat, "RunningHost::set_default_permissions must dynamically disable allow_chat");

    let _ = tx2.send(&Msg::Close(Close { code: 0, reason: "viewer2 done".into() }).into()).await;
    ch2.close(0, "done");
    host.stop();
}
