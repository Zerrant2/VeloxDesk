//! Host-side audio pipelines (ADR-006: dedicated OS threads):
//! * [`AudioPipeline`]: loopback capture → Opus low-delay encode → `AudioSender` (`stream_id = 0`).
//! * [`MicPlayback`]: viewer microphone `EncodedAudio` (`stream_id = 1`) → `AudioJitterBuffer`
//!   → `OpusDecoder` → host `AudioPlayback`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use veloxdesk_audio::pcm::{LinearResampler, remix_channels};
use veloxdesk_audio::{
    AcousticEchoCanceller, AecConfig, AecReferenceFeed, AudioJitterBuffer, DenoiseConfig, JitterConfig, JitterPop,
    NoiseSuppressor, OpusDecoder,
};
use veloxdesk_core_traits::audio::{
    AudioCaptureConfig, AudioCodecId, AudioDecoder, AudioEncoderConfig, AudioFormat, AudioFrame, AudioPlayback,
    EncodedAudio,
};
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_protocol::control::{AudioConfig, SessionMode};
use veloxdesk_transport::AudioSender;

use crate::backends::HostBackends;

/// Static audio stream parameters for one host session.
#[derive(Debug, Clone, Copy)]
pub struct AudioParams {
    pub stream_id: u8,
    pub bitrate_kbps: u32,
    pub frame_duration_us: u32,
    /// Enable Windows process loopback exclusion and optional NLMS AEC when mic playback is active.
    pub echo_cancellation: bool,
}

/// Running host system-audio capture → Opus encode → QUIC datagram thread.
pub struct AudioPipeline {
    stop: Arc<AtomicBool>,
    redundant: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioPipeline {
    /// Start loopback capture + Opus encoding on a dedicated OS thread.
    /// Returns the running handle and the wire [`AudioConfig`] to advertise in `SessionConfig`.
    pub fn start(
        params: AudioParams,
        backends: &HostBackends,
        channel: Arc<dyn TransportChannel>,
    ) -> Result<(Self, AudioConfig)> {
        Self::start_with_aec(params, backends, channel, None)
    }

    /// Like [`Self::start`], optionally connecting an [`AecReferenceFeed`] from [`MicPlayback`]
    /// so NLMS [`AcousticEchoCanceller`] suppresses echoed viewer microphone audio.
    pub fn start_with_aec(
        params: AudioParams,
        backends: &HostBackends,
        channel: Arc<dyn TransportChannel>,
        aec_ref: Option<AecReferenceFeed>,
    ) -> Result<(Self, AudioConfig)> {
        Self::start_with_live_aec(params, backends, channel, aec_ref, None)
    }

    /// Like [`Self::start_with_aec`], plus an optional live `echo_cancel_flag` updated by
    /// [`crate::RunningHost::set_echo_cancellation`] so AEC can be toggled on the fly.
    pub fn start_with_live_aec(
        params: AudioParams,
        backends: &HostBackends,
        channel: Arc<dyn TransportChannel>,
        aec_ref: Option<AecReferenceFeed>,
        echo_cancel_flag: Option<Arc<AtomicBool>>,
    ) -> Result<(Self, AudioConfig)> {
        let cap_factory = backends
            .audio_capture
            .as_ref()
            .ok_or_else(|| CoreError::unsupported("audio capture backend not configured"))?;
        let enc_factory = backends
            .audio_encoder
            .as_ref()
            .ok_or_else(|| CoreError::unsupported("audio encoder backend not configured"))?;

        let initial_aec = echo_cancel_flag
            .as_ref()
            .map_or(params.echo_cancellation, |f| f.load(Ordering::Relaxed));
        let frame_duration = Duration::from_micros(params.frame_duration_us as u64);
        let mut capture = if Arc::ptr_eq(cap_factory, &crate::host::system_audio_capture_factory()) {
            veloxdesk_audio::create_loopback_capture_with_options(initial_aec)?
        } else {
            cap_factory()?
        };
        let cap_fmt = capture.start(&AudioCaptureConfig {
            format: AudioFormat::STEREO_48K,
            chunk: frame_duration,
            device: None,
            microphone: false,
        })?;

        // Opus runs at 48 kHz; normalize to 48 kHz mono/stereo if the OS endpoint differs.
        let enc_fmt = AudioFormat { sample_rate: 48_000, channels: cap_fmt.channels.clamp(1, 2) };
        let mut encoder = enc_factory(&AudioEncoderConfig {
            format: enc_fmt,
            bitrate_kbps: params.bitrate_kbps,
            frame_duration,
            low_delay: true,
        })?;

        let wire_cfg = AudioConfig {
            codec: encoder.codec() as i32,
            sample_rate: enc_fmt.sample_rate,
            channels: enc_fmt.channels as u32,
            frame_duration_us: params.frame_duration_us,
            bitrate_kbps: params.bitrate_kbps,
        };

        let stop = Arc::new(AtomicBool::new(false));
        let redundant = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let red_bg = redundant.clone();
        let mut sender = AudioSender::new(channel, params.stream_id);
        let aec_flag = echo_cancel_flag.unwrap_or_else(|| Arc::new(AtomicBool::new(params.echo_cancellation)));

        let thread = std::thread::Builder::new()
            .name("vdk-audio-tx".into())
            .spawn(move || {
                let mut resampler = (cap_fmt.sample_rate != enc_fmt.sample_rate).then(|| {
                    LinearResampler::new(cap_fmt.sample_rate, enc_fmt.sample_rate, enc_fmt.channels as usize)
                });
                let mut aec = aec_ref.is_some().then(|| {
                    AcousticEchoCanceller::new(AecConfig {
                        sample_rate: enc_fmt.sample_rate,
                        channels: enc_fmt.channels,
                        ..AecConfig::default()
                    })
                });
                let mut scratch = Vec::new();
                let mut aec_ref_scratch = Vec::new();
                while !stop_bg.load(Ordering::Relaxed) {
                    let want_red = red_bg.load(Ordering::Relaxed);
                    if sender.is_redundant() != want_red {
                        sender.set_redundant(want_red);
                    }
                    match capture.read(Duration::from_millis(20)) {
                        Ok(Some(mut frame)) => {
                            if frame.format.channels != enc_fmt.channels {
                                remix_channels(
                                    &frame.samples,
                                    frame.format.channels as usize,
                                    enc_fmt.channels as usize,
                                    &mut scratch,
                                );
                                std::mem::swap(&mut frame.samples, &mut scratch);
                                frame.format.channels = enc_fmt.channels;
                            }
                            if let Some(rs) = resampler.as_mut() {
                                rs.process(&frame.samples, &mut scratch);
                                std::mem::swap(&mut frame.samples, &mut scratch);
                                frame.format.sample_rate = enc_fmt.sample_rate;
                            }
                            if let (Some(aec), Some(feed)) = (aec.as_mut(), aec_ref.as_ref()) {
                                if aec_flag.load(Ordering::Relaxed) {
                                    if enc_fmt.channels == 2 {
                                        aec.process_capture_with_feed(&mut frame.samples, feed);
                                    } else {
                                        // MicPlayback feeds stereo 48 kHz reference samples; downmix to mono.
                                        aec_ref_scratch.clear();
                                        feed.drain_into(&mut aec_ref_scratch, frame.samples.len() * 2);
                                        if !aec_ref_scratch.is_empty() {
                                            remix_channels(&aec_ref_scratch, 2, 1, &mut scratch);
                                            aec.push_reference(&scratch);
                                        }
                                        aec.process_capture(&mut frame.samples);
                                    }
                                } else {
                                    // Keep reference ring drained while AEC is bypassed so re-enabling starts fresh.
                                    aec_ref_scratch.clear();
                                    let drain_n = frame.samples.len() * if enc_fmt.channels == 2 { 1 } else { 2 };
                                    feed.drain_into(&mut aec_ref_scratch, drain_n);
                                }
                            }
                            match encoder.encode(&frame) {
                                Ok(pkts) => {
                                    for pkt in pkts {
                                        if let Err(CoreError::Closed) = sender.send(&pkt) {
                                            stop_bg.store(true, Ordering::Relaxed);
                                            break;
                                        }
                                    }
                                }
                                Err(e) => tracing::debug!("audio encode error: {e}"),
                            }
                        }
                        Ok(None) | Err(CoreError::Timeout) => {}
                        Err(e) => {
                            tracing::warn!("audio capture error: {e}");
                            std::thread::sleep(Duration::from_millis(20));
                        }
                    }
                }
                capture.stop();
                tracing::debug!("audio tx thread exit");
            })
            .map_err(CoreError::Io)?;

        Ok((Self { stop, redundant, thread: Some(thread) }, wire_cfg))
    }

    /// Enable duplicate-packet audio redundancy when network loss exceeds ~2%.
    pub fn set_redundant(&self, enabled: bool) {
        self.redundant.store(enabled, Ordering::Relaxed);
    }

    /// Stop and join the audio thread (blocking; call from `spawn_blocking`).
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Optional viewer-microphone playback thread (`stream_id == 1`).
pub struct MicPlayback {
    tx: Option<Sender<EncodedAudio>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Cloneable non-blocking handle for pushing incoming viewer mic packets.
#[derive(Clone)]
pub struct MicHandle {
    tx: Sender<EncodedAudio>,
}

impl MicHandle {
    pub fn push(&self, pkt: EncodedAudio) {
        let _ = self.tx.try_send(pkt);
    }
}

fn make_mic_jitter_buffer(frame_dur: Duration) -> AudioJitterBuffer {
    AudioJitterBuffer::new(JitterConfig {
        frame_duration: frame_dur,
        target_delay: (frame_dur * 4).max(Duration::from_millis(20)),
        max_delay: (frame_dur * 16).max(Duration::from_millis(100)),
    })
}

fn is_valid_opus_duration_us(us: u32) -> bool {
    matches!(us, 2_500 | 5_000 | 10_000 | 20_000)
}

impl MicPlayback {
    pub fn start(frame_duration_us: u32, backends: &HostBackends) -> Result<Self> {
        Self::start_with_device(frame_duration_us, backends, None, false, None)
    }

    /// Start viewer-microphone playback and optionally feed decoded speaker samples into
    /// `aec_ref` for [`AudioPipeline`] echo cancellation.
    pub fn start_with_aec_feed(
        frame_duration_us: u32,
        backends: &HostBackends,
        aec_ref: Option<AecReferenceFeed>,
    ) -> Result<Self> {
        Self::start_with_device(frame_duration_us, backends, aec_ref, false, None)
    }

    /// Start viewer-microphone playback with optional [`AecReferenceFeed`] and pure-Rust
    /// [`NoiseSuppressor`] denoising on incoming microphone audio (v0.6.0).
    pub fn start_with_options(
        frame_duration_us: u32,
        backends: &HostBackends,
        aec_ref: Option<AecReferenceFeed>,
        mic_noise_suppression: bool,
    ) -> Result<Self> {
        Self::start_with_device(frame_duration_us, backends, aec_ref, mic_noise_suppression, None)
    }

    /// Start viewer-microphone playback routed to an optional `mic_output_device`
    /// (`None` = default speakers, `"auto"` = virtual cable if installed, `"speakers"` = default
    /// speakers, or a specific WASAPI endpoint ID).
    pub fn start_with_device(
        frame_duration_us: u32,
        backends: &HostBackends,
        aec_ref: Option<AecReferenceFeed>,
        mic_noise_suppression: bool,
        mic_output_device: Option<&str>,
    ) -> Result<Self> {
        if backends.mic_playback.is_none() && backends.audio_playback.is_none() {
            return Err(CoreError::unsupported("audio playback backend not configured"));
        }

        let initial_us = if is_valid_opus_duration_us(frame_duration_us) {
            frame_duration_us
        } else {
            5_000
        };
        let initial_frame_dur = Duration::from_micros(initial_us as u64);
        let fmt = AudioFormat::STEREO_48K;
        let target_latency = Duration::from_millis(25);

        let (target_dev, mut routed_away) = match mic_output_device {
            Some(pref) if backends.mic_playback.is_some() => crate::config::resolve_mic_output_device(Some(pref)),
            _ => (None, false),
        };

        let open_and_start = |dev: Option<&str>| -> Result<Box<dyn AudioPlayback>> {
            let mut pb = if let Some(mic_factory) = backends.mic_playback.as_ref() {
                mic_factory(dev)?
            } else if let Some(pb_factory) = backends.audio_playback.as_ref() {
                pb_factory()?
            } else {
                return Err(CoreError::unsupported("audio playback backend not configured"));
            };
            pb.start(fmt, target_latency)?;
            Ok(pb)
        };

        let mut playback = if let Some(dev_id) = target_dev.as_deref() {
            match open_and_start(Some(dev_id)) {
                Ok(pb) => {
                    tracing::info!(device_id = %dev_id, "host mic playback routed to dedicated output device");
                    pb
                }
                Err(err) => {
                    tracing::warn!(
                        device_id = %dev_id,
                        error = %err,
                        "failed to open configured mic output device; falling back to default speakers"
                    );
                    routed_away = false;
                    open_and_start(None)?
                }
            }
        } else {
            routed_away = false;
            open_and_start(None)?
        };

        let effective_aec_ref = if routed_away { None } else { aec_ref };
        let mut decoder = OpusDecoder::new(fmt, initial_frame_dur)?;
        let (tx, rx): (Sender<EncodedAudio>, Receiver<EncodedAudio>) = crossbeam_channel::bounded(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();

        let thread = std::thread::Builder::new()
            .name("vdk-audio-mic".into())
            .spawn(move || {
                let mut frame_dur = initial_frame_dur;
                let mut jitter = make_mic_jitter_buffer(frame_dur);
                let mut denoiser = mic_noise_suppression.then(|| {
                    NoiseSuppressor::new(DenoiseConfig {
                        sample_rate: fmt.sample_rate,
                        channels: fmt.channels,
                        ..DenoiseConfig::default()
                    })
                });
                let mut next_tick = Instant::now() + frame_dur;

                let push_pkt = |pkt: EncodedAudio,
                                frame_dur: &mut Duration,
                                jitter: &mut AudioJitterBuffer,
                                next_tick: &mut Instant| {
                    if is_valid_opus_duration_us(pkt.duration_us) {
                        let pkt_dur = Duration::from_micros(pkt.duration_us as u64);
                        if pkt_dur != *frame_dur {
                            tracing::info!(
                                old_us = frame_dur.as_micros() as u32,
                                new_us = pkt.duration_us,
                                "adapting host mic playback cadence to incoming Opus packet duration"
                            );
                            *frame_dur = pkt_dur;
                            *jitter = make_mic_jitter_buffer(pkt_dur);
                            let now = Instant::now();
                            if *next_tick > now + pkt_dur {
                                *next_tick = now + pkt_dur;
                            }
                        }
                    }
                    jitter.push(pkt);
                };

                while !stop_bg.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    if now < next_tick {
                        match rx.recv_timeout(next_tick - now) {
                            Ok(pkt) => push_pkt(pkt, &mut frame_dur, &mut jitter, &mut next_tick),
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }

                    loop {
                        match rx.try_recv() {
                            Ok(pkt) => push_pkt(pkt, &mut frame_dur, &mut jitter, &mut next_tick),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                stop_bg.store(true, Ordering::Relaxed);
                                break;
                            }
                        }
                    }
                    if stop_bg.load(Ordering::Relaxed) {
                        break;
                    }

                    let mut caught_up = 0u32;
                    while Instant::now() >= next_tick && caught_up < 6 {
                        next_tick += frame_dur;
                        caught_up += 1;

                        let frame: Option<AudioFrame> = match jitter.pop() {
                            JitterPop::Packet(pkt) => {
                                if pkt.codec == AudioCodecId::Opus {
                                    decoder.decode(Some(&pkt)).ok()
                                } else {
                                    None
                                }
                            }
                            JitterPop::Missing { .. } => decoder.decode(None).ok(),
                            JitterPop::Buffering => None,
                        };
                        if let Some(mut pcm) = frame {
                            if let Some(ns) = denoiser.as_mut() {
                                ns.process(&mut pcm.samples);
                            }
                            if let Some(feed) = &effective_aec_ref {
                                feed.push(&pcm.samples);
                            }
                            let _ = playback.push(&pcm);
                        }
                    }

                    let after = Instant::now();
                    if next_tick + frame_dur * 6 < after {
                        next_tick = after + frame_dur;
                    }
                }
                playback.stop();
                tracing::debug!("audio mic playback thread exit");
            })
            .map_err(CoreError::Io)?;

        Ok(Self { tx: Some(tx), stop, thread: Some(thread) })
    }

    pub fn handle(&self) -> Option<MicHandle> {
        self.tx.clone().map(|tx| MicHandle { tx })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for MicPlayback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.tx.take();
    }
}

/// Convert `audio_frame_ms` (2.5 / 5.0 / 10.0 / 20.0) into exact microseconds.
pub fn frame_ms_to_us(ms: f32) -> Option<u32> {
    if (ms - 2.5).abs() < 0.01 {
        Some(2_500)
    } else if (ms - 5.0).abs() < 0.01 {
        Some(5_000)
    } else if (ms - 10.0).abs() < 0.01 {
        Some(10_000)
    } else if (ms - 20.0).abs() < 0.01 {
        Some(20_000)
    } else {
        None
    }
}

/// Resolve an effective valid Opus frame duration (µs) from a requested value and [`SessionMode`].
///
/// `0` selects mode-dependent Auto (`5_000 µs` for Gaming, `10_000 µs` for Desktop);
/// non-zero values are clamped/snapped to the nearest valid Opus frame duration
/// (`2_500`, `5_000`, `10_000`, `20_000 µs`).
pub fn normalize_opus_frame_us(requested_us: u32, mode: SessionMode) -> u32 {
    match requested_us {
        0 => match mode {
            SessionMode::Gaming => 5_000,
            SessionMode::Desktop => 10_000,
        },
        u if u <= 3_750 => 2_500,
        u if u <= 7_500 => 5_000,
        u if u <= 15_000 => 10_000,
        _ => 20_000,
    }
}

/// Negotiate initial Opus frame duration (µs) from host config (`audio_frame_ms`, `explicit`) and [`SessionMode`].
pub fn negotiate_audio_frame_us(audio_frame_ms: f32, explicit: bool, mode: SessionMode) -> u32 {
    if explicit
        && let Some(us) = frame_ms_to_us(audio_frame_ms)
    {
        return us;
    }
    normalize_opus_frame_us(0, mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_durations() {
        assert_eq!(frame_ms_to_us(2.5), Some(2_500));
        assert_eq!(frame_ms_to_us(5.0), Some(5_000));
        assert_eq!(frame_ms_to_us(10.0), Some(10_000));
        assert_eq!(frame_ms_to_us(20.0), Some(20_000));
        assert_eq!(frame_ms_to_us(7.0), None);
    }

    #[test]
    fn audio_pipeline_streams_encoded_datagrams() {
        use bytes::Bytes;
        use std::sync::Mutex;
        use veloxdesk_core_traits::Timestamp;
        use veloxdesk_core_traits::audio::{AudioCapture, AudioEncoder};
        use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
        use veloxdesk_protocol::StreamKind;
        use veloxdesk_transport::AudioReceiver;

        struct ToneCapture {
            n: usize,
        }
        impl AudioCapture for ToneCapture {
            fn backend(&self) -> &'static str {
                "tone"
            }
            fn start(&mut self, cfg: &AudioCaptureConfig) -> Result<AudioFormat> {
                Ok(cfg.format)
            }
            fn read(&mut self, _timeout: Duration) -> Result<Option<AudioFrame>> {
                std::thread::sleep(Duration::from_millis(2));
                self.n += 1;
                let samples = vec![0.1f32; 240 * 2]; // 5 ms @ 48 kHz stereo
                Ok(Some(AudioFrame { format: AudioFormat::STEREO_48K, samples, capture_ts: Timestamp::now() }))
            }
            fn stop(&mut self) {}
        }

        struct SinkChannel(Arc<Mutex<Vec<Bytes>>>);
        #[async_trait::async_trait]
        impl TransportChannel for SinkChannel {
            fn max_datagram_size(&self) -> Option<usize> {
                Some(1200)
            }
            fn send_datagram(&self, data: Bytes) -> Result<()> {
                self.0.lock().unwrap().push(data);
                Ok(())
            }
            async fn recv_datagram(&self) -> Result<Bytes> {
                std::future::pending().await
            }
            async fn open_bi(&self, _: StreamKind) -> Result<(SendStream, RecvStream)> {
                Err(CoreError::Closed)
            }
            async fn open_uni(&self, _: StreamKind) -> Result<SendStream> {
                Err(CoreError::Closed)
            }
            async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)> {
                Err(CoreError::Closed)
            }
            async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)> {
                Err(CoreError::Closed)
            }
            fn stats(&self) -> TransportStats {
                TransportStats::default()
            }
            fn peer_public_key(&self) -> Option<[u8; 32]> {
                None
            }
            fn close(&self, _: u32, _: &str) {}
        }

        let sent = Arc::new(Mutex::new(Vec::new()));
        let ch: Arc<dyn TransportChannel> = Arc::new(SinkChannel(sent.clone()));
        let mut backends = crate::HostBackends::system().without_audio();
        backends.audio_capture = Some(Arc::new(|| Ok(Box::new(ToneCapture { n: 0 }))));
        backends.audio_encoder =
            Some(Arc::new(|cfg| Ok(Box::new(veloxdesk_audio::OpusEncoder::new(cfg)?) as Box<dyn AudioEncoder>)));

        let (pipe, wire) = AudioPipeline::start(
            AudioParams { stream_id: 0, bitrate_kbps: 128, frame_duration_us: 5_000, echo_cancellation: true },
            &backends,
            ch,
        )
        .unwrap();
        assert_eq!(wire.sample_rate, 48_000);
        assert_eq!(wire.channels, 2);
        assert_eq!(wire.frame_duration_us, 5_000);

        for _ in 0..50 {
            if sent.lock().unwrap().len() >= 3 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        pipe.stop();
        let dgrams = sent.lock().unwrap();
        assert!(dgrams.len() >= 3, "expected >=3 audio datagrams, got {}", dgrams.len());
        let (sid, pkt) = AudioReceiver::parse_datagram(&dgrams[0]).expect("valid audio datagram");
        assert_eq!(sid, 0);
        assert_eq!(pkt.codec, AudioCodecId::Opus);
        assert_eq!(pkt.duration_us, 5_000);
    }

    #[test]
    fn audio_pipeline_with_aec_reference_feed_runs_cleanly() {
        use bytes::Bytes;
        use std::sync::Mutex;
        use veloxdesk_core_traits::Timestamp;
        use veloxdesk_core_traits::audio::{AudioCapture, AudioEncoder};
        use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
        use veloxdesk_protocol::StreamKind;

        struct ToneCapture;
        impl AudioCapture for ToneCapture {
            fn backend(&self) -> &'static str {
                "tone"
            }
            fn start(&mut self, cfg: &AudioCaptureConfig) -> Result<AudioFormat> {
                Ok(cfg.format)
            }
            fn read(&mut self, _timeout: Duration) -> Result<Option<AudioFrame>> {
                std::thread::sleep(Duration::from_millis(2));
                Ok(Some(AudioFrame {
                    format: AudioFormat::STEREO_48K,
                    samples: vec![0.15f32; 240 * 2],
                    capture_ts: Timestamp::now(),
                }))
            }
            fn stop(&mut self) {}
        }

        struct SinkChannel(Arc<Mutex<Vec<Bytes>>>);
        #[async_trait::async_trait]
        impl TransportChannel for SinkChannel {
            fn max_datagram_size(&self) -> Option<usize> {
                Some(1200)
            }
            fn send_datagram(&self, data: Bytes) -> Result<()> {
                self.0.lock().unwrap().push(data);
                Ok(())
            }
            async fn recv_datagram(&self) -> Result<Bytes> {
                std::future::pending().await
            }
            async fn open_bi(&self, _: StreamKind) -> Result<(SendStream, RecvStream)> {
                Err(CoreError::Closed)
            }
            async fn open_uni(&self, _: StreamKind) -> Result<SendStream> {
                Err(CoreError::Closed)
            }
            async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)> {
                Err(CoreError::Closed)
            }
            async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)> {
                Err(CoreError::Closed)
            }
            fn stats(&self) -> TransportStats {
                TransportStats::default()
            }
            fn peer_public_key(&self) -> Option<[u8; 32]> {
                None
            }
            fn close(&self, _: u32, _: &str) {}
        }

        let sent = Arc::new(Mutex::new(Vec::new()));
        let ch: Arc<dyn TransportChannel> = Arc::new(SinkChannel(sent.clone()));
        let mut backends = crate::HostBackends::system().without_audio();
        backends.audio_capture = Some(Arc::new(|| Ok(Box::new(ToneCapture))));
        backends.audio_encoder =
            Some(Arc::new(|cfg| Ok(Box::new(veloxdesk_audio::OpusEncoder::new(cfg)?) as Box<dyn AudioEncoder>)));

        let feed = AecReferenceFeed::new(4_800);
        feed.push(&vec![0.1f32; 480]);
        let (pipe, _) = AudioPipeline::start_with_aec(
            AudioParams { stream_id: 0, bitrate_kbps: 128, frame_duration_us: 5_000, echo_cancellation: true },
            &backends,
            ch,
            Some(feed.clone()),
        )
        .unwrap();

        for _ in 0..50 {
            if sent.lock().unwrap().len() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        pipe.stop();
        assert!(sent.lock().unwrap().len() >= 2);
        // Reference samples pushed before capture should have been drained by the AEC loop.
        let mut rem = Vec::new();
        assert_eq!(feed.drain_into(&mut rem, 480), 0);
    }

    #[test]
    fn mic_playback_handles_5ms_opus_stream_and_device_routing() {
        use std::sync::Mutex;
        use veloxdesk_core_traits::Timestamp;
        use veloxdesk_core_traits::audio::AudioEncoder;

        struct MockPlayback {
            frames: Arc<Mutex<Vec<usize>>>,
            fail_start: bool,
        }
        impl AudioPlayback for MockPlayback {
            fn backend(&self) -> &'static str {
                "mock-pb"
            }
            fn start(&mut self, _fmt: AudioFormat, _lat: Duration) -> Result<()> {
                if self.fail_start { Err(CoreError::platform("unplugged")) } else { Ok(()) }
            }
            fn push(&mut self, frame: &AudioFrame) -> Result<()> {
                assert!(frame.samples.iter().all(|s| s.is_finite()));
                self.frames.lock().unwrap().push(frame.frames_per_channel());
                Ok(())
            }
            fn buffered_us(&self) -> u32 {
                0
            }
            fn stop(&mut self) {}
        }

        let mut enc = veloxdesk_audio::OpusEncoder::new(&AudioEncoderConfig {
            format: AudioFormat::STEREO_48K,
            bitrate_kbps: 96,
            frame_duration: Duration::from_micros(5_000),
            low_delay: true,
        })
        .unwrap();
        let mut pkts = Vec::with_capacity(40);
        for i in 0..40usize {
            let samples: Vec<f32> = (0..240 * 2).map(|k| ((i * 240 + k / 2) as f32 * 0.05).sin() * 0.3).collect();
            let pcm = AudioFrame { format: AudioFormat::STEREO_48K, samples, capture_ts: Timestamp(i as u64 * 5_000) };
            pkts.extend(enc.encode(&pcm).unwrap());
        }
        assert_eq!(pkts.len(), 40);

        for &init_us in &[10_000u32, 5_000] {
            let pushed = Arc::new(Mutex::new(Vec::new()));
            let pushed_c = pushed.clone();
            let mut backends = crate::HostBackends::system().without_audio();
            backends.audio_playback =
                Some(Arc::new(move || Ok(Box::new(MockPlayback { frames: pushed_c.clone(), fail_start: false }))));

            let mic = MicPlayback::start(init_us, &backends).unwrap();
            let handle = mic.handle().unwrap();
            let t0 = Instant::now();
            for (i, pkt) in pkts.iter().cloned().enumerate() {
                let target = t0 + Duration::from_millis(5 * i as u64);
                let now = Instant::now();
                if now < target {
                    std::thread::sleep(target - now);
                }
                handle.push(pkt);
            }
            std::thread::sleep(Duration::from_millis(60));
            mic.stop();

            let got = pushed.lock().unwrap();
            assert!(got.len() >= 30, "init_us={init_us}: expected >= 30 decoded frames, got {}", got.len());
            assert!(got.iter().all(|&spc| spc == 240));
        }

        // Verify virtual cable routing suppresses aec_ref while unplugged device falls back to default & feeds aec_ref.
        let opened_devs = Arc::new(Mutex::new(Vec::<Option<String>>::new()));
        let opened_c = opened_devs.clone();
        let pushed = Arc::new(Mutex::new(Vec::new()));
        let pushed_c = pushed.clone();
        let mut backends = crate::HostBackends::system().without_audio().with_mic_playback(Arc::new(move |dev| {
            opened_c.lock().unwrap().push(dev.map(str::to_owned));
            let fail = dev == Some("unplugged-id");
            Ok(Box::new(MockPlayback { frames: pushed_c.clone(), fail_start: fail }))
        }));
        let _ = &mut backends;

        let aec_routed = AecReferenceFeed::new(4_800);
        let mic1 = MicPlayback::start_with_device(5_000, &backends, Some(aec_routed.clone()), false, Some("cable-id")).unwrap();
        let h1 = mic1.handle().unwrap();
        for pkt in pkts.iter().take(10).cloned() {
            h1.push(pkt);
        }
        std::thread::sleep(Duration::from_millis(80));
        mic1.stop();
        assert!(aec_routed.is_empty(), "aec_ref must not be fed when routed away from speakers");

        let aec_fallback = AecReferenceFeed::new(4_800);
        let mic2 =
            MicPlayback::start_with_device(5_000, &backends, Some(aec_fallback.clone()), false, Some("unplugged-id")).unwrap();
        let h2 = mic2.handle().unwrap();
        for pkt in pkts.iter().take(10).cloned() {
            h2.push(pkt);
        }
        std::thread::sleep(Duration::from_millis(80));
        mic2.stop();
        assert!(!aec_fallback.is_empty(), "aec_ref must be fed after fallback to default speakers");
        assert_eq!(
            *opened_devs.lock().unwrap(),
            vec![Some("cable-id".into()), Some("unplugged-id".into()), None]
        );
    }
}
