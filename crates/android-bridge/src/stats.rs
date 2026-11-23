//! HUD metrics aggregator for the Android viewer (~4 Hz smoothed snapshot publication).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::metrics::Ema;
use veloxdesk_core_traits::time::ClockSync;
use veloxdesk_core_traits::transport::PathKind;
use veloxdesk_protocol::control::{FrameTimingReport, ReceiverReport};

use crate::constants;

const HOST_HISTORY: usize = 512;
const PUBLISH_EVERY: Duration = Duration::from_millis(250);
const ALPHA: f64 = 0.1;

/// Flattened HUD snapshot matching `VeloxSessionCallback.onStats(...)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HudMetrics {
    pub fps: f32,
    pub bitrate_kbps: i32,
    pub rtt_ms: f32,
    pub loss_pct: f32,
    pub fec_pct: f32,
    pub capture_ms: f32,
    pub encode_ms: f32,
    pub net_ms: f32,
    pub decode_ms: f32,
    pub total_ms: f32,
    pub path_kind: i32,
}

impl Default for HudMetrics {
    fn default() -> Self {
        Self {
            fps: 0.0,
            bitrate_kbps: 0,
            rtt_ms: 0.0,
            loss_pct: 0.0,
            fec_pct: 100.0,
            capture_ms: 0.0,
            encode_ms: 0.0,
            net_ms: 0.0,
            decode_ms: 0.0,
            total_ms: 0.0,
            path_kind: constants::PATH_UNKNOWN,
        }
    }
}

/// Map `veloxdesk_core_traits::transport::PathKind` to `BridgeConstants.PATH_*`.
pub fn path_kind_to_int(path: PathKind) -> i32 {
    match path {
        PathKind::Unknown => constants::PATH_UNKNOWN,
        PathKind::Direct => constants::PATH_DIRECT,
        PathKind::P2p => constants::PATH_P2P_HOLE_PUNCH,
        PathKind::Relay => constants::PATH_RELAY,
    }
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
    rtt_us: u32,
    fps: f32,
    bitrate_kbps: u32,
    loss_pct: f32,
    fec_pct: f32,
    path: PathKind,
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
            rtt_us: 0,
            fps: 0.0,
            bitrate_kbps: 0,
            loss_pct: 0.0,
            fec_pct: 100.0,
            path: PathKind::Unknown,
        }
    }

    pub fn set_path(&mut self, path: PathKind) {
        self.path = path;
    }

    pub fn on_frame_timing(&mut self, report: FrameTimingReport) {
        if self.host.len() == HOST_HISTORY {
            self.host.pop_front();
        }
        self.host.push_back(report);
    }

    pub fn on_bytes_received(&mut self, n: usize) {
        self.window_bytes = self.window_bytes.saturating_add(n as u64);
    }

    pub fn on_receiver_report(&mut self, rr: &ReceiverReport) {
        let total = rr.packets_received as f32 + rr.packets_lost as f32;
        if total > 0.0 {
            self.loss_pct = rr.packets_lost as f32 * 100.0 / total;
        }
        self.fec_pct = if rr.packets_lost > 0 {
            (rr.packets_recovered_fec as f32 * 100.0 / rr.packets_lost as f32).min(100.0)
        } else {
            100.0
        };
    }

    pub fn on_rtt(&mut self, rtt_us: u32) {
        self.rtt_us = rtt_us;
    }

    /// Record MediaCodec decode & render timing reported asynchronously from Kotlin.
    pub fn on_decode_render_us(&mut self, decode_us: u32, render_us: u32) {
        if decode_us > 0 {
            self.decode.update(decode_us as f64);
        }
        if render_us > 0 {
            self.render.update(render_us as f64);
        }
    }

    fn host_timing(&self, frame_id: u32) -> Option<&FrameTimingReport> {
        self.host.iter().rev().find(|r| r.frame_id == frame_id)
    }

    /// Record one complete reassembled video frame handed to the Android MediaCodec surface.
    pub fn on_frame_received(
        &mut self,
        frame_id: u32,
        capture_ts: Timestamp,
        recv_complete: Timestamp,
        decode_us: u32,
        render_us: u32,
        clock: Option<&ClockSync>,
    ) {
        self.window_frames = self.window_frames.saturating_add(1);
        let host_us = self
            .host_timing(frame_id)
            .map(|r| (r.capture_us.saturating_add(r.convert_us), r.encode_us))
            .map(|(cap, enc)| {
                self.capture.update(cap as f64);
                self.encode.update(enc as f64);
                cap.saturating_add(enc)
            });

        if let Some(local_capture) = clock.and_then(|c| c.remote_to_local(capture_ts)) {
            let complete = recv_complete.since(local_capture);
            let total = complete.saturating_add(decode_us as u64).saturating_add(render_us as u64);
            if total < 5_000_000 {
                self.total.update(total as f64);
                self.network.update(complete.saturating_sub(host_us.unwrap_or(0) as u64) as f64);
            }
        }
    }

    /// Returns a fresh [`HudMetrics`] snapshot roughly every 250 ms.
    pub fn maybe_publish(&mut self, now: Instant) -> Option<HudMetrics> {
        if now.duration_since(self.last_publish) < PUBLISH_EVERY {
            return None;
        }
        self.last_publish = now;
        let dt = now.duration_since(self.window_start).as_secs_f64().max(1e-3);
        if dt >= 1.0 {
            self.fps = (self.window_frames as f64 / dt) as f32;
            self.bitrate_kbps = (self.window_bytes as f64 * 8.0 / 1000.0 / dt) as u32;
            self.window_start = now;
            self.window_frames = 0;
            self.window_bytes = 0;
        } else if self.fps == 0.0 && self.window_frames > 0 && dt >= 0.2 {
            self.fps = (self.window_frames as f64 / dt) as f32;
            self.bitrate_kbps = (self.window_bytes as f64 * 8.0 / 1000.0 / dt) as u32;
        }

        let decode_us = self.decode.get();
        let render_us = self.render.get();
        let total_us = if self.total.get() > 0.0 {
            self.total.get()
        } else {
            self.capture.get() + self.encode.get() + self.network.get() + decode_us + render_us
        };

        Some(HudMetrics {
            fps: self.fps,
            bitrate_kbps: self.bitrate_kbps as i32,
            rtt_ms: self.rtt_us as f32 / 1000.0,
            loss_pct: self.loss_pct,
            fec_pct: self.fec_pct,
            capture_ms: (self.capture.get() / 1000.0) as f32,
            encode_ms: (self.encode.get() / 1000.0) as f32,
            net_ms: (self.network.get() / 1000.0) as f32,
            decode_ms: ((decode_us + render_us) / 1000.0) as f32,
            total_ms: (total_us / 1000.0) as f32,
            path_kind: path_kind_to_int(self.path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregates_timings_and_loss() {
        let mut hud = HudAggregator::new();
        hud.set_path(PathKind::Direct);
        hud.on_rtt(12_500);
        hud.on_receiver_report(&ReceiverReport {
            packets_received: 95,
            packets_lost: 5,
            packets_recovered_fec: 4,
            ..Default::default()
        });
        let mut clock = ClockSync::default();
        clock.add_sample(100, 1_000_150, 1_000_160, 210);

        hud.on_frame_timing(FrameTimingReport {
            frame_id: 1,
            capture_ts_us: 2_000_000,
            capture_us: 1_000,
            convert_us: 500,
            encode_us: 2_000,
            ..Default::default()
        });
        hud.on_decode_render_us(3_000, 1_000);
        hud.on_bytes_received(12_500);
        hud.on_frame_received(1, Timestamp(2_000_000), Timestamp(1_005_000), 3_000, 1_000, Some(&clock));

        hud.last_publish = Instant::now() - Duration::from_secs(1);
        hud.window_start = Instant::now() - Duration::from_secs(1);
        let m = hud.maybe_publish(Instant::now()).unwrap();
        assert_eq!(m.path_kind, constants::PATH_DIRECT);
        assert!((m.rtt_ms - 12.5).abs() < 1e-3);
        assert!((m.loss_pct - 5.0).abs() < 1e-3);
        assert!((m.fec_pct - 80.0).abs() < 1e-3);
        assert!((m.capture_ms - 1.5).abs() < 1e-2);
        assert!((m.encode_ms - 2.0).abs() < 1e-2);
        assert!((m.decode_ms - 4.0).abs() < 1e-2);
        assert!((m.net_ms - 1.5).abs() < 0.05);
        assert!((m.total_ms - 9.0).abs() < 0.05);
        assert!(m.bitrate_kbps > 0);
        assert!(m.fps > 0.0);
    }
}
