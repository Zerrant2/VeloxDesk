//! Pure-Rust microphone background noise suppressor + smooth VAD noise gate.
//!
//! Architecture:
//! - Zero-latency 5-band power-complementary TPT (Topology-Preserving Transform)
//!   crossover filterbank (`<250 Hz`, `250–800 Hz`, `800–2200 Hz`, `2200–5500 Hz`, `>5500 Hz`)
//!   with exact unity reconstruction (`sum(bands) == x`) and zero phase overshoot.
//! - Adaptive per-band & broadband stationary noise-floor estimator (tracks fans, AC,
//!   line hiss; freezes adaptation during detected speech).
//! - Per-band Wiener-style soft spectral attenuation combined with a hysteresis VAD
//!   noise gate (fast attack so word onsets are never clipped, hangover + smooth release
//!   to eliminate clicks and pumping).
//! - Strict `NaN` / `Inf` / overload clamping protection.

use std::f32::consts::PI;

const NUM_BANDS: usize = 5;
const SPLIT_FREQS_HZ: [f32; NUM_BANDS - 1] = [250.0, 800.0, 2_200.0, 5_500.0];
/// Minimum RMS level below which a signal is treated as digital silence.
const SILENCE_FLOOR_RMS: f32 = 1e-5;
/// Physical ceiling on tracked stationary background noise RMS (-16 dBFS).
const MAX_NOISE_FLOOR_RMS: f32 = 0.16;
/// Hysteresis margin (in dB) below `vad_threshold_db` before speech transitions to inactive.
const VAD_HYSTERESIS_DB: f32 = 3.0;
/// Input clamping ceiling to protect internal filter state from extreme overload spikes.
const INPUT_OVERLOAD_CLAMP: f32 = 4.0;

/// Configuration for [`NoiseSuppressor`].
#[derive(Debug, Clone, Copy)]
pub struct DenoiseConfig {
    pub sample_rate: u32,
    pub channels: u16,
    /// Target stationary noise attenuation floor in dB (negative, e.g. `-24.0`).
    pub attenuation_db: f32,
    /// Speech detection threshold above estimated noise floor in dB (e.g. `6.0`).
    pub vad_threshold_db: f32,
    /// Fast attack time in ms so word onsets are never clipped (e.g. `3.0`).
    pub attack_ms: f32,
    /// Smooth release/hold time in ms preventing clicks/pumping (e.g. `120.0`).
    pub release_ms: f32,
}

impl Default for DenoiseConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 1,
            attenuation_db: -24.0,
            vad_threshold_db: 6.0,
            attack_ms: 3.0,
            release_ms: 120.0,
        }
    }
}

/// 1-pole Topology-Preserving Transform (trapezoidal) complementary low-pass / high-pass split.
///
/// Guarantees `lp + hp == x` identically and `|H_lp(jw)|^2 + |H_hp(jw)|^2 == 1` at all frequencies.
#[derive(Debug, Clone, Copy)]
struct OnePoleTpt {
    g_coeff: f32,
    state: f32,
}

impl OnePoleTpt {
    fn new(cutoff_hz: f32, sample_rate: f32) -> Self {
        let fc = cutoff_hz.clamp(20.0, sample_rate * 0.45);
        let g = (PI * fc / sample_rate).tan();
        Self { g_coeff: g / (1.0 + g), state: 0.0 }
    }

    #[inline]
    fn reset(&mut self) {
        self.state = 0.0;
    }

    #[inline]
    fn split(&mut self, x: f32) -> (f32, f32) {
        let v = (x - self.state) * self.g_coeff;
        let lp = v + self.state;
        let next = lp + v;
        if next.is_finite() {
            self.state = next;
            (lp, x - lp)
        } else {
            self.state = 0.0;
            (0.0, x)
        }
    }
}

/// Per-channel 5-band cascaded complementary crossover state.
#[derive(Debug, Clone)]
struct ChannelBank {
    splits: [OnePoleTpt; NUM_BANDS - 1],
}

impl ChannelBank {
    fn new(sample_rate: f32) -> Self {
        Self {
            splits: [
                OnePoleTpt::new(SPLIT_FREQS_HZ[0], sample_rate),
                OnePoleTpt::new(SPLIT_FREQS_HZ[1], sample_rate),
                OnePoleTpt::new(SPLIT_FREQS_HZ[2], sample_rate),
                OnePoleTpt::new(SPLIT_FREQS_HZ[3], sample_rate),
            ],
        }
    }

    fn reset(&mut self) {
        for s in &mut self.splits {
            s.reset();
        }
    }

    #[inline]
    fn decompose(&mut self, x: f32) -> [f32; NUM_BANDS] {
        let (b0, r0) = self.splits[0].split(x);
        let (b1, r1) = self.splits[1].split(r0);
        let (b2, r2) = self.splits[2].split(r1);
        let (b3, b4) = self.splits[3].split(r2);
        [b0, b1, b2, b3, b4]
    }
}

#[inline]
fn exp_coeff(time_ms: f32, sample_rate: f32) -> f32 {
    let tau_samples = (time_ms * 0.001 * sample_rate).max(1.0);
    1.0 - (-1.0 / tau_samples).exp()
}

#[inline]
fn db_to_gain(db: f32) -> f32 {
    10.0f32.powf(db * 0.05)
}

#[inline]
fn power_to_db(power: f32) -> f32 {
    10.0 * power.max(1e-12).log10()
}

/// Pure-Rust microphone background noise suppressor & VAD gate.
pub struct NoiseSuppressor {
    cfg: DenoiseConfig,
    banks: Vec<ChannelBank>,
    /// Fast per-band signal power envelopes (linked across channels).
    band_power: [f32; NUM_BANDS],
    /// Medium-window per-band power envelopes (~18 ms) for stable VAD / pink-noise immunity.
    band_vad_power: [f32; NUM_BANDS],
    /// Adaptive per-band stationary noise power estimates.
    band_noise: [f32; NUM_BANDS],
    /// Smoothed per-band Wiener-like linear gains.
    band_gain: [f32; NUM_BANDS],
    /// Fast broadband signal power envelope (~4 ms).
    sig_power: f32,
    /// Medium broadband signal power envelope (~18 ms) for stable VAD.
    vad_power: f32,
    /// Slow broadband signal power envelope (~60 ms) for stationarity tracking.
    slow_power: f32,
    /// Adaptive broadband stationary noise power estimate.
    noise_power: f32,
    /// Smooth broadband VAD noise gate linear gain.
    gate_gain: f32,
    /// Remaining speech hangover frames before gate begins smooth release.
    hangover_frames: usize,
    /// Total processed frames since creation or [`Self::reset`].
    frames_processed: u64,
    /// Precomputed smoothing coefficients and bounds.
    alpha_fast_env: f32,
    alpha_vad_env: f32,
    alpha_slow_env: f32,
    alpha_noise_down: f32,
    alpha_noise_up: f32,
    alpha_noise_init: f32,
    alpha_gate_attack: f32,
    alpha_gate_release: f32,
    alpha_band_attack: f32,
    alpha_band_release: f32,
    hangover_len: usize,
    init_frames: u64,
    min_gain: f32,
    /// Public telemetry fields.
    speech_active: bool,
    noise_floor_rms: f32,
    last_suppression_db: f32,
}

impl NoiseSuppressor {
    pub fn new(cfg: DenoiseConfig) -> Self {
        let sample_rate = cfg.sample_rate.clamp(8_000, 192_000);
        let channels = cfg.channels.clamp(1, 8);
        let attenuation_db = if cfg.attenuation_db.is_finite() {
            cfg.attenuation_db.clamp(-60.0, 0.0)
        } else {
            -24.0
        };
        let vad_threshold_db = if cfg.vad_threshold_db.is_finite() {
            cfg.vad_threshold_db.clamp(0.5, 40.0)
        } else {
            6.0
        };
        let attack_ms = if cfg.attack_ms.is_finite() { cfg.attack_ms.clamp(0.5, 100.0) } else { 3.0 };
        let release_ms = if cfg.release_ms.is_finite() { cfg.release_ms.clamp(10.0, 1000.0) } else { 120.0 };

        let normalized = DenoiseConfig {
            sample_rate,
            channels,
            attenuation_db,
            vad_threshold_db,
            attack_ms,
            release_ms,
        };
        let sr = sample_rate as f32;
        let banks = (0..channels as usize).map(|_| ChannelBank::new(sr)).collect();

        let min_gain = db_to_gain(attenuation_db).clamp(1e-3, 1.0);
        // Hangover is ~35% of release_ms (clamped 15..=60 ms) so short pauses don't pump.
        let hangover_ms = (release_ms * 0.35).clamp(15.0, 60.0);
        let hangover_len = ((hangover_ms * 0.001 * sr).round() as usize).max(1);
        // Initial fast noise-calibration window (~80 ms) for stationary broadband startup noise.
        let init_frames = ((0.080 * sr).round() as u64).max(64);

        // Exponential smoothing constants; attack reaches ~90% within attack_ms.
        let alpha_gate_attack = exp_coeff((attack_ms * 0.45).max(0.25), sr);
        let alpha_gate_release = exp_coeff((release_ms * 0.45).max(4.0), sr);

        Self {
            cfg: normalized,
            banks,
            band_power: [0.0; NUM_BANDS],
            band_vad_power: [0.0; NUM_BANDS],
            band_noise: [1e-10; NUM_BANDS],
            band_gain: [1.0; NUM_BANDS],
            sig_power: 0.0,
            vad_power: 0.0,
            slow_power: 0.0,
            noise_power: 1e-10,
            gate_gain: 1.0,
            hangover_frames: 0,
            frames_processed: 0,
            alpha_fast_env: exp_coeff(4.0, sr),
            alpha_vad_env: exp_coeff(18.0, sr),
            alpha_slow_env: exp_coeff(60.0, sr),
            alpha_noise_down: exp_coeff(35.0, sr),
            alpha_noise_up: exp_coeff(350.0, sr),
            alpha_noise_init: exp_coeff(20.0, sr),
            alpha_gate_attack,
            alpha_gate_release,
            alpha_band_attack: exp_coeff(3.0, sr),
            alpha_band_release: exp_coeff(45.0, sr),
            hangover_len,
            init_frames,
            min_gain,
            speech_active: false,
            noise_floor_rms: 0.0,
            last_suppression_db: 0.0,
        }
    }

    pub fn config(&self) -> DenoiseConfig {
        self.cfg
    }

    pub fn reset(&mut self) {
        for bank in &mut self.banks {
            bank.reset();
        }
        self.band_power = [0.0; NUM_BANDS];
        self.band_vad_power = [0.0; NUM_BANDS];
        self.band_noise = [1e-10; NUM_BANDS];
        self.band_gain = [1.0; NUM_BANDS];
        self.sig_power = 0.0;
        self.vad_power = 0.0;
        self.slow_power = 0.0;
        self.noise_power = 1e-10;
        self.gate_gain = 1.0;
        self.hangover_frames = 0;
        self.frames_processed = 0;
        self.speech_active = false;
        self.noise_floor_rms = 0.0;
        self.last_suppression_db = 0.0;
    }

    /// Process interleaved `f32` PCM samples in-place.
    pub fn process(&mut self, samples: &mut [f32]) {
        if samples.is_empty() {
            return;
        }
        let ch_count = self.banks.len();
        let n_frames = samples.len() / ch_count;
        // Sanitize any trailing incomplete interleaved samples.
        for s in &mut samples[n_frames * ch_count..] {
            *s = if s.is_finite() { s.clamp(-1.0, 1.0) } else { 0.0 };
        }
        if n_frames == 0 {
            return;
        }

        let inv_ch = 1.0 / ch_count as f32;
        let max_noise_pow = MAX_NOISE_FLOOR_RMS * MAX_NOISE_FLOOR_RMS;
        let silence_pow = SILENCE_FLOOR_RMS * SILENCE_FLOOR_RMS;
        let open_thr_db = self.cfg.vad_threshold_db;
        let close_thr_db = (open_thr_db - VAD_HYSTERESIS_DB).max(1.0);
        let max_att_db = (-self.cfg.attenuation_db).max(0.0);

        let mut in_energy = 0.0f64;
        let mut out_energy = 0.0f64;
        // Stack buffer for up to 8 channels x 5 bands (zero heap allocations on hot path).
        let mut ch_bands = [[0.0f32; NUM_BANDS]; 8];

        for frame_idx in 0..n_frames {
            let base = frame_idx * ch_count;
            let mut frame_inst_pow = 0.0f32;
            let mut band_inst_pow = [0.0f32; NUM_BANDS];

            for c in 0..ch_count {
                let raw = samples[base + c];
                let x = if raw.is_finite() {
                    raw.clamp(-INPUT_OVERLOAD_CLAMP, INPUT_OVERLOAD_CLAMP)
                } else {
                    0.0
                };
                in_energy += (x as f64) * (x as f64);
                frame_inst_pow += x * x;

                let bands = self.banks[c].decompose(x);
                ch_bands[c] = bands;
                for b in 0..NUM_BANDS {
                    band_inst_pow[b] += bands[b] * bands[b];
                }
            }

            frame_inst_pow *= inv_ch;
            for p in &mut band_inst_pow {
                *p *= inv_ch;
            }

            // Update fast, VAD-smoothed (~18 ms), and slow broadband & per-band envelopes.
            self.sig_power += self.alpha_fast_env * (frame_inst_pow - self.sig_power);
            self.vad_power += self.alpha_vad_env * (frame_inst_pow - self.vad_power);
            self.slow_power += self.alpha_slow_env * (frame_inst_pow - self.slow_power);
            for b in 0..NUM_BANDS {
                self.band_power[b] += self.alpha_fast_env * (band_inst_pow[b] - self.band_power[b]);
                self.band_vad_power[b] += self.alpha_vad_env * (band_inst_pow[b] - self.band_vad_power[b]);
            }

            // Spectral shape cues:
            // - Voiced speech / tonal signals concentrate >95% of energy below 5.5 kHz with
            //   very low high-band energy (`>5.5 kHz`) and low spectral flatness across bands.
            // - Stationary white/pink noise spreads power across all 5 bands including `>5.5 kHz`.
            let mut sum_bp = 0.0f32;
            let mut log_sum = 0.0f32;
            for &bp in &self.band_vad_power {
                let p = bp.max(1e-12);
                sum_bp += p;
                log_sum += p.ln();
            }
            let arith_mean = (sum_bp / NUM_BANDS as f32).max(1e-12);
            let geom_mean = (log_sum / NUM_BANDS as f32).exp();
            let flatness = (geom_mean / arith_mean).clamp(0.0, 1.0);
            let voice_band_ratio =
                (self.band_vad_power[0] + self.band_vad_power[1] + self.band_vad_power[2]) / sum_bp.max(1e-12);
            let high_band_ratio = self.band_vad_power[4] / sum_bp.max(1e-12);

            // Detect clean voiced/tonal speech even with zero prior noise calibration.
            let clearly_tonal = self.sig_power > silence_pow
                && high_band_ratio < 0.025
                && voice_band_ratio > 0.85
                && flatness < 0.12;
            let broadband_noise_like = high_band_ratio >= 0.035 && flatness >= 0.12;

            // Initial calibration window: rapidly lock onto stationary broadband noise if present,
            // never absorbing clean speech/tones or loud signals above the noise floor ceiling.
            let in_init = self.frames_processed < self.init_frames;
            if in_init && broadband_noise_like && !clearly_tonal && self.sig_power <= max_noise_pow {
                let a = self.alpha_noise_init;
                self.noise_power += a * (self.vad_power - self.noise_power);
                for b in 0..NUM_BANDS {
                    self.band_noise[b] += a * (self.band_vad_power[b] - self.band_noise[b]);
                }
            }

            // Broadband SNR (using fast envelope for quick speech onset, and smoothed VAD
            // envelope for pink-noise stability) and peak mid-speech-band SNR.
            let fast_snr_db = power_to_db(self.sig_power) - power_to_db(self.noise_power);
            let smooth_snr_db = power_to_db(self.vad_power) - power_to_db(self.noise_power);
            let mut max_speech_band_snr_db = f32::NEG_INFINITY;
            for b in 1..=2 {
                let b_snr = power_to_db(self.band_vad_power[b]) - power_to_db(self.band_noise[b]);
                if b_snr > max_speech_band_snr_db {
                    max_speech_band_snr_db = b_snr;
                }
            }

            // Hysteresis VAD decision + hangover timer.
            let above_silence = self.sig_power > silence_pow;
            let vad_raw = if self.speech_active {
                above_silence
                    && (smooth_snr_db >= close_thr_db
                        || fast_snr_db >= open_thr_db
                        || max_speech_band_snr_db >= close_thr_db + 4.5
                        || clearly_tonal)
            } else {
                (above_silence
                    && !in_init
                    && (smooth_snr_db >= open_thr_db
                        || (fast_snr_db >= open_thr_db + 2.5 && max_speech_band_snr_db >= open_thr_db + 2.0)
                        || max_speech_band_snr_db >= open_thr_db + 6.0
                        || clearly_tonal))
                    || (in_init && clearly_tonal)
            };

            if vad_raw {
                self.speech_active = true;
                self.hangover_frames = self.hangover_len;
            } else if self.hangover_frames > 0 {
                self.hangover_frames -= 1;
                self.speech_active = true;
            } else {
                self.speech_active = false;
            }

            // Adaptive stationary noise estimator (FREEZES upward updates during speech!).
            // Downward adaptation always runs so the noise floor tracks quieter rooms quickly.
            if self.vad_power < self.noise_power {
                self.noise_power += self.alpha_noise_down * (self.vad_power - self.noise_power);
            } else if !self.speech_active && !clearly_tonal && broadband_noise_like {
                let steady = self.slow_power > 1e-12 && (self.vad_power / self.slow_power) < 2.2;
                if steady || in_init {
                    let target = self.vad_power.min(max_noise_pow);
                    self.noise_power += self.alpha_noise_up * (target - self.noise_power);
                }
            }

            for b in 0..NUM_BANDS {
                if self.band_vad_power[b] < self.band_noise[b] {
                    self.band_noise[b] += self.alpha_noise_down * (self.band_vad_power[b] - self.band_noise[b]);
                } else if !self.speech_active && !clearly_tonal && broadband_noise_like {
                    let target = self.band_vad_power[b].min(max_noise_pow);
                    self.band_noise[b] += self.alpha_noise_up * (target - self.band_noise[b]);
                }
            }

            // Smooth broadband VAD noise gate with fast attack & smooth release.
            let target_gate = if self.speech_active { 1.0 } else { self.min_gain };
            let gate_alpha = if target_gate > self.gate_gain {
                self.alpha_gate_attack
            } else {
                self.alpha_gate_release
            };
            self.gate_gain += gate_alpha * (target_gate - self.gate_gain);

            // Per-band soft Wiener spectral suppression:
            // - Preserves bands carrying active speech (SNR >> 1 -> gain ~ 1.0).
            // - Attenuates noise-dominated bands even while speech is active in other bands.
            for b in 0..NUM_BANDS {
                let p = self.band_power[b];
                let n = self.band_noise[b];
                let target_band_gain = if p <= 1e-12 {
                    self.min_gain
                } else {
                    // Over-subtraction factor of 1.35 cleans residual stationary noise smoothly.
                    let clean_est = (p - 1.35 * n).max(0.0);
                    let wiener = (clean_est / (clean_est + 0.65 * n + 1e-12)).sqrt();
                    wiener.clamp(self.min_gain, 1.0)
                };
                let b_alpha = if target_band_gain > self.band_gain[b] {
                    self.alpha_band_attack
                } else {
                    self.alpha_band_release
                };
                self.band_gain[b] += b_alpha * (target_band_gain - self.band_gain[b]);
            }

            // Reconstruct output: during active speech, per-band Wiener preserves speech bands
            // at ~1.0 while suppressing out-of-band noise; during noise pauses, gate smoothly
            // brings the entire signal down to `min_gain` (`attenuation_db`).
            for c in 0..ch_count {
                let mut acc = 0.0f32;
                for b in 0..NUM_BANDS {
                    let g = (self.band_gain[b] * self.gate_gain).clamp(self.min_gain, 1.0);
                    acc += ch_bands[c][b] * g;
                }
                let out = if acc.is_finite() { acc.clamp(-1.0, 1.0) } else { 0.0 };
                samples[base + c] = out;
                out_energy += (out as f64) * (out as f64);
            }

            self.frames_processed = self.frames_processed.saturating_add(1);
        }

        self.noise_floor_rms = self.noise_power.max(0.0).sqrt();
        if in_energy > 1e-12 {
            let ratio = (in_energy / out_energy.max(1e-15)).max(1.0);
            self.last_suppression_db = (10.0 * ratio.log10() as f32).clamp(0.0, max_att_db);
        } else {
            self.last_suppression_db = (-20.0 * self.gate_gain.clamp(1e-4, 1.0).log10()).clamp(0.0, max_att_db);
        }
    }

    /// Whether the VAD currently detects active speech.
    pub fn is_speech_active(&self) -> bool {
        self.speech_active
    }

    /// Current estimated stationary background noise RMS level.
    pub fn noise_floor_rms(&self) -> f32 {
        self.noise_floor_rms
    }

    /// Last measured noise suppression / attenuation in dB.
    pub fn last_suppression_db(&self) -> f32 {
        self.last_suppression_db
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aec::{AcousticEchoCanceller, AecConfig};

    /// Deterministic xorshift64 PRNG for repeatable white/pink noise tests.
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

    /// Paul Kellet 3-stage pink noise filter (`-3 dB/oct` spectrum).
    struct PinkNoise {
        rng: Rng,
        b0: f32,
        b1: f32,
        b2: f32,
    }

    impl PinkNoise {
        fn new(seed: u64) -> Self {
            Self { rng: Rng::new(seed), b0: 0.0, b1: 0.0, b2: 0.0 }
        }
        fn next(&mut self, scale: f32) -> f32 {
            let white = self.rng.next_f32(1.0);
            self.b0 = 0.99765 * self.b0 + white * 0.0990460;
            self.b1 = 0.96300 * self.b1 + white * 0.2965164;
            self.b2 = 0.57000 * self.b2 + white * 1.0526913;
            (self.b0 + self.b1 + self.b2 + white * 0.1848) * scale * 0.22
        }
    }

    fn rms(samples: &[f32]) -> f64 {
        let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum_sq / samples.len().max(1) as f64).sqrt()
    }

    fn speech_signal(sr: usize, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| {
                let t = i as f32 / sr as f32;
                // Smooth 5 ms fade-in / fade-out envelope + voiced harmonic complex (220/440/660/880 Hz).
                let env_in = (i as f32 / (sr as f32 * 0.005)).min(1.0);
                let env_out = ((len - 1 - i) as f32 / (sr as f32 * 0.005)).min(1.0);
                let env = env_in.min(env_out);
                let tone = 0.22 * (2.0 * PI * 220.0 * t).sin()
                    + 0.30 * (2.0 * PI * 440.0 * t).sin()
                    + 0.16 * (2.0 * PI * 660.0 * t).sin()
                    + 0.10 * (2.0 * PI * 880.0 * t).sin();
                tone * env
            })
            .collect()
    }

    #[test]
    fn stationary_white_and_pink_noise_suppression_exceeds_12db_snr_gain() {
        let sr = 48_000usize;
        let chunk = 480; // 10 ms

        for kind in ["white", "pink"] {
            let mut ns = NoiseSuppressor::new(DenoiseConfig::default());
            assert_eq!(ns.config().sample_rate, 48_000);

            // 0.6s stationary noise -> 0.5s speech + noise -> 0.5s stationary noise.
            let n_lead = sr * 6 / 10;
            let n_speech = sr / 2;
            let n_tail = sr / 2;
            let total = n_lead + n_speech + n_tail;

            let mut rng = Rng::new(0xA1B2_C3D4_E5F6_0718);
            let mut pink = PinkNoise::new(0x1357_2468_9ACE_BDF0);
            let noise: Vec<f32> = (0..total)
                .map(|_| if kind == "white" { rng.next_f32(0.08) } else { pink.next(0.14) })
                .collect();
            let speech = speech_signal(sr, n_speech);

            let mut input = noise.clone();
            for i in 0..n_speech {
                input[n_lead + i] += speech[i];
            }

            let mut output = input.clone();
            let mut speech_detected_frames = 0usize;
            for off in (0..total).step_by(chunk) {
                let end = (off + chunk).min(total);
                ns.process(&mut output[off..end]);
                if off >= n_lead + chunk * 2 && end <= n_lead + n_speech - chunk * 2 && ns.is_speech_active() {
                    speech_detected_frames += 1;
                }
            }

            // Measure input vs output SNR: speech segment vs steady-state tail noise segment.
            let sp_slice_in = &input[n_lead + 480..n_lead + n_speech - 480];
            let sp_slice_out = &output[n_lead + 480..n_lead + n_speech - 480];
            let tail_start = n_lead + n_speech + sr * 25 / 100; // after hangover + release settles
            let ns_slice_in = &noise[tail_start..];
            let ns_slice_out = &output[tail_start..];

            let snr_in_db = 20.0 * (rms(sp_slice_in) / rms(ns_slice_in).max(1e-12)).log10();
            let snr_out_db = 20.0 * (rms(sp_slice_out) / rms(ns_slice_out).max(1e-12)).log10();
            let snr_gain_db = snr_out_db - snr_in_db;
            let noise_att_db = 20.0 * (rms(ns_slice_in) / rms(ns_slice_out).max(1e-12)).log10();

            eprintln!(
                "Denoise ({kind}): SNR_in={snr_in_db:.2} dB -> SNR_out={snr_out_db:.2} dB (gain={snr_gain_db:.2} dB, noise_att={noise_att_db:.2} dB, floor_rms={:.4}, last_supp={:.2} dB)",
                ns.noise_floor_rms(),
                ns.last_suppression_db()
            );

            assert!(speech_detected_frames > 20, "{kind}: VAD failed to activate during speech");
            assert!(!ns.is_speech_active(), "{kind}: VAD should deactivate during trailing noise");
            assert!(ns.noise_floor_rms() > 0.005, "{kind}: noise floor estimator did not track noise");
            assert!(
                snr_gain_db >= 12.0,
                "{kind}: expected >= 12 dB SNR improvement, got {snr_gain_db:.2} dB"
            );
            assert!(
                noise_att_db >= 15.0,
                "{kind}: expected >= 15 dB stationary noise attenuation, got {noise_att_db:.2} dB"
            );
            assert!(
                ns.last_suppression_db() >= 12.0,
                "{kind}: last_suppression_db should report >= 12 dB in noise, got {:.2}",
                ns.last_suppression_db()
            );
        }
    }

    #[test]
    fn speech_and_tonal_signal_preservation_without_clicks() {
        let sr = 48_000usize;
        let chunk = 240; // 5 ms
        let mut ns = NoiseSuppressor::new(DenoiseConfig {
            sample_rate: sr as u32,
            channels: 1,
            attenuation_db: -24.0,
            vad_threshold_db: 6.0,
            attack_ms: 3.0,
            release_ms: 120.0,
        });

        // Clean speech/tonal signal from t=0 (no noise lead-in) followed by silence.
        let n_speech = sr / 2;
        let n_silence = sr / 4;
        let speech = speech_signal(sr, n_speech);
        let mut buf = speech.clone();
        buf.resize(n_speech + n_silence, 0.0);

        for off in (0..buf.len()).step_by(chunk) {
            let end = (off + chunk).min(buf.len());
            ns.process(&mut buf[off..end]);
        }

        // After 10 ms attack, speech should correlate closely with original and retain energy.
        let start = sr / 100; // 10 ms
        let end = n_speech - sr / 100;
        let mut dot = 0.0f64;
        let mut norm_ref = 0.0f64;
        let mut norm_out = 0.0f64;
        for i in start..end {
            let r = speech[i] as f64;
            let o = buf[i] as f64;
            dot += r * o;
            norm_ref += r * r;
            norm_out += o * o;
        }
        let corr = dot / (norm_ref.sqrt() * norm_out.sqrt()).max(1e-12);
        let gain_ratio = (norm_out / norm_ref.max(1e-12)).sqrt();

        // Check for clicks: maximum sample-to-sample step should stay close to input's max step.
        let max_step_in = speech.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let max_step_out = buf.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);

        eprintln!(
            "Denoise preservation: corr={corr:.4}, gain_ratio={gain_ratio:.4}, max_step_in={max_step_in:.4}, max_step_out={max_step_out:.4}"
        );
        assert!(corr > 0.96, "speech waveform distorted (corr={corr:.4})");
        assert!(
            (0.85..=1.05).contains(&gain_ratio),
            "speech amplitude altered too much (gain_ratio={gain_ratio:.4})"
        );
        assert!(
            max_step_out <= max_step_in * 1.35 + 0.01,
            "detected click/discontinuity in output (max_step_out={max_step_out:.4} vs in={max_step_in:.4})"
        );
    }

    #[test]
    fn combined_noise_suppressor_and_aec_chain() {
        let sr = 48_000usize;
        let chunk = 480; // 10 ms mono
        let mut ns = NoiseSuppressor::new(DenoiseConfig {
            sample_rate: sr as u32,
            channels: 1,
            ..Default::default()
        });
        let mut aec = AcousticEchoCanceller::new(AecConfig {
            sample_rate: sr as u32,
            channels: 1,
            filter_len: 256,
            step_size: 0.45,
            epsilon: 1e-4,
            double_talk_threshold: 0.65,
        });

        // 1.0s far-end echo + stationary background noise -> 0.4s near-end speech + echo + noise -> 0.5s noise only.
        let n_echo = sr;
        let n_speech = sr * 4 / 10;
        let n_tail = sr / 2;
        let total = n_echo + n_speech + n_tail;

        let mut rng_far = Rng::new(0xDEAD_BEEF_1111_2222);
        let mut rng_noise = Rng::new(0xCAFE_BABE_3333_4444);
        let mut far = vec![0.0f32; total];
        for s in far.iter_mut().take(n_echo + n_speech) {
            *s = rng_far.next_f32(0.45);
        }
        let noise: Vec<f32> = (0..total).map(|_| rng_noise.next_f32(0.04)).collect();
        let speech = speech_signal(sr, n_speech);

        // Acoustic echo path: delayed far-end reflections.
        let mut mic = vec![0.0f32; total];
        for i in 0..total {
            let mut echo = 0.0f32;
            if i >= 40 {
                echo += 0.38 * far[i - 40];
            }
            if i >= 95 {
                echo -= 0.16 * far[i - 95];
            }
            let near = if i >= n_echo && i < n_echo + n_speech {
                speech[i - n_echo]
            } else {
                0.0
            };
            mic[i] = echo + near + noise[i];
        }

        // Run combined chain: AEC cancels echo first (preserving linear echo path), then NoiseSuppressor removes residual noise.
        // Also verify NoiseSuppressor -> AEC order stays numerically stable and finite.
        let mut out_aec_ns = mic.clone();
        let mut out_ns_aec = mic.clone();
        let mut ns2 = NoiseSuppressor::new(DenoiseConfig {
            sample_rate: sr as u32,
            channels: 1,
            ..Default::default()
        });
        let mut aec2 = AcousticEchoCanceller::new(AecConfig {
            sample_rate: sr as u32,
            channels: 1,
            filter_len: 256,
            step_size: 0.45,
            epsilon: 1e-4,
            double_talk_threshold: 0.65,
        });

        for off in (0..total).step_by(chunk) {
            let end = (off + chunk).min(total);
            aec.process_aligned(&mut out_aec_ns[off..end], &far[off..end]);
            ns.process(&mut out_aec_ns[off..end]);

            ns2.process(&mut out_ns_aec[off..end]);
            aec2.process_aligned(&mut out_ns_aec[off..end], &far[off..end]);
        }

        assert!(out_aec_ns.iter().all(|s| s.is_finite()));
        assert!(out_ns_aec.iter().all(|s| s.is_finite()));

        // Trailing stationary noise must be heavily suppressed in both chains.
        let tail_start = n_echo + n_speech + sr * 25 / 100;
        let tail_in_rms = rms(&mic[tail_start..]);
        let tail_aec_ns_rms = rms(&out_aec_ns[tail_start..]);
        let tail_ns_aec_rms = rms(&out_ns_aec[tail_start..]);
        let att_aec_ns = 20.0 * (tail_in_rms / tail_aec_ns_rms.max(1e-12)).log10();
        let att_ns_aec = 20.0 * (tail_in_rms / tail_ns_aec_rms.max(1e-12)).log10();
        eprintln!("Combined chain tail noise attenuation: AEC->NS={att_aec_ns:.2} dB, NS->AEC={att_ns_aec:.2} dB");
        assert!(att_aec_ns >= 15.0, "AEC->NS tail suppression too low: {att_aec_ns:.2} dB");
        assert!(att_ns_aec >= 12.0, "NS->AEC tail suppression too low: {att_ns_aec:.2} dB");

        // Speech segment must remain strong in both chains.
        let sp_rms_aec_ns = rms(&out_aec_ns[n_echo + 960..n_echo + n_speech - 960]);
        let sp_rms_ns_aec = rms(&out_ns_aec[n_echo + 960..n_echo + n_speech - 960]);
        assert!(sp_rms_aec_ns > 0.15, "AEC->NS attenuated speech too much: {sp_rms_aec_ns}");
        assert!(sp_rms_ns_aec > 0.12, "NS->AEC attenuated speech too much: {sp_rms_ns_aec}");
    }

    #[test]
    fn nan_inf_overload_protection_and_reset() {
        let mut ns = NoiseSuppressor::new(DenoiseConfig {
            sample_rate: 48_000,
            channels: 2,
            ..Default::default()
        });
        let mut buf = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 1e9, -1e9, 0.25, -0.25, 0.0];
        ns.process(&mut buf);
        assert!(buf.iter().all(|s| s.is_finite() && (-1.0..=1.0).contains(s)));

        ns.reset();
        assert!(!ns.is_speech_active());
        assert_eq!(ns.noise_floor_rms(), 0.0);
        assert_eq!(ns.last_suppression_db(), 0.0);
    }
}
