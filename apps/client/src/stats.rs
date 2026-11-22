//! HUD aggregation: per-frame samples (host timings, network, decode, render, glass-to-glass)
//! smoothed with EMAs and published ~4×/s as a `HudSnapshot`.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::metrics::{Ema, HudSnapshot, LatencySamples};
use veloxdesk_core_traits::time::ClockSync;
use veloxdesk_protocol::control::{FrameTimingReport, ReceiverReport};

const HOST_HISTORY: usize = 512;
const PUBLISH_EVERY: Duration = Duration::from_millis(250);
const ALPHA: f64 = 0.1;

/// Per-frame viewer-side measurements.
#[derive(Debug, Clone, Copy)]
pub struct PresentedFrame {
    pub frame_id: u32,
    /// Host capture timestamp (host clock, µs).
    pub capture_ts: Timestamp,
    /// Local time the frame finished reassembly.
    pub recv_complete: Timestamp,
    /// Local time present returned.
    pub presented: Timestamp,
    pub decode_us: u32,
    pub render_us: u32,
}

pub struct HudAggregator {
    host: VecDeque<FrameTimingReport>,
    capture: Ema,
    encode: Ema,
    network: Ema,
    decode: Ema,
    render: Ema,
    total: Ema,
    window_start: Instant,
    window_frames: u32,
    window_bytes: u64,
    last_publish: Instant,
    pub snapshot: HudSnapshot,
    /// Raw samples since start (for the end-of-session log).
    pub render_samples: LatencySamples,
    pub total_samples: LatencySamples,
}

impl Default for HudAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl HudAggregator {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            host: VecDeque::with_capacity(HOST_HISTORY),
            capture: Ema::new(ALPHA),
            encode: Ema::new(ALPHA),
            network: Ema::new(ALPHA),
            decode: Ema::new(ALPHA),
            render: Ema::new(ALPHA),
            total: Ema::new(ALPHA),
            window_start: now,
            window_frames: 0,
            window_bytes: 0,
            last_publish: now,
            snapshot: HudSnapshot::default(),
            render_samples: LatencySamples::default(),
            total_samples: LatencySamples::default(),
        }
    }

    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn on_frame_timing(&mut self, r: FrameTimingReport) {
        if self.host.len() == HOST_HISTORY {
            self.host.pop_front();
        }
        self.host.push_back(r);
    }

    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn on_bytes_received(&mut self, n: usize) {
        self.window_bytes += n as u64;
    }

    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn on_receiver_report(&mut self, rr: &ReceiverReport) {
        let total = rr.packets_received as f32 + rr.packets_lost as f32;
        if total > 0.0 {
            self.snapshot.packet_loss_pct = rr.packets_lost as f32 * 100.0 / total;
        }
        self.snapshot.fec_recovery_pct = if rr.packets_lost > 0 {
            (rr.packets_recovered_fec as f32 * 100.0 / rr.packets_lost as f32).min(100.0)
        } else {
            100.0
        };
    }

    #[cfg_attr(not(feature = "net"), allow(dead_code))]
    pub fn on_rtt(&mut self, rtt_us: u32) {
        self.snapshot.rtt_us = rtt_us;
    }

    fn host_timing(&self, frame_id: u32) -> Option<&FrameTimingReport> {
        self.host.iter().rev().find(|r| r.frame_id == frame_id)
    }

    /// Record one presented frame. `clock` maps host timestamps to local time.
    pub fn on_presented(&mut self, f: PresentedFrame, clock: Option<&ClockSync>) {
        self.window_frames += 1;
        self.decode.update(f.decode_us as f64);
        self.render.update(f.render_us as f64);
        self.render_samples.push(f.render_us);
        let host_us = self
            .host_timing(f.frame_id)
            .map(|r| (r.capture_us + r.convert_us, r.encode_us))
            .map(|(cap, enc)| {
                self.capture.update(cap as f64);
                self.encode.update(enc as f64);
                cap + enc
            });
        if let Some(local_capture) = clock.and_then(|c| c.remote_to_local(f.capture_ts)) {
            let total = f.presented.since(local_capture);
            let complete = f.recv_complete.since(local_capture);
            // Discard nonsense (clock not converged yet / stale frames).
            if total < 5_000_000 {
                self.total.update(total as f64);
                self.total_samples.push(total as u32);
                self.network.update(complete.saturating_sub(host_us.unwrap_or(0) as u64) as f64);
            }
        }
    }

    /// Returns a fresh snapshot every 250 ms.
    pub fn maybe_publish(&mut self, now: Instant) -> Option<HudSnapshot> {
        if now.duration_since(self.last_publish) < PUBLISH_EVERY {
            return None;
        }
        self.last_publish = now;
        let dt = now.duration_since(self.window_start).as_secs_f64().max(1e-3);
        if dt >= 1.0 {
            self.snapshot.fps = (self.window_frames as f64 / dt) as f32;
            self.snapshot.bitrate_kbps = (self.window_bytes as f64 * 8.0 / 1000.0 / dt) as u32;
            self.window_start = now;
            self.window_frames = 0;
            self.window_bytes = 0;
        }
        let t = &mut self.snapshot.timings;
        t.capture_us = self.capture.get() as u32;
        t.convert_us = 0; // folded into capture_us above
        t.encode_us = self.encode.get() as u32;
        t.network_us = self.network.get() as u32;
        t.decode_us = self.decode.get() as u32;
        t.render_us = self.render.get() as u32;
        t.total_us = self.total.get() as u32;
        Some(self.snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_with_clock() {
        let mut a = HudAggregator::new();
        let mut clock = ClockSync::default();
        // host clock = local + 1_000_000
        clock.add_sample(100, 1_000_150, 1_000_160, 210);
        a.on_frame_timing(FrameTimingReport {
            frame_id: 7,
            capture_ts_us: 2_000_000,
            capture_us: 500,
            convert_us: 100,
            encode_us: 2000,
            ..Default::default()
        });
        // local capture = 1_000_000; complete at +4 ms, present at +8 ms
        a.on_presented(
            PresentedFrame {
                frame_id: 7,
                capture_ts: Timestamp(2_000_000),
                recv_complete: Timestamp(1_004_000),
                presented: Timestamp(1_008_000),
                decode_us: 1500,
                render_us: 900,
            },
            Some(&clock),
        );
        a.last_publish = Instant::now() - Duration::from_secs(1);
        let s = a.maybe_publish(Instant::now()).unwrap();
        let tol = |v: u32, want: u32| (v as i64 - want as i64).abs() <= 20;
        assert!(tol(s.timings.total_us, 8000), "{:?}", s.timings);
        assert!(tol(s.timings.network_us, 4000 - 2600), "{:?}", s.timings);
        assert_eq!(s.timings.capture_us, 600);
        assert_eq!(s.timings.encode_us, 2000);
        assert_eq!(s.timings.render_us, 900);
    }

    #[test]
    fn loss_and_fec() {
        let mut a = HudAggregator::new();
        a.on_receiver_report(&ReceiverReport {
            packets_received: 990,
            packets_lost: 10,
            packets_recovered_fec: 9,
            ..Default::default()
        });
        assert!((a.snapshot.packet_loss_pct - 1.0).abs() < 1e-3);
        assert!((a.snapshot.fec_recovery_pct - 90.0).abs() < 1e-3);
    }
}
