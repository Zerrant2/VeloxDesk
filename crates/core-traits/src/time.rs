//! Monotonic microsecond time base (ARCHITECTURE ADR-008).

use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Microseconds on this process's monotonic clock. Host timestamps travel on the wire;
/// the viewer maps them to its own clock using a Ping/Pong-estimated offset
/// ([`ClockSync`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(pub u64);

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

impl Timestamp {
    /// Current monotonic time.
    #[inline]
    pub fn now() -> Self {
        Timestamp(epoch().elapsed().as_micros() as u64)
    }
    #[inline]
    pub fn as_micros(self) -> u64 {
        self.0
    }
    /// Saturating elapsed time between `earlier` and `self`, in µs.
    #[inline]
    pub fn since(self, earlier: Timestamp) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
    #[inline]
    pub fn elapsed_us(self) -> u64 {
        Timestamp::now().since(self)
    }
    #[inline]
    pub fn low32(self) -> u32 {
        self.0 as u32
    }
    /// Reconstruct a full 64-bit timestamp from its low 32 bits, choosing the value
    /// closest to `reference` (handles ~71 min wrap).
    pub fn from_low32(low: u32, reference: Timestamp) -> Timestamp {
        let base = reference.0 & !0xFFFF_FFFF;
        let candidates = [
            base.wrapping_sub(1 << 32) | low as u64,
            base | low as u64,
            base.wrapping_add(1 << 32) | low as u64,
        ];
        let best = candidates
            .into_iter()
            .min_by_key(|c| c.abs_diff(reference.0))
            .unwrap_or(base | low as u64);
        Timestamp(best)
    }
}

impl std::ops::Add<Duration> for Timestamp {
    type Output = Timestamp;
    fn add(self, d: Duration) -> Timestamp {
        Timestamp(self.0 + d.as_micros() as u64)
    }
}

/// NTP-style clock offset estimator: keeps the sample with the minimum RTT in a sliding
/// window. `offset_us` = remote − local.
#[derive(Debug, Clone)]
pub struct ClockSync {
    samples: Vec<(u64, i64)>, // (rtt, offset)
    window: usize,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self { samples: Vec::with_capacity(32), window: 32 }
    }
}

impl ClockSync {
    /// Feed one Ping/Pong exchange: `t0` local send, `t1` remote recv, `t2` remote send,
    /// `t3` local recv (all µs).
    pub fn add_sample(&mut self, t0: u64, t1: u64, t2: u64, t3: u64) {
        let rtt = (t3.saturating_sub(t0)).saturating_sub(t2.saturating_sub(t1));
        let offset = ((t1 as i64 - t0 as i64) + (t2 as i64 - t3 as i64)) / 2;
        if self.samples.len() == self.window {
            self.samples.remove(0);
        }
        self.samples.push((rtt, offset));
    }
    /// Best estimate (min-RTT sample): `(rtt_us, offset_us)`.
    pub fn estimate(&self) -> Option<(u64, i64)> {
        self.samples.iter().copied().min_by_key(|s| s.0)
    }
    /// Convert a remote timestamp to the local clock.
    pub fn remote_to_local(&self, remote: Timestamp) -> Option<Timestamp> {
        self.estimate().map(|(_, off)| Timestamp((remote.0 as i64 - off).max(0) as u64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn low32_reconstruct() {
        let r = Timestamp((5u64 << 32) + 10);
        assert_eq!(Timestamp::from_low32(5, r).0, (5u64 << 32) + 5);
        assert_eq!(Timestamp::from_low32(0xFFFF_FFF0, r).0, (4u64 << 32) + 0xFFFF_FFF0);
    }

    #[test]
    fn clock_sync() {
        let mut c = ClockSync::default();
        // remote is +1000 µs ahead, one-way 50 µs
        c.add_sample(0, 1050, 1060, 120);
        c.add_sample(200, 1300, 1310, 400); // noisier
        let (rtt, off) = c.estimate().unwrap();
        assert_eq!(rtt, 110);
        assert_eq!(off, 995);
    }
}
