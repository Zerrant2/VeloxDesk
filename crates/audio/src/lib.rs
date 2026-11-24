//! # veloxdesk-audio
//!
//! Low-latency audio loopback/microphone capture, pure-Rust Opus encode/decode
//! (`RESTRICTED_LOWDELAY`, 2.5–20 ms frames, PLC), adaptive jitter buffer, and
//! low-latency playback.
//!
//! ## Backends
//! * **Windows**: WASAPI loopback & microphone capture (`wasapi-loopback`, `wasapi-mic`)
//!   and shared-mode low-latency render (`wasapi-render`).
//! * **Linux**: PipeWire / ALSA interface behind `#[cfg(target_os = "linux")]`.
//! * **Codec**: Pure-Rust `rusty-opus` (RFC 6716 / RFC 8251, runtime AVX2/NEON SIMD).

pub mod aec;
pub mod denoise;
pub mod jitter;
pub mod opus;
pub mod pcm;

#[cfg(windows)]
pub mod wasapi;

#[cfg(target_os = "linux")]
pub mod linux;

pub use aec::{AcousticEchoCanceller, AecConfig, AecReferenceFeed};
pub use denoise::{DenoiseConfig, NoiseSuppressor};
pub use jitter::{AudioJitterBuffer, JitterConfig, JitterPop, JitterStats};
pub use opus::{OpusDecoder, OpusEncoder};

#[allow(unused_imports)]
use veloxdesk_core_traits::CoreError;
use veloxdesk_core_traits::Result;
use veloxdesk_core_traits::audio::{AudioCapture, AudioPlayback};

/// Create the OS system-audio loopback capture backend.
pub fn create_loopback_capture() -> Result<Box<dyn AudioCapture>> {
    create_loopback_capture_with_options(false)
}

/// Create the OS system-audio loopback capture backend, optionally excluding the
/// current process tree (Parsec-class Windows 10 2004+ / 11 Process Loopback
/// Exclusion, with transparent fallback to standard loopback on older OSes).
pub fn create_loopback_capture_with_options(exclude_self_process: bool) -> Result<Box<dyn AudioCapture>> {
    #[cfg(windows)]
    {
        if exclude_self_process {
            Ok(Box::new(wasapi::WasapiCapture::loopback_excluding_self()))
        } else {
            Ok(Box::new(wasapi::WasapiCapture::loopback()))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = exclude_self_process;
        Ok(Box::new(linux::LinuxAudioCapture::loopback()))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = exclude_self_process;
        Err(CoreError::unsupported("audio loopback capture is not implemented on this OS"))
    }
}

/// Create the OS microphone capture backend.
pub fn create_mic_capture() -> Result<Box<dyn AudioCapture>> {
    #[cfg(windows)]
    {
        Ok(Box::new(wasapi::WasapiCapture::microphone()))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxAudioCapture::microphone()))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(CoreError::unsupported("microphone capture is not implemented on this OS"))
    }
}

/// Metadata for an enumerated audio playback (render) endpoint on the host OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_virtual_cable: bool,
}

/// Returns `true` when an audio endpoint friendly name matches a known virtual audio cable
/// (VB-Audio Virtual Cable, VoiceMeeter, Steam Streaming Microphone, Virtual Audio Cable, etc.).
pub fn is_virtual_cable_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    const KEYWORDS: [&str; 6] = [
        "cable input",
        "vb-audio",
        "virtual cable",
        "steam streaming microphone",
        "virtual audio",
        "voicemeeter",
    ];
    KEYWORDS.iter().any(|kw| lower.contains(kw))
}

pub(crate) fn select_virtual_mic_device(devices: &[AudioDeviceInfo]) -> Option<AudioDeviceInfo> {
    if let Some(d) = devices.iter().find(|d| {
        let lower = d.name.to_lowercase();
        lower.contains("cable input") || lower.contains("vb-audio")
    }) {
        return Some(d.clone());
    }
    if let Some(d) = devices.iter().find(|d| d.name.to_lowercase().contains("steam streaming microphone")) {
        return Some(d.clone());
    }
    devices.iter().find(|d| d.is_virtual_cable).cloned()
}

/// Enumerate active audio playback (`eRender`) endpoints on the host OS.
pub fn list_playback_devices() -> Vec<AudioDeviceInfo> {
    #[cfg(windows)]
    {
        wasapi::list_playback_devices()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Pick the preferred virtual audio cable playback endpoint (if any is active),
/// preferring VB-CABLE (`"cable input"` / `"vb-audio"`), then `"steam streaming microphone"`,
/// then any other `is_virtual_cable` device.
pub fn find_virtual_mic_playback_device() -> Option<AudioDeviceInfo> {
    let devices = list_playback_devices();
    select_virtual_mic_device(&devices)
}

/// Create the OS low-latency audio playback backend on the default output endpoint.
pub fn create_playback() -> Result<Box<dyn AudioPlayback>> {
    create_playback_for_device(None)
}

/// Create the OS low-latency audio playback backend targeting an optional `device_id`
/// (`None` selects the OS default speakers/headphones).
pub fn create_playback_for_device(device_id: Option<&str>) -> Result<Box<dyn AudioPlayback>> {
    #[cfg(windows)]
    {
        Ok(Box::new(wasapi::WasapiPlayback::with_device(device_id.map(str::to_owned))))
    }
    #[cfg(target_os = "linux")]
    {
        let _ = device_id;
        Ok(Box::new(linux::LinuxAudioPlayback::new()))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = device_id;
        Err(CoreError::unsupported("audio playback is not implemented on this OS"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use veloxdesk_core_traits::Timestamp;
    use veloxdesk_core_traits::audio::{
        AudioCaptureConfig, AudioCodecId, AudioDecoder, AudioEncoder, AudioEncoderConfig, AudioFormat, AudioFrame,
    };

    use super::*;

    fn sine_frame(fmt: AudioFormat, samples_per_ch: usize, phase_start: usize, freq_hz: f32) -> AudioFrame {
        let mut samples = Vec::with_capacity(samples_per_ch * fmt.channels as usize);
        for i in 0..samples_per_ch {
            let t = (phase_start + i) as f32 / fmt.sample_rate as f32;
            let l = (2.0 * std::f32::consts::PI * freq_hz * t).sin() * 0.5;
            let r = (2.0 * std::f32::consts::PI * (freq_hz * 1.5) * t).sin() * 0.4;
            samples.push(l);
            if fmt.channels == 2 {
                samples.push(r);
            }
        }
        AudioFrame {
            format: fmt,
            samples,
            capture_ts: Timestamp((phase_start as u64 * 1_000_000) / fmt.sample_rate as u64),
        }
    }

    #[test]
    fn opus_low_delay_roundtrip_and_plc() {
        for &dur_us in &[2_500u32, 5_000, 10_000, 20_000] {
            let dur = Duration::from_micros(dur_us as u64);
            let cfg = AudioEncoderConfig {
                format: AudioFormat::STEREO_48K,
                bitrate_kbps: 128,
                frame_duration: dur,
                low_delay: true,
            };
            let mut enc = OpusEncoder::new(&cfg).expect("opus enc");
            let mut dec = OpusDecoder::new(AudioFormat::STEREO_48K, dur).expect("opus dec");
            assert_eq!(enc.codec(), AudioCodecId::Opus);
            assert_eq!(dec.codec(), AudioCodecId::Opus);

            let spc = (48_000u64 * dur_us as u64 / 1_000_000) as usize;
            let mut enc_us = Vec::new();
            let mut dec_us = Vec::new();
            let mut total_bytes = 0usize;

            for i in 0..20usize {
                let pcm = sine_frame(AudioFormat::STEREO_48K, spc, i * spc, 440.0);
                let t0 = Instant::now();
                let pkts = enc.encode(&pcm).expect("encode");
                let e_us = t0.elapsed().as_micros() as u32;
                assert_eq!(pkts.len(), 1);
                assert_eq!(pkts[0].seq, i as u32);
                assert_eq!(pkts[0].duration_us, dur_us);
                total_bytes += pkts[0].data.len();

                let t1 = Instant::now();
                let out = if i == 10 {
                    // Simulate lost packet -> PLC!
                    dec.decode(None).expect("plc")
                } else {
                    dec.decode(Some(&pkts[0])).expect("decode")
                };
                let d_us = t1.elapsed().as_micros() as u32;
                assert_eq!(out.format, AudioFormat::STEREO_48K);
                assert_eq!(out.frames_per_channel(), spc);
                assert!(out.samples.iter().all(|s| s.is_finite()));
                // Signal energy after warmup (including PLC frame) must be non-zero.
                if i >= 3 {
                    let rms = (out.samples.iter().map(|s| s * s).sum::<f32>() / out.samples.len() as f32).sqrt();
                    assert!(rms > 0.05, "dur={dur_us}us frame {i} rms too low: {rms}");
                    enc_us.push(e_us);
                    dec_us.push(d_us);
                }
            }
            let avg = |v: &[u32]| (v.iter().map(|&x| x as u64).sum::<u64>() / v.len().max(1) as u64) as u32;
            eprintln!(
                "Opus low-delay {dur_us}us stereo 48kHz @128kbps: avg_enc={}us avg_dec={}us avg_pkt={}B",
                avg(&enc_us),
                avg(&dec_us),
                total_bytes / 20
            );
        }
    }

    #[test]
    fn opus_fifo_handles_arbitrary_chunk_sizes() {
        let cfg = AudioEncoderConfig {
            format: AudioFormat::STEREO_48K,
            bitrate_kbps: 96,
            frame_duration: Duration::from_millis(10), // 480 samples/ch
            low_delay: true,
        };
        let mut enc = OpusEncoder::new(&cfg).expect("enc");
        let mut dec = OpusDecoder::with_format(AudioFormat::STEREO_48K).expect("dec");
        let mut jb = AudioJitterBuffer::new(JitterConfig::default());

        // Feed irregular 300-sample chunks (6.25 ms each): 8 chunks = 2400 samples = 5 packets.
        let mut produced = 0usize;
        for i in 0..8usize {
            let pcm = sine_frame(AudioFormat::STEREO_48K, 300, i * 300, 660.0);
            for pkt in enc.encode(&pcm).expect("encode") {
                jb.push(pkt);
                produced += 1;
            }
        }
        assert_eq!(produced, 5);

        let mut decoded_frames = 0usize;
        for _ in 0..5 {
            if let Some(frame) = jb.pop_decoded(&mut dec).expect("pop_decoded") {
                assert_eq!(frame.frames_per_channel(), 480);
                decoded_frames += 1;
            }
        }
        assert_eq!(decoded_frames, 5);
    }

    #[test]
    fn wasapi_loopback_and_playback_smoke() {
        let mut cap = create_loopback_capture().expect("create_loopback_capture");
        let cfg = AudioCaptureConfig {
            format: AudioFormat::STEREO_48K,
            chunk: Duration::from_millis(10),
            device: None,
            microphone: false,
        };
        match cap.start(&cfg) {
            Ok(fmt) => {
                eprintln!("loopback capture started ({}): {fmt:?}", cap.backend());
                assert_eq!(fmt, AudioFormat::STEREO_48K);
                let res = cap.read(Duration::from_millis(30)).expect("loopback read");
                eprintln!("loopback read got {} samples", res.as_ref().map(|f| f.samples.len()).unwrap_or(0));
                cap.stop();
            }
            Err(e) => {
                eprintln!("skipping live loopback smoke (no audio endpoint in env): {e}");
            }
        }

        let mut pb = create_playback().expect("create_playback");
        match pb.start(AudioFormat::STEREO_48K, Duration::from_millis(20)) {
            Ok(()) => {
                let silence = AudioFrame {
                    format: AudioFormat::STEREO_48K,
                    samples: vec![0.0; 480 * 2],
                    capture_ts: Timestamp::now(),
                };
                pb.push(&silence).expect("playback push");
                let buf_us = pb.buffered_us();
                eprintln!("playback started ({}): buffered={buf_us}us", pb.backend());
                pb.stop();
            }
            Err(e) => {
                eprintln!("skipping live playback smoke (no audio endpoint in env): {e}");
            }
        }
    }

    #[test]
    fn wasapi_process_loopback_exclusion_smoke_and_fallback() {
        let mut cap = create_loopback_capture_with_options(true).expect("create_loopback_capture_with_options(true)");
        let cfg = AudioCaptureConfig {
            format: AudioFormat::STEREO_48K,
            chunk: Duration::from_millis(10),
            device: None,
            microphone: false,
        };
        match cap.start(&cfg) {
            Ok(fmt) => {
                let backend = cap.backend();
                eprintln!("process-excluding loopback started ({backend}): {fmt:?}");
                assert_eq!(fmt, AudioFormat::STEREO_48K);
                assert!(
                    backend == "wasapi-process-loopback" || backend == "wasapi-loopback",
                    "unexpected backend: {backend}"
                );
                let res = cap.read(Duration::from_millis(30)).expect("process loopback read");
                eprintln!(
                    "process-excluding loopback read got {} samples",
                    res.as_ref().map(|f| f.samples.len()).unwrap_or(0)
                );
                cap.stop();
            }
            Err(e) => {
                eprintln!("skipping live process-loopback smoke (no audio endpoint in env): {e}");
            }
        }
    }

    #[test]
    fn virtual_cable_name_matching_and_priority() {
        assert!(is_virtual_cable_name("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_virtual_cable_name("Динамики (VB-Audio Point)"));
        assert!(is_virtual_cable_name("Line 1 (Virtual Audio Cable)"));
        assert!(is_virtual_cable_name("Steam Streaming Microphone"));
        assert!(is_virtual_cable_name("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"));
        assert!(is_virtual_cable_name("voicemeeter aux input"));
        assert!(!is_virtual_cable_name("Speakers (Realtek(R) Audio)"));
        assert!(!is_virtual_cable_name("Headphones (Sony WH-1000XM5)"));

        let devs = vec![
            AudioDeviceInfo {
                id: "realtek".into(),
                name: "Speakers (Realtek(R) Audio)".into(),
                is_virtual_cable: false,
            },
            AudioDeviceInfo {
                id: "vac".into(),
                name: "Line 1 (Virtual Audio Cable)".into(),
                is_virtual_cable: true,
            },
            AudioDeviceInfo {
                id: "steam".into(),
                name: "Speakers (Steam Streaming Microphone)".into(),
                is_virtual_cable: true,
            },
            AudioDeviceInfo {
                id: "vbcable".into(),
                name: "CABLE Input (VB-Audio Virtual Cable)".into(),
                is_virtual_cable: true,
            },
        ];
        // VB-CABLE beats Steam Streaming Microphone and generic Virtual Audio Cable.
        assert_eq!(select_virtual_mic_device(&devs).as_ref().map(|d| d.id.as_str()), Some("vbcable"));
        // Steam Streaming Microphone beats generic Virtual Audio Cable when VB-CABLE is absent.
        assert_eq!(select_virtual_mic_device(&devs[..3]).as_ref().map(|d| d.id.as_str()), Some("steam"));
        // Generic virtual cable is picked when neither VB-CABLE nor Steam is present.
        assert_eq!(select_virtual_mic_device(&devs[..2]).as_ref().map(|d| d.id.as_str()), Some("vac"));
        // None when only physical speakers exist.
        assert_eq!(select_virtual_mic_device(&devs[..1]), None);
    }

    #[test]
    fn list_playback_devices_and_device_playback_smoke() {
        let devices = list_playback_devices();
        eprintln!("enumerated {} playback device(s):", devices.len());
        for d in &devices {
            assert!(!d.id.is_empty(), "device id must not be empty");
            assert!(!d.name.is_empty(), "device name must not be empty");
            assert_eq!(d.is_virtual_cable, is_virtual_cable_name(&d.name));
            eprintln!("  - id={} name={:?} virtual_cable={}", d.id, d.name, d.is_virtual_cable);
        }
        let _ = find_virtual_mic_playback_device();

        if let Some(first) = devices.first() {
            let mut pb = create_playback_for_device(Some(&first.id)).expect("create_playback_for_device");
            if pb.start(AudioFormat::STEREO_48K, Duration::from_millis(20)).is_ok() {
                pb.stop();
            }
        }
    }
}
