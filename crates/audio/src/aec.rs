//! Pure-Rust adaptive Acoustic Echo Canceller (NLMS).
//!
//! Contract skeleton frozen at Phase 0; full NLMS filter + double-talk detector +
//! ERLE metrics implemented by `audio-rec-worker`.

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

/// Configuration for [`AcousticEchoCanceller`].
#[derive(Debug, Clone, Copy)]
pub struct AecConfig {
    pub sample_rate: u32,
    pub channels: u16,
    /// Adaptive FIR filter length in taps per channel.
    pub filter_len: usize,
    /// NLMS adaptation step size (`0.0..=1.0`).
    pub step_size: f32,
    /// Regularization constant preventing division by zero on silence.
    pub epsilon: f32,
    /// Double-talk detector energy ratio threshold.
    pub double_talk_threshold: f32,
}

impl Default for AecConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            filter_len: 1024,
            step_size: 0.4,
            epsilon: 1e-4,
            double_talk_threshold: 0.65,
        }
    }
}

/// Thread-safe bounded ring buffer carrying playback (far-end speaker) samples to
/// a capture thread running [`AcousticEchoCanceller`].
#[derive(Clone)]
pub struct AecReferenceFeed {
    inner: Arc<Mutex<VecDeque<f32>>>,
    max_samples: usize,
}

impl AecReferenceFeed {
    pub fn new(max_samples: usize) -> Self {
        let cap = max_samples.max(480);
        Self { inner: Arc::new(Mutex::new(VecDeque::with_capacity(cap))), max_samples: cap }
    }

    pub fn push(&self, samples: &[f32]) {
        let mut q = self.inner.lock();
        if samples.len() >= self.max_samples {
            q.clear();
            q.extend(&samples[samples.len() - self.max_samples..]);
            return;
        }
        let overflow = (q.len() + samples.len()).saturating_sub(self.max_samples);
        if overflow > 0 {
            q.drain(..overflow);
        }
        q.extend(samples.iter().copied());
    }

    pub fn drain_into(&self, dst: &mut Vec<f32>, max_count: usize) -> usize {
        let mut q = self.inner.lock();
        let n = q.len().min(max_count);
        dst.reserve(n);
        for _ in 0..n {
            if let Some(s) = q.pop_front() {
                dst.push(s);
            }
        }
        n
    }

    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

/// Lightweight pure-Rust NLMS Acoustic Echo Canceller.
pub struct AcousticEchoCanceller {
    cfg: AecConfig,
    ref_buf: VecDeque<f32>,
    last_erle_db: f32,
}

impl AcousticEchoCanceller {
    pub fn new(cfg: AecConfig) -> Self {
        Self { cfg, ref_buf: VecDeque::new(), last_erle_db: 0.0 }
    }

    pub fn config(&self) -> AecConfig {
        self.cfg
    }

    pub fn reset(&mut self) {
        self.ref_buf.clear();
        self.last_erle_db = 0.0;
    }

    /// Push far-end speaker playback samples (interleaved `f32`) into the reference buffer.
    pub fn push_reference(&mut self, playback_samples: &[f32]) {
        self.ref_buf.extend(playback_samples.iter().copied());
    }

    /// Cancel echo in-place from captured microphone samples using buffered reference audio.
    pub fn process_capture(&mut self, capture_samples: &mut [f32]) {
        let _ = capture_samples;
    }

    /// Cancel echo in-place using an explicit aligned far-end reference slice of matching length.
    pub fn process_aligned(&mut self, mic_samples: &mut [f32], far_end_reference: &[f32]) {
        let _ = (mic_samples, far_end_reference);
    }

    /// Drain reference audio from a shared [`AecReferenceFeed`] and process `capture_samples` in-place.
    pub fn process_capture_with_feed(&mut self, capture_samples: &mut [f32], feed: &AecReferenceFeed) {
        let mut tmp = Vec::with_capacity(capture_samples.len());
        feed.drain_into(&mut tmp, capture_samples.len());
        if !tmp.is_empty() {
            self.push_reference(&tmp);
        }
        self.process_capture(capture_samples);
    }

    /// Last measured Echo Return Loss Enhancement (ERLE) in dB.
    pub fn last_erle_db(&self) -> f32 {
        self.last_erle_db
    }
}
