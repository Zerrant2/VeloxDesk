//! Low-latency adaptive audio jitter buffer with sequence reordering and PLC triggers.

use std::collections::BTreeMap;
use std::time::Duration;

use veloxdesk_core_traits::Result;
use veloxdesk_core_traits::audio::{AudioDecoder, AudioFrame, EncodedAudio};

/// Configuration for [`AudioJitterBuffer`].
#[derive(Debug, Clone, Copy)]
pub struct JitterConfig {
    /// Expected nominal packet duration (e.g. 5 ms or 10 ms).
    pub frame_duration: Duration,
    /// Target buffered playout depth before releasing packets (e.g. 15–20 ms).
    pub target_delay: Duration,
    /// Upper cap; oldest packets are dropped when occupancy exceeds this (e.g. 60–80 ms).
    pub max_delay: Duration,
}

impl Default for JitterConfig {
    fn default() -> Self {
        Self {
            frame_duration: Duration::from_millis(10),
            target_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(60),
        }
    }
}

/// Outcome of pulling the next playout slot from [`AudioJitterBuffer`].
#[derive(Debug, Clone)]
pub enum JitterPop {
    /// Next in-order packet ready for decoding.
    Packet(EncodedAudio),
    /// Expected sequence number never arrived in time — caller should invoke PLC (`decode(None)`).
    Missing { seq: u32 },
    /// Still filling up to `target_delay` after startup/reset.
    Buffering,
}

/// Telemetry counters for the Performance HUD / diagnostics.
#[derive(Debug, Clone, Copy, Default)]
pub struct JitterStats {
    pub buffered_packets: usize,
    pub buffered_us: u32,
    pub received: u64,
    pub late_dropped: u64,
    pub duplicates: u64,
    pub plc_concealed: u64,
    pub overrun_dropped: u64,
}

/// Wrapping sequence comparison (`a < b` in modular `u32` space).
#[inline]
fn seq_before(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

/// Sequence-ordered jitter buffer for Opus packets.
///
/// Packets are keyed by unwrapped `u64` sequence numbers derived from `EncodedAudio::seq`
/// so `BTreeMap` ordering handles `u32` wrap-around transparently.
pub struct AudioJitterBuffer {
    cfg: JitterConfig,
    queue: BTreeMap<u64, EncodedAudio>,
    next_seq: Option<u64>,
    highest_seq: Option<u64>,
    primed: bool,
    stats: JitterStats,
}

impl AudioJitterBuffer {
    pub fn new(cfg: JitterConfig) -> Self {
        Self {
            cfg,
            queue: BTreeMap::new(),
            next_seq: None,
            highest_seq: None,
            primed: false,
            stats: JitterStats::default(),
        }
    }

    pub fn config(&self) -> JitterConfig {
        self.cfg
    }

    pub fn reset(&mut self) {
        self.queue.clear();
        self.next_seq = None;
        self.highest_seq = None;
        self.primed = false;
        self.stats.buffered_packets = 0;
        self.stats.buffered_us = 0;
    }

    fn unwrap_seq(&self, seq32: u32) -> u64 {
        let Some(anchor) = self.highest_seq.or(self.next_seq) else {
            // Bias by 2^32 so earlier out-of-order packets near seq=0 never underflow u64.
            return (1u64 << 32) + seq32 as u64;
        };
        let anchor32 = anchor as u32;
        let diff = seq32.wrapping_sub(anchor32) as i32;
        if diff >= 0 {
            anchor.saturating_add(diff as u64)
        } else {
            anchor.saturating_sub((-diff) as u64)
        }
    }

    fn target_packets(&self) -> usize {
        let frame_us = self.cfg.frame_duration.as_micros().max(1);
        self.cfg.target_delay.as_micros().div_ceil(frame_us).max(1) as usize
    }

    fn max_packets(&self) -> usize {
        let frame_us = self.cfg.frame_duration.as_micros().max(1);
        let target = self.target_packets();
        (self.cfg.max_delay.as_micros().div_ceil(frame_us) as usize).max(target + 1)
    }

    /// Insert an incoming packet into the jitter buffer.
    pub fn push(&mut self, pkt: EncodedAudio) {
        self.stats.received += 1;
        let seq64 = self.unwrap_seq(pkt.seq);

        if let Some(next) = self.next_seq
            && seq_before(pkt.seq, next as u32)
            && seq64 < next
        {
            self.stats.late_dropped += 1;
            return;
        }

        if self.queue.contains_key(&seq64) {
            self.stats.duplicates += 1;
            return;
        }

        if self.highest_seq.is_none_or(|h| seq64 > h) {
            self.highest_seq = Some(seq64);
        }

        self.queue.insert(seq64, pkt);

        // Enforce max_delay cap to bound audio latency under network bursts.
        let max_pkts = self.max_packets();
        while self.queue.len() > max_pkts {
            if let Some((&oldest_seq, _)) = self.queue.first_key_value() {
                self.queue.remove(&oldest_seq);
                self.stats.overrun_dropped += 1;
                if let Some(next) = self.next_seq
                    && oldest_seq >= next
                {
                    self.next_seq = Some(oldest_seq + 1);
                }
            }
        }
        self.sync_buffered_metrics();
    }

    /// Pull the next playout slot (call once per `frame_duration`).
    pub fn pop(&mut self) -> JitterPop {
        if !self.primed {
            if self.queue.len() >= self.target_packets() {
                self.primed = true;
                // Start from the earliest packet currently queued.
                if let Some(&first) = self.queue.keys().next() {
                    self.next_seq = Some(first);
                }
            } else {
                self.sync_buffered_metrics();
                return JitterPop::Buffering;
            }
        }

        let Some(expected) = self.next_seq else {
            self.primed = false;
            return JitterPop::Buffering;
        };

        if let Some(pkt) = self.queue.remove(&expected) {
            self.next_seq = Some(expected + 1);
            self.sync_buffered_metrics();
            return JitterPop::Packet(pkt);
        }

        // Expected packet is missing.
        self.next_seq = Some(expected + 1);
        self.stats.plc_concealed += 1;
        // If completely drained for too long, re-prime once fresh packets arrive.
        if self.queue.is_empty() {
            self.primed = false;
        }
        self.sync_buffered_metrics();
        JitterPop::Missing { seq: expected as u32 }
    }

    /// Convenience helper: pop the next slot and decode it (running PLC on `Missing`).
    pub fn pop_decoded(&mut self, dec: &mut impl AudioDecoder) -> Result<Option<AudioFrame>> {
        match self.pop() {
            JitterPop::Packet(pkt) => Ok(Some(dec.decode(Some(&pkt))?)),
            JitterPop::Missing { .. } => Ok(Some(dec.decode(None)?)),
            JitterPop::Buffering => Ok(None),
        }
    }

    pub fn buffered_us(&self) -> u32 {
        self.stats.buffered_us
    }

    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    fn sync_buffered_metrics(&mut self) {
        self.stats.buffered_packets = self.queue.len();
        let total_us: u64 = self
            .queue
            .values()
            .map(|p| {
                if p.duration_us > 0 {
                    p.duration_us as u64
                } else {
                    self.cfg.frame_duration.as_micros() as u64
                }
            })
            .sum();
        self.stats.buffered_us = total_us.min(u32::MAX as u64) as u32;
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use veloxdesk_core_traits::Timestamp;
    use veloxdesk_core_traits::audio::AudioCodecId;

    use super::*;

    fn pkt(seq: u32) -> EncodedAudio {
        EncodedAudio {
            data: Bytes::copy_from_slice(&[seq as u8]),
            codec: AudioCodecId::Opus,
            seq,
            capture_ts: Timestamp(seq as u64 * 10_000),
            duration_us: 10_000,
        }
    }

    #[test]
    fn reorders_out_of_order_and_triggers_plc_on_gap() {
        let mut jb = AudioJitterBuffer::new(JitterConfig {
            frame_duration: Duration::from_millis(10),
            target_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(60),
        });

        // Push 1 packet -> still buffering.
        jb.push(pkt(1));
        assert!(matches!(jb.pop(), JitterPop::Buffering));

        // Push 0 out-of-order and 3 (gap at 2).
        jb.push(pkt(0));
        jb.push(pkt(3));
        assert_eq!(jb.buffered_us(), 30_000);

        // First pops should come out in order: 0, 1, Missing(2), 3.
        assert!(matches!(jb.pop(), JitterPop::Packet(p) if p.seq == 0));
        assert!(matches!(jb.pop(), JitterPop::Packet(p) if p.seq == 1));
        assert!(matches!(jb.pop(), JitterPop::Missing { seq: 2 }));
        assert!(matches!(jb.pop(), JitterPop::Packet(p) if p.seq == 3));

        // Late packet 2 arrives after we already concealed it -> dropped.
        jb.push(pkt(2));
        assert_eq!(jb.stats().late_dropped, 1);
        assert_eq!(jb.stats().plc_concealed, 1);
    }

    #[test]
    fn handles_u32_wraparound_and_overrun() {
        let mut jb = AudioJitterBuffer::new(JitterConfig {
            frame_duration: Duration::from_millis(10),
            target_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(40),
        });
        let base = u32::MAX - 2;
        for i in 0..8u32 {
            jb.push(pkt(base.wrapping_add(i)));
        }
        assert!(jb.stats().overrun_dropped >= 3);
        let mut seqs = Vec::new();
        while let JitterPop::Packet(p) = jb.pop() {
            seqs.push(p.seq);
        }
        assert!(!seqs.is_empty());
        for w in seqs.windows(2) {
            assert_eq!(w[0].wrapping_add(1), w[1]);
        }
    }
}
