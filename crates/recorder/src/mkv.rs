//! Pure-Rust Matroska (`.mkv`) muxer and structural validator.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use veloxdesk_core_traits::audio::EncodedAudio;
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{CoreError, Result};

use crate::codec::{
    annex_b_to_length_prefixed, build_av1c_from_obu, build_avcc_from_annex_b, build_hvcc_from_annex_b,
    build_opus_head, video_codec_id,
};
use crate::ebml::{
    UNKNOWN_SIZE_8, encode_vint_size_fixed8, read_element_id, read_uint, read_vint_size, read_vint_value,
    write_bytes_element, write_element_id, write_f64_element, write_master_element, write_str_element,
    write_uint_element, write_vint_size,
};
use crate::{MkvValidationSummary, RecorderConfig};

// ───────────────────── Matroska / EBML Element IDs ─────────────────────

const ID_EBML: u32 = 0x1A45_DFA3;
const ID_EBML_VERSION: u32 = 0x4286;
const ID_EBML_READ_VERSION: u32 = 0x42F7;
const ID_EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const ID_EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const ID_DOC_TYPE: u32 = 0x4282;
const ID_DOC_TYPE_VERSION: u32 = 0x4287;
const ID_DOC_TYPE_READ_VERSION: u32 = 0x4285;

const ID_SEGMENT: u32 = 0x1853_8067;

const ID_INFO: u32 = 0x1549_A966;
const ID_TIMESTAMP_SCALE: u32 = 0x002A_D7B1;
const ID_MUXING_APP: u32 = 0x4D80;
const ID_WRITING_APP: u32 = 0x5741;
const ID_DURATION: u32 = 0x4489;

const ID_TRACKS: u32 = 0x1654_AE6B;
const ID_TRACK_ENTRY: u32 = 0xAE;
const ID_TRACK_NUMBER: u32 = 0xD7;
const ID_TRACK_UID: u32 = 0x73C5;
const ID_TRACK_TYPE: u32 = 0x83;
const ID_FLAG_LACING: u32 = 0x9C;
const ID_CODEC_ID: u32 = 0x86;
const ID_CODEC_PRIVATE: u32 = 0x63A2;
const ID_CODEC_DELAY: u32 = 0x56AA;
const ID_SEEK_PRE_ROLL: u32 = 0x56BB;
const ID_DEFAULT_DURATION: u32 = 0x0023_E383;

const ID_VIDEO: u32 = 0xE0;
const ID_PIXEL_WIDTH: u32 = 0xB0;
const ID_PIXEL_HEIGHT: u32 = 0xBA;

const ID_AUDIO: u32 = 0xE1;
const ID_SAMPLING_FREQUENCY: u32 = 0xB5;
const ID_CHANNELS: u32 = 0x9F;

const ID_CLUSTER: u32 = 0x1F43_B675;
const ID_TIMESTAMP: u32 = 0xE7;
const ID_SIMPLE_BLOCK: u32 = 0xA3;

const ID_CUES: u32 = 0x1C53_BB6B;
const ID_CUE_POINT: u32 = 0xBB;
const ID_CUE_TIME: u32 = 0xB3;
const ID_CUE_TRACK_POSITIONS: u32 = 0xB7;
const ID_CUE_TRACK: u32 = 0xF7;
const ID_CUE_CLUSTER_POSITION: u32 = 0xF1;

const TRACK_VIDEO: u64 = 1;
const TRACK_AUDIO: u64 = 2;
const TRACK_TYPE_VIDEO: u64 = 1;
const TRACK_TYPE_AUDIO: u64 = 2;

/// TimestampScale = 1_000_000 ns = 1 ms per tick.
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;
/// Rotate clusters roughly every 2 seconds (well within i16 relative SimpleBlock limit of 32767 ms).
const CLUSTER_ROTATE_MS: u64 = 2_000;
/// Opus pre-skip 312 samples @ 48 kHz = 6.5 ms in nanoseconds.
const OPUS_CODEC_DELAY_NS: u64 = 6_500_000;
/// Opus standard seek pre-roll = 80 ms in nanoseconds.
const OPUS_SEEK_PRE_ROLL_NS: u64 = 80_000_000;

struct CueEntry {
    time_ms: u64,
    cluster_rel_pos: u64,
}

/// Streaming pure-Rust Matroska (`.mkv`) muxer.
pub struct MkvMuxer {
    cfg: RecorderConfig,
    writer: BufWriter<File>,
    written_pos: u64,
    header_written: bool,
    segment_size_offset: u64,
    segment_data_offset: u64,
    duration_value_offset: u64,
    base_ts_us: Option<u64>,
    last_pts_ms: u64,
    cluster_open: bool,
    cluster_ts_ms: u64,
    cluster_has_keyframe: bool,
    cluster_buf: Vec<u8>,
    cues: Vec<CueEntry>,
    frame_scratch: Vec<u8>,
}

impl MkvMuxer {
    pub fn create(cfg: RecorderConfig) -> Result<Self> {
        if video_codec_id(cfg.video_codec).is_none() {
            return Err(CoreError::invalid(format!("unsupported recorder video codec: {:?}", cfg.video_codec)));
        }
        let file = File::create(&cfg.output_path).map_err(CoreError::Io)?;
        Ok(Self {
            cfg,
            writer: BufWriter::with_capacity(256 * 1024, file),
            written_pos: 0,
            header_written: false,
            segment_size_offset: 0,
            segment_data_offset: 0,
            duration_value_offset: 0,
            base_ts_us: None,
            last_pts_ms: 0,
            cluster_open: false,
            cluster_ts_ms: 0,
            cluster_has_keyframe: false,
            cluster_buf: Vec::with_capacity(256 * 1024),
            cues: Vec::new(),
            frame_scratch: Vec::with_capacity(128 * 1024),
        })
    }

    fn write_all_tracked(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes).map_err(CoreError::Io)?;
        self.written_pos += bytes.len() as u64;
        Ok(())
    }

    fn build_video_codec_private(&self, first_keyframe: Option<&[u8]>) -> Vec<u8> {
        let Some(kf) = first_keyframe else {
            return Vec::new();
        };
        match self.cfg.video_codec {
            VideoCodec::H264 => build_avcc_from_annex_b(kf),
            VideoCodec::H265 => build_hvcc_from_annex_b(kf),
            VideoCodec::Av1 => build_av1c_from_obu(kf),
            VideoCodec::Vp9 | VideoCodec::Unspecified => Vec::new(),
        }
    }

    fn write_headers(&mut self, first_keyframe: Option<&[u8]>, bytes_written: &AtomicU64) -> Result<()> {
        if self.header_written {
            return Ok(());
        }

        // 1. EBML Header
        let mut ebml_body = Vec::with_capacity(64);
        write_uint_element(&mut ebml_body, ID_EBML_VERSION, 1);
        write_uint_element(&mut ebml_body, ID_EBML_READ_VERSION, 1);
        write_uint_element(&mut ebml_body, ID_EBML_MAX_ID_LENGTH, 4);
        write_uint_element(&mut ebml_body, ID_EBML_MAX_SIZE_LENGTH, 8);
        write_str_element(&mut ebml_body, ID_DOC_TYPE, "matroska");
        write_uint_element(&mut ebml_body, ID_DOC_TYPE_VERSION, 4);
        write_uint_element(&mut ebml_body, ID_DOC_TYPE_READ_VERSION, 2);

        let mut header_buf = Vec::with_capacity(128);
        write_master_element(&mut header_buf, ID_EBML, &ebml_body);
        self.write_all_tracked(&header_buf)?;

        // 2. Segment element with 8-byte unknown-size placeholder (patched on finalize).
        let mut seg_hdr = Vec::with_capacity(12);
        write_element_id(&mut seg_hdr, ID_SEGMENT);
        self.write_all_tracked(&seg_hdr)?;
        self.segment_size_offset = self.written_pos;
        self.write_all_tracked(&UNKNOWN_SIZE_8)?;
        self.segment_data_offset = self.written_pos;

        // 3. Segment Info (Duration written last as fixed 8-byte f64 so we know its exact offset).
        let mut info_body = Vec::with_capacity(96);
        write_uint_element(&mut info_body, ID_TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
        write_str_element(&mut info_body, ID_MUXING_APP, "VeloxDesk 0.5.0");
        write_str_element(&mut info_body, ID_WRITING_APP, "veloxdesk-recorder 0.5.0");
        write_f64_element(&mut info_body, ID_DURATION, 0.0);

        let mut info_el = Vec::with_capacity(info_body.len() + 12);
        write_master_element(&mut info_el, ID_INFO, &info_body);
        // Duration payload is the final 8 bytes of `info_el`.
        let info_start = self.written_pos;
        self.write_all_tracked(&info_el)?;
        self.duration_value_offset = info_start + info_el.len() as u64 - 8;

        // 4. Tracks
        let mut tracks_body = Vec::with_capacity(256);

        // Video Track (TrackNumber = 1)
        let codec_id = video_codec_id(self.cfg.video_codec)
            .ok_or_else(|| CoreError::invalid("unsupported video codec"))?;
        let codec_private = self.build_video_codec_private(first_keyframe);
        let default_dur_ns = 1_000_000_000u64 / self.cfg.fps.max(1) as u64;

        let mut video_settings = Vec::with_capacity(24);
        write_uint_element(&mut video_settings, ID_PIXEL_WIDTH, self.cfg.width as u64);
        write_uint_element(&mut video_settings, ID_PIXEL_HEIGHT, self.cfg.height as u64);

        let mut video_track = Vec::with_capacity(128 + codec_private.len());
        write_uint_element(&mut video_track, ID_TRACK_NUMBER, TRACK_VIDEO);
        write_uint_element(&mut video_track, ID_TRACK_UID, TRACK_VIDEO);
        write_uint_element(&mut video_track, ID_TRACK_TYPE, TRACK_TYPE_VIDEO);
        write_uint_element(&mut video_track, ID_FLAG_LACING, 0);
        write_str_element(&mut video_track, ID_CODEC_ID, codec_id);
        write_uint_element(&mut video_track, ID_DEFAULT_DURATION, default_dur_ns);
        if !codec_private.is_empty() {
            write_bytes_element(&mut video_track, ID_CODEC_PRIVATE, &codec_private);
        }
        write_master_element(&mut video_track, ID_VIDEO, &video_settings);
        write_master_element(&mut tracks_body, ID_TRACK_ENTRY, &video_track);

        // Audio Track (TrackNumber = 2, Opus)
        if self.cfg.audio_enabled {
            let opus_head = build_opus_head(self.cfg.audio_channels, self.cfg.audio_sample_rate);
            let mut audio_settings = Vec::with_capacity(24);
            write_f64_element(&mut audio_settings, ID_SAMPLING_FREQUENCY, self.cfg.audio_sample_rate as f64);
            write_uint_element(&mut audio_settings, ID_CHANNELS, self.cfg.audio_channels as u64);

            let mut audio_track = Vec::with_capacity(128);
            write_uint_element(&mut audio_track, ID_TRACK_NUMBER, TRACK_AUDIO);
            write_uint_element(&mut audio_track, ID_TRACK_UID, TRACK_AUDIO);
            write_uint_element(&mut audio_track, ID_TRACK_TYPE, TRACK_TYPE_AUDIO);
            write_uint_element(&mut audio_track, ID_FLAG_LACING, 0);
            write_str_element(&mut audio_track, ID_CODEC_ID, "A_OPUS");
            write_bytes_element(&mut audio_track, ID_CODEC_PRIVATE, &opus_head);
            write_uint_element(&mut audio_track, ID_CODEC_DELAY, OPUS_CODEC_DELAY_NS);
            write_uint_element(&mut audio_track, ID_SEEK_PRE_ROLL, OPUS_SEEK_PRE_ROLL_NS);
            write_master_element(&mut audio_track, ID_AUDIO, &audio_settings);
            write_master_element(&mut tracks_body, ID_TRACK_ENTRY, &audio_track);
        }

        let mut tracks_el = Vec::with_capacity(tracks_body.len() + 12);
        write_master_element(&mut tracks_el, ID_TRACKS, &tracks_body);
        self.write_all_tracked(&tracks_el)?;

        self.header_written = true;
        bytes_written.store(self.written_pos, Ordering::Release);
        Ok(())
    }

    fn flush_cluster(&mut self, bytes_written: &AtomicU64) -> Result<()> {
        if !self.cluster_open || self.cluster_buf.is_empty() {
            return Ok(());
        }
        let cluster_rel_pos = self.written_pos - self.segment_data_offset;
        if self.cluster_has_keyframe {
            self.cues.push(CueEntry { time_ms: self.cluster_ts_ms, cluster_rel_pos });
        }

        let mut hdr = Vec::with_capacity(16);
        write_element_id(&mut hdr, ID_CLUSTER);
        write_vint_size(&mut hdr, self.cluster_buf.len() as u64);
        self.write_all_tracked(&hdr)?;
        self.writer.write_all(&self.cluster_buf).map_err(CoreError::Io)?;
        self.written_pos += self.cluster_buf.len() as u64;

        self.cluster_buf.clear();
        self.cluster_open = false;
        self.cluster_has_keyframe = false;
        bytes_written.store(self.written_pos, Ordering::Release);
        Ok(())
    }

    fn start_cluster(&mut self, ts_ms: u64) {
        self.cluster_buf.clear();
        write_uint_element(&mut self.cluster_buf, ID_TIMESTAMP, ts_ms);
        self.cluster_ts_ms = ts_ms;
        self.cluster_open = true;
        self.cluster_has_keyframe = false;
    }

    fn normalize_pts_ms(&mut self, capture_ts_us: u64) -> u64 {
        let base = *self.base_ts_us.get_or_insert(capture_ts_us);
        let raw_ms = capture_ts_us.saturating_sub(base) / 1_000;
        let mono_ms = raw_ms.max(self.last_pts_ms);
        self.last_pts_ms = mono_ms;
        mono_ms
    }

    fn append_simple_block(&mut self, track_num: u64, rel_ts: i16, keyframe: bool, payload: &[u8]) {
        let block_len = 1 + 2 + 1 + payload.len(); // 1-byte track VINT + i16 rel_ts + flags + payload
        write_element_id(&mut self.cluster_buf, ID_SIMPLE_BLOCK);
        write_vint_size(&mut self.cluster_buf, block_len as u64);
        self.cluster_buf.push(0x80 | (track_num as u8));
        self.cluster_buf.extend_from_slice(&rel_ts.to_be_bytes());
        self.cluster_buf.push(if keyframe { 0x80 } else { 0x00 });
        self.cluster_buf.extend_from_slice(payload);
    }

    /// Mux one encoded video packet. Drops leading non-keyframes until the first IDR arrives.
    pub fn write_video(
        &mut self,
        pkt: &EncodedPacket,
        waiting_keyframe: &AtomicBool,
        video_frames: &AtomicU64,
        bytes_written: &AtomicU64,
    ) -> Result<bool> {
        if pkt.data.is_empty() {
            return Ok(false);
        }

        if waiting_keyframe.load(Ordering::Acquire) {
            if !pkt.keyframe {
                return Ok(false);
            }
            self.write_headers(Some(&pkt.data), bytes_written)?;
            self.base_ts_us = Some(pkt.capture_ts.as_micros());
            self.last_pts_ms = 0;
            waiting_keyframe.store(false, Ordering::Release);
        }

        let pts_ms = self.normalize_pts_ms(pkt.capture_ts.as_micros());

        let should_rotate = !self.cluster_open
            || pkt.keyframe
            || pts_ms.saturating_sub(self.cluster_ts_ms) >= CLUSTER_ROTATE_MS;
        if should_rotate {
            self.flush_cluster(bytes_written)?;
            self.start_cluster(pts_ms);
        }

        let rel_ms = pts_ms.saturating_sub(self.cluster_ts_ms).min(i16::MAX as u64) as i16;
        if pkt.keyframe {
            self.cluster_has_keyframe = true;
        }

        match self.cfg.video_codec {
            VideoCodec::H264 | VideoCodec::H265 => {
                let mut scratch = std::mem::take(&mut self.frame_scratch);
                annex_b_to_length_prefixed(&pkt.data, &mut scratch);
                self.append_simple_block(TRACK_VIDEO, rel_ms, pkt.keyframe, &scratch);
                self.frame_scratch = scratch;
            }
            VideoCodec::Av1 | VideoCodec::Vp9 => {
                self.append_simple_block(TRACK_VIDEO, rel_ms, pkt.keyframe, &pkt.data);
            }
            VideoCodec::Unspecified => return Err(CoreError::invalid("unspecified video codec")),
        }

        video_frames.fetch_add(1, Ordering::AcqRel);
        bytes_written.store(self.written_pos + self.cluster_buf.len() as u64, Ordering::Release);
        Ok(true)
    }

    /// Mux one encoded Opus audio packet (ignored until the first video keyframe starts the stream).
    pub fn write_audio(
        &mut self,
        pkt: &EncodedAudio,
        waiting_keyframe: &AtomicBool,
        audio_packets: &AtomicU64,
        bytes_written: &AtomicU64,
    ) -> Result<bool> {
        if !self.cfg.audio_enabled || pkt.data.is_empty() || waiting_keyframe.load(Ordering::Acquire) {
            return Ok(false);
        }

        let pts_ms = self.normalize_pts_ms(pkt.capture_ts.as_micros());
        if !self.cluster_open || pts_ms.saturating_sub(self.cluster_ts_ms) >= CLUSTER_ROTATE_MS {
            self.flush_cluster(bytes_written)?;
            self.start_cluster(pts_ms);
        }
        let rel_ms = pts_ms.saturating_sub(self.cluster_ts_ms).min(i16::MAX as u64) as i16;

        // Opus frames are independently decodable, so Matroska sets SimpleBlock keyframe flag 0x80.
        self.append_simple_block(TRACK_AUDIO, rel_ms, true, &pkt.data);
        audio_packets.fetch_add(1, Ordering::AcqRel);
        bytes_written.store(self.written_pos + self.cluster_buf.len() as u64, Ordering::Release);
        Ok(true)
    }

    /// Flush pending cluster, write Cues, and seek back to finalize Segment size & Duration.
    pub fn finalize(&mut self, bytes_written: &AtomicU64) -> Result<u64> {
        if !self.header_written {
            self.write_headers(None, bytes_written)?;
        }

        self.flush_cluster(bytes_written)?;

        // Write Cues element pointing to every keyframe cluster.
        if !self.cues.is_empty() {
            let mut cues_body = Vec::with_capacity(self.cues.len() * 24);
            for cue in &self.cues {
                let mut pos_body = Vec::with_capacity(16);
                write_uint_element(&mut pos_body, ID_CUE_TRACK, TRACK_VIDEO);
                write_uint_element(&mut pos_body, ID_CUE_CLUSTER_POSITION, cue.cluster_rel_pos);

                let mut pt_body = Vec::with_capacity(24);
                write_uint_element(&mut pt_body, ID_CUE_TIME, cue.time_ms);
                write_master_element(&mut pt_body, ID_CUE_TRACK_POSITIONS, &pos_body);

                write_master_element(&mut cues_body, ID_CUE_POINT, &pt_body);
            }
            let mut cues_el = Vec::with_capacity(cues_body.len() + 12);
            write_master_element(&mut cues_el, ID_CUES, &cues_body);
            self.write_all_tracked(&cues_el)?;
        }

        self.writer.flush().map_err(CoreError::Io)?;
        let end_pos = self.written_pos;
        let segment_payload_len = end_pos.saturating_sub(self.segment_data_offset);

        // Include final frame duration in reported Info.Duration (ms).
        let frame_ms = (1_000.0 / self.cfg.fps.max(1) as f64).max(1.0);
        let duration_ms = if self.base_ts_us.is_some() { self.last_pts_ms as f64 + frame_ms } else { 0.0 };

        let file = self.writer.get_mut();
        file.seek(SeekFrom::Start(self.segment_size_offset)).map_err(CoreError::Io)?;
        file.write_all(&encode_vint_size_fixed8(segment_payload_len)).map_err(CoreError::Io)?;

        file.seek(SeekFrom::Start(self.duration_value_offset)).map_err(CoreError::Io)?;
        file.write_all(&duration_ms.to_be_bytes()).map_err(CoreError::Io)?;

        file.seek(SeekFrom::Start(end_pos)).map_err(CoreError::Io)?;
        file.flush().map_err(CoreError::Io)?;

        bytes_written.store(end_pos, Ordering::Release);
        Ok(end_pos)
    }
}

// ───────────────────── Matroska Validator ─────────────────────

/// Parse and structurally validate a Matroska (`.mkv`) file on disk.
pub fn validate_mkv_file(path: &Path) -> Result<MkvValidationSummary> {
    let data = std::fs::read(path).map_err(CoreError::Io)?;
    let mut summary = MkvValidationSummary {
        file_size: data.len() as u64,
        timestamps_monotonic: true,
        ..Default::default()
    };

    let mut pos = 0usize;
    let mut track_types: HashMap<u64, u64> = HashMap::new();
    let mut last_abs_ts: Option<i64> = None;

    while pos < data.len() {
        let Some(id) = read_element_id(&data, &mut pos) else {
            break;
        };
        let Some(size_opt) = read_vint_size(&data, &mut pos) else {
            break;
        };
        let end = match size_opt {
            Some(sz) => (pos + sz as usize).min(data.len()),
            None => data.len(),
        };

        match id {
            ID_EBML => {
                summary.has_ebml_header = true;
                parse_ebml_header(&data[pos..end], &mut summary);
            }
            ID_SEGMENT => {
                summary.has_segment = true;
                parse_segment(&data[pos..end], &mut summary, &mut track_types, &mut last_abs_ts);
            }
            _ => {}
        }
        pos = end;
    }

    Ok(summary)
}

fn parse_ebml_header(slice: &[u8], summary: &mut MkvValidationSummary) {
    let mut pos = 0usize;
    while pos < slice.len() {
        let Some(id) = read_element_id(slice, &mut pos) else {
            break;
        };
        let Some(Some(sz)) = read_vint_size(slice, &mut pos) else {
            break;
        };
        let end = (pos + sz as usize).min(slice.len());
        if id == ID_DOC_TYPE {
            summary.doc_type = String::from_utf8_lossy(&slice[pos..end]).trim_end_matches('\0').to_string();
        }
        pos = end;
    }
}

fn parse_segment(
    slice: &[u8],
    summary: &mut MkvValidationSummary,
    track_types: &mut HashMap<u64, u64>,
    last_abs_ts: &mut Option<i64>,
) {
    let mut pos = 0usize;
    while pos < slice.len() {
        let Some(id) = read_element_id(slice, &mut pos) else {
            break;
        };
        let Some(size_opt) = read_vint_size(slice, &mut pos) else {
            break;
        };
        let end = match size_opt {
            Some(sz) => (pos + sz as usize).min(slice.len()),
            None => slice.len(),
        };

        match id {
            ID_TRACKS => {
                summary.has_tracks = true;
                parse_tracks(&slice[pos..end], summary, track_types);
            }
            ID_CLUSTER => {
                summary.cluster_count += 1;
                parse_cluster(&slice[pos..end], summary, track_types, last_abs_ts);
            }
            ID_CUES if has_cue_point(&slice[pos..end]) => {
                summary.has_cues = true;
            }
            _ => {}
        }
        pos = end;
    }
}

fn parse_tracks(slice: &[u8], summary: &mut MkvValidationSummary, track_types: &mut HashMap<u64, u64>) {
    let mut pos = 0usize;
    while pos < slice.len() {
        let Some(id) = read_element_id(slice, &mut pos) else {
            break;
        };
        let Some(Some(sz)) = read_vint_size(slice, &mut pos) else {
            break;
        };
        let end = (pos + sz as usize).min(slice.len());

        if id == ID_TRACK_ENTRY {
            let mut tpos = pos;
            let mut track_num = 0u64;
            let mut track_type = 0u64;
            let mut codec_id: Option<String> = None;

            while tpos < end {
                let Some(tid) = read_element_id(slice, &mut tpos) else {
                    break;
                };
                let Some(Some(tsz)) = read_vint_size(slice, &mut tpos) else {
                    break;
                };
                let tend = (tpos + tsz as usize).min(end);
                match tid {
                    ID_TRACK_NUMBER => track_num = read_uint(&slice[tpos..tend]),
                    ID_TRACK_TYPE => track_type = read_uint(&slice[tpos..tend]),
                    ID_CODEC_ID => {
                        codec_id = Some(String::from_utf8_lossy(&slice[tpos..tend]).trim_end_matches('\0').to_string());
                    }
                    _ => {}
                }
                tpos = tend;
            }

            if track_num > 0 && track_type > 0 {
                track_types.insert(track_num, track_type);
            }
            if track_type == TRACK_TYPE_VIDEO {
                summary.video_codec_id = codec_id;
            } else if track_type == TRACK_TYPE_AUDIO {
                summary.audio_codec_id = codec_id;
            }
        }
        pos = end;
    }
}

fn parse_cluster(
    slice: &[u8],
    summary: &mut MkvValidationSummary,
    track_types: &HashMap<u64, u64>,
    last_abs_ts: &mut Option<i64>,
) {
    let mut pos = 0usize;
    let mut cluster_ts = 0u64;

    while pos < slice.len() {
        let Some(id) = read_element_id(slice, &mut pos) else {
            break;
        };
        let Some(Some(sz)) = read_vint_size(slice, &mut pos) else {
            break;
        };
        let end = (pos + sz as usize).min(slice.len());

        match id {
            ID_TIMESTAMP => {
                cluster_ts = read_uint(&slice[pos..end]);
            }
            ID_SIMPLE_BLOCK => {
                let block = &slice[pos..end];
                let mut bpos = 0usize;
                if let Some(track_num) = read_vint_value(block, &mut bpos)
                    && bpos + 3 <= block.len()
                {
                    let rel_ts = i16::from_be_bytes([block[bpos], block[bpos + 1]]);
                    let flags = block[bpos + 2];
                    let abs_ts = cluster_ts as i64 + rel_ts as i64;

                    if let Some(prev) = *last_abs_ts
                        && abs_ts < prev
                    {
                        summary.timestamps_monotonic = false;
                    }
                    *last_abs_ts = Some(abs_ts);

                    let ttype = track_types.get(&track_num).copied().unwrap_or(0);
                    if ttype == TRACK_TYPE_VIDEO {
                        if summary.video_block_count == 0 {
                            summary.first_video_is_keyframe = (flags & 0x80) != 0;
                        }
                        summary.video_block_count += 1;
                    } else if ttype == TRACK_TYPE_AUDIO {
                        summary.audio_block_count += 1;
                    }
                }
            }
            _ => {}
        }
        pos = end;
    }
}

fn has_cue_point(slice: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos < slice.len() {
        let Some(id) = read_element_id(slice, &mut pos) else {
            break;
        };
        let Some(Some(sz)) = read_vint_size(slice, &mut pos) else {
            break;
        };
        let end = (pos + sz as usize).min(slice.len());
        if id == ID_CUE_POINT {
            return true;
        }
        pos = end;
    }
    false
}
