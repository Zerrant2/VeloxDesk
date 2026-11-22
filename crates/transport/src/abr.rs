//! Host-side adaptive bitrate + FEC ratio controller (GCC-like, ARCHITECTURE §6).
//!
//! Input: `ReceiverReport`s from the viewer (every ~50 ms). The controller tracks the
//! *network budget* (total datagram rate incl. parity) and derives the encoder bitrate
//! as `budget / (1 + parity%)`.
//!
//! * **Overuse** (delay gradient > threshold for 2 reports, or > 2× threshold once):
//!   budget = 0.85 × min(budget, measured recv rate), then hold increases for 500 ms.
//! * **Heavy loss** (> 10 % raw loss): budget × (1 − 0.5·loss).
//! * **Underuse** (gradient < −threshold, queues draining): hold.
//! * **Normal**: additive increase of `increase_per_sec` (default +5 %/s) up to max.
//! * **Parity**: `clamp(2 × loss_ema, 5 %, 50 %)` — loss EMA rises fast, decays slowly.

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::video::EncoderUpdate;
use veloxdesk_protocol::control::{BitrateUpdate, ReceiverReport};

/// New encoder / FEC targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbrDecision {
    /// Encoder target bitrate (video payload only, parity excluded).
    pub bitrate_kbps: u32,
    pub parity_percent: u8,
}

impl AbrDecision {
    pub fn encoder_update(&self) -> EncoderUpdate {
        EncoderUpdate { bitrate_kbps: Some(self.bitrate_kbps), fps: None }
    }
}

/// Tunables of [`AbrController`].
#[derive(Debug, Clone)]
pub struct AbrConfig {
    pub min_kbps: u32,
    pub max_kbps: u32,
    /// Overuse threshold of `ReceiverReport::delay_gradient` (µs of queue growth per frame).
    pub overuse_us_per_frame: f32,
    pub decrease_factor: f64,
    pub increase_per_sec: f64,
    pub hard_loss: f64,
    pub min_parity_percent: u8,
    pub max_parity_percent: u8,
    pub hold_after_decrease_us: u64,
    pub min_decrease_interval_us: u64,
}

impl Default for AbrConfig {
    fn default() -> Self {
        Self {
            min_kbps: 500,
            max_kbps: 150_000,
            overuse_us_per_frame: 250.0,
            decrease_factor: 0.85,
            increase_per_sec: 0.05,
            hard_loss: 0.10,
            min_parity_percent: 5,
            max_parity_percent: 50,
            hold_after_decrease_us: 500_000,
            min_decrease_interval_us: 200_000,
        }
    }
}

/// Bandwidth usage state from the delay-gradient detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageState {
    Normal,
    Overuse,
    Underuse,
}

pub struct AbrController {
    cfg: AbrConfig,
    /// Network budget incl. parity (kbps).
    budget: f64,
    parity: u8,
    loss_ema: f64,
    recv_kbps_ema: f64,
    overuse_reports: u32,
    last_decrease: Option<Timestamp>,
    last_report: Option<Timestamp>,
    last_emitted: Option<AbrDecision>,
    state: UsageState,
}

impl AbrController {
    pub fn new(start_kbps: u32, min_kbps: u32, max_kbps: u32) -> Self {
        Self::with_config(start_kbps, AbrConfig { min_kbps, max_kbps: max_kbps.max(min_kbps), ..Default::default() })
    }

    pub fn with_config(start_kbps: u32, cfg: AbrConfig) -> Self {
        let parity = cfg.min_parity_percent.min(cfg.max_parity_percent);
        let enc = start_kbps.clamp(cfg.min_kbps, cfg.max_kbps) as f64;
        let budget = enc * (1.0 + parity as f64 / 100.0);
        Self {
            budget,
            parity,
            cfg,
            loss_ema: 0.0,
            recv_kbps_ema: budget,
            overuse_reports: 0,
            last_decrease: None,
            last_report: None,
            last_emitted: None,
            state: UsageState::Normal,
        }
    }

    /// Current encoder target.
    pub fn bitrate_kbps(&self) -> u32 {
        self.current().bitrate_kbps
    }

    pub fn parity_percent(&self) -> u8 {
        self.parity
    }

    pub fn state(&self) -> UsageState {
        self.state
    }

    /// Smoothed raw (pre-FEC) loss fraction.
    pub fn loss(&self) -> f64 {
        self.loss_ema
    }

    fn current(&self) -> AbrDecision {
        let enc = (self.budget / (1.0 + self.parity as f64 / 100.0)).round() as u32;
        AbrDecision { bitrate_kbps: enc.clamp(self.cfg.min_kbps, self.cfg.max_kbps), parity_percent: self.parity }
    }

    /// Informational message for the viewer HUD.
    pub fn bitrate_update(&self, stream_id: u32, fps: u32) -> BitrateUpdate {
        BitrateUpdate { stream_id, bitrate_kbps: self.bitrate_kbps(), fps, fec_parity_percent: self.parity as u32 }
    }

    /// Process one report; returns a decision when the targets changed noticeably
    /// (≥ 2 % bitrate or any parity change) — apply it to the encoder and `VideoSender`.
    pub fn on_report(&mut self, rr: &ReceiverReport, now: Timestamp) -> Option<AbrDecision> {
        let dt_us = match self.last_report {
            Some(t) => now.since(t).min(1_000_000),
            None => (rr.interval_ms as u64 * 1000).min(1_000_000),
        };
        self.last_report = Some(now);

        // Loss (raw, before FEC).
        let total = rr.packets_received as u64 + rr.packets_lost as u64;
        if total > 0 {
            let loss = rr.packets_lost as f64 / total as f64;
            let a = if loss > self.loss_ema { 0.5 } else { 0.05 };
            self.loss_ema += a * (loss - self.loss_ema);
        }
        if rr.frames_unrecoverable > 0 {
            self.loss_ema = self.loss_ema.max(0.05);
        }
        let loss_now = if total > 0 { rr.packets_lost as f64 / total as f64 } else { 0.0 };

        // Smooth measured receive throughput so a single jitter-perturbed 50 ms interval
        // never causes a cliff-drop during Overuse.
        if rr.recv_kbps > 0 {
            self.recv_kbps_ema += 0.25 * (rr.recv_kbps as f64 - self.recv_kbps_ema);
        }

        // Parity ratio: clamp(2 * loss_ema, min, max).
        let p = (2.0 * self.loss_ema)
            .clamp(self.cfg.min_parity_percent as f64 / 100.0, self.cfg.max_parity_percent as f64 / 100.0);
        self.parity = ((p * 100.0).ceil() as u8).clamp(self.cfg.min_parity_percent, self.cfg.max_parity_percent);

        // Delay-gradient detector.
        let thr = self.cfg.overuse_us_per_frame;
        let g = rr.delay_gradient;
        if g > thr {
            self.overuse_reports += 1;
        } else {
            self.overuse_reports = 0;
        }
        self.state = if self.overuse_reports >= 2 || g > 2.0 * thr {
            UsageState::Overuse
        } else if g < -thr {
            UsageState::Underuse
        } else {
            UsageState::Normal
        };

        let can_decrease = self.last_decrease.is_none_or(|t| now.since(t) >= self.cfg.min_decrease_interval_us);
        if loss_now > self.cfg.hard_loss && can_decrease {
            self.budget *= 1.0 - 0.5 * loss_now.min(0.8);
            self.last_decrease = Some(now);
        } else if self.state == UsageState::Overuse && can_decrease {
            let smoothed = self.recv_kbps_ema.max(self.budget * 0.5);
            let base = if rr.recv_kbps > 0 { self.budget.min(smoothed) } else { self.budget };
            self.budget = base * self.cfg.decrease_factor;
            self.last_decrease = Some(now);
        } else if (self.loss_ema >= 0.03 || rr.frames_unrecoverable > 0) && can_decrease {
            // Moderate sustained loss (3-10%) or unrecoverable frame: gentle step-down.
            self.budget *= 0.92;
            self.last_decrease = Some(now);
        } else if self.state == UsageState::Normal
            && self.loss_ema < 0.02
            && rr.frames_unrecoverable == 0
            && self.last_decrease.is_none_or(|t| now.since(t) >= self.cfg.hold_after_decrease_us)
        {
            self.budget *= 1.0 + self.cfg.increase_per_sec * dt_us as f64 / 1e6;
        }
        let pf = 1.0 + self.parity as f64 / 100.0;
        self.budget = self.budget.clamp(self.cfg.min_kbps as f64 * pf, self.cfg.max_kbps as f64 * pf);

        let cur = self.current();
        let changed = match self.last_emitted {
            None => true,
            Some(prev) => {
                prev.parity_percent != cur.parity_percent
                    || (cur.bitrate_kbps as f64 - prev.bitrate_kbps as f64).abs() >= 0.02 * prev.bitrate_kbps as f64
            }
        };
        if changed {
            self.last_emitted = Some(cur);
            Some(cur)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rr(received: u32, lost: u32, gradient: f32, recv_kbps: u32) -> ReceiverReport {
        ReceiverReport {
            packets_received: received,
            packets_lost: lost,
            delay_gradient: gradient,
            recv_kbps,
            interval_ms: 50,
            ..Default::default()
        }
    }

    fn run(abr: &mut AbrController, t: &mut u64, n: usize, r: &ReceiverReport) {
        for _ in 0..n {
            *t += 50_000;
            abr.on_report(r, Timestamp(*t));
        }
    }

    #[test]
    fn increases_slowly_when_clean() {
        let mut abr = AbrController::new(10_000, 1_000, 50_000);
        let mut t = 0;
        run(&mut abr, &mut t, 20, &rr(1000, 0, 0.0, 11_000)); // 1 s
        let b = abr.bitrate_kbps();
        assert!((10_400..=10_600).contains(&b), "≈+5%/s, got {b}");
        assert_eq!(abr.parity_percent(), 5);
    }

    #[test]
    fn overuse_decreases_multiplicatively() {
        let mut abr = AbrController::new(20_000, 1_000, 50_000);
        let mut t = 0;
        run(&mut abr, &mut t, 2, &rr(1000, 0, 1000.0, 22_000));
        let b = abr.bitrate_kbps();
        assert!(b < 20_000 * 9 / 10, "got {b}");
        assert_eq!(abr.state(), UsageState::Overuse);
        // hold: no increase right after a decrease
        run(&mut abr, &mut t, 4, &rr(1000, 0, 0.0, 15_000));
        assert!(abr.bitrate_kbps() <= b);
    }

    #[test]
    fn heavy_loss_hard_decrease_and_parity_up() {
        let mut abr = AbrController::new(20_000, 1_000, 50_000);
        let mut t = 0;
        let d = {
            t += 50_000;
            abr.on_report(&rr(800, 200, 0.0, 20_000), Timestamp(t)).unwrap()
        };
        assert!(d.bitrate_kbps < 20_000 * 95 / 100);
        assert!(d.parity_percent >= 20, "parity {}", d.parity_percent);
        // loss goes away: parity decays slowly toward the minimum
        run(&mut abr, &mut t, 200, &rr(1000, 0, 0.0, 20_000));
        assert!(abr.parity_percent() <= 6);
    }

    #[test]
    fn respects_bounds() {
        let mut abr = AbrController::new(2_000, 1_000, 3_000);
        let mut t = 0;
        run(&mut abr, &mut t, 100, &rr(1000, 500, 5000.0, 100));
        assert_eq!(abr.bitrate_kbps(), 1_000);
        run(&mut abr, &mut t, 2000, &rr(1000, 0, 0.0, 5000));
        assert_eq!(abr.bitrate_kbps(), 3_000);
    }

    #[test]
    fn adapts_smoothly_under_loss_and_jitter_then_recovers() {
        let mut abr = AbrController::new(25_000, 2_000, 60_000);
        let mut t = 0;

        // Phase 1: 4% loss + 5-15 ms jitter (delay gradient spikes + oscillating recv_kbps).
        for i in 0..20 {
            t += 50_000;
            let grad = if i % 3 == 0 { 650.0 } else if i % 3 == 1 { 320.0 } else { -200.0 };
            // Occasional low 50 ms sample due to jitter burst must not cliff-drop bitrate.
            let measured = if i == 5 { 3_000 } else { 20_000 };
            abr.on_report(&rr(960, 40, grad, measured), Timestamp(t));
        }
        let degraded_kbps = abr.bitrate_kbps();
        let degraded_parity = abr.parity_percent();
        assert!(degraded_kbps < 22_000, "expected bitrate step-down, got {degraded_kbps}");
        assert!(degraded_kbps > 5_000, "must not cliff-drop under jitter, got {degraded_kbps}");
        assert!(degraded_parity >= 8, "expected parity >= 8% at 4% loss, got {degraded_parity}");

        // Phase 2: link becomes clean -> bitrate ramps smoothly back up and parity decays.
        run(&mut abr, &mut t, 160, &rr(1000, 0, 0.0, 30_000)); // 8 s clean
        let recovered_kbps = abr.bitrate_kbps();
        assert!(
            recovered_kbps > degraded_kbps * 120 / 100,
            "expected >=20% recovery above {degraded_kbps}, got {recovered_kbps}"
        );
        assert_eq!(abr.parity_percent(), 5);
    }
}
