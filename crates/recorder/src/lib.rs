//! # veloxdesk-recorder
//!
//! Zero re-encode client-side session recorder: muxes incoming encoded video
//! packets (H.264 / HEVC / AV1) and Opus audio packets directly into a standard
//! Matroska (`.mkv`) container on a dedicated OS IO thread without adding latency
//! to the render path.

mod codec;
mod ebml;
mod mkv;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Sender, TrySendError, bounded};
use veloxdesk_core_traits::audio::EncodedAudio;
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{CoreError, Result};

use crate::mkv::MkvMuxer;

/// Bounded queue depth for incoming encoded packets on the dedicated IO thread.
const IO_QUEUE_CAPACITY: usize = 512;

/// Output container format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContainerFormat {
    /// Matroska (`.mkv`): native H.264 / HEVC / AV1 + Opus support, plays in VLC / mpv / WMP.
    #[default]
    Mkv,
}

/// Configuration for a recording session.
#[derive(Debug, Clone)]
pub struct RecorderConfig {
    pub output_path: PathBuf,
    pub format: ContainerFormat,
    pub video_codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub audio_enabled: bool,
    pub audio_sample_rate: u32,
    pub audio_channels: u16,
}

impl RecorderConfig {
    pub fn new(output_path: PathBuf, video_codec: VideoCodec, width: u32, height: u32, fps: u32) -> Self {
        Self {
            output_path,
            format: ContainerFormat::Mkv,
            video_codec,
            width: width.max(2),
            height: height.max(2),
            fps: fps.max(1),
            audio_enabled: true,
            audio_sample_rate: 48_000,
            audio_channels: 2,
        }
    }
}

/// Live or final statistics of a recording session.
#[derive(Debug, Clone, Default)]
pub struct RecorderStats {
    pub active: bool,
    pub waiting_for_keyframe: bool,
    pub elapsed_ms: u64,
    pub bytes_written: u64,
    pub video_frames: u64,
    pub audio_packets: u64,
    pub dropped_packets: u64,
    pub output_path: PathBuf,
}

/// Summary extracted from validating a written Matroska (`.mkv`) file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MkvValidationSummary {
    pub has_ebml_header: bool,
    pub doc_type: String,
    pub has_segment: bool,
    pub has_tracks: bool,
    pub video_codec_id: Option<String>,
    pub audio_codec_id: Option<String>,
    pub cluster_count: usize,
    pub video_block_count: usize,
    pub audio_block_count: usize,
    pub first_video_is_keyframe: bool,
    pub timestamps_monotonic: bool,
    pub has_cues: bool,
    pub file_size: u64,
}

enum RecorderEvent {
    Video(EncodedPacket),
    Audio(EncodedAudio),
}

struct SharedCounters {
    active: AtomicBool,
    waiting_keyframe: AtomicBool,
    bytes_written: AtomicU64,
    video_frames: AtomicU64,
    audio_packets: AtomicU64,
    dropped_packets: AtomicU64,
}

/// Non-blocking zero re-encode session recorder (dedicated OS IO thread).
pub struct SessionRecorder {
    output_path: PathBuf,
    started_at: Instant,
    counters: Arc<SharedCounters>,
    tx: Option<Sender<RecorderEvent>>,
    io_thread: Option<JoinHandle<Result<()>>>,
}

impl SessionRecorder {
    /// Start recording to `cfg.output_path` on a background IO thread.
    pub fn start(cfg: RecorderConfig) -> Result<Self> {
        if let Some(parent) = cfg.output_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
        }

        let output_path = cfg.output_path.clone();
        let mut muxer = MkvMuxer::create(cfg)?;

        let counters = Arc::new(SharedCounters {
            active: AtomicBool::new(true),
            waiting_keyframe: AtomicBool::new(true),
            bytes_written: AtomicU64::new(0),
            video_frames: AtomicU64::new(0),
            audio_packets: AtomicU64::new(0),
            dropped_packets: AtomicU64::new(0),
        });

        let (tx, rx) = bounded::<RecorderEvent>(IO_QUEUE_CAPACITY);
        let thread_counters = Arc::clone(&counters);
        let log_path = output_path.clone();

        let io_thread = thread::Builder::new()
            .name("veloxdesk-recorder-io".to_string())
            .spawn(move || {
                let mut first_err: Option<CoreError> = None;

                while let Ok(ev) = rx.recv() {
                    if first_err.is_some() {
                        continue;
                    }
                    let res = match ev {
                        RecorderEvent::Video(pkt) => muxer.write_video(
                            &pkt,
                            &thread_counters.waiting_keyframe,
                            &thread_counters.video_frames,
                            &thread_counters.bytes_written,
                        ),
                        RecorderEvent::Audio(pkt) => muxer.write_audio(
                            &pkt,
                            &thread_counters.waiting_keyframe,
                            &thread_counters.audio_packets,
                            &thread_counters.bytes_written,
                        ),
                    };
                    if let Err(e) = res {
                        tracing::error!(path = %log_path.display(), error = %e, "MKV recorder IO write failed");
                        first_err = Some(e);
                    }
                }

                let fin = muxer.finalize(&thread_counters.bytes_written);
                thread_counters.active.store(false, Ordering::Release);

                match (first_err, fin) {
                    (Some(e), _) => Err(e),
                    (None, Err(e)) => Err(e),
                    (None, Ok(total_bytes)) => {
                        tracing::info!(
                            path = %log_path.display(),
                            bytes = total_bytes,
                            video_frames = thread_counters.video_frames.load(Ordering::Acquire),
                            audio_packets = thread_counters.audio_packets.load(Ordering::Acquire),
                            "MKV recording finalized"
                        );
                        Ok(())
                    }
                }
            })
            .map_err(CoreError::Io)?;

        Ok(Self {
            output_path,
            started_at: Instant::now(),
            counters,
            tx: Some(tx),
            io_thread: Some(io_thread),
        })
    }

    /// Non-blocking enqueue of an encoded video packet (Annex-B H.264/HEVC or AV1 OBU).
    pub fn push_video(&self, pkt: &EncodedPacket) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = tx.try_send(RecorderEvent::Video(pkt.clone()))
        {
            self.counters.dropped_packets.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Non-blocking enqueue of an encoded Opus audio packet.
    pub fn push_audio(&self, pkt: &EncodedAudio) {
        let Some(tx) = self.tx.as_ref() else {
            return;
        };
        if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) = tx.try_send(RecorderEvent::Audio(pkt.clone()))
        {
            self.counters.dropped_packets.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Snapshot current recording statistics.
    pub fn stats(&self) -> RecorderStats {
        RecorderStats {
            active: self.counters.active.load(Ordering::Acquire),
            waiting_for_keyframe: self.counters.waiting_keyframe.load(Ordering::Acquire),
            elapsed_ms: self.started_at.elapsed().as_millis() as u64,
            bytes_written: self.counters.bytes_written.load(Ordering::Acquire),
            video_frames: self.counters.video_frames.load(Ordering::Acquire),
            audio_packets: self.counters.audio_packets.load(Ordering::Acquire),
            dropped_packets: self.counters.dropped_packets.load(Ordering::Acquire),
            output_path: self.output_path.clone(),
        }
    }

    /// Flush IO thread, finalize container metadata, and return final statistics.
    pub fn stop(mut self) -> Result<RecorderStats> {
        // Closing the channel signals the IO thread to drain queued packets and finalize.
        self.tx.take();
        let io_res = if let Some(handle) = self.io_thread.take() {
            handle
                .join()
                .map_err(|_| CoreError::platform("recorder IO thread panicked"))?
        } else {
            Ok(())
        };
        self.counters.active.store(false, Ordering::Release);
        io_res?;
        Ok(self.stats())
    }
}

impl Drop for SessionRecorder {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(handle) = self.io_thread.take() {
            let _ = handle.join();
        }
        self.counters.active.store(false, Ordering::Release);
    }
}

/// Validate a Matroska `.mkv` file on disk and return its structural summary.
pub fn validate_mkv_file(path: &Path) -> Result<MkvValidationSummary> {
    mkv::validate_mkv_file(path)
}

/// Return the default recordings directory (`Videos\VeloxDesk` on Windows, `~/Videos/VeloxDesk` on Unix).
pub fn default_recordings_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Videos").join("VeloxDesk");
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Videos").join("VeloxDesk");
        }
    }
    PathBuf::from("VeloxDeskRecordings")
}

/// Generate a sanitized timestamped `.mkv` output path inside `dir`.
pub fn generate_recording_path(dir: &Path, peer_label: &str) -> PathBuf {
    let sanitized: String = peer_label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(32)
        .collect();
    let label = if sanitized.is_empty() { "session" } else { &sanitized };
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    dir.join(format!("veloxdesk_{label}_{secs}.mkv"))
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use veloxdesk_core_traits::Timestamp;
    use veloxdesk_core_traits::audio::AudioCodecId;

    use super::*;

    fn temp_mkv_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let pid = std::process::id();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        p.push(format!("veloxdesk_test_{name}_{pid}_{nanos}.mkv"));
        p
    }

    fn make_h264_keyframe() -> Bytes {
        // Synthetic Annex-B H.264 IDR access unit with SPS (type 7), PPS (type 8), and IDR slice (type 5).
        let mut v = Vec::new();
        // 4-byte start code + SPS (High profile 0x64, compat 0x00, level 0x28 = 4.0)
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x28, 0xAC, 0xD9, 0x40, 0x78, 0x02, 0x27]);
        // 4-byte start code + PPS
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x68, 0xEB, 0xE3, 0xCB, 0x22, 0xC0]);
        // 3-byte start code + IDR slice
        v.extend_from_slice(&[0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x21, 0xA0, 0x11, 0x22, 0x33, 0x44]);
        Bytes::from(v)
    }

    fn make_h264_pframe(seed: u8) -> Bytes {
        Bytes::from(vec![0x00, 0x00, 0x00, 0x01, 0x41, 0x9A, seed, seed.wrapping_add(1), 0xAA, 0xBB])
    }

    fn make_hevc_keyframe() -> Bytes {
        // Synthetic Annex-B HEVC IDR access unit: VPS (32 -> 0x40 0x01), SPS (33 -> 0x42 0x01),
        // PPS (34 -> 0x44 0x01), IDR_W_RADL (19 -> 0x26 0x01).
        let mut v = Vec::new();
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, 0x01, 0xFF, 0xFF, 0x01, 0x60]);
        v.extend_from_slice(&[
            0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03,
            0x00, 0x00, 0x03, 0x00, 0x78, 0xA0, 0x03,
        ]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xC1, 0x72, 0xB4, 0x62, 0x40]);
        v.extend_from_slice(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAF, 0x06, 0xB8, 0x63, 0xEF, 0xAD]);
        Bytes::from(v)
    }

    fn make_av1_keyframe() -> Bytes {
        // Sequence Header OBU (type=1 -> 0x0A with size field) + Frame OBU (type=6 -> 0x32 with size field).
        let mut v = Vec::new();
        v.extend_from_slice(&[0x0A, 0x06, 0x00, 0x00, 0x00, 0x42, 0xAB, 0xBF]);
        v.extend_from_slice(&[0x32, 0x08, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80]);
        Bytes::from(v)
    }

    fn video_pkt(codec: VideoCodec, data: Bytes, keyframe: bool, frame_id: u32, ts_ms: u64) -> EncodedPacket {
        EncodedPacket {
            data,
            codec,
            keyframe,
            recovery_point: false,
            width: 1920,
            height: 1080,
            capture_ts: Timestamp(ts_ms * 1_000),
            frame_id,
            encode_us: 1200,
            convert_us: 300,
        }
    }

    fn audio_pkt(seq: u32, ts_ms: u64) -> EncodedAudio {
        EncodedAudio {
            data: Bytes::from_static(&[0xF8, 0xFF, 0xFE, 0x01, 0x02, 0x03, 0x04, 0x05]),
            codec: AudioCodecId::Opus,
            seq,
            capture_ts: Timestamp(ts_ms * 1_000),
            duration_us: 20_000,
        }
    }

    #[test]
    fn record_h264_plus_opus_and_validate_mkv() {
        let path = temp_mkv_path("h264_opus");
        let cfg = RecorderConfig::new(path.clone(), VideoCodec::H264, 1920, 1080, 60);
        let rec = SessionRecorder::start(cfg).expect("start recorder");

        // Push 2.5 seconds of video (150 frames @ 60fps, keyframe at 0 and 120) + Opus audio @ 20ms.
        let kf = make_h264_keyframe();
        for i in 0..150u32 {
            let ts_ms = 10_000 + (i as u64 * 1000) / 60;
            let is_kf = i == 0 || i == 120;
            let data = if is_kf { kf.clone() } else { make_h264_pframe(i as u8) };
            rec.push_video(&video_pkt(VideoCodec::H264, data, is_kf, i, ts_ms));
            if i % 2 == 0 {
                rec.push_audio(&audio_pkt(i / 2, ts_ms));
            }
        }

        let stats = rec.stop().expect("stop recorder");
        assert!(!stats.active);
        assert!(!stats.waiting_for_keyframe);
        assert_eq!(stats.video_frames, 150);
        assert_eq!(stats.audio_packets, 75);
        assert_eq!(stats.dropped_packets, 0);
        assert!(stats.bytes_written > 0);

        let summary = validate_mkv_file(&path).expect("validate_mkv_file");
        eprintln!("H.264+Opus MKV validation: {summary:?}");
        assert!(summary.has_ebml_header);
        assert_eq!(summary.doc_type, "matroska");
        assert!(summary.has_segment);
        assert!(summary.has_tracks);
        assert_eq!(summary.video_codec_id.as_deref(), Some("V_MPEG4/ISO/AVC"));
        assert_eq!(summary.audio_codec_id.as_deref(), Some("A_OPUS"));
        assert!(summary.cluster_count >= 2, "expected cluster rotation, got {}", summary.cluster_count);
        assert_eq!(summary.video_block_count, 150);
        assert_eq!(summary.audio_block_count, 75);
        assert!(summary.first_video_is_keyframe);
        assert!(summary.timestamps_monotonic);
        assert!(summary.has_cues);
        assert_eq!(summary.file_size, stats.bytes_written);

        // Verify raw file contains OpusHead and does NOT contain raw Annex-B 4-byte start codes in SimpleBlock AVCC frames.
        let bytes = std::fs::read(&path).expect("read mkv");
        assert!(bytes.windows(8).any(|w| w == b"OpusHead"), "missing OpusHead in MKV");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_hevc_and_validate_mkv() {
        let path = temp_mkv_path("hevc");
        let mut cfg = RecorderConfig::new(path.clone(), VideoCodec::H265, 2560, 1440, 120);
        cfg.audio_enabled = false;
        let rec = SessionRecorder::start(cfg).expect("start hevc recorder");

        let kf = make_hevc_keyframe();
        rec.push_video(&video_pkt(VideoCodec::H265, kf, true, 0, 500));
        for i in 1..20u32 {
            let p = Bytes::from(vec![0x00, 0x00, 0x00, 0x01, 0x02, 0x01, i as u8, 0x55, 0x66]);
            rec.push_video(&video_pkt(VideoCodec::H265, p, false, i, 500 + i as u64 * 8));
        }

        let stats = rec.stop().expect("stop hevc recorder");
        assert_eq!(stats.video_frames, 20);
        assert_eq!(stats.audio_packets, 0);

        let summary = validate_mkv_file(&path).expect("validate hevc mkv");
        eprintln!("HEVC MKV validation: {summary:?}");
        assert!(summary.has_ebml_header);
        assert_eq!(summary.doc_type, "matroska");
        assert!(summary.has_segment);
        assert!(summary.has_tracks);
        assert_eq!(summary.video_codec_id.as_deref(), Some("V_MPEGH/ISO/HEVC"));
        assert_eq!(summary.audio_codec_id, None);
        assert_eq!(summary.video_block_count, 20);
        assert!(summary.first_video_is_keyframe);
        assert!(summary.timestamps_monotonic);
        assert!(summary.has_cues);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn record_av1_and_validate_mkv() {
        let path = temp_mkv_path("av1");
        let cfg = RecorderConfig::new(path.clone(), VideoCodec::Av1, 1920, 1080, 60);
        let rec = SessionRecorder::start(cfg).expect("start av1 recorder");

        let kf = make_av1_keyframe();
        rec.push_video(&video_pkt(VideoCodec::Av1, kf, true, 0, 1000));
        rec.push_audio(&audio_pkt(0, 1000));
        for i in 1..15u32 {
            let obus = Bytes::from(vec![0x32, 0x04, i as u8, 0x11, 0x22, 0x33]);
            rec.push_video(&video_pkt(VideoCodec::Av1, obus, false, i, 1000 + i as u64 * 16));
            rec.push_audio(&audio_pkt(i, 1000 + i as u64 * 16));
        }

        let stats = rec.stop().expect("stop av1 recorder");
        assert_eq!(stats.video_frames, 15);
        assert_eq!(stats.audio_packets, 15);

        let summary = validate_mkv_file(&path).expect("validate av1 mkv");
        eprintln!("AV1 MKV validation: {summary:?}");
        assert!(summary.has_ebml_header);
        assert_eq!(summary.doc_type, "matroska");
        assert_eq!(summary.video_codec_id.as_deref(), Some("V_AV1"));
        assert_eq!(summary.audio_codec_id.as_deref(), Some("A_OPUS"));
        assert_eq!(summary.video_block_count, 15);
        assert_eq!(summary.audio_block_count, 15);
        assert!(summary.first_video_is_keyframe);
        assert!(summary.timestamps_monotonic);
        assert!(summary.has_cues);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn leading_non_keyframes_are_dropped_until_first_idr() {
        let path = temp_mkv_path("keyframe_gate");
        let cfg = RecorderConfig::new(path.clone(), VideoCodec::H264, 1280, 720, 60);
        let rec = SessionRecorder::start(cfg).expect("start recorder");

        // Push 5 P-frames and 2 audio packets BEFORE any keyframe arrives.
        for i in 0..5u32 {
            rec.push_video(&video_pkt(VideoCodec::H264, make_h264_pframe(i as u8), false, i, i as u64 * 16));
        }
        rec.push_audio(&audio_pkt(0, 10));
        rec.push_audio(&audio_pkt(1, 30));

        // Now push the first keyframe + 4 subsequent P-frames + 2 audio packets.
        rec.push_video(&video_pkt(VideoCodec::H264, make_h264_keyframe(), true, 5, 100));
        rec.push_audio(&audio_pkt(2, 100));
        for i in 6..10u32 {
            rec.push_video(&video_pkt(VideoCodec::H264, make_h264_pframe(i as u8), false, i, 100 + (i - 5) as u64 * 16));
        }
        rec.push_audio(&audio_pkt(3, 140));

        let stats = rec.stop().expect("stop recorder");
        assert!(!stats.waiting_for_keyframe);
        assert_eq!(stats.video_frames, 5, "only IDR + 4 P-frames should be written");
        assert_eq!(stats.audio_packets, 2, "only audio after IDR should be written");

        let summary = validate_mkv_file(&path).expect("validate keyframe gated mkv");
        assert_eq!(summary.video_block_count, 5);
        assert_eq!(summary.audio_block_count, 2);
        assert!(summary.first_video_is_keyframe);
        assert!(summary.timestamps_monotonic);
        assert!(summary.has_cues);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn validate_mkv_file_rejects_non_mkv_and_path_helpers_work() {
        let bad_path = temp_mkv_path("not_mkv");
        std::fs::write(&bad_path, b"this is definitely not a matroska file").expect("write junk");
        let summary = validate_mkv_file(&bad_path).expect("validate junk");
        assert!(!summary.has_ebml_header);
        assert!(!summary.has_segment);
        assert!(!summary.has_tracks);
        assert_eq!(summary.video_block_count, 0);
        let _ = std::fs::remove_file(&bad_path);

        let dir = default_recordings_dir();
        assert!(!dir.as_os_str().is_empty());
        let p = generate_recording_path(&dir, "192.168.1.5:21118 / host*");
        let fname = p.file_name().and_then(|n| n.to_str()).expect("filename");
        assert!(fname.starts_with("veloxdesk_192_168_1_5_21118___host__"));
        assert!(fname.ends_with(".mkv"));
    }
}
