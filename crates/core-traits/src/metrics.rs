//! Per-frame latency accounting shared by host, viewer and the Performance HUD.

use crate::transport::PathKind;

/// Per-stage durations of one frame, µs. Host stages arrive via
/// `FrameTimingReport`; viewer stages are measured locally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageTimings {
    pub capture_us: u32,
    pub convert_us: u32,
    pub encode_us: u32,
    /// Packetize + FEC + network transit + reassembly (capture-ts → frame complete on
    /// the viewer, minus host stages; needs clock sync).
    pub network_us: u32,
    pub decode_us: u32,
    pub render_us: u32,
    /// Glass-to-glass estimate: host capture timestamp → present on the viewer.
    pub total_us: u32,
}

/// Snapshot rendered by the HUD (updated ~4×/s with smoothed values).
#[derive(Debug, Clone, Default)]
pub struct HudSnapshot {
    pub timings: StageTimings,
    pub rtt_us: u32,
    pub fps: f32,
    pub codec: String,
    pub encoder: String,
    pub hw_encode: bool,
    pub decoder: String,
    pub hw_decode: bool,
    pub zero_copy: bool,
    pub bitrate_kbps: u32,
    pub packet_loss_pct: f32,
    pub fec_recovery_pct: f32,
    pub path: PathKind,
    pub resolution: (u32, u32),
}

/// Exponential moving average helper for HUD smoothing.
#[derive(Debug, Clone, Copy)]
pub struct Ema {
    value: f64,
    alpha: f64,
    init: bool,
}

impl Ema {
    pub fn new(alpha: f64) -> Self {
        Self { value: 0.0, alpha, init: false }
    }
    pub fn update(&mut self, sample: f64) -> f64 {
        if self.init {
            self.value += self.alpha * (sample - self.value);
        } else {
            self.value = sample;
            self.init = true;
        }
        self.value
    }
    pub fn get(&self) -> f64 {
        self.value
    }
}

/// Fixed-capacity latency histogram (for p50/p95/p99 in benches and the HUD).
#[derive(Debug, Clone, Default)]
pub struct LatencySamples {
    samples: Vec<u32>,
}

impl LatencySamples {
    pub fn push(&mut self, us: u32) {
        self.samples.push(us);
    }
    pub fn len(&self) -> usize {
        self.samples.len()
    }
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }
    /// Percentile in 0..=100 (nearest-rank).
    pub fn percentile(&self, p: f64) -> Option<u32> {
        if self.samples.is_empty() {
            return None;
        }
        let mut s = self.samples.clone();
        s.sort_unstable();
        let rank = ((p / 100.0) * s.len() as f64).ceil().max(1.0) as usize - 1;
        Some(s[rank.min(s.len() - 1)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut l = LatencySamples::default();
        for i in 1..=100 {
            l.push(i);
        }
        assert_eq!(l.percentile(50.0), Some(50));
        assert_eq!(l.percentile(99.0), Some(99));
        assert_eq!(l.percentile(100.0), Some(100));
    }
}
