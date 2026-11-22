//! Linux audio capture/playback backend placeholders (PipeWire / ALSA interface for M3 Linux).

use std::time::Duration;

use veloxdesk_core_traits::audio::{AudioCapture, AudioCaptureConfig, AudioFormat, AudioFrame, AudioPlayback};
use veloxdesk_core_traits::{CoreError, Result};

/// PipeWire / ALSA audio capture backend (Linux).
pub struct LinuxAudioCapture {
    microphone: bool,
}

impl LinuxAudioCapture {
    pub fn loopback() -> Self {
        Self { microphone: false }
    }

    pub fn microphone() -> Self {
        Self { microphone: true }
    }
}

impl AudioCapture for LinuxAudioCapture {
    fn backend(&self) -> &'static str {
        if self.microphone { "linux-mic" } else { "linux-loopback" }
    }

    fn start(&mut self, _cfg: &AudioCaptureConfig) -> Result<AudioFormat> {
        Err(CoreError::unsupported("Linux PipeWire/ALSA audio capture not yet wired"))
    }

    fn read(&mut self, _timeout: Duration) -> Result<Option<AudioFrame>> {
        Err(CoreError::unsupported("Linux PipeWire/ALSA audio capture not yet wired"))
    }

    fn stop(&mut self) {}
}

/// PipeWire / ALSA low-latency playback backend (Linux).
#[derive(Default)]
pub struct LinuxAudioPlayback;

impl LinuxAudioPlayback {
    pub fn new() -> Self {
        Self
    }
}

impl AudioPlayback for LinuxAudioPlayback {
    fn backend(&self) -> &'static str {
        "linux-playback"
    }

    fn start(&mut self, _format: AudioFormat, _target_latency: Duration) -> Result<()> {
        Err(CoreError::unsupported("Linux PipeWire/ALSA audio playback not yet wired"))
    }

    fn push(&mut self, _frame: &AudioFrame) -> Result<()> {
        Err(CoreError::unsupported("Linux PipeWire/ALSA audio playback not yet wired"))
    }

    fn buffered_us(&self) -> u32 {
        0
    }

    fn stop(&mut self) {}
}
