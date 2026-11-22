//! Network impairment simulator.
//!
//! Two layers:
//! * [`Impairer`] — a pure, deterministic (seeded) per-direction model: random loss,
//!   Gilbert–Elliott burst loss, base latency + jitter, reordering and a token-bucket
//!   bandwidth cap with a bounded queue. It turns "packet of `len` bytes arrived at `t`"
//!   into a [`Verdict`] (drop, or deliver at `t'`). Unit-testable without sockets.
//! * [`UdpImpairProxy`] — an async (tokio) UDP proxy placed between a client and a server
//!   (e.g. two QUIC endpoints). The client talks to [`UdpImpairProxy::local_addr`]; the
//!   proxy forwards to `upstream`, applying one [`Impairer`] per direction. Delivery runs
//!   on a dedicated OS thread with a deadline heap and a short spin for the last
//!   millisecond, so sub-millisecond delays are honoured even on Windows (where tokio
//!   timers have ~1–15 ms granularity).

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::io;
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

// ───────────────────────────── RNG ─────────────────────────────

/// Small, fast, seedable PRNG (SplitMix64). Deterministic across platforms.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, 1)`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }
    /// `true` with probability `p` (clamped to 0..=1).
    #[inline]
    pub fn chance(&mut self, p: f64) -> bool {
        p > 0.0 && self.next_f64() < p
    }
}

// ───────────────────────────── config ─────────────────────────────

/// Packet loss model.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LossModel {
    #[default]
    None,
    /// Independent (Bernoulli) loss with probability `p` (0..=1).
    Random(f64),
    /// Two-state Gilbert–Elliott burst loss.
    GilbertElliott {
        /// P(Good → Bad) per packet.
        p_good_to_bad: f64,
        /// P(Bad → Good) per packet (mean burst length = 1 / p_bad_to_good).
        p_bad_to_good: f64,
        /// Loss probability while Good (usually ~0).
        loss_in_good: f64,
        /// Loss probability while Bad (usually ~1).
        loss_in_bad: f64,
    },
}

impl LossModel {
    /// Gilbert–Elliott model with average loss `avg` (0..1) and mean burst length
    /// `burst_len` packets (lossless Good state, lossy Bad state).
    pub fn bursty(avg: f64, burst_len: f64) -> Self {
        let r = 1.0 / burst_len.max(1.0);
        // Stationary P(bad) = p / (p + r) = avg  →  p = avg * r / (1 - avg)
        let p = if avg >= 1.0 { 1.0 } else { avg * r / (1.0 - avg) };
        LossModel::GilbertElliott { p_good_to_bad: p.min(1.0), p_bad_to_good: r, loss_in_good: 0.0, loss_in_bad: 1.0 }
    }

    /// Expected long-run loss rate.
    pub fn expected_loss(&self) -> f64 {
        match *self {
            LossModel::None => 0.0,
            LossModel::Random(p) => p.clamp(0.0, 1.0),
            LossModel::GilbertElliott { p_good_to_bad, p_bad_to_good, loss_in_good, loss_in_bad } => {
                let denom = p_good_to_bad + p_bad_to_good;
                if denom <= 0.0 {
                    return loss_in_good;
                }
                let pb = p_good_to_bad / denom;
                pb * loss_in_bad + (1.0 - pb) * loss_in_good
            }
        }
    }
}

/// Token-bucket bandwidth cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandwidthCap {
    /// Sustained rate in kilobits per second.
    pub rate_kbps: u32,
    /// Bucket depth in bytes (burst allowance).
    pub burst_bytes: u32,
    /// Max queueing delay before tail-drop (models the bottleneck buffer).
    pub max_queue_delay: Duration,
}

impl BandwidthCap {
    pub fn new(rate_kbps: u32) -> Self {
        Self { rate_kbps, burst_bytes: 16 * 1500, max_queue_delay: Duration::from_millis(50) }
    }
}

/// Per-direction impairment settings. `Default` = transparent link.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ImpairmentConfig {
    pub loss: LossModel,
    /// Constant one-way delay.
    pub latency: Duration,
    /// Uniform jitter in `[-jitter, +jitter]` added to `latency` (clamped at 0). Like
    /// netem, jitter may itself reorder packets.
    pub jitter: Duration,
    /// Probability (0..=1) that a packet is held back by `reorder_delay` so that later
    /// packets overtake it.
    pub reorder: f64,
    pub reorder_delay: Duration,
    pub bandwidth: Option<BandwidthCap>,
}

impl ImpairmentConfig {
    pub fn transparent() -> Self {
        Self::default()
    }
    pub fn with_loss(mut self, loss: LossModel) -> Self {
        self.loss = loss;
        self
    }
    pub fn with_random_loss(self, p: f64) -> Self {
        self.with_loss(LossModel::Random(p))
    }
    pub fn with_latency(mut self, latency: Duration, jitter: Duration) -> Self {
        self.latency = latency;
        self.jitter = jitter;
        self
    }
    pub fn with_reorder(mut self, p: f64, delay: Duration) -> Self {
        self.reorder = p;
        self.reorder_delay = delay;
        self
    }
    pub fn with_bandwidth(mut self, cap: BandwidthCap) -> Self {
        self.bandwidth = Some(cap);
        self
    }
    pub fn is_transparent(&self) -> bool {
        *self == Self::default()
    }
}

// ───────────────────────────── model ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    Loss,
    /// Bandwidth-cap queue overflow.
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Drop(DropReason),
    Deliver(Instant),
}

/// Deterministic per-direction impairment model.
#[derive(Debug, Clone)]
pub struct Impairer {
    cfg: ImpairmentConfig,
    rng: SplitMix64,
    ge_bad: bool,
    /// Token bucket: bytes available (may go negative = backlog).
    tokens: f64,
    last_refill: Option<Instant>,
}

impl Impairer {
    pub fn new(cfg: ImpairmentConfig, seed: u64) -> Self {
        let tokens = cfg.bandwidth.map(|b| b.burst_bytes as f64).unwrap_or(0.0);
        Self { cfg, rng: SplitMix64::new(seed), ge_bad: false, tokens, last_refill: None }
    }

    pub fn config(&self) -> &ImpairmentConfig {
        &self.cfg
    }

    /// Replace the configuration (keeps RNG state; resets the bucket if the cap changed).
    pub fn set_config(&mut self, cfg: ImpairmentConfig) {
        if cfg.bandwidth != self.cfg.bandwidth {
            self.tokens = cfg.bandwidth.map(|b| b.burst_bytes as f64).unwrap_or(0.0);
            self.last_refill = None;
        }
        self.cfg = cfg;
    }

    fn lost(&mut self) -> bool {
        match self.cfg.loss {
            LossModel::None => false,
            LossModel::Random(p) => self.rng.chance(p),
            LossModel::GilbertElliott { p_good_to_bad, p_bad_to_good, loss_in_good, loss_in_bad } => {
                // Transition first, then emit according to the new state.
                if self.ge_bad {
                    if self.rng.chance(p_bad_to_good) {
                        self.ge_bad = false;
                    }
                } else if self.rng.chance(p_good_to_bad) {
                    self.ge_bad = true;
                }
                self.rng.chance(if self.ge_bad { loss_in_bad } else { loss_in_good })
            }
        }
    }

    /// Token bucket: returns the extra queueing delay, or `None` on tail-drop.
    fn shape(&mut self, len: usize, now: Instant) -> Option<Duration> {
        let Some(cap) = self.cfg.bandwidth else { return Some(Duration::ZERO) };
        let rate = cap.rate_kbps.max(1) as f64 * 1000.0 / 8.0; // bytes/s
        if let Some(last) = self.last_refill {
            let dt = now.saturating_duration_since(last).as_secs_f64();
            self.tokens = (self.tokens + dt * rate).min(cap.burst_bytes as f64);
        }
        self.last_refill = Some(now);
        let after = self.tokens - len as f64;
        if after >= 0.0 {
            self.tokens = after;
            return Some(Duration::ZERO);
        }
        let wait = Duration::from_secs_f64(-after / rate);
        if wait > cap.max_queue_delay {
            return None;
        }
        self.tokens = after;
        Some(wait)
    }

    /// Decide the fate of one packet of `len` bytes arriving at `now`.
    pub fn process(&mut self, len: usize, now: Instant) -> Verdict {
        if self.lost() {
            return Verdict::Drop(DropReason::Loss);
        }
        let Some(queue) = self.shape(len, now) else { return Verdict::Drop(DropReason::Queue) };
        let mut delay = self.cfg.latency.as_secs_f64();
        if !self.cfg.jitter.is_zero() {
            let j = self.cfg.jitter.as_secs_f64();
            delay += (self.rng.next_f64() * 2.0 - 1.0) * j;
        }
        if self.rng.chance(self.cfg.reorder) {
            delay += self.cfg.reorder_delay.as_secs_f64();
        }
        let delay = Duration::from_secs_f64(delay.max(0.0)) + queue;
        Verdict::Deliver(now + delay)
    }
}

// ───────────────────────────── proxy ─────────────────────────────

/// Counters for one direction of the proxy.
#[derive(Debug, Default)]
pub struct DirCounters {
    pub received: AtomicU64,
    pub delivered: AtomicU64,
    pub dropped_loss: AtomicU64,
    pub dropped_queue: AtomicU64,
    pub bytes_delivered: AtomicU64,
}

/// Snapshot of [`DirCounters`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirStats {
    pub received: u64,
    pub delivered: u64,
    pub dropped_loss: u64,
    pub dropped_queue: u64,
    pub bytes_delivered: u64,
}

impl DirStats {
    pub fn loss_rate(&self) -> f64 {
        if self.received == 0 { 0.0 } else { (self.dropped_loss + self.dropped_queue) as f64 / self.received as f64 }
    }
}

impl DirCounters {
    pub fn snapshot(&self) -> DirStats {
        DirStats {
            received: self.received.load(Ordering::Relaxed),
            delivered: self.delivered.load(Ordering::Relaxed),
            dropped_loss: self.dropped_loss.load(Ordering::Relaxed),
            dropped_queue: self.dropped_queue.load(Ordering::Relaxed),
            bytes_delivered: self.bytes_delivered.load(Ordering::Relaxed),
        }
    }
}

/// Direction through the proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server (upstream).
    ToServer,
    /// Server → client (downstream).
    ToClient,
}

struct Pending {
    at: Instant,
    seq: u64,
    data: Vec<u8>,
    dest: SocketAddr,
}
impl PartialEq for Pending {
    fn eq(&self, o: &Self) -> bool {
        (self.at, self.seq) == (o.at, o.seq)
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Pending {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.at, self.seq).cmp(&(o.at, o.seq))
    }
}

/// Delivery thread: sends queued packets at their deadline (heap ordered, FIFO for ties).
fn delivery_thread(sock: StdUdpSocket, rx: mpsc::Receiver<Pending>, counters: Arc<DirCounters>) {
    let mut heap: BinaryHeap<Reverse<Pending>> = BinaryHeap::new();
    let mut open = true;
    loop {
        // Drain whatever arrived.
        loop {
            match rx.try_recv() {
                Ok(p) => heap.push(Reverse(p)),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    open = false;
                    break;
                }
            }
        }
        let now = Instant::now();
        while heap.peek().is_some_and(|p| p.0.at <= now) {
            let Reverse(p) = heap.pop().expect("peeked");
            if sock.send_to(&p.data, p.dest).is_ok() {
                counters.delivered.fetch_add(1, Ordering::Relaxed);
                counters.bytes_delivered.fetch_add(p.data.len() as u64, Ordering::Relaxed);
            }
        }
        match heap.peek() {
            None => {
                if !open {
                    return;
                }
                match rx.recv() {
                    Ok(p) => heap.push(Reverse(p)),
                    Err(_) => return,
                }
            }
            Some(next) => {
                let wait = next.0.at.saturating_duration_since(Instant::now());
                if wait > Duration::from_micros(2000) && open {
                    // Coarse sleep, waking early for new packets; leaves ~1.5 ms to spin.
                    match rx.recv_timeout(wait - Duration::from_micros(1500)) {
                        Ok(p) => heap.push(Reverse(p)),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => open = false,
                    }
                } else {
                    thread::yield_now();
                }
            }
        }
    }
}

struct Direction_ {
    impairer: Mutex<Impairer>,
    counters: Arc<DirCounters>,
}

/// Async UDP proxy with per-direction impairment. Supports one client (the last source
/// address seen on the listen socket receives the server's replies). Dropping the proxy
/// stops it.
pub struct UdpImpairProxy {
    local: SocketAddr,
    upstream: SocketAddr,
    dirs: [Arc<Direction_>; 2],
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for UdpImpairProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpImpairProxy").field("local", &self.local).field("upstream", &self.upstream).finish()
    }
}

fn bind_like(addr: &SocketAddr) -> io::Result<StdUdpSocket> {
    let bind: SocketAddr = if addr.is_ipv4() {
        SocketAddr::from(([127, 0, 0, 1], 0))
    } else {
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 0))
    };
    // Loopback only unless the upstream is non-loopback.
    let bind = if addr.ip().is_loopback() { bind } else { SocketAddr::new(unspecified(addr), 0) };
    let s = StdUdpSocket::bind(bind)?;
    s.set_nonblocking(true)?;
    Ok(s)
}

fn unspecified(addr: &SocketAddr) -> std::net::IpAddr {
    if addr.is_ipv4() { std::net::Ipv4Addr::UNSPECIFIED.into() } else { std::net::Ipv6Addr::UNSPECIFIED.into() }
}

impl UdpImpairProxy {
    /// Start a proxy forwarding to `upstream`. Must be called inside a tokio runtime.
    pub async fn start(upstream: SocketAddr, to_server: ImpairmentConfig, to_client: ImpairmentConfig) -> io::Result<Self> {
        Self::start_seeded(upstream, to_server, to_client, 0x5EED_F00D_FACE).await
    }

    /// Like [`start`](Self::start) with an explicit RNG seed (reproducible runs).
    pub async fn start_seeded(
        upstream: SocketAddr,
        to_server: ImpairmentConfig,
        to_client: ImpairmentConfig,
        seed: u64,
    ) -> io::Result<Self> {
        let client_side = bind_like(&upstream)?;
        let server_side = bind_like(&upstream)?;
        let local = client_side.local_addr()?;

        let dirs = [
            Arc::new(Direction_ { impairer: Mutex::new(Impairer::new(to_server, seed)), counters: Default::default() }),
            Arc::new(Direction_ {
                impairer: Mutex::new(Impairer::new(to_client, seed ^ 0xA5A5_A5A5_A5A5_A5A5)),
                counters: Default::default(),
            }),
        ];

        // Delivery threads (server_side sends upstream, client_side sends downstream).
        let (tx_up, rx_up) = mpsc::channel::<Pending>();
        let (tx_down, rx_down) = mpsc::channel::<Pending>();
        {
            let s = server_side.try_clone()?;
            let c = dirs[0].counters.clone();
            thread::Builder::new().name("impair-up".into()).spawn(move || delivery_thread(s, rx_up, c))?;
            let s = client_side.try_clone()?;
            let c = dirs[1].counters.clone();
            thread::Builder::new().name("impair-down".into()).spawn(move || delivery_thread(s, rx_down, c))?;
        }

        let client_sock = UdpSocket::from_std(client_side)?;
        let server_sock = UdpSocket::from_std(server_side)?;
        let client_addr: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));

        let mut tasks = Vec::new();
        // client → server
        {
            let dir = dirs[0].clone();
            let client_addr = client_addr.clone();
            tasks.push(tokio::spawn(async move {
                let mut seq = 0u64;
                let mut buf = vec![0u8; 65536];
                loop {
                    let (n, from) = match client_sock.recv_from(&mut buf).await {
                        Ok(v) => v,
                        Err(_) => continue, // e.g. WSAECONNRESET on Windows
                    };
                    *client_addr.lock().expect("poisoned") = Some(from);
                    if !forward(&dir, &buf[..n], upstream, &mut seq, &tx_up) {
                        return;
                    }
                }
            }));
        }
        // server → client
        {
            let dir = dirs[1].clone();
            tasks.push(tokio::spawn(async move {
                let mut seq = 0u64;
                let mut buf = vec![0u8; 65536];
                loop {
                    let (n, from) = match server_sock.recv_from(&mut buf).await {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if from != upstream {
                        continue;
                    }
                    let dest = *client_addr.lock().expect("poisoned");
                    let Some(dest) = dest else { continue };
                    if !forward(&dir, &buf[..n], dest, &mut seq, &tx_down) {
                        return;
                    }
                }
            }));
        }

        Ok(Self { local, upstream, dirs, tasks })
    }

    /// Address clients should send to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn upstream(&self) -> SocketAddr {
        self.upstream
    }

    fn dir(&self, d: Direction) -> &Direction_ {
        match d {
            Direction::ToServer => &self.dirs[0],
            Direction::ToClient => &self.dirs[1],
        }
    }

    pub fn stats(&self, d: Direction) -> DirStats {
        self.dir(d).counters.snapshot()
    }

    /// Change impairment of one direction at runtime.
    pub fn set_config(&self, d: Direction, cfg: ImpairmentConfig) {
        self.dir(d).impairer.lock().expect("poisoned").set_config(cfg);
    }
}

fn forward(dir: &Direction_, data: &[u8], dest: SocketAddr, seq: &mut u64, tx: &mpsc::Sender<Pending>) -> bool {
    dir.counters.received.fetch_add(1, Ordering::Relaxed);
    let now = Instant::now();
    let verdict = dir.impairer.lock().expect("poisoned").process(data.len(), now);
    match verdict {
        Verdict::Drop(DropReason::Loss) => {
            dir.counters.dropped_loss.fetch_add(1, Ordering::Relaxed);
            true
        }
        Verdict::Drop(DropReason::Queue) => {
            dir.counters.dropped_queue.fetch_add(1, Ordering::Relaxed);
            true
        }
        Verdict::Deliver(at) => {
            *seq += 1;
            tx.send(Pending { at, seq: *seq, data: data.to_vec(), dest }).is_ok()
        }
    }
}

impl Drop for UdpImpairProxy {
    fn drop(&mut self) {
        // Aborting the tasks drops the channel senders → delivery threads flush and exit.
        for t in &self.tasks {
            t.abort();
        }
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn run(imp: &mut Impairer, n: usize) -> Vec<Verdict> {
        let t0 = Instant::now();
        (0..n).map(|i| imp.process(1200, t0 + Duration::from_micros(i as u64 * 100))).collect()
    }

    #[test]
    fn transparent_delivers_immediately() {
        let mut imp = Impairer::new(ImpairmentConfig::default(), 1);
        let t = Instant::now();
        assert_eq!(imp.process(1200, t), Verdict::Deliver(t));
    }

    #[test]
    fn random_loss_rate() {
        for p in [0.01, 0.03, 0.05, 0.2] {
            let mut imp = Impairer::new(ImpairmentConfig::default().with_random_loss(p), 42);
            let v = run(&mut imp, 200_000);
            let lost = v.iter().filter(|v| matches!(v, Verdict::Drop(_))).count() as f64 / v.len() as f64;
            assert!((lost - p).abs() < p * 0.1 + 0.002, "p={p} got {lost}");
        }
    }

    #[test]
    fn gilbert_elliott_is_bursty() {
        let model = LossModel::bursty(0.05, 8.0);
        assert!((model.expected_loss() - 0.05).abs() < 1e-9);
        let mut imp = Impairer::new(ImpairmentConfig::default().with_loss(model), 7);
        let v = run(&mut imp, 400_000);
        let lost: Vec<bool> = v.iter().map(|v| matches!(v, Verdict::Drop(_))).collect();
        let rate = lost.iter().filter(|&&l| l).count() as f64 / lost.len() as f64;
        assert!((rate - 0.05).abs() < 0.01, "rate {rate}");
        // Mean burst length should be ≈ 8 (vs ≈ 1.05 for random loss).
        let mut bursts = 0usize;
        let mut prev = false;
        for &l in &lost {
            if l && !prev {
                bursts += 1;
            }
            prev = l;
        }
        let mean_burst = lost.iter().filter(|&&l| l).count() as f64 / bursts as f64;
        assert!((6.0..10.0).contains(&mean_burst), "mean burst {mean_burst}");
    }

    #[test]
    fn deterministic_with_seed() {
        let cfg = ImpairmentConfig::default().with_random_loss(0.1).with_latency(Duration::from_millis(5), Duration::from_millis(2));
        let t0 = Instant::now();
        let rel = |v: Vec<Verdict>| -> Vec<Option<Duration>> {
            v.into_iter()
                .map(|v| match v {
                    Verdict::Deliver(at) => Some(at - t0),
                    Verdict::Drop(_) => None,
                })
                .collect()
        };
        let mut a = Impairer::new(cfg, 99);
        let mut b = Impairer::new(cfg, 99);
        let va: Vec<Verdict> = (0..1000u64).map(|i| a.process(1200, t0 + Duration::from_micros(i * 100))).collect();
        let vb: Vec<Verdict> = (0..1000u64).map(|i| b.process(1200, t0 + Duration::from_micros(i * 100))).collect();
        assert_eq!(rel(va), rel(vb));
    }

    #[test]
    fn latency_and_jitter_bounds() {
        let cfg = ImpairmentConfig::default().with_latency(Duration::from_millis(10), Duration::from_millis(3));
        let mut imp = Impairer::new(cfg, 3);
        let t = Instant::now();
        let mut min = Duration::MAX;
        let mut max = Duration::ZERO;
        for _ in 0..10_000 {
            let Verdict::Deliver(at) = imp.process(100, t) else { panic!("no loss configured") };
            let d = at - t;
            min = min.min(d);
            max = max.max(d);
        }
        assert!(min >= Duration::from_millis(7) && min < Duration::from_micros(7100), "{min:?}");
        assert!(max <= Duration::from_millis(13) && max > Duration::from_micros(12900), "{max:?}");
    }

    #[test]
    fn reorder_holds_back_packets() {
        let cfg = ImpairmentConfig::default().with_reorder(0.1, Duration::from_millis(5));
        let mut imp = Impairer::new(cfg, 5);
        let t0 = Instant::now();
        let mut deliveries: Vec<(Instant, usize)> = (0..10_000)
            .map(|i| match imp.process(100, t0 + Duration::from_micros(i as u64 * 100)) {
                Verdict::Deliver(at) => (at, i),
                Verdict::Drop(_) => panic!("no loss"),
            })
            .collect();
        deliveries.sort();
        let out_of_order = deliveries.windows(2).filter(|w| w[1].1 < w[0].1).count();
        assert!(out_of_order > 500, "{out_of_order}");
    }

    #[test]
    fn token_bucket_caps_rate_and_tail_drops() {
        // 8 Mbit/s = 1 MB/s; offer 2 MB/s for 1 s.
        let cap = BandwidthCap { rate_kbps: 8_000, burst_bytes: 10_000, max_queue_delay: Duration::from_millis(20) };
        let mut imp = Impairer::new(ImpairmentConfig::default().with_bandwidth(cap), 1);
        let t0 = Instant::now();
        let mut last = t0;
        let mut bytes = 0usize;
        let mut drops = 0usize;
        for i in 0..2000u64 {
            let now = t0 + Duration::from_micros(i * 500); // 1000 B every 0.5 ms
            match imp.process(1000, now) {
                Verdict::Deliver(at) => {
                    assert!(at >= now);
                    assert!(at - now <= Duration::from_millis(20));
                    bytes += 1000;
                    last = last.max(at);
                }
                Verdict::Drop(r) => {
                    assert_eq!(r, DropReason::Queue);
                    drops += 1;
                }
            }
        }
        let rate = bytes as f64 / (last - t0).as_secs_f64();
        assert!((rate - 1_000_000.0).abs() < 50_000.0, "rate {rate}");
        assert!(drops > 900 && drops < 1100, "drops {drops}");
    }

    #[test]
    fn token_bucket_under_rate_is_free() {
        let cap = BandwidthCap::new(100_000);
        let mut imp = Impairer::new(ImpairmentConfig::default().with_bandwidth(cap), 1);
        let t0 = Instant::now();
        for i in 0..1000u64 {
            let now = t0 + Duration::from_millis(i);
            assert_eq!(imp.process(1200, now), Verdict::Deliver(now));
        }
    }

    async fn echo_server() -> (SocketAddr, JoinHandle<()>) {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        let h = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = s.recv_from(&mut buf).await {
                let _ = s.send_to(&buf[..n], from).await;
            }
        });
        (addr, h)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn proxy_roundtrip_transparent() {
        let (srv, h) = echo_server().await;
        let proxy = UdpImpairProxy::start(srv, ImpairmentConfig::default(), ImpairmentConfig::default()).await.unwrap();
        let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        c.connect(proxy.local_addr()).await.unwrap();
        let mut buf = [0u8; 64];
        for i in 0..100u32 {
            c.send(&i.to_le_bytes()).await.unwrap();
            let n = tokio::time::timeout(Duration::from_secs(2), c.recv(&mut buf)).await.unwrap().unwrap();
            assert_eq!(&buf[..n], &i.to_le_bytes());
        }
        assert_eq!(proxy.stats(Direction::ToServer).delivered, 100);
        assert_eq!(proxy.stats(Direction::ToClient).delivered, 100);
        h.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn proxy_applies_latency_and_loss() {
        let (srv, h) = echo_server().await;
        let up = ImpairmentConfig::default().with_latency(Duration::from_millis(5), Duration::ZERO);
        let down = ImpairmentConfig::default().with_latency(Duration::from_millis(5), Duration::ZERO);
        let proxy = UdpImpairProxy::start(srv, up, down).await.unwrap();
        let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        c.connect(proxy.local_addr()).await.unwrap();
        let mut buf = [0u8; 64];
        let t = Instant::now();
        c.send(b"x").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), c.recv(&mut buf)).await.unwrap().unwrap();
        let rtt = t.elapsed();
        assert!(rtt >= Duration::from_millis(10) && rtt < Duration::from_millis(40), "rtt {rtt:?}");

        // Now 30% loss upstream: count echoes.
        proxy.set_config(Direction::ToServer, ImpairmentConfig::default().with_random_loss(0.3));
        proxy.set_config(Direction::ToClient, ImpairmentConfig::default());
        for i in 0..1000u32 {
            c.send(&i.to_le_bytes()).await.unwrap();
        }
        let mut got = 0;
        while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(300), c.recv(&mut buf)).await {
            got += 1;
        }
        let s = proxy.stats(Direction::ToServer);
        assert_eq!(s.received, 1001);
        assert!((250..350).contains(&s.dropped_loss), "{s:?}");
        assert_eq!(got as u64, s.delivered - 1);
        h.abort();
    }
}
