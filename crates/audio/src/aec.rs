//! Pure-Rust adaptive Acoustic Echo Canceller (NLMS).
//!
//! Per-channel Normalized Least Mean Squares (NLMS) adaptive FIR filter with:
//! - Contiguous doubled-ring delay line for cache-friendly dot products
//! - Regularization (`epsilon`) and gentle weight leakage + NaN/Inf protection
//! - Energy-ratio / Geigel double-talk detector with hangover (freezes adaptation
//!   during near-end speech so the local voice is preserved without weight divergence)
//! - Real-time Echo Return Loss Enhancement (`ERLE`, in dB) telemetry

use std::collections::VecDeque;
use std::sync::Arc;

use parking_lot::Mutex;

/// Gentle per-sample weight leakage to prevent long-term drift on narrowband inputs.
const WEIGHT_LEAKAGE: f32 = 1.0 - 1e-7;
/// Clamp bound on individual FIR filter taps to guard against numerical blowup.
const MAX_WEIGHT_MAG: f32 = 4.0;
/// Periodic exact energy recomputation cadence to eliminate floating-point drift.
const ENERGY_REFRESH_INTERVAL: usize = 1024;

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

    /// Current number of buffered reference samples.
    pub fn len(&self) -> usize {
        self.inner.lock().len()
    }

    /// Returns `true` if no reference samples are buffered.
    pub fn is_empty(&self) -> bool {
        self.inner.lock().is_empty()
    }
}

/// Per-channel NLMS adaptive FIR filter + double-talk detector state.
struct NlmsChannel {
    weights: Vec<f32>,
    /// Doubled ring buffer of length `2 * filter_len` so `&x_buf[head..head + filter_len]`
    /// is always a contiguous slice `[x[n], x[n-1], ..., x[n-L+1]]`.
    x_buf: Vec<f32>,
    head: usize,
    x_energy: f32,
    samples_since_refresh: usize,
    x_power_ema: f32,
    d_power_ema: f32,
    d_fast_ema: f32,
    erle_d_power: f32,
    erle_e_power: f32,
    dtd_hangover: usize,
}

impl NlmsChannel {
    fn new(filter_len: usize) -> Self {
        let len = filter_len.max(1);
        Self {
            weights: vec![0.0; len],
            x_buf: vec![0.0; len * 2],
            head: 0,
            x_energy: 0.0,
            samples_since_refresh: 0,
            x_power_ema: 0.0,
            d_power_ema: 0.0,
            d_fast_ema: 0.0,
            erle_d_power: 0.0,
            erle_e_power: 0.0,
            dtd_hangover: 0,
        }
    }

    fn reset(&mut self) {
        self.weights.fill(0.0);
        self.x_buf.fill(0.0);
        self.head = 0;
        self.x_energy = 0.0;
        self.samples_since_refresh = 0;
        self.x_power_ema = 0.0;
        self.d_power_ema = 0.0;
        self.d_fast_ema = 0.0;
        self.erle_d_power = 0.0;
        self.erle_e_power = 0.0;
        self.dtd_hangover = 0;
    }

    #[inline]
    #[allow(clippy::too_many_arguments)]
    fn process_sample(
        &mut self,
        mic_raw: f32,
        far_raw: f32,
        mu: f32,
        epsilon: f32,
        dtd_thr_sq: f32,
        alpha_env: f32,
        alpha_erle: f32,
        hangover_len: usize,
    ) -> f32 {
        let d = if mic_raw.is_finite() { mic_raw } else { 0.0 };
        let x = if far_raw.is_finite() { far_raw } else { 0.0 };

        let len = self.weights.len();
        self.head = if self.head == 0 { len - 1 } else { self.head - 1 };

        // Outgoing sample leaving the L-tap window is currently at `self.head`.
        let x_old = self.x_buf[self.head];
        self.x_buf[self.head] = x;
        self.x_buf[self.head + len] = x;

        self.x_energy = (self.x_energy + x * x - x_old * x_old).max(0.0);
        self.samples_since_refresh += 1;

        let x_slice = &self.x_buf[self.head..self.head + len];

        if self.samples_since_refresh >= ENERGY_REFRESH_INTERVAL {
            self.samples_since_refresh = 0;
            self.x_energy = x_slice.iter().map(|&v| v * v).sum();
        }

        // Predict echo y[n] = w^T x[n].
        let mut y = 0.0f32;
        for (&w, &xv) in self.weights.iter().zip(x_slice.iter()) {
            y += w * xv;
        }

        let e = d - y;
        if !e.is_finite() {
            // Numerical blowup protection: reset channel weights cleanly and pass through mic.
            self.reset();
            return d.clamp(-1.0, 1.0);
        }

        // Short-term power trackers for Geigel/energy-ratio double-talk detection.
        self.x_power_ema += alpha_env * (x * x - self.x_power_ema);
        self.d_power_ema += alpha_env * (d * d - self.d_power_ema);
        let alpha_fast = (alpha_env * 8.0).min(0.25);
        self.d_fast_ema += alpha_fast * (d * d - self.d_fast_ema);

        let x_window_power = self.x_energy / len as f32;
        let x_ref_power = x_window_power.max(self.x_power_ema);
        let far_end_active = self.x_energy > epsilon * 0.25 && x_ref_power > 1e-8;

        // Energy-ratio + fast-attack Geigel double-talk detector: when microphone energy
        // exceeds threshold * far-end energy, near-end speech is present -> freeze weights + hangover.
        if self.d_power_ema > dtd_thr_sq * x_ref_power + 1e-7
            || self.d_fast_ema > (dtd_thr_sq * 1.4) * x_ref_power + 1e-6
        {
            self.dtd_hangover = hangover_len;
        }

        let double_talk = if self.dtd_hangover > 0 {
            self.dtd_hangover -= 1;
            true
        } else {
            false
        };

        if far_end_active && !double_talk {
            let norm = self.x_energy + epsilon;
            let step = (mu * e) / norm;
            if step.is_finite() {
                for (w, &xv) in self.weights.iter_mut().zip(x_slice.iter()) {
                    *w = (*w * WEIGHT_LEAKAGE + step * xv).clamp(-MAX_WEIGHT_MAG, MAX_WEIGHT_MAG);
                }
            }

            // Update ERLE accumulators during single-talk echo cancellation.
            self.erle_d_power += alpha_erle * (d * d - self.erle_d_power);
            self.erle_e_power += alpha_erle * (e * e - self.erle_e_power);
        }

        e.clamp(-1.0, 1.0)
    }
}

/// Lightweight pure-Rust NLMS Acoustic Echo Canceller.
pub struct AcousticEchoCanceller {
    cfg: AecConfig,
    channels: Vec<NlmsChannel>,
    ref_buf: VecDeque<f32>,
    scratch_ref: Vec<f32>,
    last_erle_db: f32,
}

impl AcousticEchoCanceller {
    pub fn new(cfg: AecConfig) -> Self {
        let ch_count = cfg.channels.max(1) as usize;
        let filter_len = cfg.filter_len.clamp(16, 8192);
        let channels = (0..ch_count).map(|_| NlmsChannel::new(filter_len)).collect();
        let normalized_cfg = AecConfig {
            sample_rate: cfg.sample_rate.max(8_000),
            channels: ch_count as u16,
            filter_len,
            step_size: if cfg.step_size.is_finite() { cfg.step_size.clamp(0.0, 1.5) } else { 0.4 },
            epsilon: if cfg.epsilon.is_finite() && cfg.epsilon > 0.0 { cfg.epsilon.max(1e-7) } else { 1e-4 },
            double_talk_threshold: if cfg.double_talk_threshold.is_finite() && cfg.double_talk_threshold > 0.0 {
                cfg.double_talk_threshold.clamp(0.05, 10.0)
            } else {
                0.65
            },
        };
        Self {
            cfg: normalized_cfg,
            channels,
            ref_buf: VecDeque::with_capacity(normalized_cfg.sample_rate as usize * ch_count / 5),
            scratch_ref: Vec::new(),
            last_erle_db: 0.0,
        }
    }

    pub fn config(&self) -> AecConfig {
        self.cfg
    }

    pub fn reset(&mut self) {
        for ch in &mut self.channels {
            ch.reset();
        }
        self.ref_buf.clear();
        self.scratch_ref.clear();
        self.last_erle_db = 0.0;
    }

    /// Push far-end speaker playback samples (interleaved `f32`) into the reference buffer.
    pub fn push_reference(&mut self, playback_samples: &[f32]) {
        let max_buf = (self.cfg.sample_rate as usize * self.cfg.channels as usize).max(4800);
        if playback_samples.len() >= max_buf {
            self.ref_buf.clear();
            self.ref_buf.extend(&playback_samples[playback_samples.len() - max_buf..]);
            return;
        }
        let overflow = (self.ref_buf.len() + playback_samples.len()).saturating_sub(max_buf);
        if overflow > 0 {
            self.ref_buf.drain(..overflow);
        }
        self.ref_buf.extend(playback_samples.iter().copied());
    }

    /// Cancel echo in-place from captured microphone samples using buffered reference audio.
    pub fn process_capture(&mut self, capture_samples: &mut [f32]) {
        if capture_samples.is_empty() {
            return;
        }
        let needed = capture_samples.len();
        self.scratch_ref.clear();
        self.scratch_ref.reserve(needed);

        let avail = self.ref_buf.len().min(needed);
        for _ in 0..avail {
            if let Some(s) = self.ref_buf.pop_front() {
                self.scratch_ref.push(s);
            }
        }
        if self.scratch_ref.len() < needed {
            self.scratch_ref.resize(needed, 0.0);
        }

        // Move scratch out briefly so we can borrow `self` mutably without allocating.
        let far_ref = std::mem::take(&mut self.scratch_ref);
        self.process_aligned(capture_samples, &far_ref);
        self.scratch_ref = far_ref;
    }

    /// Cancel echo in-place using an explicit aligned far-end reference slice of matching length.
    pub fn process_aligned(&mut self, mic_samples: &mut [f32], far_end_reference: &[f32]) {
        let ch_count = self.channels.len();
        let n_samples = mic_samples.len().min(far_end_reference.len());
        let n_frames = n_samples / ch_count;
        if n_frames == 0 {
            return;
        }

        let mu = self.cfg.step_size;
        let eps = self.cfg.epsilon;
        let dtd_thr_sq = self.cfg.double_talk_threshold * self.cfg.double_talk_threshold;
        let sr = self.cfg.sample_rate as f32;
        // ~10 ms envelope smoother for DTD, ~25 ms smoother for ERLE readout.
        let alpha_env = (1.0 / (sr * 0.010)).clamp(0.0005, 0.05);
        let alpha_erle = (1.0 / (sr * 0.025)).clamp(0.0002, 0.02);
        let hangover_len = ((self.cfg.sample_rate as usize) / 66).max(64); // ~15 ms

        for frame_idx in 0..n_frames {
            let base = frame_idx * ch_count;
            for (c, ch) in self.channels.iter_mut().enumerate() {
                let idx = base + c;
                mic_samples[idx] = ch.process_sample(
                    mic_samples[idx],
                    far_end_reference[idx],
                    mu,
                    eps,
                    dtd_thr_sq,
                    alpha_env,
                    alpha_erle,
                    hangover_len,
                );
            }
        }

        let mut total_d = 0.0f32;
        let mut total_e = 0.0f32;
        for ch in &self.channels {
            total_d += ch.erle_d_power;
            total_e += ch.erle_e_power;
        }
        if total_d > 1e-9 {
            let ratio = (total_d / total_e.max(1e-12)).max(1.0);
            self.last_erle_db = (10.0 * ratio.log10()).clamp(0.0, 60.0);
        }
    }

    /// Drain reference audio from a shared [`AecReferenceFeed`] and process `capture_samples` in-place.
    pub fn process_capture_with_feed(&mut self, capture_samples: &mut [f32], feed: &AecReferenceFeed) {
        if capture_samples.is_empty() {
            return;
        }
        let mut tmp = std::mem::take(&mut self.scratch_ref);
        tmp.clear();
        feed.drain_into(&mut tmp, capture_samples.len());
        if !tmp.is_empty() {
            self.push_reference(&tmp);
        }
        self.scratch_ref = tmp;
        self.process_capture(capture_samples);
    }

    /// Last measured Echo Return Loss Enhancement (ERLE) in dB.
    pub fn last_erle_db(&self) -> f32 {
        self.last_erle_db
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;

    /// Deterministic xorshift64 PRNG producing zero-mean samples in `[-amp, amp]`.
    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.max(1))
        }
        fn next_f32(&mut self, amp: f32) -> f32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            let u = (x >> 40) as f32 / ((1u32 << 24) as f32);
            (u * 2.0 - 1.0) * amp
        }
    }

    /// Simulate a multi-tap delayed room impulse response (RIR) echo path on interleaved stereo PCM.
    fn simulate_echo_path(far: &[f32], channels: usize, rir_l: &[(usize, f32)], rir_r: &[(usize, f32)]) -> Vec<f32> {
        let frames = far.len() / channels;
        let mut out = vec![0.0f32; frames * channels];
        for n in 0..frames {
            for c in 0..channels {
                let taps = if c == 0 { rir_l } else { rir_r };
                let mut acc = 0.0f32;
                for &(delay, gain) in taps {
                    if n >= delay {
                        acc += gain * far[(n - delay) * channels + c];
                    }
                }
                out[n * channels + c] = acc;
            }
        }
        out
    }

    #[test]
    fn nlms_cancels_multi_tap_delayed_echo_above_15db_erle() {
        let cfg = AecConfig {
            sample_rate: 48_000,
            channels: 2,
            filter_len: 512,
            step_size: 0.5,
            epsilon: 1e-4,
            double_talk_threshold: 0.65,
        };
        let mut aec = AcousticEchoCanceller::new(cfg);
        assert_eq!(aec.config().filter_len, 512);

        // Generate 1.5 seconds of broadband far-end audio (stereo 48 kHz).
        let total_frames = 48_000 * 3 / 2;
        let mut rng = Rng::new(0xDEAD_BEEF_CAFE_BABE);
        let far: Vec<f32> = (0..total_frames * 2).map(|_| rng.next_f32(0.5)).collect();

        // Acoustic echo path: propagation delay + early room reflections (different per channel).
        let rir_l = [(48, 0.42), (96, -0.21), (173, 0.14), (289, -0.09)];
        let rir_r = [(64, 0.38), (131, 0.19), (220, -0.13), (340, 0.07)];
        let echo = simulate_echo_path(&far, 2, &rir_l, &rir_r);

        let chunk_samples = 480 * 2; // 10 ms stereo
        let mut processed = echo.clone();
        for offset in (0..processed.len()).step_by(chunk_samples) {
            let end = (offset + chunk_samples).min(processed.len());
            aec.process_aligned(&mut processed[offset..end], &far[offset..end]);
        }

        // Measure steady-state ERLE over the final 0.5 seconds.
        let tail_start = 48_000 * 2;
        let in_power: f64 = echo[tail_start..].iter().map(|&s| (s as f64) * (s as f64)).sum();
        let out_power: f64 = processed[tail_start..].iter().map(|&s| (s as f64) * (s as f64)).sum();
        let measured_erle_db = 10.0 * (in_power / out_power.max(1e-15)).log10();
        let reported_erle_db = aec.last_erle_db();

        eprintln!("AEC NLMS steady-state ERLE: measured={measured_erle_db:.2} dB, reported={reported_erle_db:.2} dB");
        assert!(measured_erle_db >= 15.0, "expected measured ERLE >= 15 dB, got {measured_erle_db:.2} dB");
        assert!(reported_erle_db >= 15.0, "expected reported ERLE >= 15 dB, got {reported_erle_db:.2} dB");
    }

    #[test]
    fn double_talk_preserves_near_end_speech_and_prevents_divergence() {
        let cfg = AecConfig {
            sample_rate: 48_000,
            channels: 1,
            filter_len: 256,
            step_size: 0.45,
            epsilon: 1e-4,
            double_talk_threshold: 0.65,
        };
        let mut aec = AcousticEchoCanceller::new(cfg);

        // 1.0s convergence + 0.5s double-talk + 0.5s post-double-talk echo only.
        let sr = 48_000usize;
        let total = sr * 2;
        let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
        let far: Vec<f32> = (0..total).map(|_| rng.next_f32(0.45)).collect();
        let rir = [(32, 0.40), (77, -0.18), (140, 0.11)];
        let echo = simulate_echo_path(&far, 1, &rir, &rir);

        let dt_start = sr;
        let dt_end = sr + sr / 2;
        let mut near_end = vec![0.0f32; total];
        for (i, slot) in near_end.iter_mut().enumerate().take(dt_end).skip(dt_start) {
            let t = i as f32 / sr as f32;
            *slot = 0.42 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                + 0.18 * (2.0 * std::f32::consts::PI * 880.0 * t).sin();
        }

        let mut mic: Vec<f32> = echo.iter().zip(near_end.iter()).map(|(&e, &s)| e + s).collect();
        let chunk = 480;
        for off in (0..total).step_by(chunk) {
            let end = (off + chunk).min(total);
            aec.process_aligned(&mut mic[off..end], &far[off..end]);
        }

        // During double-talk, output should closely match near_end (echo cancelled, near-end preserved!).
        let mut dot = 0.0f64;
        let mut norm_out = 0.0f64;
        let mut norm_near = 0.0f64;
        for i in (dt_start + 480)..dt_end {
            let o = mic[i] as f64;
            let s = near_end[i] as f64;
            dot += o * s;
            norm_out += o * o;
            norm_near += s * s;
        }
        let corr = dot / (norm_out.sqrt() * norm_near.sqrt()).max(1e-12);
        let energy_ratio = norm_out / norm_near.max(1e-12);
        eprintln!("AEC double-talk: correlation={corr:.4}, energy_ratio={energy_ratio:.4}");
        assert!(corr > 0.92, "near-end signal distorted during double-talk (corr={corr:.4})");
        assert!(
            (0.75..=1.25).contains(&energy_ratio),
            "near-end signal energy changed excessively during double-talk ({energy_ratio:.4})"
        );

        // Immediately after double-talk ends, echo cancellation must still be >= 15 dB (no divergence).
        let post_start = dt_end + 480;
        let echo_pow: f64 = echo[post_start..].iter().map(|&s| (s as f64) * (s as f64)).sum();
        let res_pow: f64 = mic[post_start..].iter().map(|&s| (s as f64) * (s as f64)).sum();
        let post_erle = 10.0 * (echo_pow / res_pow.max(1e-15)).log10();
        eprintln!("AEC post-double-talk ERLE: {post_erle:.2} dB");
        assert!(post_erle >= 15.0, "weights diverged during double-talk (post ERLE={post_erle:.2} dB)");
    }

    #[test]
    fn reference_feed_wraparound_and_concurrency() {
        // Wraparound keeps newest `max_samples`.
        let feed = AecReferenceFeed::new(500);
        let batch1: Vec<f32> = (0..400).map(|i| i as f32).collect();
        let batch2: Vec<f32> = (400..700).map(|i| i as f32).collect();
        feed.push(&batch1);
        feed.push(&batch2);
        assert_eq!(feed.len(), 500);

        let mut drained = Vec::new();
        let n = feed.drain_into(&mut drained, 500);
        assert_eq!(n, 500);
        assert_eq!(drained[0], 200.0);
        assert_eq!(drained[499], 699.0);
        assert!(feed.is_empty());

        // Single push larger than capacity keeps tail.
        let huge: Vec<f32> = (0..1200).map(|i| i as f32).collect();
        feed.push(&huge);
        assert_eq!(feed.len(), 500);
        drained.clear();
        feed.drain_into(&mut drained, 500);
        assert_eq!(drained[0], 700.0);
        assert_eq!(drained[499], 1199.0);

        // Multi-threaded producer/consumer with `process_capture_with_feed`.
        let shared = AecReferenceFeed::new(4800);
        let producer_feed = shared.clone();
        let producer = thread::spawn(move || {
            let chunk = vec![0.25f32; 480];
            for _ in 0..40 {
                producer_feed.push(&chunk);
                thread::yield_now();
            }
        });

        let mut aec = AcousticEchoCanceller::new(AecConfig { channels: 1, filter_len: 128, ..Default::default() });
        let mut mic = vec![0.1f32; 480];
        for _ in 0..40 {
            aec.process_capture_with_feed(&mut mic, &shared);
            assert!(mic.iter().all(|s| s.is_finite()));
            thread::yield_now();
        }
        producer.join().expect("producer thread panicked");
        shared.clear();
        assert!(shared.is_empty());
    }

    #[test]
    fn reset_and_nan_protection() {
        let mut aec = AcousticEchoCanceller::new(AecConfig { channels: 2, filter_len: 64, ..Default::default() });
        let mut mic = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.25];
        let far = vec![f32::INFINITY, f32::NAN, 0.5, -0.5];
        aec.process_aligned(&mut mic, &far);
        assert!(mic.iter().all(|s| s.is_finite()));

        aec.reset();
        assert_eq!(aec.last_erle_db(), 0.0);
    }
}
