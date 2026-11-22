//! FEC packetize → lossy, reordered delivery → reassembly, without a network.
//! Exercises parity recovery, NACK retransmission, PLI and keyframe supersede.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};

use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{CoreError, Result, Timestamp, TransportChannel};
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::envelope::Msg;
use veloxdesk_transport::{VideoReceiver, VideoSender};

#[derive(Default)]
struct MockChannel {
    out: Mutex<Vec<Bytes>>,
}

#[async_trait]
impl TransportChannel for MockChannel {
    fn max_datagram_size(&self) -> Option<usize> {
        Some(1200)
    }
    fn send_datagram(&self, data: Bytes) -> Result<()> {
        assert!(data.len() <= 1200);
        self.out.lock().push(data);
        Ok(())
    }
    async fn recv_datagram(&self) -> Result<Bytes> {
        Err(CoreError::Closed)
    }
    async fn open_bi(&self, _: StreamKind) -> Result<(SendStream, RecvStream)> {
        Err(CoreError::Closed)
    }
    async fn open_uni(&self, _: StreamKind) -> Result<SendStream> {
        Err(CoreError::Closed)
    }
    async fn accept_bi(&self) -> Result<(StreamKind, SendStream, RecvStream)> {
        Err(CoreError::Closed)
    }
    async fn accept_uni(&self) -> Result<(StreamKind, RecvStream)> {
        Err(CoreError::Closed)
    }
    fn stats(&self) -> TransportStats {
        TransportStats::default()
    }
    fn peer_public_key(&self) -> Option<[u8; 32]> {
        None
    }
    fn close(&self, _: u32, _: &str) {}
}

fn payload(frame_id: u32, len: usize) -> Bytes {
    let mut x = frame_id.wrapping_mul(2_654_435_761).wrapping_add(1) as u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect::<Vec<u8>>()
        .into()
}

fn packet(frame_id: u32, key: bool, len: usize, ts: Timestamp) -> EncodedPacket {
    EncodedPacket {
        data: payload(frame_id, len),
        codec: VideoCodec::H264,
        keyframe: key,
        recovery_point: false,
        width: 1920,
        height: 1080,
        capture_ts: ts,
        frame_id,
        encode_us: 0,
        convert_us: 0,
    }
}

struct Sim {
    ch: Arc<MockChannel>,
    tx: VideoSender,
    rx: VideoReceiver,
    rng: StdRng,
    now: u64,
    delivered: Vec<(u32, bool)>,
}

impl Sim {
    fn new(seed: u64, parity: u8, rtt_us: u32) -> Self {
        let ch = Arc::new(MockChannel::default());
        let mut tx = VideoSender::new(ch.clone(), 0);
        tx.set_parity_percent(parity);
        let mut rx = VideoReceiver::new();
        rx.set_rtt_us(rtt_us);
        rx.set_host_time_reference(Timestamp(1_000_000));
        Self { ch, tx, rx, rng: StdRng::seed_from_u64(seed), now: 1_000_000, delivered: Vec::new() }
    }

    /// Deliver what the sender queued, dropping with probability `loss` and shuffling.
    fn deliver(&mut self, loss: f64) {
        let mut d: Vec<Bytes> = std::mem::take(&mut *self.ch.out.lock());
        d.shuffle(&mut self.rng);
        for dg in d {
            if self.rng.random::<f64>() < loss {
                continue;
            }
            self.now += 5;
            let mut f = self.rx.push_datagram(&dg, Timestamp(self.now));
            while let Some(fr) = f {
                assert_eq!(fr.packet.data, payload(fr.packet.frame_id, fr.packet.data.len()), "corrupt frame");
                self.delivered.push((fr.packet.frame_id, fr.discontinuity));
                f = self.rx.pop_frame();
            }
        }
    }

    fn feedback(&mut self) -> (u32, u32) {
        let (mut nacks, mut plis) = (0, 0);
        for env in self.rx.poll_feedback(Timestamp(self.now)) {
            match env.msg {
                Some(Msg::Nack(n)) => {
                    nacks += 1;
                    self.tx.on_nack(&n);
                }
                Some(Msg::KeyframeRequest(_)) => plis += 1,
                _ => {}
            }
        }
        while let Some(fr) = self.rx.pop_frame() {
            self.delivered.push((fr.packet.frame_id, fr.discontinuity));
        }
        (nacks, plis)
    }
}

#[test]
fn parity_recovers_random_loss_out_of_order() {
    let mut sim = Sim::new(7, 25, 0); // NACK disabled (rtt unknown)
    let n = 240;
    for id in 0..n {
        let key = id % 120 == 0;
        let pkt = packet(id, key, if key { 200_000 } else { 20_000 }, Timestamp(sim.now));
        sim.tx.send_frame(&pkt).unwrap();
        sim.deliver(0.03);
        sim.now += 8_333;
        sim.feedback();
    }
    sim.now += 100_000;
    sim.feedback();
    let st = sim.rx.stats();
    println!("parity-only: {st:?}");
    assert!(st.packets_recovered_fec > 0);
    // 25 % parity vs 3 % iid loss: essentially everything is recovered.
    assert!(sim.delivered.len() as u32 >= n - 2, "delivered {}", sim.delivered.len());
    // In-order delivery.
    assert!(sim.delivered.windows(2).all(|w| w[0].0 < w[1].0));
}

#[test]
fn nack_recovers_what_parity_cannot() {
    // 10 % loss with only 5 % parity: many blocks need retransmission.
    let mut sim = Sim::new(11, 5, 2_000);
    let n = 120;
    let mut nacks = 0;
    for id in 0..n {
        let key = id == 0;
        let pkt = packet(id, key, if key { 200_000 } else { 20_000 }, Timestamp(sim.now));
        sim.tx.send_frame(&pkt).unwrap();
        sim.deliver(0.10);
        // A few feedback rounds within the frame interval (retransmits are lossless here).
        for _ in 0..4 {
            sim.now += 2_000;
            nacks += sim.feedback().0;
            sim.deliver(0.0);
        }
    }
    sim.now += 200_000;
    sim.feedback();
    println!("nack: nacks={nacks} retransmitted={} stats={:?}", sim.tx.retransmitted(), sim.rx.stats());
    assert!(nacks > 0);
    assert_eq!(sim.delivered.len() as u32, n, "all frames recovered via FEC+NACK");
    assert!(sim.delivered.iter().all(|d| !d.1));
}

#[test]
fn unrecoverable_loss_requests_keyframe_and_flags_discontinuity() {
    let mut sim = Sim::new(3, 0, 0);
    sim.tx.send_frame(&packet(0, true, 5_000, Timestamp(sim.now))).unwrap();
    sim.deliver(0.0);
    // Frame 1 vanishes entirely.
    sim.tx.send_frame(&packet(1, false, 5_000, Timestamp(sim.now))).unwrap();
    sim.ch.out.lock().clear();
    sim.now += 8_000;
    sim.tx.send_frame(&packet(2, false, 5_000, Timestamp(sim.now))).unwrap();
    sim.deliver(0.0);
    sim.now += 10_000;
    let (_, plis) = sim.feedback();
    assert_eq!(plis, 1);
    assert_eq!(sim.delivered, vec![(0, false), (2, true)]);
    // Rate limited.
    sim.now += 1_000;
    assert_eq!(sim.feedback().1, 0);
    // Keyframe clears the request.
    sim.tx.send_frame(&packet(3, true, 50_000, Timestamp(sim.now))).unwrap();
    sim.deliver(0.0);
    sim.now += 100_000;
    assert_eq!(sim.feedback().1, 0);
    assert_eq!(sim.delivered.last(), Some(&(3, false)));
}

#[test]
fn keyframe_supersedes_pending_frames() {
    let mut sim = Sim::new(5, 0, 0);
    sim.tx.send_frame(&packet(0, true, 3_000, Timestamp(sim.now))).unwrap();
    sim.deliver(0.0);
    // Frame 1 loses one shard (no parity) → pending; keyframe 2 completes → delivered now.
    sim.tx.send_frame(&packet(1, false, 3_000, Timestamp(sim.now))).unwrap();
    sim.ch.out.lock().remove(0);
    sim.tx.send_frame(&packet(2, true, 3_000, Timestamp(sim.now))).unwrap();
    sim.deliver(0.0);
    assert_eq!(sim.delivered, vec![(0, false), (2, false)]);
    assert_eq!(sim.rx.stats().frames_superseded, 1);
}

#[test]
fn receiver_reports_loss_and_rate() {
    let mut sim = Sim::new(9, 20, 0);
    let mut reports = Vec::new();
    for id in 0..60 {
        sim.tx.send_frame(&packet(id, id == 0, 10_000, Timestamp(sim.now))).unwrap();
        sim.deliver(0.05);
        sim.now += 8_333;
        for env in sim.rx.poll_feedback(Timestamp(sim.now)) {
            if let Some(Msg::ReceiverReport(r)) = env.msg {
                reports.push(r);
            }
        }
    }
    assert!(reports.len() >= 8, "{} reports", reports.len());
    let lost: u32 = reports.iter().map(|r| r.packets_lost).sum();
    let recv: u32 = reports.iter().map(|r| r.packets_received).sum();
    let loss = lost as f64 / (lost + recv) as f64;
    assert!((0.02..0.09).contains(&loss), "measured loss {loss}");
    assert!(reports.iter().all(|r| r.recv_kbps > 0 && r.interval_ms >= 50));
}

#[test]
fn fec_timings() {
    let ch = Arc::new(MockChannel::default());
    let mut tx = VideoSender::new(ch.clone(), 0);
    tx.set_parity_percent(20);
    let mut rx = VideoReceiver::new();
    let mut enc = Vec::new();
    for id in 0..50u32 {
        let st = tx.send_frame(&packet(id, true, 200_000, Timestamp(1000))).unwrap();
        enc.push(st.fec_encode_us);
        let mut d = std::mem::take(&mut *ch.out.lock());
        // drop 10 % of the data shards of every block (forces RS decode)
        let mut i = 0;
        d.retain(|_| {
            i += 1;
            i % 10 != 3
        });
        let mut got = None;
        for dg in d {
            if let Some(f) = rx.push_datagram(&dg, Timestamp(2000 + id as u64)) {
                got = Some(f);
            }
        }
        assert!(got.is_some(), "keyframe {id} recovered");
    }
    enc.sort_unstable();
    let st = rx.stats();
    println!(
        "FEC 200KB keyframe @20%: encode p50 {} µs, decode avg {} µs/block over {} blocks",
        enc[enc.len() / 2],
        st.fec_decode_us_total / st.fec_decodes.max(1),
        st.fec_decodes
    );
}
