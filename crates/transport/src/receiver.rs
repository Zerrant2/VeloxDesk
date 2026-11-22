//! Viewer side of the media plane: datagrams → complete frames + feedback (ARCHITECTURE §6).
//!
//! * Shards may arrive in any order; a block is recovered as soon as any `k` of its
//!   `k + m` shards are present (systematic RS, `reed-solomon-simd`).
//! * Frames are released **in frame-id order** the moment they are complete — there is no
//!   jitter buffer. A complete frame is held only while an *earlier* frame is still
//!   being resolved (NACK round-trip or reorder window, a few ms), because handing a
//!   decoder frame N+1 without N corrupts the picture. A complete **keyframe** is never
//!   held: it supersedes everything before it.
//! * A frame is declared lost when newer data has arrived and it is still incomplete
//!   after the reorder window (and after one NACK round-trip when `rtt ≤ nack_max_rtt`),
//!   or after `stall_timeout` without any progress. Loss ⇒ `KeyframeRequest` (rate
//!   limited) and `ReceivedFrame::discontinuity = true` on the next delivered frame.
//!
//! Usage: feed every datagram to [`VideoReceiver::push_datagram`], drain extra frames with
//! [`VideoReceiver::pop_frame`], and call [`VideoReceiver::poll_feedback`] after each push
//! burst and at least every ~5 ms (it drives timeouts and the 50 ms `ReceiverReport`);
//! send the returned envelopes on the control stream.
//!
//! Frame ids must be consecutive per stream (a skipped id is treated as a lost frame).

use std::collections::{BTreeMap, VecDeque};

use bytes::Bytes;
use reed_solomon_simd::ReedSolomonDecoder;

use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::video::EncodedPacket;
use veloxdesk_protocol::control::{Envelope, KeyframeRequest, Nack, ReceiverReport, envelope::Msg};
use veloxdesk_protocol::media::{MediaHeader, MediaKind, WireCodec, flags};

use crate::fec::video_codec;

/// Tunables of [`VideoReceiver`].
#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    /// `ReceiverReport` period.
    pub report_interval_ms: u32,
    /// How long an incomplete frame may wait for reordered shards once newer data exists.
    pub reorder_window_us: u64,
    /// NACKs are sent only when the RTT is at most this.
    pub nack_max_rtt_us: u32,
    /// An incomplete frame without newer data is resolved after this long without progress.
    pub stall_timeout_us: u64,
    /// Finalized frames are kept this long to count late shards for loss statistics.
    pub linger_us: u64,
    /// Max frames tracked before the oldest unresolved one is force-dropped.
    pub max_tracked_frames: usize,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            report_interval_ms: 50,
            reorder_window_us: 3_000,
            nack_max_rtt_us: 40_000,
            stall_timeout_us: 30_000,
            linger_us: 60_000,
            max_tracked_frames: 256,
        }
    }
}

/// A complete, verified frame ready for decoding.
#[derive(Debug, Clone)]
pub struct ReceivedFrame {
    /// `capture_ts` is on the **host** clock (reconstructed from the 32-bit wire value;
    /// see [`VideoReceiver::set_host_time_reference`]). `width/height` come from
    /// [`VideoReceiver::set_dimensions`]; `encode_us/convert_us` are 0.
    pub packet: EncodedPacket,
    /// Local time the frame became complete.
    pub recv_complete_ts: Timestamp,
    /// Local time the first shard of the frame arrived.
    pub first_shard_ts: Timestamp,
    /// Data shards reconstructed by FEC.
    pub recovered_shards: u32,
    /// At least one earlier frame was lost since the previous delivered frame: the decoder
    /// reference chain is broken unless this is a keyframe.
    pub discontinuity: bool,
    /// The frame needed a NACK retransmission.
    pub retransmitted: bool,
}

/// Cumulative counters (HUD / tests).
#[derive(Debug, Clone, Copy, Default)]
pub struct ReceiverStats {
    pub frames_delivered: u64,
    pub frames_lost: u64,
    pub frames_superseded: u64,
    pub packets_received: u64,
    pub packets_lost: u64,
    pub packets_recovered_fec: u64,
    pub packets_duplicate: u64,
    pub packets_stale: u64,
    pub packets_invalid: u64,
    pub nacks_sent: u64,
    pub keyframe_requests: u64,
    pub fec_decode_us_total: u64,
    pub fec_decodes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Pending,
    Complete,
    Delivered,
    Lost,
}

struct Block {
    k: usize,
    m: usize,
    shard_len: usize,
    block_len: usize,
    data: Vec<u8>,
    have: Vec<bool>,
    parity: Vec<Option<Vec<u8>>>,
    count: usize,
    complete: bool,
}

impl Block {
    fn new(h: &MediaHeader) -> Self {
        let k = h.data_shards as usize;
        let m = h.parity_shards as usize;
        let sl = h.shard_len as usize;
        Self {
            k,
            m,
            shard_len: sl,
            block_len: h.block_len as usize,
            data: vec![0; k * sl],
            have: vec![false; k + m],
            parity: (0..m).map(|_| None).collect(),
            count: 0,
            complete: false,
        }
    }

    fn matches(&self, h: &MediaHeader) -> bool {
        self.k == h.data_shards as usize
            && self.m == h.parity_shards as usize
            && self.shard_len == h.shard_len as usize
            && self.block_len == h.block_len as usize
    }

    fn missing(&self) -> impl Iterator<Item = usize> + '_ {
        self.have.iter().enumerate().filter(|(_, h)| !**h).map(|(i, _)| i)
    }
}

struct FrameAsm {
    frame_id: u32,
    block_count: usize,
    blocks: Vec<Option<Block>>,
    blocks_complete: usize,
    flags: u8,
    codec: WireCodec,
    capture_low: u32,
    first_arrival: Option<Timestamp>,
    last_activity: Timestamp,
    state: State,
    received: u32,
    recovered: u32,
    gap_since: Option<Timestamp>,
    nack_deadline: Option<Timestamp>,
    nacked: bool,
    payload: Option<Bytes>,
    complete_ts: Timestamp,
}

impl FrameAsm {
    fn placeholder(frame_id: u32, now: Timestamp) -> Self {
        Self {
            frame_id,
            block_count: 0,
            blocks: Vec::new(),
            blocks_complete: 0,
            flags: 0,
            codec: WireCodec::Unknown,
            capture_low: 0,
            first_arrival: None,
            last_activity: now,
            state: State::Pending,
            received: 0,
            recovered: 0,
            gap_since: None,
            nack_deadline: None,
            nacked: false,
            payload: None,
            complete_ts: now,
        }
    }

    fn expected_shards(&self) -> Option<u32> {
        if self.block_count == 0 {
            return None;
        }
        let known: Vec<u32> = self.blocks.iter().flatten().map(|b| (b.k + b.m) as u32).collect();
        if known.is_empty() {
            return None;
        }
        let avg = known.iter().sum::<u32>() / known.len() as u32;
        Some(self.blocks.iter().map(|b| b.as_ref().map_or(avg, |b| (b.k + b.m) as u32)).sum())
    }
}

enum Added {
    Ignored,
    Duplicate,
    Accepted,
    Completed,
}

/// Linear-regression trendline over smoothed accumulated delay (GCC-style).
struct Trendline {
    acc: f64,
    smoothed: f64,
    samples: VecDeque<(f64, f64)>,
    window: usize,
}

impl Trendline {
    fn new() -> Self {
        Self { acc: 0.0, smoothed: 0.0, samples: VecDeque::with_capacity(21), window: 20 }
    }
    fn add(&mut self, t_us: u64, delta_us: f64) {
        self.acc += delta_us;
        self.smoothed = 0.9 * self.smoothed + 0.1 * self.acc;
        if self.samples.len() == self.window {
            self.samples.pop_front();
        }
        self.samples.push_back((t_us as f64, self.smoothed));
    }
    /// Slope in µs of queueing delay per µs of wall time.
    fn slope(&self) -> f64 {
        let n = self.samples.len();
        if n < 3 {
            return 0.0;
        }
        let (mx, my) = self.samples.iter().fold((0.0, 0.0), |a, s| (a.0 + s.0, a.1 + s.1));
        let (mx, my) = (mx / n as f64, my / n as f64);
        let (mut num, mut den) = (0.0, 0.0);
        for &(x, y) in &self.samples {
            num += (x - mx) * (y - my);
            den += (x - mx) * (x - mx);
        }
        if den > 0.0 { num / den } else { 0.0 }
    }
}

/// Reassembles FEC-protected video frames and produces viewer feedback.
pub struct VideoReceiver {
    cfg: ReceiverConfig,
    frames: BTreeMap<u64, FrameAsm>,
    highest: Option<u64>,
    cursor: Option<u64>,
    ready: VecDeque<ReceivedFrame>,
    feedback: Vec<Envelope>,
    decoder: Option<ReedSolomonDecoder>,
    stream_id: u8,
    width: u32,
    height: u32,
    host_ref: Option<Timestamp>,
    rtt_us: u32,
    decode_us: u32,
    render_us: u32,
    discontinuity: bool,
    need_keyframe: bool,
    last_pli: Option<Timestamp>,
    last_good_frame_id: u32,
    highest_delivered_id: u32,
    // interval counters
    iv_received: u32,
    iv_lost: u32,
    iv_recovered: u32,
    iv_unrecoverable: u32,
    iv_bytes: u64,
    last_report: Option<Timestamp>,
    stats: ReceiverStats,
    // delay-gradient / jitter
    trend: Trendline,
    prev_first: Option<(Timestamp, u32)>,
    jitter_us: f64,
    frame_interval_us: f64,
    expected_per_frame: f64,
}

impl Default for VideoReceiver {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoReceiver {
    pub fn new() -> Self {
        Self::with_config(ReceiverConfig::default())
    }

    pub fn with_config(cfg: ReceiverConfig) -> Self {
        Self {
            cfg,
            frames: BTreeMap::new(),
            highest: None,
            cursor: None,
            ready: VecDeque::new(),
            feedback: Vec::new(),
            decoder: None,
            stream_id: 0,
            width: 0,
            height: 0,
            host_ref: None,
            rtt_us: 0,
            decode_us: 0,
            render_us: 0,
            discontinuity: false,
            need_keyframe: false,
            last_pli: None,
            last_good_frame_id: 0,
            highest_delivered_id: 0,
            iv_received: 0,
            iv_lost: 0,
            iv_recovered: 0,
            iv_unrecoverable: 0,
            iv_bytes: 0,
            last_report: None,
            stats: ReceiverStats::default(),
            trend: Trendline::new(),
            prev_first: None,
            jitter_us: 0.0,
            frame_interval_us: 16_667.0,
            expected_per_frame: 0.0,
        }
    }

    /// Current RTT (from `TransportChannel::stats` / Ping) — enables NACK when small.
    pub fn set_rtt_us(&mut self, rtt_us: u32) {
        self.rtt_us = rtt_us;
    }

    /// Stream dimensions (from `VideoConfig`) copied into delivered packets.
    pub fn set_dimensions(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
    }

    /// A recent host-clock timestamp (e.g. `Pong::peer_send_us`) so the 32-bit wire
    /// capture time is expanded to the correct 64-bit host time.
    pub fn set_host_time_reference(&mut self, host_now: Timestamp) {
        self.host_ref = Some(host_now);
    }

    /// Viewer decode/render times echoed in the next `ReceiverReport`.
    pub fn set_decode_render_us(&mut self, decode_us: u32, render_us: u32) {
        self.decode_us = decode_us;
        self.render_us = render_us;
    }

    pub fn stats(&self) -> ReceiverStats {
        self.stats
    }

    /// Ask for a keyframe at the next `poll_feedback` (e.g. after a decoder error).
    pub fn request_keyframe(&mut self) {
        self.need_keyframe = true;
    }

    /// Next complete frame not yet returned by `push_datagram`.
    pub fn pop_frame(&mut self) -> Option<ReceivedFrame> {
        self.ready.pop_front()
    }

    fn unwrap_idx(&self, fid: u32) -> u64 {
        match self.highest {
            None => (1u64 << 32) + fid as u64,
            Some(h) => {
                let diff = fid.wrapping_sub(h as u32) as i32 as i64;
                (h as i64 + diff) as u64
            }
        }
    }

    /// Feed one datagram. Returns the next deliverable frame, if any (more may be
    /// queued: drain with [`Self::pop_frame`]).
    pub fn push_datagram(&mut self, dgram: &[u8], now: Timestamp) -> Option<ReceivedFrame> {
        self.iv_bytes += dgram.len() as u64;
        let Ok((h, payload)) = MediaHeader::parse(dgram) else {
            self.stats.packets_invalid += 1;
            return self.ready.pop_front();
        };
        if h.kind != MediaKind::Video || h.validate().is_err() || payload.len() > h.shard_len as usize {
            self.stats.packets_invalid += 1;
            return self.ready.pop_front();
        }
        self.stream_id = h.stream_id;
        let idx = self.unwrap_idx(h.frame_id);
        let cursor = *self.cursor.get_or_insert(idx);
        if idx < cursor && !self.frames.contains_key(&idx) {
            self.stats.packets_stale += 1;
            return self.ready.pop_front();
        }
        if idx > cursor + 4 * self.cfg.max_tracked_frames as u64 {
            // Stream restart / huge jump: start over.
            self.reset_tracking(idx);
        }
        let highest = self.highest.unwrap_or(idx.saturating_sub(1));
        if self.highest.is_none() || idx > highest {
            let start = (highest + 1).max(self.cursor.unwrap_or(idx));
            for missing in start..idx {
                self.frames.entry(missing).or_insert_with(|| FrameAsm::placeholder(missing as u32, now));
            }
            self.highest = Some(idx);
            self.on_new_frame(now, h.capture_ts);
        }
        let frame = self.frames.entry(idx).or_insert_with(|| FrameAsm::placeholder(h.frame_id, now));
        match add_shard(frame, &h, payload, now, &mut self.decoder, &mut self.stats) {
            Added::Ignored => self.stats.packets_invalid += 1,
            Added::Duplicate => self.stats.packets_duplicate += 1,
            Added::Accepted => {
                self.iv_received += 1;
                self.stats.packets_received += 1;
            }
            Added::Completed => {
                self.iv_received += 1;
                self.stats.packets_received += 1;
                self.on_complete(idx, now);
            }
        }
        self.check_pending(now);
        self.advance(now);
        self.ready.pop_front()
    }

    fn reset_tracking(&mut self, idx: u64) {
        self.frames.clear();
        self.cursor = Some(idx);
        self.highest = None;
        self.prev_first = None;
        self.need_keyframe = true;
        self.discontinuity = true;
    }

    fn on_new_frame(&mut self, now: Timestamp, capture_low: u32) {
        if let Some((pa, pc)) = self.prev_first {
            let cap_d = capture_low.wrapping_sub(pc) as i32 as i64;
            if cap_d > 0 {
                let arr_d = now.as_micros() as i64 - pa.as_micros() as i64;
                let d = (arr_d - cap_d) as f64;
                self.jitter_us += (d.abs() - self.jitter_us) / 16.0;
                self.frame_interval_us += (cap_d as f64 - self.frame_interval_us) / 16.0;
                self.trend.add(now.as_micros(), d);
            }
        }
        self.prev_first = Some((now, capture_low));
    }

    fn on_complete(&mut self, idx: u64, now: Timestamp) {
        let Some(f) = self.frames.get_mut(&idx) else { return };
        f.complete_ts = now;
        let key = f.flags & flags::KEYFRAME != 0;
        let Some(cursor) = self.cursor else { return };
        if key && idx > cursor {
            // A keyframe supersedes every unresolved / held frame before it.
            let mut superseded = 0;
            for (_, g) in self.frames.range_mut(cursor..idx) {
                if matches!(g.state, State::Pending | State::Complete) {
                    g.state = State::Lost;
                    superseded += 1;
                }
            }
            self.stats.frames_superseded += superseded;
            self.cursor = Some(idx);
        }
    }

    /// Resolve incomplete frames: NACK, then declare lost.
    fn check_pending(&mut self, now: Timestamp) {
        let (Some(cursor), Some(highest)) = (self.cursor, self.highest) else { return };
        let nack_ok = self.rtt_us > 0 && self.rtt_us <= self.cfg.nack_max_rtt_us;
        let reorder = self.cfg.reorder_window_us.max((2.0 * self.jitter_us) as u64).min(20_000);
        let stall = self.cfg.stall_timeout_us.max(3 * self.rtt_us as u64);
        let mut lost = Vec::new();
        for (&idx, f) in self.frames.range_mut(cursor..) {
            if f.state != State::Pending {
                continue;
            }
            let newer = idx < highest;
            let stalled = now.since(f.last_activity) >= stall;
            if !newer && !stalled {
                continue;
            }
            let since = *f.gap_since.get_or_insert(now);
            if now.since(since) < reorder && !stalled {
                continue;
            }
            if nack_ok && !f.nacked {
                f.nacked = true;
                f.nack_deadline = Some(now + std::time::Duration::from_micros(self.rtt_us as u64 * 3 / 2 + reorder + 2_000));
                for n in build_nacks(f, self.stream_id) {
                    self.stats.nacks_sent += 1;
                    self.feedback.push(Msg::Nack(n).into());
                }
                continue;
            }
            if f.nack_deadline.is_some_and(|d| now.as_micros() < d.as_micros()) {
                continue;
            }
            f.state = State::Lost;
            lost.push(idx);
        }
        if !lost.is_empty() {
            self.stats.frames_lost += lost.len() as u64;
            self.iv_unrecoverable += lost.len() as u32;
            self.need_keyframe = true;
            self.discontinuity = true;
        }
    }

    /// Deliver complete frames in order and purge old state.
    fn advance(&mut self, now: Timestamp) {
        let Some(mut cursor) = self.cursor else { return };
        let highest = self.highest.unwrap_or(cursor);
        while cursor <= highest {
            let Some(f) = self.frames.get_mut(&cursor) else {
                cursor += 1;
                continue;
            };
            match f.state {
                State::Pending => break,
                State::Delivered | State::Lost => cursor += 1,
                State::Complete => {
                    f.state = State::Delivered;
                    let key = f.flags & flags::KEYFRAME != 0;
                    let low = f.capture_low;
                    let reference = self.host_ref.unwrap_or(Timestamp(low as u64));
                    let capture_ts = Timestamp::from_low32(low, reference);
                    self.host_ref = Some(capture_ts);
                    let frame = ReceivedFrame {
                        packet: EncodedPacket {
                            data: f.payload.take().unwrap_or_default(),
                            codec: video_codec(f.codec),
                            keyframe: key,
                            recovery_point: f.flags & flags::RECOVERY_POINT != 0,
                            width: self.width,
                            height: self.height,
                            capture_ts,
                            frame_id: f.frame_id,
                            encode_us: 0,
                            convert_us: 0,
                        },
                        recv_complete_ts: f.complete_ts,
                        first_shard_ts: f.first_arrival.unwrap_or(f.complete_ts),
                        recovered_shards: f.recovered,
                        discontinuity: self.discontinuity && !key,
                        retransmitted: f.nacked,
                    };
                    self.discontinuity = false;
                    if key || frame.packet.recovery_point {
                        self.need_keyframe = false;
                    }
                    self.last_good_frame_id = f.frame_id;
                    self.highest_delivered_id = f.frame_id;
                    self.stats.frames_delivered += 1;
                    self.ready.push_back(frame);
                    cursor += 1;
                }
            }
        }
        self.cursor = Some(cursor);

        // Force progress if too many frames are tracked (stuck cursor).
        while self.frames.len() > self.cfg.max_tracked_frames {
            let Some((&first, _)) = self.frames.iter().next() else { break };
            if let Some(f) = self.frames.remove(&first)
                && f.state == State::Pending
            {
                self.stats.frames_lost += 1;
                self.iv_unrecoverable += 1;
                self.need_keyframe = true;
                self.discontinuity = true;
            }
            self.account(first, now, true);
            if self.cursor.is_some_and(|c| c <= first) {
                self.cursor = Some(first + 1);
                return self.advance(now);
            }
        }
        // Purge finalized frames behind the cursor after the linger time.
        while let Some((&first, f)) = self.frames.iter().next() {
            if first >= cursor || now.since(f.last_activity) < self.cfg.linger_us {
                break;
            }
            self.account(first, now, false);
        }
    }

    /// Remove frame `idx` (if still present) and add its losses to the counters.
    fn account(&mut self, idx: u64, _now: Timestamp, _forced: bool) {
        let Some(f) = self.frames.remove(&idx) else { return };
        let expected = match f.expected_shards() {
            Some(e) => {
                self.expected_per_frame += (e as f64 - self.expected_per_frame) / 8.0;
                e
            }
            None => self.expected_per_frame.round() as u32,
        };
        let lost = expected.saturating_sub(f.received);
        self.iv_lost += lost;
        self.stats.packets_lost += lost as u64;
        self.iv_recovered += f.recovered;
    }

    /// Drive timeouts and return feedback to send on the control stream:
    /// `Nack`s, `KeyframeRequest` (rate limited) and the periodic `ReceiverReport`.
    pub fn poll_feedback(&mut self, now: Timestamp) -> Vec<Envelope> {
        self.check_pending(now);
        self.advance(now);
        let mut out = std::mem::take(&mut self.feedback);
        if self.need_keyframe {
            let gap = (2 * self.rtt_us as u64 + 20_000).max(50_000);
            if self.last_pli.is_none_or(|t| now.since(t) >= gap) {
                self.last_pli = Some(now);
                self.stats.keyframe_requests += 1;
                out.push(
                    Msg::KeyframeRequest(KeyframeRequest {
                        stream_id: self.stream_id as u32,
                        last_good_frame_id: self.last_good_frame_id,
                    })
                    .into(),
                );
            }
        }
        match self.last_report {
            None => self.last_report = Some(now),
            Some(last) => {
                let elapsed = now.since(last);
                if elapsed >= self.cfg.report_interval_ms as u64 * 1000 {
                    out.push(Msg::ReceiverReport(self.build_report(elapsed)).into());
                    self.last_report = Some(now);
                }
            }
        }
        out
    }

    fn build_report(&mut self, elapsed_us: u64) -> ReceiverReport {
        let interval_ms = (elapsed_us / 1000).max(1);
        let rr = ReceiverReport {
            stream_id: self.stream_id as u32,
            highest_frame_id: self.highest_delivered_id,
            packets_received: self.iv_received,
            packets_lost: self.iv_lost,
            packets_recovered_fec: self.iv_recovered,
            frames_unrecoverable: self.iv_unrecoverable,
            recv_kbps: (self.iv_bytes * 8 / interval_ms) as u32,
            delay_gradient: (self.trend.slope() * self.frame_interval_us) as f32,
            jitter_us: self.jitter_us as u32,
            decode_us: self.decode_us,
            render_us: self.render_us,
            rtt_us: self.rtt_us,
            interval_ms: interval_ms as u32,
        };
        self.iv_received = 0;
        self.iv_lost = 0;
        self.iv_recovered = 0;
        self.iv_unrecoverable = 0;
        self.iv_bytes = 0;
        rr
    }
}

fn add_shard(
    f: &mut FrameAsm,
    h: &MediaHeader,
    payload: &[u8],
    now: Timestamp,
    decoder: &mut Option<ReedSolomonDecoder>,
    stats: &mut ReceiverStats,
) -> Added {
    if f.block_count == 0 {
        f.block_count = h.block_count as usize;
        f.blocks = (0..f.block_count).map(|_| None).collect();
        f.flags = h.flags & !(flags::PARITY | flags::END_OF_FRAME);
        f.codec = h.codec;
        f.capture_low = h.capture_ts;
    } else if f.block_count != h.block_count as usize {
        return Added::Ignored;
    }
    let bi = h.block_index as usize;
    let block = f.blocks[bi].get_or_insert_with(|| Block::new(h));
    if !block.matches(h) {
        return Added::Ignored;
    }
    let si = h.shard_index as usize;
    if block.have[si] {
        return Added::Duplicate;
    }
    block.have[si] = true;
    f.received += 1;
    f.first_arrival.get_or_insert(now);
    f.last_activity = now;
    if block.complete || f.state != State::Pending {
        return Added::Accepted;
    }
    let sl = block.shard_len;
    if si < block.k {
        block.data[si * sl..si * sl + payload.len()].copy_from_slice(payload);
    } else {
        let mut v = Vec::with_capacity(sl);
        v.extend_from_slice(payload);
        v.resize(sl, 0);
        block.parity[si - block.k] = Some(v);
    }
    block.count += 1;
    if block.count < block.k {
        return Added::Accepted;
    }
    // Any k of k+m shards present: complete the block.
    let missing_data: Vec<usize> = (0..block.k).filter(|&i| !block.have[i]).collect();
    if !missing_data.is_empty() {
        let t0 = std::time::Instant::now();
        let ok = recover_block(block, decoder);
        stats.fec_decode_us_total += t0.elapsed().as_micros() as u64;
        stats.fec_decodes += 1;
        if !ok {
            return Added::Accepted;
        }
        f.recovered += missing_data.len() as u32;
        stats.packets_recovered_fec += missing_data.len() as u64;
    }
    block.complete = true;
    block.parity = Vec::new();
    f.blocks_complete += 1;
    if f.blocks_complete < f.block_count {
        return Added::Accepted;
    }
    // Assemble the frame.
    let total: usize = f.blocks.iter().flatten().map(|b| b.block_len).sum();
    let mut out = Vec::with_capacity(total);
    for b in f.blocks.iter_mut().flatten() {
        out.extend_from_slice(&b.data[..b.block_len]);
        b.data = Vec::new();
    }
    f.payload = Some(Bytes::from(out));
    f.state = State::Complete;
    Added::Completed
}

fn recover_block(block: &mut Block, decoder: &mut Option<ReedSolomonDecoder>) -> bool {
    let (k, m, sl) = (block.k, block.m, block.shard_len);
    let dec = match decoder.as_mut() {
        Some(d) => {
            if d.reset(k, m, sl).is_err() {
                return false;
            }
            d
        }
        None => match ReedSolomonDecoder::new(k, m, sl) {
            Ok(d) => decoder.insert(d),
            Err(_) => return false,
        },
    };
    for i in 0..k {
        if block.have[i] && dec.add_original_shard(i, &block.data[i * sl..(i + 1) * sl]).is_err() {
            return false;
        }
    }
    for (j, p) in block.parity.iter().enumerate() {
        if let Some(p) = p
            && dec.add_recovery_shard(j, p).is_err()
        {
            return false;
        }
    }
    let Ok(result) = dec.decode() else { return false };
    for (i, shard) in result.restored_original_iter() {
        block.data[i * sl..(i + 1) * sl].copy_from_slice(shard);
    }
    true
}

fn build_nacks(f: &FrameAsm, stream_id: u8) -> Vec<Nack> {
    let base = |block_index: u32, shard_indices: Vec<u32>| Nack {
        stream_id: stream_id as u32,
        frame_id: f.frame_id,
        block_index,
        shard_indices,
    };
    if f.block_count == 0 {
        return vec![base(u32::MAX, Vec::new())];
    }
    let mut out = Vec::new();
    for (b, blk) in f.blocks.iter().enumerate() {
        match blk {
            None => out.push(base(b as u32, Vec::new())),
            Some(blk) if !blk.complete => {
                let need = blk.k.saturating_sub(blk.count);
                // Prefer data shards (cheaper to place, no decode needed).
                let idx: Vec<u32> = blk.missing().take(need).map(|i| i as u32).collect();
                out.push(base(b as u32, idx));
            }
            _ => {}
        }
    }
    out
}
