//! Host-side audio pipelines (ADR-006: dedicated OS threads):
//! * [`AudioPipeline`]: loopback capture → Opus low-delay encode → `AudioSender` (`stream_id = 0`).
//! * [`MicPlayback`]: viewer microphone `EncodedAudio` (`stream_id = 1`) → `AudioJitterBuffer`
//!   → `OpusDecoder` → host `AudioPlayback`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TryRecvError};
use veloxdesk_audio::pcm::{LinearResampler, remix_channels};
use veloxdesk_audio::{
    AcousticEchoCanceller, AecConfig, AecReferenceFeed, AudioJitterBuffer, DenoiseConfig, JitterConfig, JitterPop,
    NoiseSuppressor, OpusDecoder,
};
use veloxdesk_core_traits::audio::{
    AudioCaptureConfig, AudioCodecId, AudioDecoder, AudioEncoderConfig, AudioFormat, AudioFrame, EncodedAudio,
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

impl MicPlayback {
    pub fn start(frame_duration_us: u32, backends: &HostBackends) -> Result<Self> {
        Self::start_with_aec_feed(frame_duration_us, backends, None)
    }

    /// Start viewer-microphone playback and optionally feed decoded speaker samples into
    /// `aec_ref` for [`AudioPipeline`] echo cancellation.
    pub fn start_with_aec_feed(
        frame_duration_us: u32,
        backends: &HostBackends,
        aec_ref: Option<AecReferenceFeed>,
    ) -> Result<Self> {
        Self::start_with_options(frame_duration_us, backends, aec_ref, false)
    }

    /// Start viewer-microphone playback with optional [`AecReferenceFeed`] and pure-Rust
    /// [`NoiseSuppressor`] denoising on incoming microphone audio (v0.6.0).
    pub fn start_with_options(
        frame_duration_us: u32,
        backends: &HostBackends,
        aec_ref: Option<AecReferenceFeed>,
        mic_noise_suppression: bool,
    ) -> Result<Self> {
        let pb_factory = backends
            .audio_playback
            .as_ref()
            .ok_or_else(|| CoreError::unsupported("audio playback backend not configured"))?;
        let frame_duration = Duration::from_micros(frame_duration_us.max(2500) as u64);
        let fmt = AudioFormat::STEREO_48K;
        let mut playback = pb_factory()?;
        playback.start(fmt, Duration::from_millis(15))?;
        let mut decoder = OpusDecoder::new(fmt, frame_duration)?;
        let (tx, rx): (Sender<EncodedAudio>, Receiver<EncodedAudio>) = crossbeam_channel::bounded(32);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();

        let thread = std::thread::Builder::new()
            .name("vdk-audio-mic".into())
            .spawn(move || {
                let mut jitter = AudioJitterBuffer::new(JitterConfig {
                    frame_duration,
                    target_delay: frame_duration * 2,
                    max_delay: frame_duration * 8,
                });
                let mut denoiser = mic_noise_suppression.then(|| {
                    NoiseSuppressor::new(DenoiseConfig {
                        sample_rate: fmt.sample_rate,
                        channels: fmt.channels,
                        ..DenoiseConfig::default()
                    })
                });
                let tick = frame_duration.max(Duration::from_millis(2));
                while !stop_bg.load(Ordering::Relaxed) {
                    loop {
                        match rx.try_recv() {
                            Ok(pkt) => jitter.push(pkt),
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
                        if let Some(feed) = &aec_ref {
                            feed.push(&pcm.samples);
                        }
                        let _ = playback.push(&pcm);
                    }
                    std::thread::sleep(tick);
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
}
