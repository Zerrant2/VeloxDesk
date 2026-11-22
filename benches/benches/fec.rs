//! Micro-benchmarks for Reed-Solomon FEC packetize (encode) and reassembly/recovery (decode).

use std::hint::black_box;

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_transport::{Packetizer, VideoReceiver};

fn sample_packet(frame_id: u32, len: usize, keyframe: bool) -> EncodedPacket {
    let mut data = vec![0u8; len];
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i as u32).wrapping_mul(31).wrapping_add(frame_id)) as u8;
    }
    EncodedPacket {
        data: Bytes::from(data),
        codec: VideoCodec::H264,
        keyframe,
        recovery_point: false,
        width: 1920,
        height: 1080,
        capture_ts: Timestamp::now(),
        frame_id,
        encode_us: 0,
        convert_us: 0,
    }
}

fn fec_benches(c: &mut Criterion) {
    const MAX_DGRAM: usize = 1200;

    for &(label, bytes, keyframe) in &[("p_frame_20kb", 20_000usize, false), ("keyframe_200kb", 200_000usize, true)] {
        let mut g = c.benchmark_group(format!("fec/{label}"));
        g.throughput(Throughput::Bytes(bytes as u64));

        // 1. Packetize + RS encode at 20% parity.
        let pkt = sample_packet(1, bytes, keyframe);
        g.bench_function("encode_20pct_parity", |b| {
            let mut pack = Packetizer::new(0);
            pack.set_parity_percent(20);
            let mut out = Vec::with_capacity(256);
            let mut starts = Vec::with_capacity(8);
            b.iter(|| {
                out.clear();
                let res = pack.packetize(black_box(&pkt), MAX_DGRAM, &mut out, &mut starts).unwrap();
                black_box((out.len(), res.1.fec_encode_us))
            })
        });

        // Pre-packetize once for decode benches.
        let mut pack = Packetizer::new(0);
        pack.set_parity_percent(20);
        let mut all_dgrams = Vec::new();
        let mut starts = Vec::new();
        pack.packetize(&pkt, MAX_DGRAM, &mut all_dgrams, &mut starts).unwrap();

        // Build a 5%-loss view (dropping every 20th data shard so RS recovery runs).
        let mut lossy_dgrams = Vec::with_capacity(all_dgrams.len());
        for &(start, k, m) in &starts {
            let block = &all_dgrams[start..start + k + m];
            // Drop up to min(m, max(1, k/20)) data shards so recovery is guaranteed.
            let drop_n = (k / 20).max(1).min(m);
            for (idx, d) in block.iter().enumerate() {
                if idx < drop_n {
                    continue;
                }
                lossy_dgrams.push(d.clone());
            }
        }

        // Helper: stamp a fresh frame_id into a mutable copy of datagram headers so a
        // single VideoReceiver can be reused across iterations (steady-state RS decode).
        let stamp = |dgrams: &mut [Vec<u8>], fid: u32| {
            let b = fid.to_le_bytes();
            for d in dgrams.iter_mut() {
                d[4..8].copy_from_slice(&b);
            }
        };

        // 2. Receive + reassemble with 0% loss (fast path, no RS decode).
        g.bench_function("decode_0pct_loss", |b| {
            let mut rx = VideoReceiver::new();
            let mut dgrams: Vec<Vec<u8>> = all_dgrams.iter().map(|d| d.to_vec()).collect();
            let mut fid = 1u32;
            let mut t = 10_000u64;
            b.iter(|| {
                stamp(&mut dgrams, fid);
                fid = fid.wrapping_add(1);
                t += 8_333;
                let now = Timestamp(t);
                let mut complete = None;
                for d in &dgrams {
                    if let Some(f) = rx.push_datagram(black_box(d), now) {
                        complete = Some(f);
                    }
                }
                while let Some(f) = rx.pop_frame() {
                    complete = Some(f);
                }
                black_box(complete.expect("frame must complete"))
            })
        });

        // 3. Receive + RS recover with ~5% data shard loss.
        g.bench_function("decode_5pct_loss_rs_recover", |b| {
            let mut rx = VideoReceiver::new();
            let mut dgrams: Vec<Vec<u8>> = lossy_dgrams.iter().map(|d| d.to_vec()).collect();
            let mut fid = 1u32;
            let mut t = 10_000u64;
            b.iter(|| {
                stamp(&mut dgrams, fid);
                fid = fid.wrapping_add(1);
                t += 8_333;
                let now = Timestamp(t);
                let mut complete = None;
                for d in &dgrams {
                    if let Some(f) = rx.push_datagram(black_box(d), now) {
                        complete = Some(f);
                    }
                }
                while let Some(f) = rx.pop_frame() {
                    complete = Some(f);
                }
                let f = complete.expect("frame must recover via FEC");
                assert!(f.recovered_shards > 0);
                black_box(f)
            })
        });

        g.finish();
    }
}

criterion_group!(benches, fec_benches);
criterion_main!(benches);
