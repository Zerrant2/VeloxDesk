//! Protocol compatibility: old/new envelopes, golden wire bytes, framing.
//!
//! "Old" and "future" peers are modelled with locally defined prost structs that share
//! tags with the frozen v1 messages in `veloxdesk_protocol::control`.

use bytes::BytesMut;
use prost::Message;
use veloxdesk_protocol::control::{
    envelope::Msg, Envelope, Hello, KeyframeRequest, Nack, Ping, ReceiverReport, VideoCodecCap,
};
use veloxdesk_protocol::framing::{decode_frame, encode_frame, frame_to_bytes, stream_preamble, MAX_FRAME_LEN};
use veloxdesk_protocol::media::{flags, MediaHeader, MediaKind, WireCodec, MEDIA_HEADER_LEN};
use veloxdesk_protocol::{ProtocolError, StreamKind};

/// A minimal "old" Hello (only the first 3 fields existed).
#[derive(Clone, PartialEq, Message)]
struct HelloV0 {
    #[prost(uint32, tag = "1")]
    protocol_version: u32,
    #[prost(string, tag = "2")]
    device_id: String,
    #[prost(bytes = "vec", tag = "3")]
    public_key: Vec<u8>,
}

/// A "future" Hello with an extra field the current version does not know.
#[derive(Clone, PartialEq, Message)]
struct HelloV2 {
    #[prost(uint32, tag = "1")]
    protocol_version: u32,
    #[prost(string, tag = "2")]
    device_id: String,
    #[prost(string, tag = "4")]
    device_name: String,
    #[prost(string, tag = "100")]
    future_field: String,
    #[prost(uint64, repeated, tag = "101")]
    future_list: Vec<u64>,
}

/// "Old" envelope that knew only Hello and Ping.
#[derive(Clone, PartialEq, Message)]
struct EnvelopeV0 {
    #[prost(oneof = "env_v0::Msg", tags = "1, 6")]
    msg: Option<env_v0::Msg>,
}
mod env_v0 {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Msg {
        #[prost(message, tag = "1")]
        Hello(super::HelloV0),
        #[prost(message, tag = "6")]
        Ping(veloxdesk_protocol::control::Ping),
    }
}

/// "Future" envelope with a message type at a tag unknown today.
#[derive(Clone, PartialEq, Message)]
struct EnvelopeV2 {
    #[prost(oneof = "env_v2::Msg", tags = "1, 200")]
    msg: Option<env_v2::Msg>,
}
mod env_v2 {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Msg {
        #[prost(message, tag = "1")]
        Hello(super::HelloV2),
        #[prost(message, tag = "200")]
        Hologram(super::HelloV0),
    }
}

fn hello_v1() -> Hello {
    Hello {
        protocol_version: 1,
        device_id: "123456789".into(),
        public_key: vec![7; 32],
        device_name: "viewer".into(),
        os: "windows".into(),
        app_version: "0.1.0".into(),
        mode: 1,
        video_caps: vec![VideoCodecCap { codec: 2, hw_decode: true, max_width: 3840, max_height: 2160, max_fps: 120, yuv444: false, ten_bit: true }],
        audio_codecs: vec![1],
        viewer_width: 2560,
        viewer_height: 1440,
        viewer_refresh_hz: 144,
        max_bitrate_kbps: 50_000,
    }
}

#[test]
fn new_peer_message_decodes_on_current_version() {
    let fut = EnvelopeV2 {
        msg: Some(env_v2::Msg::Hello(HelloV2 {
            protocol_version: 2,
            device_id: "987654321".into(),
            device_name: "future".into(),
            future_field: "ignored".into(),
            future_list: vec![1, 2, 3],
        })),
    };
    let env = Envelope::decode(fut.encode_to_vec().as_slice()).expect("future Hello must decode");
    let Some(Msg::Hello(h)) = env.msg else { panic!("expected Hello, got {:?}", env.msg) };
    assert_eq!(h.protocol_version, 2);
    assert_eq!(h.device_id, "987654321");
    assert_eq!(h.device_name, "future");
}

#[test]
fn unknown_future_message_type_is_none() {
    let fut = EnvelopeV2 { msg: Some(env_v2::Msg::Hologram(HelloV0 { protocol_version: 9, ..Default::default() })) };
    let env = Envelope::decode(fut.encode_to_vec().as_slice()).expect("unknown oneof tag must not error");
    assert!(env.msg.is_none());
}

#[test]
fn current_message_decodes_on_old_peer() {
    let env: Envelope = Msg::Hello(hello_v1()).into();
    let old = EnvelopeV0::decode(env.encode_to_vec().as_slice()).expect("old peer must decode v1 Hello");
    let Some(env_v0::Msg::Hello(h)) = old.msg else { panic!("expected Hello") };
    assert_eq!(h.protocol_version, 1);
    assert_eq!(h.device_id, "123456789");
    assert_eq!(h.public_key, vec![7; 32]);

    // Messages the old peer does not know decode as None rather than failing.
    let env: Envelope = Msg::KeyframeRequest(KeyframeRequest { stream_id: 0, last_good_frame_id: 5 }).into();
    let old = EnvelopeV0::decode(env.encode_to_vec().as_slice()).unwrap();
    assert!(old.msg.is_none());
}

#[test]
fn old_peer_message_decodes_with_defaults() {
    let old = EnvelopeV0 {
        msg: Some(env_v0::Msg::Hello(HelloV0 { protocol_version: 0, device_id: "111222333".into(), public_key: vec![1; 32] })),
    };
    let env = Envelope::decode(old.encode_to_vec().as_slice()).unwrap();
    let Some(Msg::Hello(h)) = env.msg else { panic!() };
    assert_eq!(h.device_id, "111222333");
    assert_eq!(h.viewer_width, 0);
    assert!(h.video_caps.is_empty());
}

/// Golden bytes freeze the tag numbers: if anyone renumbers a field these break.
#[test]
fn golden_envelope_bytes() {
    let ping: Envelope = Msg::Ping(Ping { seq: 1, sent_us: 2 }).into();
    // field 6 (len) = 0x32, len 4, [field1 varint 1][field2 varint 2]
    assert_eq!(ping.encode_to_vec(), vec![0x32, 0x04, 0x08, 0x01, 0x10, 0x02]);

    let nack: Envelope = Msg::Nack(Nack { stream_id: 0, frame_id: 3, block_index: 1, shard_indices: vec![4, 5] }).into();
    // field 11 (len) = 0x5A; frame_id tag2=3, block tag3=1, packed tag4 [4,5]
    assert_eq!(nack.encode_to_vec(), vec![0x5A, 0x08, 0x10, 0x03, 0x18, 0x01, 0x22, 0x02, 0x04, 0x05]);

    let rr: Envelope = Msg::ReceiverReport(ReceiverReport { packets_lost: 7, ..Default::default() }).into();
    assert_eq!(rr.encode_to_vec(), vec![0x62, 0x02, 0x20, 0x07]);
}

#[test]
fn all_envelope_kinds_roundtrip_through_framing() {
    let msgs: Vec<Envelope> = vec![
        Msg::Hello(hello_v1()).into(),
        Msg::Ping(Ping { seq: u32::MAX, sent_us: u64::MAX }).into(),
        Msg::Nack(Nack { stream_id: 1, frame_id: 2, block_index: 3, shard_indices: (0..200).collect() }).into(),
        Msg::ReceiverReport(ReceiverReport { delay_gradient: -1.5, rtt_us: 400, ..Default::default() }).into(),
        Envelope { msg: None },
    ];
    let mut wire = BytesMut::new();
    for m in &msgs {
        encode_frame(m, &mut wire).unwrap();
    }
    // Feed byte-by-byte to exercise partial frames.
    let all = wire.freeze();
    let mut buf = BytesMut::new();
    let mut out = Vec::new();
    for b in all.iter() {
        buf.extend_from_slice(&[*b]);
        while let Some(m) = decode_frame::<Envelope>(&mut buf).unwrap() {
            out.push(m);
        }
    }
    assert_eq!(out, msgs);
    assert!(buf.is_empty());
}

#[test]
fn framing_rejects_oversized_length() {
    let mut buf = BytesMut::new();
    buf.extend_from_slice(&((MAX_FRAME_LEN as u32) + 1).to_le_bytes());
    assert!(matches!(decode_frame::<Envelope>(&mut buf), Err(ProtocolError::TooLarge(_))));
    let ok = frame_to_bytes(&Envelope::from(Msg::Ping(Ping::default()))).unwrap();
    assert_eq!(&ok[..4], &2u32.to_le_bytes()); // empty Ping inside field 6: 0x32 0x00
}

#[test]
fn stream_kind_preamble_is_stable() {
    for (k, b) in [
        (StreamKind::Control, 1u8),
        (StreamKind::Input, 2),
        (StreamKind::Clipboard, 3),
        (StreamKind::FileTransfer, 4),
        (StreamKind::Tunnel, 5),
        (StreamKind::Terminal, 6),
        (StreamKind::Audio, 7),
        (StreamKind::Video, 8),
    ] {
        assert_eq!(stream_preamble(k), [b]);
        assert_eq!(StreamKind::from_u8(b), Some(k));
    }
    assert_eq!(StreamKind::from_u8(0), None);
    assert_eq!(StreamKind::from_u8(9), None);
}

#[test]
fn media_header_golden_layout() {
    let h = MediaHeader {
        kind: MediaKind::Video,
        flags: flags::KEYFRAME | flags::END_OF_FRAME,
        stream_id: 2,
        block_index: 1,
        frame_id: 0x0403_0201,
        shard_index: 0x0605,
        data_shards: 10,
        parity_shards: 2,
        block_count: 3,
        codec: WireCodec::H265,
        shard_len: 0x0807,
        block_len: 0x0C0B_0A09 & 0x0000_FFFF, // must fit 10 * shard_len
        capture_ts: 0x1413_1211,
    };
    // Distinct byte values per field (not a semantically valid header; layout only).
    let b = h.to_bytes();
    assert_eq!(b.len(), MEDIA_HEADER_LEN);
    assert_eq!(b[0], 0x11); // version 1, kind video
    assert_eq!(b[1], flags::KEYFRAME | flags::END_OF_FRAME);
    assert_eq!(&b[2..4], &[2, 1]);
    assert_eq!(&b[4..8], &[1, 2, 3, 4]);
    assert_eq!(&b[8..10], &[5, 6]);
    assert_eq!(&b[10..13], &[10, 2, 3]);
    assert_eq!(b[13], WireCodec::H265 as u8);
    assert_eq!(&b[14..16], &[7, 8]);
    assert_eq!(&b[16..20], &[9, 0x0A, 0, 0]);
    assert_eq!(&b[20..24], &[0x11, 0x12, 0x13, 0x14]);
    let (p, payload) = MediaHeader::parse(&b).unwrap();
    assert_eq!(p, h);
    assert!(payload.is_empty());
}

#[test]
fn media_header_rejects_future_version_and_garbage() {
    let mut b = [0u8; MEDIA_HEADER_LEN];
    b[0] = 0x21;
    assert!(matches!(MediaHeader::parse(&b), Err(ProtocolError::BadVersion(2))));
    assert!(MediaHeader::parse(&b[..MEDIA_HEADER_LEN - 1]).is_err());
    // Fuzz-ish: random bytes never panic.
    let mut x = 0x1234_5678_9ABC_DEF0u64;
    for _ in 0..100_000 {
        let mut d = [0u8; 40];
        for c in d.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            c.copy_from_slice(&x.to_le_bytes());
        }
        d[0] = (d[0] & 0x0F) | 0x10;
        if let Ok((h, _)) = MediaHeader::parse(&d) {
            let _ = h.validate();
        }
    }
}
