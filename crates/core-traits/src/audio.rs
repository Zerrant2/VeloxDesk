//! Audio types and traits (loopback capture, Opus, playback).

use std::time::Duration;

use bytes::Bytes;

use crate::{Result, Timestamp};

pub use veloxdesk_protocol::control::AudioCodec as AudioCodecId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

impl AudioFormat {
    pub const STEREO_48K: AudioFormat = AudioFormat { sample_rate: 48_000, channels: 2 };
}

/// Interleaved f32 PCM.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub format: AudioFormat,
    pub samples: Vec<f32>,
    pub capture_ts: Timestamp,
}

impl AudioFrame {
    pub fn frames_per_channel(&self) -> usize {
        self.samples.len() / self.format.channels.max(1) as usize
    }
}

#[derive(Debug, Clone)]
pub struct EncodedAudio {
    pub data: Bytes,
    pub codec: AudioCodecId,
    pub seq: u32,
    pub capture_ts: Timestamp,
    pub duration_us: u32,
}

#[derive(Debug, Clone)]
pub struct AudioCaptureConfig {
    pub format: AudioFormat,
    /// Desired chunk size (e.g. 2.5–10 ms).
    pub chunk: Duration,
    /// `None` = default output device loopback. `Some(id)` = specific device / microphone.
    pub device: Option<String>,
    /// Capture microphone instead of system loopback.
    pub microphone: bool,
}

/// System audio loopback (WASAPI / PipeWire / CoreAudio) or microphone capture.
pub trait AudioCapture: Send {
    fn backend(&self) -> &'static str;
    fn start(&mut self, cfg: &AudioCaptureConfig) -> Result<AudioFormat>;
    /// Wait up to `timeout` for the next chunk; `Ok(None)` on timeout / silence.
    fn read(&mut self, timeout: Duration) -> Result<Option<AudioFrame>>;
    fn stop(&mut self);
}

#[derive(Debug, Clone)]
pub struct AudioEncoderConfig {
    pub format: AudioFormat,
    pub bitrate_kbps: u32,
    /// Opus frame duration: 2.5, 5, 10 or 20 ms.
    pub frame_duration: Duration,
    /// Restricted low-delay mode (Opus `OPUS_APPLICATION_RESTRICTED_LOWDELAY`).
    pub low_delay: bool,
}

pub trait AudioEncoder: Send {
    fn codec(&self) -> AudioCodecId;
    /// Feed PCM; returns zero or more encoded packets (buffers partial frames internally).
    fn encode(&mut self, pcm: &AudioFrame) -> Result<Vec<EncodedAudio>>;
}

pub trait AudioDecoder: Send {
    fn codec(&self) -> AudioCodecId;
    /// Decode one packet. `None` = packet lost → run packet-loss concealment.
    fn decode(&mut self, packet: Option<&EncodedAudio>) -> Result<AudioFrame>;
}

/// Convenience marker for a type that is both encoder and decoder of one codec.
pub trait AudioCodec: AudioEncoder + AudioDecoder {}
impl<T: AudioEncoder + AudioDecoder> AudioCodec for T {}

/// Low-latency playback sink with an internal jitter buffer.
pub trait AudioPlayback: Send {
    fn backend(&self) -> &'static str;
    fn start(&mut self, format: AudioFormat, target_latency: Duration) -> Result<()>;
    fn push(&mut self, frame: &AudioFrame) -> Result<()>;
    /// Currently buffered audio (µs) — exposed to the HUD.
    fn buffered_us(&self) -> u32;
    fn stop(&mut self);
}
