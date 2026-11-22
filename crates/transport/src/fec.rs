//! Reed-Solomon FEC packetization of video frames (ARCHITECTURE ADR-005, §4.2, §6).
//!
//! A frame of `N` bytes is split into `block_count ≤ 255` FEC blocks; every block has
//! `k` data shards of `shard_len` bytes (the last data shard of the frame is sent
//! unpadded, the receiver zero-pads it) followed by `m = ceil(k · parity%)` parity
//! shards (systematic RS over GF(2^8), `reed-solomon-simd`). `24 + shard_len` never
//! exceeds the channel's `max_datagram_size`, and `k + m ≤ 255`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use bytes::{BufMut, Bytes, BytesMut};
use reed_solomon_simd::ReedSolomonEncoder;

use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{CoreError, Result, TransportChannel};
use veloxdesk_protocol::control::Nack;
use veloxdesk_protocol::media::{MAX_SHARDS_PER_BLOCK, MEDIA_HEADER_LEN, MediaHeader, MediaKind, WireCodec, flags};

/// Smallest shard payload we accept (smaller MTUs make FEC pointless).
pub const MIN_SHARD_LEN: usize = 64;
/// Default parity ratio (ADR-005: 10–20 %).
pub const DEFAULT_PARITY_PERCENT: u8 = 15;

/// Parity shards for `k` data shards at `pct` %.
#[inline]
pub fn parity_for(k: usize, pct: u8) -> usize {
    if pct == 0 { 0 } else { (k * pct as usize).div_ceil(100) }
}

/// Largest `k` such that `k + parity_for(k) ≤ 255`.
pub fn max_data_shards(pct: u8) -> usize {
    let mut k = MAX_SHARDS_PER_BLOCK;
    while k > 1 && k + parity_for(k, pct) > MAX_SHARDS_PER_BLOCK {
        k -= 1;
    }
    k
}

/// Map an encoder codec to its wire id.
pub fn wire_codec(c: VideoCodec) -> WireCodec {
    match c {
        VideoCodec::H264 => WireCodec::H264,
        VideoCodec::H265 => WireCodec::H265,
        VideoCodec::Av1 => WireCodec::Av1,
        VideoCodec::Vp9 => WireCodec::Vp9,
        VideoCodec::Unspecified => WireCodec::Unknown,
    }
}

/// Map a wire codec id back to the control-plane enum.
pub fn video_codec(c: WireCodec) -> VideoCodec {
    match c {
        WireCodec::H264 => VideoCodec::H264,
        WireCodec::H265 => VideoCodec::H265,
        WireCodec::Av1 => VideoCodec::Av1,
        WireCodec::Vp9 => VideoCodec::Vp9,
        _ => VideoCodec::Unspecified,
    }
}

/// Block/shard geometry of one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameLayout {
    pub frame_len: usize,
    pub block_count: usize,
    /// Data shards of every full block (the last block may have fewer).
    pub k: usize,
    pub shard_len: usize,
    pub parity_percent: u8,
}

impl FrameLayout {
    /// Plan the layout for `frame_len` bytes with `max_payload` bytes per datagram
    /// payload (i.e. `max_datagram_size - 24`).
    pub fn plan(frame_len: usize, max_payload: usize, parity_percent: u8) -> Result<Self> {
        let max_shard = max_payload.min(u16::MAX as usize - 1) & !1;
        if max_shard < MIN_SHARD_LEN {
            return Err(CoreError::invalid("datagram size too small for FEC shards"));
        }
        let kmax = max_data_shards(parity_percent);
        let d = frame_len.div_ceil(max_shard).max(1);
        let blocks = d.div_ceil(kmax);
        let k = d.div_ceil(blocks);
        let shard_len = (frame_len.div_ceil(blocks * k).max(1) + 1) & !1;
        let block_count = frame_len.div_ceil(k * shard_len).max(1);
        if block_count > MAX_SHARDS_PER_BLOCK {
            return Err(CoreError::invalid("frame too large (> 255 FEC blocks)"));
        }
        Ok(Self { frame_len, block_count, k, shard_len, parity_percent })
    }

    /// `(start, len)` of block `b` inside the frame.
    pub fn block_range(&self, b: usize) -> (usize, usize) {
        let full = self.k * self.shard_len;
        let start = (b * full).min(self.frame_len);
        (start, full.min(self.frame_len - start))
    }

    /// Data shards of block `b`.
    pub fn block_k(&self, b: usize) -> usize {
        self.block_range(b).1.div_ceil(self.shard_len).max(1)
    }

    pub fn block_m(&self, b: usize) -> usize {
        parity_for(self.block_k(b), self.parity_percent)
    }
}

/// Per-frame packetization result.
#[derive(Debug, Clone, Copy, Default)]
pub struct PacketizeInfo {
    pub blocks: u32,
    pub data_shards: u32,
    pub parity_shards: u32,
    pub shard_len: u32,
    /// Reed-Solomon encode time for the whole frame (µs).
    pub fec_encode_us: u32,
}

/// Splits frames into FEC-protected datagrams (no I/O; reusable buffers).
pub struct Packetizer {
    stream_id: u8,
    parity_percent: u8,
    encoder: Option<ReedSolomonEncoder>,
    pad: Vec<u8>,
}

impl Packetizer {
    pub fn new(stream_id: u8) -> Self {
        Self { stream_id, parity_percent: DEFAULT_PARITY_PERCENT, encoder: None, pad: Vec::new() }
    }

    pub fn set_parity_percent(&mut self, pct: u8) {
        self.parity_percent = pct.min(100);
    }

    pub fn parity_percent(&self) -> u8 {
        self.parity_percent
    }

    /// Append the datagrams of `pkt` to `out` (block by block: data shards, then parity).
    /// Returns the layout and stats; `block_starts[b]` (pushed into `starts`) is the index
    /// in `out` (relative to its length on entry) of the first datagram of block `b`.
    pub fn packetize(
        &mut self,
        pkt: &EncodedPacket,
        max_datagram: usize,
        out: &mut Vec<Bytes>,
        starts: &mut Vec<(usize, usize, usize)>,
    ) -> Result<(FrameLayout, PacketizeInfo)> {
        let data = &pkt.data[..];
        let layout = FrameLayout::plan(data.len(), max_datagram.saturating_sub(MEDIA_HEADER_LEN), self.parity_percent)?;
        let sl = layout.shard_len;
        let mut info = PacketizeInfo { blocks: layout.block_count as u32, shard_len: sl as u32, ..Default::default() };
        let mut base_flags = 0u8;
        if pkt.keyframe {
            base_flags |= flags::KEYFRAME;
        }
        if pkt.recovery_point {
            base_flags |= flags::RECOVERY_POINT;
        }
        let codec = wire_codec(pkt.codec);
        let capture_ts = pkt.capture_ts.low32();
        let first = out.len();
        starts.clear();

        for b in 0..layout.block_count {
            let (start, len) = layout.block_range(b);
            let k = layout.block_k(b);
            let m = layout.block_m(b);
            starts.push((out.len() - first, k, m));
            let mut hdr = MediaHeader {
                kind: MediaKind::Video,
                flags: base_flags,
                stream_id: self.stream_id,
                block_index: b as u8,
                frame_id: pkt.frame_id,
                shard_index: 0,
                data_shards: k as u8,
                parity_shards: m as u8,
                block_count: layout.block_count as u8,
                codec,
                shard_len: sl as u16,
                block_len: len as u32,
                capture_ts,
            };
            let block = &data[start..start + len];
            // Data shards (last one unpadded on the wire).
            for i in 0..k {
                let s = (i * sl).min(len);
                let e = ((i + 1) * sl).min(len);
                hdr.shard_index = i as u16;
                out.push(make_datagram(&hdr, &block[s..e]));
            }
            info.data_shards += k as u32;
            if m == 0 {
                continue;
            }
            let t0 = Instant::now();
            let enc = match self.encoder.as_mut() {
                Some(enc) => {
                    enc.reset(k, m, sl).map_err(CoreError::other)?;
                    enc
                }
                None => self.encoder.insert(ReedSolomonEncoder::new(k, m, sl).map_err(CoreError::other)?),
            };
            for i in 0..k {
                let s = i * sl;
                if s + sl <= len {
                    enc.add_original_shard(&block[s..s + sl]).map_err(CoreError::other)?;
                } else {
                    self.pad.clear();
                    self.pad.extend_from_slice(&block[s.min(len)..]);
                    self.pad.resize(sl, 0);
                    enc.add_original_shard(&self.pad).map_err(CoreError::other)?;
                }
            }
            let result = enc.encode().map_err(CoreError::other)?;
            hdr.flags = base_flags | flags::PARITY;
            for (j, shard) in result.recovery_iter().enumerate() {
                hdr.shard_index = (k + j) as u16;
                out.push(make_datagram(&hdr, shard));
            }
            info.fec_encode_us += t0.elapsed().as_micros() as u32;
            info.parity_shards += m as u32;
        }
        // Mark the last datagram of the frame.
        if let Some(last) = out.last_mut() {
            let mut v = BytesMut::from(&last[..]);
            v[1] |= flags::END_OF_FRAME;
            *last = v.freeze();
        }
        Ok((layout, info))
    }
}

fn make_datagram(hdr: &MediaHeader, payload: &[u8]) -> Bytes {
    let mut b = BytesMut::with_capacity(MEDIA_HEADER_LEN + payload.len());
    b.put_slice(&hdr.to_bytes());
    b.put_slice(payload);
    b.freeze()
}

/// Result of [`VideoSender::send_frame`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SendStats {
    pub frame_id: u32,
    pub bytes: usize,
    pub datagrams: u32,
    pub blocks: u32,
    pub data_shards: u32,
    pub parity_shards: u32,
    pub shard_len: u32,
    /// Datagrams that could not be queued (buffer full / too large).
    pub dropped: u32,
    pub fec_encode_us: u32,
    /// Total packetize + FEC + queueing time (µs) — `FrameTimingReport::packetize_us`.
    pub packetize_us: u32,
}

struct SentFrame {
    frame_id: u32,
    datagrams: Vec<Bytes>,
    /// Per block: (index of first datagram, k, m).
    blocks: Vec<(usize, usize, usize)>,
    bytes: usize,
}

/// Host side of the media plane: `EncodedPacket` → FEC shards → datagrams.
///
/// Synchronous; call from the encode thread. Keeps the last frames for NACK
/// retransmission (bounded by [`VideoSender::HISTORY_FRAMES`] / bytes).
pub struct VideoSender {
    ch: Arc<dyn TransportChannel>,
    packetizer: Packetizer,
    history: VecDeque<SentFrame>,
    history_bytes: usize,
    starts: Vec<(usize, usize, usize)>,
    retransmitted: u64,
}

impl VideoSender {
    pub const HISTORY_FRAMES: usize = 32;
    pub const HISTORY_BYTES: usize = 16 * 1024 * 1024;

    pub fn new(ch: Arc<dyn TransportChannel>, stream_id: u8) -> Self {
        Self {
            ch,
            packetizer: Packetizer::new(stream_id),
            history: VecDeque::with_capacity(Self::HISTORY_FRAMES + 1),
            history_bytes: 0,
            starts: Vec::new(),
            retransmitted: 0,
        }
    }

    /// Packetize and queue one frame. Errors only if the session is closed or datagrams
    /// are unsupported; individual datagram drops are counted in [`SendStats::dropped`].
    pub fn send_frame(&mut self, pkt: &EncodedPacket) -> Result<SendStats> {
        let t0 = Instant::now();
        let max = self.ch.max_datagram_size().ok_or_else(|| CoreError::unsupported("peer does not support datagrams"))?;
        let mut dgrams = Vec::new();
        let (_layout, info) = self.packetizer.packetize(pkt, max, &mut dgrams, &mut self.starts)?;
        let mut stats = SendStats {
            frame_id: pkt.frame_id,
            bytes: pkt.data.len(),
            datagrams: dgrams.len() as u32,
            blocks: info.blocks,
            data_shards: info.data_shards,
            parity_shards: info.parity_shards,
            shard_len: info.shard_len,
            fec_encode_us: info.fec_encode_us,
            ..Default::default()
        };
        for d in &dgrams {
            match self.ch.send_datagram(d.clone()) {
                Ok(()) => {}
                Err(CoreError::Closed) => return Err(CoreError::Closed),
                Err(CoreError::Timeout) => return Err(CoreError::Timeout),
                Err(_) => stats.dropped += 1,
            }
        }
        let bytes: usize = dgrams.iter().map(|d| d.len()).sum();
        self.history_bytes += bytes;
        self.history.push_back(SentFrame { frame_id: pkt.frame_id, datagrams: dgrams, blocks: self.starts.clone(), bytes });
        while self.history.len() > Self::HISTORY_FRAMES || (self.history_bytes > Self::HISTORY_BYTES && self.history.len() > 1) {
            if let Some(old) = self.history.pop_front() {
                self.history_bytes -= old.bytes;
            }
        }
        stats.packetize_us = t0.elapsed().as_micros() as u32;
        Ok(stats)
    }

    pub fn set_parity_percent(&mut self, pct: u8) {
        self.packetizer.set_parity_percent(pct);
    }

    pub fn parity_percent(&self) -> u8 {
        self.packetizer.parity_percent()
    }

    /// Total shards retransmitted because of NACKs.
    pub fn retransmitted(&self) -> u64 {
        self.retransmitted
    }

    /// Retransmit the requested shards if the frame is still in the history.
    ///
    /// Semantics: `block_index == u32::MAX` ⇒ every block; empty `shard_indices` ⇒ all
    /// data shards of the block.
    pub fn on_nack(&mut self, nack: &Nack) {
        let Some(f) = self.history.iter().find(|f| f.frame_id == nack.frame_id) else {
            return;
        };
        let blocks: Vec<usize> = if nack.block_index == u32::MAX {
            (0..f.blocks.len()).collect()
        } else if (nack.block_index as usize) < f.blocks.len() {
            vec![nack.block_index as usize]
        } else {
            return;
        };
        for b in blocks {
            let (first, k, m) = f.blocks[b];
            let mut send = |i: usize| {
                if i < k + m
                    && let Some(d) = f.datagrams.get(first + i)
                    && self.ch.send_datagram(d.clone()).is_ok()
                {
                    self.retransmitted += 1;
                }
            };
            if nack.shard_indices.is_empty() || nack.block_index == u32::MAX {
                (0..k).for_each(&mut send);
            } else {
                nack.shard_indices.iter().for_each(|&i| send(i as usize));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_limits() {
        for &(n, pct) in &[(0usize, 20u8), (1, 0), (1100, 15), (20_000, 20), (200_000, 20), (5_000_000, 50), (123_457, 100)] {
            let l = FrameLayout::plan(n, 1200 - 24, pct).unwrap();
            assert!(l.shard_len <= 1176 && l.shard_len % 2 == 0);
            assert!(l.block_count <= 255);
            let mut total = 0;
            for b in 0..l.block_count {
                let (s, len) = l.block_range(b);
                assert_eq!(s, total);
                total += len;
                assert!(l.block_k(b) + l.block_m(b) <= 255);
                assert!(len <= l.block_k(b) * l.shard_len);
            }
            assert_eq!(total, n);
        }
    }

    #[test]
    fn parity_math() {
        assert_eq!(parity_for(10, 20), 2);
        assert_eq!(parity_for(1, 5), 1);
        assert_eq!(parity_for(7, 0), 0);
        assert_eq!(max_data_shards(0), 255);
        assert_eq!(max_data_shards(100), 127);
        let k = max_data_shards(20);
        assert!(k + parity_for(k, 20) <= 255 && k + 1 + parity_for(k + 1, 20) > 255);
    }
}
