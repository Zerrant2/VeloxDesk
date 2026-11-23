//! Protocol compatibility: old/new envelopes, golden wire bytes, framing.
//!
//! "Old" and "future" peers are modelled with locally defined prost structs that share
//! tags with the frozen v1 messages in `veloxdesk_protocol::control`.

use bytes::{Bytes, BytesMut};
use prost::Message;
use veloxdesk_protocol::control::{
    envelope::Msg, AudioConfig, AuthChallenge, AuthResponse, AuthResult, BitrateUpdate, ChatAck, ChatMessage,
    ClipboardData, Close, CursorPosition, CursorShape, DisplayList, Envelope, FileAck, FileChunk, FileListing,
    FileRequest, FrameTimingReport, Hello, InputEvent, KeyframeRequest, Nack, Permissions, Ping, Pong, PrivacyMode,
    PrivacyModeResult, ReceiverReport, Rumble, SessionConfig, SwitchDisplay, SystemPowerAction, SystemPowerRequest,
    SystemPowerResult, TerminalData, TerminalExit, TerminalOpen, TerminalResize, TunnelOpen, TunnelResult,
    VideoCodecCap, VideoConfig,
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

// ───────────────────── v0.4.0 / v0.5.0 compatibility ─────────────────────

/// Decode the leading protobuf field tag and wire type from an encoded message.
fn decode_first_field_key(bytes: &[u8]) -> (u32, u8) {
    assert!(!bytes.is_empty(), "encoded protobuf message must not be empty");
    let mut key: u64 = 0;
    let mut shift = 0u32;
    for &b in bytes {
        key |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        assert!(shift < 64, "malformed varint key");
    }
    ((key >> 3) as u32, (key & 0x07) as u8)
}

/// Model of a v0.4.0 `Permissions` struct (only tags 1..=9 existed).
#[derive(Clone, PartialEq, Message)]
struct PermissionsV040 {
    #[prost(bool, tag = "1")]
    view: bool,
    #[prost(bool, tag = "2")]
    input: bool,
    #[prost(bool, tag = "3")]
    clipboard: bool,
    #[prost(bool, tag = "4")]
    file_transfer: bool,
    #[prost(bool, tag = "5")]
    audio: bool,
    #[prost(bool, tag = "6")]
    tunnel: bool,
    #[prost(bool, tag = "7")]
    terminal: bool,
    #[prost(bool, tag = "8")]
    restart: bool,
    #[prost(bool, tag = "9")]
    privacy_mode: bool,
}

/// Model of a v0.4.0 `Envelope` decoder that knows tags 1..=45, NOT v0.5.0 tags 46..=49.
#[derive(Clone, PartialEq, Message)]
struct EnvelopeV040 {
    #[prost(oneof = "env_v040::Msg", tags = "1, 6, 43, 45")]
    msg: Option<env_v040::Msg>,
}
mod env_v040 {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Msg {
        #[prost(message, tag = "1")]
        Hello(veloxdesk_protocol::control::Hello),
        #[prost(message, tag = "6")]
        Ping(veloxdesk_protocol::control::Ping),
        #[prost(message, tag = "43")]
        Permissions(super::PermissionsV040),
        #[prost(message, tag = "45")]
        DisplayList(veloxdesk_protocol::control::DisplayList),
    }
}

/// Verify exact wire tag preservation for EVERY v0.4.0 Envelope variant (tags 1..=45).
/// If any tag is accidentally renumbered or shifted, this test fails immediately.
#[test]
fn v040_envelope_wire_tags_1_through_45_preserved() {
    let cases: Vec<(u32, Msg)> = vec![
        // session 1..=8
        (1, Msg::Hello(Hello { protocol_version: 1, ..Default::default() })),
        (2, Msg::AuthChallenge(AuthChallenge { nonce: vec![1, 2], ..Default::default() })),
        (3, Msg::AuthResponse(AuthResponse { method: 1, proof: vec![3] })),
        (4, Msg::AuthResult(AuthResult { ok: true, session_id: 1, ..Default::default() })),
        (5, Msg::SessionConfig(SessionConfig { active_display: 0, host_name: "h".into(), ..Default::default() })),
        (6, Msg::Ping(Ping { seq: 1, sent_us: 2 })),
        (7, Msg::Pong(Pong { seq: 1, ping_sent_us: 2, peer_recv_us: 3, peer_send_us: 4 })),
        (8, Msg::Close(Close { code: 0, reason: "bye".into() })),
        // video / ABR 10..=16
        (10, Msg::KeyframeRequest(KeyframeRequest { stream_id: 0, last_good_frame_id: 9 })),
        (11, Msg::Nack(Nack { stream_id: 0, frame_id: 1, block_index: 0, shard_indices: vec![0] })),
        (12, Msg::ReceiverReport(ReceiverReport { packets_received: 10, ..Default::default() })),
        (13, Msg::BitrateUpdate(BitrateUpdate { stream_id: 0, bitrate_kbps: 4000, fps: 60, fec_parity_percent: 10 })),
        (14, Msg::FrameTiming(FrameTimingReport { frame_id: 1, encode_us: 500, ..Default::default() })),
        (15, Msg::SwitchDisplay(SwitchDisplay { index: 1 })),
        (16, Msg::VideoConfig(VideoConfig { codec: 1, width: 1920, height: 1080, fps: 60, ..Default::default() })),
        // input 20..=23
        (20, Msg::Input(InputEvent { ts_us: 42, event: None })),
        (21, Msg::Rumble(Rumble { pad_id: 0, low_freq: 1000, high_freq: 2000 })),
        (22, Msg::CursorShape(CursorShape { id: 1, width: 1, height: 1, hotspot_x: 0, hotspot_y: 0, rgba: Bytes::from_static(&[0, 0, 0, 255]) })),
        (23, Msg::CursorPosition(CursorPosition { x: 10, y: 20, visible: true, shape_id: 1 })),
        // features 30..=45
        (30, Msg::Clipboard(ClipboardData { format: 0, data: Bytes::from_static(b"hi"), compressed: false })),
        (31, Msg::FileRequest(FileRequest { id: 1, op: 0, path: "/".into(), ..Default::default() })),
        (32, Msg::FileListing(FileListing { id: 1, path: "/".into(), entries: vec![] })),
        (33, Msg::FileChunk(FileChunk { id: 1, offset: 0, data: Bytes::from_static(b"abc"), compressed: false, last: true })),
        (34, Msg::FileAck(FileAck { id: 1, offset: 3, error: String::new(), digest: vec![] })),
        (35, Msg::TunnelOpen(TunnelOpen { tunnel_id: 1, remote_host: "127.0.0.1".into(), remote_port: 8080 })),
        (36, Msg::TunnelResult(TunnelResult { tunnel_id: 1, ok: true, error: String::new() })),
        (37, Msg::TerminalOpen(TerminalOpen { term_id: 1, cols: 80, rows: 24, shell: String::new() })),
        (38, Msg::TerminalData(TerminalData { term_id: 1, data: Bytes::from_static(b"ls\n") })),
        (39, Msg::TerminalResize(TerminalResize { term_id: 1, cols: 120, rows: 40 })),
        (40, Msg::TerminalExit(TerminalExit { term_id: 1, code: 0 })),
        (41, Msg::PrivacyMode(PrivacyMode { enable: true })),
        (42, Msg::PrivacyModeResult(PrivacyModeResult { enabled: true, error: String::new() })),
        (43, Msg::Permissions(Permissions::interactive_default())),
        (44, Msg::AudioConfig(AudioConfig { codec: 16, sample_rate: 48_000, channels: 2, frame_duration_us: 5_000, bitrate_kbps: 64 })),
        (45, Msg::DisplayList(DisplayList { displays: vec![] })),
    ];

    assert_eq!(cases.len(), 35, "expected all 35 v0.4.0 Envelope variants");
    for (expected_tag, msg) in cases {
        let env: Envelope = msg.into();
        let bytes = env.encode_to_vec();
        let (wire_tag, wire_type) = decode_first_field_key(&bytes);
        assert_eq!(wire_type, 2, "Envelope oneof field {expected_tag} must be length-delimited (wire type 2)");
        assert_eq!(wire_tag, expected_tag, "v0.4.0 Envelope tag {expected_tag} shifted to {wire_tag}");
        assert_eq!(
            Envelope::decode(bytes.as_slice()).expect("v0.4.0 message must decode cleanly"),
            env,
            "v0.4.0 Envelope tag {expected_tag} roundtrip mismatch"
        );
    }
}

/// Verify v0.5.0 tags 46..=49 exact wire tags, golden key prefixes, framing roundtrip,
/// and clean forward-compatibility (`msg: None`) on older v0.4.0 and v0 decoders.
#[test]
fn v050_envelope_tags_46_through_49_roundtrip_and_older_decoder_compat() {
    let cases: Vec<(u32, [u8; 2], Msg)> = vec![
        (
            46,
            [0xF2, 0x02], // (46 << 3) | 2 = 370
            Msg::ChatMessage(ChatMessage {
                id: 42,
                sender_name: "Alice".into(),
                text: "Hello from v0.5.0 / Привет!".into(),
                ts_ms: 1_735_689_600_123,
            }),
        ),
        (
            47,
            [0xFA, 0x02], // (47 << 3) | 2 = 378
            Msg::ChatAck(ChatAck { id: 42 }),
        ),
        (
            48,
            [0x82, 0x03], // (48 << 3) | 2 = 386
            Msg::SystemPowerRequest(SystemPowerRequest {
                action: SystemPowerAction::RebootSafeMode as i32,
                reason: "Maintenance reboot".into(),
                delay_secs: 5,
            }),
        ),
        (
            49,
            [0x8A, 0x03], // (49 << 3) | 2 = 394
            Msg::SystemPowerResult(SystemPowerResult {
                action: SystemPowerAction::RebootSafeMode as i32,
                ok: true,
                error: String::new(),
            }),
        ),
    ];

    let mut framed_wire = BytesMut::new();
    let mut expected_envelopes = Vec::with_capacity(cases.len());

    for (expected_tag, key_prefix, msg) in cases {
        let env: Envelope = msg.into();
        let bytes = env.encode_to_vec();

        // Exact wire tag and golden varint key prefix.
        let (wire_tag, wire_type) = decode_first_field_key(&bytes);
        assert_eq!(wire_type, 2, "tag {expected_tag} must use wire type 2");
        assert_eq!(wire_tag, expected_tag, "tag mismatch for v0.5.0 message");
        assert_eq!(&bytes[..2], &key_prefix, "golden key prefix mismatch for tag {expected_tag}");

        // Current v0.5.0 decoder roundtrips identically.
        assert_eq!(Envelope::decode(bytes.as_slice()).unwrap(), env);

        // Older v0.4.0 and v0 decoders ignore unknown oneof tags cleanly (`msg: None`).
        let old_v040 = EnvelopeV040::decode(bytes.as_slice())
            .expect("v0.4.0 decoder must not fail on v0.5.0 Envelope tags 46..=49");
        assert!(old_v040.msg.is_none(), "v0.4.0 decoder must decode tag {expected_tag} as msg: None");

        let old_v0 = EnvelopeV0::decode(bytes.as_slice())
            .expect("v0 decoder must not fail on v0.5.0 Envelope tags 46..=49");
        assert!(old_v0.msg.is_none(), "v0 decoder must decode tag {expected_tag} as msg: None");

        encode_frame(&env, &mut framed_wire).unwrap();
        expected_envelopes.push(env);
    }

    // All 4 v0.5.0 messages also roundtrip cleanly through length-prefixed stream framing.
    let mut decoded = Vec::new();
    while let Some(env) = decode_frame::<Envelope>(&mut framed_wire).unwrap() {
        decoded.push(env);
    }
    assert_eq!(decoded, expected_envelopes);
    assert!(framed_wire.is_empty());

    // All SystemPowerAction enum wire values are stable.
    assert_eq!(SystemPowerAction::Reboot as i32, 0);
    assert_eq!(SystemPowerAction::RebootSafeMode as i32, 1);
    assert_eq!(SystemPowerAction::LockScreen as i32, 2);
    assert_eq!(SystemPowerAction::Sleep as i32, 3);
    for action in [
        SystemPowerAction::Reboot,
        SystemPowerAction::RebootSafeMode,
        SystemPowerAction::LockScreen,
        SystemPowerAction::Sleep,
    ] {
        let req: Envelope = Msg::SystemPowerRequest(SystemPowerRequest {
            action: action as i32,
            reason: "test".into(),
            delay_secs: 3,
        })
        .into();
        assert_eq!(Envelope::decode(req.encode_to_vec().as_slice()).unwrap(), req);

        let res: Envelope = Msg::SystemPowerResult(SystemPowerResult {
            action: action as i32,
            ok: false,
            error: "denied".into(),
        })
        .into();
        assert_eq!(Envelope::decode(res.encode_to_vec().as_slice()).unwrap(), res);
    }
}

/// Verify `Permissions` backward compatibility (v0.4.0 tags 1..=9 -> v0.5.0)
/// and forward compatibility (v0.5.0 tags 1..=11 -> v0.4.0), plus exact tag numbers 10 & 11.
#[test]
fn permissions_v040_and_v050_backward_and_forward_compat() {
    // 1. v0.4.0 bytes (tags 1..=9 only) decode cleanly in v0.5.0 with allow_chat/allow_power = false.
    let old_perms = PermissionsV040 {
        view: true,
        input: true,
        clipboard: true,
        file_transfer: false,
        audio: true,
        tunnel: false,
        terminal: true,
        restart: false,
        privacy_mode: true,
    };
    let old_bytes = old_perms.encode_to_vec();
    let decoded_v050 = Permissions::decode(old_bytes.as_slice())
        .expect("v0.5.0 Permissions must decode v0.4.0 bytes cleanly");
    assert!(decoded_v050.view);
    assert!(decoded_v050.input);
    assert!(decoded_v050.clipboard);
    assert!(!decoded_v050.file_transfer);
    assert!(decoded_v050.audio);
    assert!(!decoded_v050.tunnel);
    assert!(decoded_v050.terminal);
    assert!(!decoded_v050.restart);
    assert!(decoded_v050.privacy_mode);
    assert!(!decoded_v050.allow_chat, "missing tag 10 in v0.4.0 bytes defaults to false");
    assert!(!decoded_v050.allow_power, "missing tag 11 in v0.4.0 bytes defaults to false");

    // 2. v0.5.0 Permissions (tags 1..=11) roundtrip cleanly and decode on a v0.4.0 peer ignoring tags 10..=11.
    let full = Permissions::all();
    assert!(full.allow_chat && full.allow_power);
    let full_bytes = full.encode_to_vec();
    assert_eq!(Permissions::decode(full_bytes.as_slice()).unwrap(), full);

    let decoded_on_v040 = PermissionsV040::decode(full_bytes.as_slice())
        .expect("v0.4.0 peer must ignore unknown Permissions tags 10..=11");
    assert!(
        decoded_on_v040.view
            && decoded_on_v040.input
            && decoded_on_v040.clipboard
            && decoded_on_v040.file_transfer
            && decoded_on_v040.audio
            && decoded_on_v040.tunnel
            && decoded_on_v040.terminal
            && decoded_on_v040.restart
            && decoded_on_v040.privacy_mode
    );

    // 3. Interactive default has chat enabled and power disabled.
    let inter = Permissions::interactive_default();
    assert!(inter.allow_chat);
    assert!(!inter.allow_power);
    assert_eq!(Permissions::decode(inter.encode_to_vec().as_slice()).unwrap(), inter);

    // 4. Freeze exact wire tags 10 (`allow_chat`) and 11 (`allow_power`).
    let only_chat = Permissions { allow_chat: true, ..Default::default() }.encode_to_vec();
    assert_eq!(only_chat, vec![10 << 3, 0x01], "allow_chat must be protobuf tag 10");

    let only_power = Permissions { allow_power: true, ..Default::default() }.encode_to_vec();
    assert_eq!(only_power, vec![11 << 3, 0x01], "allow_power must be protobuf tag 11");

    // 5. Envelope(Permissions) encoded by v0.5.0 also decodes on EnvelopeV040.
    let env_v050: Envelope = Msg::Permissions(inter.clone()).into();
    let env_v040 = EnvelopeV040::decode(env_v050.encode_to_vec().as_slice()).unwrap();
    match env_v040.msg {
        Some(env_v040::Msg::Permissions(p)) => {
            assert_eq!(p.view, inter.view);
            assert_eq!(p.input, inter.input);
            assert_eq!(p.privacy_mode, inter.privacy_mode);
            assert_eq!(p.restart, inter.restart);
        }
        other => panic!("expected v0.4.0 Permissions inside Envelope, got {other:?}"),
    }
}
