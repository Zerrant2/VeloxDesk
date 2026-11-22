//! Transport loss tests: loopback QUIC through the impairment proxy at 1/3/5 % random
//! loss (and a bursty case); every frame must arrive byte-exact, recovered by FEC.
//!
//! NOTE: the agent sandbox drops loopback UDP — run with network access.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use veloxdesk_benches::impair::{Direction, ImpairmentConfig, LossModel, UdpImpairProxy};
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::transport::TransportChannel;
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_transport::{VideoReceiver, VideoSender};

fn payload(frame_id: u32, len: usize) -> Bytes {
    let mut x = 0x9E37_79B9u32 ^ frame_id.wrapping_mul(2_654_435_761);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect::<Vec<u8>>()
        .into()
}

struct Outcome {
    sent: u32,
    complete: u32,
    recovered_shards: u64,
    proxy_loss: f64,
    p50_us: u32,
    p99_us: u32,
}

async fn run(loss: LossModel, parity: u8, frames: u32) -> Outcome {
    let host = DeviceIdentity::generate();
    let viewer = DeviceIdentity::generate();
    let listener = veloxdesk_transport::listen("127.0.0.1:0".parse().unwrap(), &host).await.expect("listen");
    let proxy = UdpImpairProxy::start(listener.local_addr(), ImpairmentConfig::default(), ImpairmentConfig::default())
        .await
        .expect("proxy");
    let (h, v) = tokio::join!(listener.accept(), veloxdesk_transport::connect(proxy.local_addr(), &viewer, None));
    let host_ch: Arc<dyn TransportChannel> = Arc::new(h.expect("accept"));
    let viewer_ch: Arc<dyn TransportChannel> = Arc::new(v.expect("connect"));
    // Handshake done without loss; now impair the media direction only.
    proxy.set_config(Direction::ToClient, ImpairmentConfig::default().with_loss(loss));

    let recv = tokio::spawn({
        let ch = viewer_ch.clone();
        async move {
            let mut rx = VideoReceiver::new();
            let mut got: Vec<(EncodedPacket, Timestamp, u64)> = Vec::new();
            let mut idle_since = Instant::now();
            loop {
                match tokio::time::timeout(Duration::from_millis(5), ch.recv_datagram()).await {
                    Ok(Ok(d)) => {
                        idle_since = Instant::now();
                        if let Some(f) = rx.push_datagram(&d, Timestamp::now()) {
                            got.push((f.packet, f.recv_complete_ts, f.recovered_shards as u64));
                        }
                        while let Some(f) = rx.pop_frame() {
                            got.push((f.packet, f.recv_complete_ts, f.recovered_shards as u64));
                        }
                    }
                    Ok(Err(_)) => break,
                    Err(_) => {
                        let _ = rx.poll_feedback(Timestamp::now());
                        while let Some(f) = rx.pop_frame() {
                            got.push((f.packet, f.recv_complete_ts, f.recovered_shards as u64));
                        }
                        if idle_since.elapsed() >= Duration::from_millis(500) {
                            break;
                        }
                    }
                }
            }
            got
        }
    });

    let mut sender = VideoSender::new(host_ch.clone(), 0);
    sender.set_parity_percent(parity);
    let mut sent_at = std::collections::HashMap::new();
    let start = Instant::now();
    for i in 0..frames {
        let keyframe = i % 60 == 0;
        let len = if keyframe { 200_000 } else { 20_000 };
        let pkt = EncodedPacket {
            data: payload(i, len),
            codec: VideoCodec::H264,
            keyframe,
            recovery_point: false,
            width: 1920,
            height: 1080,
            capture_ts: Timestamp::now(),
            frame_id: i,
            encode_us: 0,
            convert_us: 0,
        };
        sent_at.insert(i, pkt.capture_ts);
        sender.send_frame(&pkt).expect("send_frame");
        // 120 fps pacing.
        let next = start + Duration::from_micros(8_333 * (i as u64 + 1));
        tokio::time::sleep_until(next.into()).await;
    }
    let got = recv.await.unwrap();

    let mut lat = veloxdesk_core_traits::metrics::LatencySamples::default();
    let mut recovered = 0;
    for (p, done, rec) in &got {
        assert_eq!(p.data, payload(p.frame_id, p.data.len()), "frame {} corrupted", p.frame_id);
        assert_eq!(p.data.len(), if p.frame_id % 60 == 0 { 200_000 } else { 20_000 });
        recovered += rec;
        lat.push(done.since(sent_at[&p.frame_id]) as u32);
    }
    let mut ids: Vec<u32> = got.iter().map(|g| g.0.frame_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), got.len(), "duplicate frames emitted");
    Outcome {
        sent: frames,
        complete: got.len() as u32,
        recovered_shards: recovered,
        proxy_loss: proxy.stats(Direction::ToClient).loss_rate(),
        p50_us: lat.percentile(50.0).unwrap_or(0),
        p99_us: lat.percentile(99.0).unwrap_or(0),
    }
}

fn report(name: &str, o: &Outcome) {
    eprintln!(
        "[transport_loss] {name}: complete {}/{} ({:.2}%), observed loss {:.2}%, FEC-recovered shards {}, send→complete p50 {:.2} ms p99 {:.2} ms",
        o.complete,
        o.sent,
        100.0 * o.complete as f64 / o.sent as f64,
        100.0 * o.proxy_loss,
        o.recovered_shards,
        o.p50_us as f64 / 1000.0,
        o.p99_us as f64 / 1000.0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_loss_all_frames_arrive() {
    let o = run(LossModel::None, 10, 240).await;
    report("0% loss", &o);
    assert!(o.complete >= o.sent - 1, "only {}/{}", o.complete, o.sent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_percent_loss_recovered() {
    let o = run(LossModel::Random(0.01), 20, 480).await;
    report("1% loss", &o);
    assert!(o.recovered_shards > 0);
    assert!(o.complete as f64 >= o.sent as f64 * 0.995, "only {}/{}", o.complete, o.sent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_percent_loss_recovered() {
    let o = run(LossModel::Random(0.03), 20, 480).await;
    report("3% loss", &o);
    assert!(o.recovered_shards > 0);
    assert!(o.complete as f64 >= o.sent as f64 * 0.99, "only {}/{}", o.complete, o.sent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn five_percent_loss_recovered() {
    let o = run(LossModel::Random(0.05), 25, 480).await;
    report("5% loss", &o);
    assert!(o.recovered_shards > 0);
    assert!(o.complete as f64 >= o.sent as f64 * 0.98, "only {}/{}", o.complete, o.sent);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bursty_loss_with_high_parity() {
    // 3% average, mean burst 3 packets; 40% parity should still recover nearly everything.
    let o = run(LossModel::bursty(0.03, 3.0), 40, 480).await;
    report("3% bursty (len 3)", &o);
    assert!(o.complete as f64 >= o.sent as f64 * 0.97, "only {}/{}", o.complete, o.sent);
}
