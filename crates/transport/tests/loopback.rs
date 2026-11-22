//! Real QUIC loopback: two endpoints on 127.0.0.1, mutual Ed25519 auth, control stream,
//! and 120 fps video (200 KB keyframes + 20 KB P-frames) with 1–5 % randomly dropped
//! datagrams. Asserts full recovery (FEC + NACK) and prints latency / FEC numbers.
//!
//! NOTE: needs working UDP loopback (the dev sandbox drops it — run unsandboxed).

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{Timestamp, TransportChannel};
use veloxdesk_crypto::DeviceIdentity;
use veloxdesk_protocol::StreamKind;
use veloxdesk_protocol::control::{Envelope, Ping, envelope::Msg};
use veloxdesk_transport::{QuicChannel, VideoReceiver, VideoSender, connect, framed, listen};

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

fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() - 1) as f64 * p).round() as usize]
}

async fn session() -> (QuicChannel, QuicChannel, [u8; 32], [u8; 32]) {
    let host_id = DeviceIdentity::generate();
    let viewer_id = DeviceIdentity::generate();
    let listener = listen("127.0.0.1:0".parse::<SocketAddr>().unwrap(), &host_id).await.unwrap();
    let addr = listener.local_addr();
    let accept = tokio::spawn(async move { listener.accept().await.map(|c| (c, listener)) });
    let viewer = connect(addr, &viewer_id, Some(host_id.public_key())).await.unwrap();
    let (host, listener) = accept.await.unwrap().unwrap();
    std::mem::forget(listener); // keep endpoint alive for the test
    (host, viewer, host_id.public_key(), viewer_id.public_key())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handshake_auth_and_control_stream() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let (host, viewer, host_pk, viewer_pk) = session().await;
        assert_eq!(viewer.peer_public_key(), Some(host_pk));
        assert_eq!(host.peer_public_key(), Some(viewer_pk));
        assert!(viewer.max_datagram_size().unwrap() >= 1100);

        let (s, r) = viewer.open_bi(StreamKind::Control).await.unwrap();
        let (mut tx, _rx) = framed(s, r);
        let ping: Envelope = Msg::Ping(Ping { seq: 7, sent_us: 42 }).into();
        tx.send(&ping).await.unwrap();
        let (kind, hs, hr) = host.accept_bi().await.unwrap();
        assert_eq!(kind, StreamKind::Control);
        let (_htx, mut hrx) = framed(hs, hr);
        assert_eq!(hrx.recv().await.unwrap(), ping);

        // Wrong expected key is rejected.
        let other = DeviceIdentity::generate();
        let listener = listen("127.0.0.1:0".parse().unwrap(), &other).await.unwrap();
        let addr = listener.local_addr();
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        assert!(connect(addr, &DeviceIdentity::generate(), Some([9; 32])).await.is_err());
        let st = viewer.stats();
        println!("stats: rtt {} µs, path {:?}", st.rtt_us, st.path);
    })
    .await
    .expect("timed out (UDP loopback blocked?)");
}

struct RunResult {
    frames: u32,
    delivered: u32,
    lat: Vec<u64>,
    enc_us: Vec<u64>,
    dec_us_per_frame: f64,
    dropped: u32,
    datagrams: u64,
    recovered: u64,
    nacks: u64,
    retransmitted: u64,
    discontinuities: u32,
}

async fn run_video(loss: f64, frames: u32, seed: u64) -> RunResult {
    let (host, viewer, _, _) = session().await;
    // Control stream viewer → host carrying feedback.
    let (vs, vr) = viewer.open_bi(StreamKind::Control).await.unwrap();
    let (mut fb_tx, _fb_rx_unused) = framed(vs, vr);
    fb_tx.send(&Msg::Ping(Ping::default()).into()).await.unwrap();
    let (_, hs, hr) = host.accept_bi().await.unwrap();
    let (_h_tx, mut h_rx) = framed(hs, hr);

    let sender = Arc::new(Mutex::new(VideoSender::new(Arc::new(host.clone()), 0)));
    sender.lock().set_parity_percent(20);

    // Host: NACK handler.
    let s2 = sender.clone();
    tokio::spawn(async move {
        while let Ok(env) = h_rx.recv().await {
            if let Some(Msg::Nack(n)) = env.msg {
                s2.lock().on_nack(&n);
            }
        }
    });

    // Host: 120 fps encode thread (sync, precise pacing).
    let done = Arc::new(AtomicBool::new(false));
    let (s3, done2) = (sender.clone(), done.clone());
    let send_thread = std::thread::spawn(move || {
        let interval = Duration::from_nanos(1_000_000_000 / 120);
        let start = Instant::now();
        let mut enc = Vec::new();
        let mut dropped = 0;
        for id in 0..frames {
            let due = start + interval * id;
            while Instant::now() < due {
                let left = due - Instant::now();
                if left > Duration::from_micros(1500) {
                    std::thread::sleep(left - Duration::from_millis(1));
                } else {
                    std::hint::spin_loop();
                }
            }
            let key = id % 120 == 0;
            let len = if key { 200_000 } else { 20_000 };
            let pkt = EncodedPacket {
                data: payload(id, len),
                codec: VideoCodec::H265,
                keyframe: key,
                recovery_point: false,
                width: 1920,
                height: 1080,
                capture_ts: Timestamp::now(),
                frame_id: id,
                encode_us: 0,
                convert_us: 0,
            };
            let st = s3.lock().send_frame(&pkt).unwrap();
            enc.push(st.fec_encode_us as u64);
            dropped += st.dropped;
        }
        done2.store(true, Ordering::SeqCst);
        (enc, dropped)
    });

    // Viewer: receive, drop randomly, reassemble, feed back.
    let mut rx = VideoReceiver::new();
    rx.set_host_time_reference(Timestamp::now());
    let mut rng = StdRng::seed_from_u64(seed);
    let mut lat = Vec::new();
    let mut delivered = 0u32;
    let mut discontinuities = 0u32;
    let mut datagrams = 0u64;
    let mut done_at: Option<Instant> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(2));
    loop {
        let mut got = Vec::new();
        tokio::select! {
            d = viewer.recv_datagram() => {
                let d = d.unwrap();
                datagrams += 1;
                if rng.random::<f64>() >= loss {
                    let now = Timestamp::now();
                    if let Some(f) = rx.push_datagram(&d, now) { got.push(f); }
                    while let Some(f) = rx.pop_frame() { got.push(f); }
                }
            }
            _ = tick.tick() => {}
        }
        rx.set_rtt_us(viewer.stats().rtt_us);
        for env in rx.poll_feedback(Timestamp::now()) {
            fb_tx.send(&env).await.unwrap();
        }
        while let Some(f) = rx.pop_frame() {
            got.push(f);
        }
        for f in got {
            assert_eq!(f.packet.data, payload(f.packet.frame_id, f.packet.data.len()), "corrupt frame");
            lat.push(f.recv_complete_ts.since(f.packet.capture_ts));
            delivered += 1;
            discontinuities += f.discontinuity as u32;
        }
        if delivered == frames {
            break;
        }
        if done.load(Ordering::SeqCst) {
            let t = *done_at.get_or_insert_with(Instant::now);
            if t.elapsed() > Duration::from_millis(500) {
                break;
            }
        }
    }
    let (enc_us, dropped) = send_thread.join().unwrap();
    let st = rx.stats();
    let s = sender.lock();
    RunResult {
        frames,
        delivered,
        lat,
        enc_us,
        dec_us_per_frame: st.fec_decode_us_total as f64 / frames as f64,
        dropped,
        datagrams,
        recovered: st.packets_recovered_fec,
        nacks: st.nacks_sent,
        retransmitted: s.retransmitted(),
        discontinuities,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_120fps_with_random_loss() {
    tokio::time::timeout(Duration::from_secs(60), async {
        for (i, loss) in [0.01, 0.03, 0.05].into_iter().enumerate() {
            let mut r = run_video(loss, 360, 100 + i as u64).await;
            let enc_p50 = pct(&mut r.enc_us, 0.5);
            let enc_p99 = pct(&mut r.enc_us, 0.99);
            println!(
                "loss {:.0}%: delivered {}/{} | send→complete p50 {:.2} ms p99 {:.2} ms max {:.2} ms | \
                 FEC enc/frame p50 {} µs p99 {} µs, dec avg {:.1} µs/frame | dgrams {} local-drop {} \
                 recovered {} nacks {} retx {} discont {}",
                loss * 100.0,
                r.delivered,
                r.frames,
                pct(&mut r.lat, 0.5) as f64 / 1000.0,
                pct(&mut r.lat, 0.99) as f64 / 1000.0,
                pct(&mut r.lat, 1.0) as f64 / 1000.0,
                enc_p50,
                enc_p99,
                r.dec_us_per_frame,
                r.datagrams,
                r.dropped,
                r.recovered,
                r.nacks,
                r.retransmitted,
                r.discontinuities
            );
            assert_eq!(r.delivered, r.frames, "all frames must be recovered at {loss}");
            assert_eq!(r.discontinuities, 0);
        }
    })
    .await
    .expect("timed out (UDP loopback blocked?)");
}
