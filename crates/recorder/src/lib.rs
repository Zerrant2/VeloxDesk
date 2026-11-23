//! # veloxdesk-recorder
//!
//! Zero re-encode client-side session recorder: muxes incoming encoded video
//! packets (H.264 / HEVC / AV1) and Opus audio packets directly into a standard
//! Matroska (`.mkv`) container on a dedicated IO thread without adding latency
//! to the render path.
//!
//! Contract skeleton frozen at Phase 0; full EBML/Matroska muxer implemented by
//! `audio-rec-worker`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use veloxdesk_core_traits::audio::EncodedAudio;
use veloxdesk_core_traits::video::{EncodedPacket, VideoCodec};
use veloxdesk_core_traits::{CoreError, Result};

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

/// Non-blocking zero re-encode session recorder (dedicated IO thread).
pub struct SessionRecorder {
    output_path: PathBuf,
    started_at: Instant,
    active: Arc<AtomicBool>,
    waiting_keyframe: Arc<AtomicBool>,
    bytes_written: Arc<AtomicU64>,
    video_frames: Arc<AtomicU64>,
    audio_packets: Arc<AtomicU64>,
    dropped_packets: Arc<AtomicU64>,
}

impl SessionRecorder {
    /// Start recording to `cfg.output_path` on a background IO thread.
    pub fn start(cfg: RecorderConfig) -> Result<Self> {
        if let Some(parent) = cfg.output_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(CoreError::Io)?;
        }
        Ok(Self {
            output_path: cfg.output_path,
            started_at: Instant::now(),
            active: Arc::new(AtomicBool::new(true)),
            waiting_keyframe: Arc::new(AtomicBool::new(true)),
            bytes_written: Arc::new(AtomicU64::new(0)),
            video_frames: Arc::new(AtomicU64::new(0)),
            audio_packets: Arc::new(AtomicU64::new(0)),
            dropped_packets: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Non-blocking enqueue of an encoded video packet (Annex-B H.264/HEVC or AV1 OBU).
    pub fn push_video(&self, _pkt: &EncodedPacket) {}

    /// Non-blocking enqueue of an encoded Opus audio packet.
    pub fn push_audio(&self, _pkt: &EncodedAudio) {}

    /// Snapshot current recording statistics.
    pub fn stats(&self) -> RecorderStats {
        RecorderStats {
            active: self.active.load(Ordering::Acquire),
            waiting_for_keyframe: self.waiting_keyframe.load(Ordering::Acquire),
            elapsed_ms: self.started_at.elapsed().as_millis() as u64,
            bytes_written: self.bytes_written.load(Ordering::Acquire),
            video_frames: self.video_frames.load(Ordering::Acquire),
            audio_packets: self.audio_packets.load(Ordering::Acquire),
            dropped_packets: self.dropped_packets.load(Ordering::Acquire),
            output_path: self.output_path.clone(),
        }
    }

    /// Flush IO thread, finalize container metadata, and return final statistics.
    pub fn stop(self) -> Result<RecorderStats> {
        self.active.store(false, Ordering::Release);
        Ok(self.stats())
    }
}

/// Validate a Matroska `.mkv` file on disk and return its structural summary.
pub fn validate_mkv_file(path: &Path) -> Result<MkvValidationSummary> {
    let meta = std::fs::metadata(path).map_err(CoreError::Io)?;
    Ok(MkvValidationSummary { file_size: meta.len(), ..Default::default() })
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
