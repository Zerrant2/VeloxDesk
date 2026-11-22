//! Low-latency media datagrams for audio (`MediaKind::Audio`) and gamepad state
//! snapshots (`MediaKind::GamepadState`) over QUIC RFC 9221 datagrams (ARCHITECTURE §4.2, M3).

use std::sync::Arc;

use bytes::{BufMut, Bytes, BytesMut};
use prost::Message;

use veloxdesk_core_traits::audio::{AudioCodecId, EncodedAudio};
use veloxdesk_core_traits::{CoreError, Result, Timestamp, TransportChannel};
use veloxdesk_protocol::control::GamepadState;
use veloxdesk_protocol::media::{MEDIA_HEADER_LEN, MediaHeader, MediaKind, WireCodec, flags};

/// 4-byte LE duration prefix inside `MediaKind::Audio` datagram payloads.
const AUDIO_META_LEN: usize = 4;

/// Peek the [`MediaKind`] of a raw media datagram without full parsing.
#[inline]
pub fn datagram_kind(dgram: &[u8]) -> Option<MediaKind> {
    let &first = dgram.first()?;
    if (first >> 4) != veloxdesk_protocol::media::MEDIA_VERSION {
        return None;
    }
    MediaKind::from_u8(first & 0x0F).ok()
}

/// Map a control-plane audio codec id to its wire byte.
#[inline]
pub fn wire_audio_codec(codec: AudioCodecId) -> WireCodec {
    match codec {
        AudioCodecId::Opus => WireCodec::Opus,
        AudioCodecId::Pcm => WireCodec::Pcm,
        AudioCodecId::Unspecified => WireCodec::Unknown,
    }
}

/// Map a wire codec byte back to the audio codec enum.
#[inline]
pub fn audio_codec_id(codec: WireCodec) -> AudioCodecId {
    match codec {
        WireCodec::Opus => AudioCodecId::Opus,
        WireCodec::Pcm => AudioCodecId::Pcm,
        _ => AudioCodecId::Unspecified,
    }
}

/// Low-latency audio datagram sender (`MediaKind::Audio`, 24-byte [`MediaHeader`]).
///
/// Convention: `stream_id = 0` for host system audio, `stream_id = 1` for viewer microphone.
pub struct AudioSender {
    ch: Arc<dyn TransportChannel>,
    stream_id: u8,
    redundant: bool,
    prev_dgram: Option<Bytes>,
}

impl AudioSender {
    pub fn new(ch: Arc<dyn TransportChannel>, stream_id: u8) -> Self {
        Self { ch, stream_id, redundant: false, prev_dgram: None }
    }

    /// Enable or disable redundant duplicate transmission (recommended under >2% loss).
    ///
    /// When enabled, each `send` transmits the current packet plus a time-shifted copy
    /// of the previous packet (or a duplicate of the first packet), surviving single-packet
    /// and short burst drops with zero FEC latency overhead.
    pub fn set_redundant(&mut self, enabled: bool) {
        self.redundant = enabled;
        if !enabled {
            self.prev_dgram = None;
        }
    }

    pub fn is_redundant(&self) -> bool {
        self.redundant
    }

    pub fn stream_id(&self) -> u8 {
        self.stream_id
    }

    /// Encode one [`EncodedAudio`] packet into a media datagram and queue it on the channel.
    pub fn send(&mut self, pkt: &EncodedAudio) -> Result<()> {
        let dgram = encode_audio_datagram(self.stream_id, pkt)?;
        if let Some(max) = self.ch.max_datagram_size()
            && dgram.len() > max
        {
            return Err(CoreError::invalid("audio packet exceeds max_datagram_size"));
        }
        self.ch.send_datagram(dgram.clone())?;
        if self.redundant {
            let extra = self.prev_dgram.replace(dgram.clone()).unwrap_or(dgram);
            // Best-effort duplicate: do not fail the call if the redundant copy hits a full queue.
            match self.ch.send_datagram(extra) {
                Ok(()) => {}
                Err(CoreError::Closed) => return Err(CoreError::Closed),
                Err(CoreError::Timeout) => return Err(CoreError::Timeout),
                Err(_) => {}
            }
        }
        Ok(())
    }
}

/// Encode an [`EncodedAudio`] packet into a standalone `MediaKind::Audio` datagram.
pub fn encode_audio_datagram(stream_id: u8, pkt: &EncodedAudio) -> Result<Bytes> {
    let payload_len = AUDIO_META_LEN + pkt.data.len();
    if payload_len > u16::MAX as usize {
        return Err(CoreError::invalid("audio packet payload too large for MediaHeader"));
    }
    let hdr = MediaHeader {
        kind: MediaKind::Audio,
        flags: flags::END_OF_FRAME,
        stream_id,
        block_index: 0,
        frame_id: pkt.seq,
        shard_index: 0,
        data_shards: 1,
        parity_shards: 0,
        block_count: 1,
        codec: wire_audio_codec(pkt.codec),
        shard_len: payload_len as u16,
        block_len: payload_len as u32,
        capture_ts: pkt.capture_ts.low32(),
    };
    let mut buf = BytesMut::with_capacity(MEDIA_HEADER_LEN + payload_len);
    buf.put_slice(&hdr.to_bytes());
    buf.put_u32_le(pkt.duration_us);
    buf.put_slice(&pkt.data);
    Ok(buf.freeze())
}

/// Stateless parser for incoming `MediaKind::Audio` datagrams.
pub struct AudioReceiver;

impl AudioReceiver {
    /// Parse a raw `MediaKind::Audio` datagram into `(stream_id, EncodedAudio)`.
    pub fn parse_datagram(dgram: &[u8]) -> Option<(u8, EncodedAudio)> {
        Self::parse_datagram_with_ref(dgram, None)
    }

    /// Parse a raw `MediaKind::Audio` datagram, expanding the 32-bit wire timestamp against
    /// an optional 64-bit host reference timestamp.
    pub fn parse_datagram_with_ref(dgram: &[u8], host_ref: Option<Timestamp>) -> Option<(u8, EncodedAudio)> {
        let (h, payload) = MediaHeader::parse(dgram).ok()?;
        if h.kind != MediaKind::Audio || h.validate().is_err() {
            return None;
        }
        let block_len = h.block_len as usize;
        if block_len < AUDIO_META_LEN || payload.len() < block_len {
            return None;
        }
        let duration_us = u32::from_le_bytes(payload[..AUDIO_META_LEN].try_into().ok()?);
        let audio_bytes = Bytes::copy_from_slice(&payload[AUDIO_META_LEN..block_len]);
        let capture_ts = match host_ref {
            Some(r) => Timestamp::from_low32(h.capture_ts, r),
            None => Timestamp(h.capture_ts as u64),
        };
        Some((
            h.stream_id,
            EncodedAudio {
                data: audio_bytes,
                codec: audio_codec_id(h.codec),
                seq: h.frame_id,
                capture_ts,
                duration_us,
            },
        ))
    }
}

/// Encode an idempotent [`GamepadState`] snapshot as a `MediaKind::GamepadState` datagram.
pub fn encode_gamepad_datagram(state: &GamepadState) -> Result<Bytes> {
    let payload = state.encode_to_vec();
    if payload.len() > u16::MAX as usize {
        return Err(CoreError::invalid("gamepad state payload too large"));
    }
    let hdr = MediaHeader {
        kind: MediaKind::GamepadState,
        flags: flags::END_OF_FRAME,
        stream_id: (state.pad_id & 0xff) as u8,
        block_index: 0,
        frame_id: state.seq,
        shard_index: 0,
        data_shards: 1,
        parity_shards: 0,
        block_count: 1,
        codec: WireCodec::Unknown,
        shard_len: payload.len() as u16,
        block_len: payload.len() as u32,
        capture_ts: 0,
    };
    let mut buf = BytesMut::with_capacity(MEDIA_HEADER_LEN + payload.len());
    buf.put_slice(&hdr.to_bytes());
    buf.put_slice(&payload);
    Ok(buf.freeze())
}

/// Decode a `MediaKind::GamepadState` datagram back into [`GamepadState`].
pub fn decode_gamepad_datagram(dgram: &[u8]) -> Option<GamepadState> {
    let (h, payload) = MediaHeader::parse(dgram).ok()?;
    if h.kind != MediaKind::GamepadState || h.validate().is_err() {
        return None;
    }
    let len = h.block_len as usize;
    if payload.len() < len {
        return None;
    }
    GamepadState::decode(&payload[..len]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex;
    use veloxdesk_core_traits::transport::{RecvStream, SendStream, TransportStats};
    use veloxdesk_protocol::StreamKind;

    #[derive(Default)]
    struct MockCh {
        sent: Mutex<Vec<Bytes>>,
    }

    #[async_trait]
    impl TransportChannel for MockCh {
        fn max_datagram_size(&self) -> Option<usize> {
            Some(1200)
        }
        fn send_datagram(&self, data: Bytes) -> Result<()> {
            self.sent.lock().push(data);
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

    #[test]
    fn audio_sender_receiver_roundtrip_and_redundancy() {
        let ch = Arc::new(MockCh::default());
        let mut tx = AudioSender::new(ch.clone(), 0);

        let pkt1 = EncodedAudio {
            data: Bytes::from_static(b"opus-frame-1"),
            codec: AudioCodecId::Opus,
            seq: 42,
            capture_ts: Timestamp(123_456),
            duration_us: 5_000,
        };
        tx.send(&pkt1).unwrap();
        assert_eq!(ch.sent.lock().len(), 1);

        let raw = ch.sent.lock().pop().unwrap();
        assert_eq!(datagram_kind(&raw), Some(MediaKind::Audio));
        let (sid, parsed) = AudioReceiver::parse_datagram(&raw).unwrap();
        assert_eq!(sid, 0);
        assert_eq!(parsed.codec, AudioCodecId::Opus);
        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.capture_ts, Timestamp(123_456));
        assert_eq!(parsed.duration_us, 5_000);
        assert_eq!(parsed.data, pkt1.data);

        // Redundant mode sends 2 datagrams per send, time-shifted by 1 packet.
        tx.set_redundant(true);
        let pkt2 = EncodedAudio { seq: 43, data: Bytes::from_static(b"opus-frame-2"), ..pkt1.clone() };
        let pkt3 = EncodedAudio { seq: 44, data: Bytes::from_static(b"opus-frame-3"), ..pkt1 };
        tx.send(&pkt2).unwrap();
        tx.send(&pkt3).unwrap();
        let sent = ch.sent.lock();
        assert_eq!(sent.len(), 4);
        let seqs: Vec<u32> = sent.iter().map(|d| AudioReceiver::parse_datagram(d).unwrap().1.seq).collect();
        assert_eq!(seqs, vec![43, 43, 44, 43]);
    }

    #[test]
    fn audio_mic_track_and_invalid_rejected() {
        let pkt = EncodedAudio {
            data: Bytes::from_static(b"pcm-samples"),
            codec: AudioCodecId::Pcm,
            seq: 7,
            capture_ts: Timestamp(9_000),
            duration_us: 2_500,
        };
        let dgram = encode_audio_datagram(1, &pkt).unwrap();
        let (sid, parsed) = AudioReceiver::parse_datagram(&dgram).unwrap();
        assert_eq!(sid, 1);
        assert_eq!(parsed.codec, AudioCodecId::Pcm);
        assert_eq!(parsed.duration_us, 2_500);
        assert!(AudioReceiver::parse_datagram(&dgram[..10]).is_none());
        assert!(decode_gamepad_datagram(&dgram).is_none());
    }

    #[test]
    fn gamepad_datagram_roundtrip() {
        let state = GamepadState {
            pad_id: 2,
            connected: true,
            buttons: 0x1001,
            left_trigger: 128,
            right_trigger: 255,
            left_x: -16_384,
            left_y: 32_767,
            right_x: 500,
            right_y: -500,
            seq: 999,
        };
        let dgram = encode_gamepad_datagram(&state).unwrap();
        assert_eq!(datagram_kind(&dgram), Some(MediaKind::GamepadState));
        let decoded = decode_gamepad_datagram(&dgram).unwrap();
        assert_eq!(decoded, state);
        assert!(AudioReceiver::parse_datagram(&dgram).is_none());
    }
}
