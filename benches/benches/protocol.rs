//! Micro-benchmarks for the wire protocol hot path: media header parse/write and
//! control envelope encode/decode + framing.

use std::hint::black_box;

use bytes::BytesMut;
use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use prost::Message;
use veloxdesk_protocol::control::{envelope::Msg, Envelope, InputEvent, ReceiverReport};
use veloxdesk_protocol::framing::{decode_frame, encode_frame};
use veloxdesk_protocol::media::{flags, MediaHeader, MediaKind, WireCodec};

fn header() -> MediaHeader {
    MediaHeader {
        kind: MediaKind::Video,
        flags: flags::END_OF_FRAME,
        stream_id: 0,
        block_index: 0,
        frame_id: 12345,
        shard_index: 3,
        data_shards: 40,
        parity_shards: 8,
        block_count: 1,
        codec: WireCodec::H265,
        shard_len: 1176,
        block_len: 40 * 1176 - 100,
        capture_ts: 987_654,
    }
}

fn media_header(c: &mut Criterion) {
    let mut g = c.benchmark_group("media_header");
    g.throughput(Throughput::Elements(1));
    let h = header();
    let mut dgram = h.to_bytes().to_vec();
    dgram.resize(1200, 0xAB);
    g.bench_function("parse+validate", |b| {
        b.iter(|| {
            let (p, payload) = MediaHeader::parse(black_box(&dgram)).unwrap();
            p.validate().unwrap();
            black_box((p, payload.len()))
        })
    });
    let mut out = [0u8; 24];
    g.bench_function("write", |b| b.iter(|| black_box(&h).write(black_box(&mut out)).unwrap()));
    g.finish();
}

fn control(c: &mut Criterion) {
    let mut g = c.benchmark_group("control");
    let input: Envelope = Msg::Input(InputEvent {
        ts_us: 123_456,
        event: Some(veloxdesk_protocol::control::input_event::Event::Key(veloxdesk_protocol::control::KeyEvent {
            hid_usage: 4,
            down: true,
            modifiers: 0,
        })),
    })
    .into();
    let rr: Envelope = Msg::ReceiverReport(ReceiverReport {
        stream_id: 0,
        highest_frame_id: 100_000,
        packets_received: 12_000,
        packets_lost: 30,
        packets_recovered_fec: 29,
        frames_unrecoverable: 1,
        recv_kbps: 40_000,
        delay_gradient: 0.25,
        jitter_us: 300,
        decode_us: 1200,
        render_us: 500,
        rtt_us: 400,
        interval_ms: 100,
    })
    .into();
    for (name, m) in [("input", &input), ("receiver_report", &rr)] {
        let bytes = m.encode_to_vec();
        g.bench_function(format!("{name}/encode"), |b| b.iter(|| black_box(m).encode_to_vec()));
        g.bench_function(format!("{name}/decode"), |b| b.iter(|| Envelope::decode(black_box(bytes.as_slice())).unwrap()));
        g.bench_function(format!("{name}/frame_roundtrip"), |b| {
            let mut buf = BytesMut::with_capacity(256);
            b.iter(|| {
                encode_frame(black_box(m), &mut buf).unwrap();
                black_box(decode_frame::<Envelope>(&mut buf).unwrap())
            })
        });
    }
    g.finish();
}

criterion_group!(benches, media_header, control);
criterion_main!(benches);
