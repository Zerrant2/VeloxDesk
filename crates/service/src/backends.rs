//! Pluggable pieces of the host pipeline. [`HostBackends::system`] wires the real
//! capture / codec / input / transport crates; tests substitute synthetic ones.

use std::sync::Arc;

use veloxdesk_clipboard::ClipboardBackend;
use veloxdesk_core_traits::audio::{AudioCapture, AudioEncoder, AudioEncoderConfig, AudioPlayback};
use veloxdesk_core_traits::video::{EncodedPacket, EncoderConfig};
use veloxdesk_core_traits::{FrameCapture, InputInjector, Result, Timestamp, TransportChannel, VideoEncoder};
use veloxdesk_file_transfer::FileTransferConfig;
use veloxdesk_protocol::control::{Nack, ReceiverReport};
use veloxdesk_tunnel::TunnelPolicy;
use veloxdesk_virtual_display::PrivacyController;

/// Media-plane sender for one video stream (FEC packetization → datagrams).
/// Called synchronously from the encode thread.
pub trait VideoSink: Send {
    /// Packetize and send; returns the time spent packetizing (µs).
    fn send_frame(&mut self, pkt: &EncodedPacket) -> Result<u32>;
    fn set_parity_percent(&mut self, pct: u8);
    fn on_nack(&mut self, nack: &Nack);
}

/// ABR decision applied to the encoder and the FEC sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateDecision {
    pub bitrate_kbps: u32,
    pub parity_percent: u8,
}

/// Host-side rate controller fed with viewer `ReceiverReport`s.
pub trait RateController: Send {
    fn on_report(&mut self, rr: &ReceiverReport, now: Timestamp) -> Option<RateDecision>;
}

pub type CaptureFactory = dyn Fn(bool) -> Result<Box<dyn FrameCapture>> + Send + Sync;
pub type EncoderFactory = dyn Fn(&EncoderConfig, bool) -> Result<Box<dyn VideoEncoder>> + Send + Sync;
pub type InjectorFactory = dyn Fn() -> Result<Box<dyn InputInjector>> + Send + Sync;
pub type SinkFactory = dyn Fn(Arc<dyn TransportChannel>, u8) -> Box<dyn VideoSink> + Send + Sync;
/// `(start_kbps, min_kbps, max_kbps)` → controller.
pub type RateFactory = dyn Fn(u32, u32, u32) -> Box<dyn RateController> + Send + Sync;
pub type AudioCaptureFactory = dyn Fn() -> Result<Box<dyn AudioCapture>> + Send + Sync;
pub type AudioEncoderFactory = dyn Fn(&AudioEncoderConfig) -> Result<Box<dyn AudioEncoder>> + Send + Sync;
pub type AudioPlaybackFactory = dyn Fn() -> Result<Box<dyn AudioPlayback>> + Send + Sync;
pub type MicPlaybackFactory = dyn Fn(Option<&str>) -> Result<Box<dyn AudioPlayback>> + Send + Sync;
pub type ClipboardFactory = dyn Fn() -> Result<Box<dyn ClipboardBackend>> + Send + Sync;

/// Factories for every platform-dependent stage. Cheap to clone (Arc'd closures).
#[derive(Clone)]
pub struct HostBackends {
    pub capture: Arc<CaptureFactory>,
    pub encoder: Arc<EncoderFactory>,
    pub injector: Arc<InjectorFactory>,
    pub video_sink: Arc<SinkFactory>,
    pub rate: Arc<RateFactory>,
    pub audio_capture: Option<Arc<AudioCaptureFactory>>,
    pub audio_encoder: Option<Arc<AudioEncoderFactory>>,
    pub audio_playback: Option<Arc<AudioPlaybackFactory>>,
    pub mic_playback: Option<Arc<MicPlaybackFactory>>,
    pub clipboard: Option<Arc<ClipboardFactory>>,
    pub privacy: Option<Arc<dyn PrivacyController>>,
    pub file_transfer: Option<FileTransferConfig>,
    pub tunnel_policy: Option<TunnelPolicy>,
}

impl HostBackends {
    /// Remove audio factories (convenient for video-only headless tests).
    pub fn without_audio(mut self) -> Self {
        self.audio_capture = None;
        self.audio_encoder = None;
        self.audio_playback = None;
        self.mic_playback = None;
        self
    }

    pub fn with_mic_playback(mut self, factory: Arc<MicPlaybackFactory>) -> Self {
        self.mic_playback = Some(factory);
        self
    }

    pub fn with_clipboard(mut self, factory: Arc<ClipboardFactory>) -> Self {
        self.clipboard = Some(factory);
        self
    }

    pub fn with_privacy(mut self, ctrl: Arc<dyn PrivacyController>) -> Self {
        self.privacy = Some(ctrl);
        self
    }

    pub fn with_file_transfer(mut self, cfg: FileTransferConfig) -> Self {
        self.file_transfer = Some(cfg);
        self
    }

    pub fn with_tunnel_policy(mut self, policy: TunnelPolicy) -> Self {
        self.tunnel_policy = Some(policy);
        self
    }
}

/// Simple GCC-flavoured fallback controller (used when no transport ABR is wired):
/// loss > 10 % ⇒ ×0.7, delay overuse ⇒ ×0.85, otherwise +5 %/report up to max;
/// parity = clamp(2 × loss, 5 %, 50 %).
pub struct SimpleRate {
    kbps: f64,
    min: f64,
    max: f64,
    last: Option<RateDecision>,
}

impl SimpleRate {
    pub fn new(start: u32, min: u32, max: u32) -> Self {
        Self { kbps: start as f64, min: min as f64, max: max as f64, last: None }
    }
}

impl RateController for SimpleRate {
    fn on_report(&mut self, rr: &ReceiverReport, _now: Timestamp) -> Option<RateDecision> {
        let total = rr.packets_received + rr.packets_lost;
        let loss = if total == 0 { 0.0 } else { rr.packets_lost as f64 / total as f64 };
        if loss > 0.10 {
            self.kbps *= 0.7;
        } else if rr.delay_gradient > 0.5 {
            self.kbps *= 0.85;
        } else if loss < 0.02 {
            self.kbps *= 1.05;
        }
        self.kbps = self.kbps.clamp(self.min, self.max);
        let parity = ((2.0 * loss).clamp(0.05, 0.5) * 100.0).round() as u8;
        let d = RateDecision { bitrate_kbps: self.kbps as u32, parity_percent: parity };
        let changed = match self.last {
            None => true,
            Some(l) => {
                l.parity_percent != d.parity_percent
                    || (l.bitrate_kbps as f64 - d.bitrate_kbps as f64).abs() > l.bitrate_kbps as f64 * 0.02
            }
        };
        if changed {
            self.last = Some(d);
            Some(d)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_rate_reacts() {
        let mut r = SimpleRate::new(10_000, 1_000, 20_000);
        let lossy = ReceiverReport { packets_received: 80, packets_lost: 20, ..Default::default() };
        let d = r.on_report(&lossy, Timestamp(0)).unwrap();
        assert_eq!(d.bitrate_kbps, 7_000);
        assert_eq!(d.parity_percent, 40);
        let clean = ReceiverReport { packets_received: 100, ..Default::default() };
        let d = r.on_report(&clean, Timestamp(0)).unwrap();
        assert_eq!(d.bitrate_kbps, 7_350);
        assert_eq!(d.parity_percent, 5);
        for _ in 0..100 {
            r.on_report(&clean, Timestamp(0));
        }
        assert_eq!(r.last.unwrap().bitrate_kbps, 20_000);
        assert!(r.on_report(&clean, Timestamp(0)).is_none());
    }
}
