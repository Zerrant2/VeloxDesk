//! Host configuration: defaults < TOML file < command-line flags.
//!
//! `apps/client` embeds [`HostArgs`] with `#[command(flatten)]` and calls
//! [`HostArgs::resolve`] to get a [`HostConfig`] for [`crate::run_host`].

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use veloxdesk_protocol::DEFAULT_DIRECT_PORT;
use veloxdesk_protocol::control::VideoCodec;

/// Everything the host pipeline needs. Construct with [`HostConfig::default`],
/// [`HostConfig::from_toml_file`] or [`HostArgs::resolve`].
#[derive(Debug, Clone, PartialEq)]
pub struct HostConfig {
    /// UDP address the QUIC listener binds to.
    pub bind: SocketAddr,
    /// Upper bound for the video frame rate (also capped by the display refresh rate and
    /// the viewer's refresh rate).
    pub max_fps: u32,
    /// Start / maximum video bitrate (ABR moves below it).
    pub bitrate_kbps: u32,
    /// Lowest bitrate the ABR may choose.
    pub min_bitrate_kbps: u32,
    /// Preferred codec (falls back to H.264 if the viewer or the encoder can't do it).
    pub codec: VideoCodec,
    /// Prefer hardware encoders (falls back to software).
    pub prefer_hw: bool,
    /// Prefer GPU-resident capture frames (zero-copy into the encoder).
    pub prefer_gpu: bool,
    /// Display index to stream; `None` = primary.
    pub display: Option<u32>,
    /// Frame rate used to refresh a static screen (re-encode of the last frame), so the
    /// stream stays alive and lost frames heal; 0 = never re-send.
    pub idle_fps: u32,
    /// Initial FEC parity percentage (ABR adapts it).
    pub fec_parity_percent: u8,
    /// Concurrent viewer sessions (desktop duplication generally allows one).
    pub max_sessions: usize,
    /// Allow remote keyboard/mouse.
    pub allow_input: bool,
    /// Path of the device identity key file (`None` = platform default chosen by the app).
    pub identity_path: Option<PathBuf>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([0, 0, 0, 0], DEFAULT_DIRECT_PORT)),
            max_fps: 60,
            bitrate_kbps: 20_000,
            min_bitrate_kbps: 1_000,
            codec: VideoCodec::H264,
            prefer_hw: true,
            prefer_gpu: true,
            display: None,
            idle_fps: 5,
            fec_parity_percent: 10,
            max_sessions: 1,
            allow_input: true,
            identity_path: None,
        }
    }
}

/// TOML file layout (every key optional). Example:
/// ```toml
/// bind = "0.0.0.0:21118"
/// max_fps = 120
/// bitrate_kbps = 40000
/// codec = "h265"
/// prefer_hw = true
/// display = 0
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfigFile {
    pub bind: Option<SocketAddr>,
    pub max_fps: Option<u32>,
    pub bitrate_kbps: Option<u32>,
    pub min_bitrate_kbps: Option<u32>,
    pub codec: Option<String>,
    pub prefer_hw: Option<bool>,
    pub prefer_gpu: Option<bool>,
    pub display: Option<u32>,
    pub idle_fps: Option<u32>,
    pub fec_parity_percent: Option<u8>,
    pub max_sessions: Option<usize>,
    pub allow_input: Option<bool>,
    pub identity_path: Option<PathBuf>,
}

/// Parse a codec name: `h264`/`avc`, `h265`/`hevc`, `av1`, `vp9` (case-insensitive).
pub fn parse_codec(s: &str) -> anyhow::Result<VideoCodec> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "h264" | "avc" | "h.264" => VideoCodec::H264,
        "h265" | "hevc" | "h.265" => VideoCodec::H265,
        "av1" => VideoCodec::Av1,
        "vp9" => VideoCodec::Vp9,
        other => bail!("unknown codec {other:?} (expected h264, h265, av1, vp9)"),
    })
}

/// Lower-case codec name (inverse of [`parse_codec`]).
pub fn codec_name(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "h264",
        VideoCodec::H265 => "h265",
        VideoCodec::Av1 => "av1",
        VideoCodec::Vp9 => "vp9",
        VideoCodec::Unspecified => "unspecified",
    }
}

impl HostConfig {
    /// Overlay the keys present in `file` onto `self`.
    pub fn apply_file(&mut self, file: &HostConfigFile) -> anyhow::Result<()> {
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = file.$f.clone() { self.$f = v; } )* };
        }
        set!(bind, max_fps, bitrate_kbps, min_bitrate_kbps, prefer_hw, prefer_gpu, idle_fps,
             fec_parity_percent, max_sessions, allow_input);
        if let Some(d) = file.display {
            self.display = Some(d);
        }
        if let Some(p) = &file.identity_path {
            self.identity_path = Some(p.clone());
        }
        if let Some(c) = &file.codec {
            self.codec = parse_codec(c)?;
        }
        Ok(())
    }

    /// Defaults overlaid with a TOML string.
    pub fn from_toml_str(s: &str) -> anyhow::Result<Self> {
        let file: HostConfigFile = toml::from_str(s).context("parsing host config TOML")?;
        let mut cfg = Self::default();
        cfg.apply_file(&file)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Defaults overlaid with a TOML file.
    pub fn from_toml_file(path: &Path) -> anyhow::Result<Self> {
        let s = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_toml_str(&s).with_context(|| format!("in {}", path.display()))
    }

    /// Serialize to the TOML file format (useful for `--print-config`).
    pub fn to_toml(&self) -> String {
        let f = HostConfigFile {
            bind: Some(self.bind),
            max_fps: Some(self.max_fps),
            bitrate_kbps: Some(self.bitrate_kbps),
            min_bitrate_kbps: Some(self.min_bitrate_kbps),
            codec: Some(codec_name(self.codec).to_string()),
            prefer_hw: Some(self.prefer_hw),
            prefer_gpu: Some(self.prefer_gpu),
            display: self.display,
            idle_fps: Some(self.idle_fps),
            fec_parity_percent: Some(self.fec_parity_percent),
            max_sessions: Some(self.max_sessions),
            allow_input: Some(self.allow_input),
            identity_path: self.identity_path.clone(),
        };
        toml::to_string(&f).unwrap_or_default()
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if !(1..=1000).contains(&self.max_fps) {
            bail!("max_fps must be 1..=1000, got {}", self.max_fps);
        }
        if self.bitrate_kbps < 100 {
            bail!("bitrate_kbps must be >= 100, got {}", self.bitrate_kbps);
        }
        if self.min_bitrate_kbps == 0 || self.min_bitrate_kbps > self.bitrate_kbps {
            bail!("min_bitrate_kbps must be in 1..=bitrate_kbps");
        }
        if self.fec_parity_percent > 100 {
            bail!("fec_parity_percent must be <= 100");
        }
        if self.max_sessions == 0 {
            bail!("max_sessions must be >= 1");
        }
        if self.codec == VideoCodec::Unspecified {
            bail!("codec must be specified");
        }
        Ok(())
    }
}

/// Host flags for the `veloxdesk` binary (`#[command(flatten)]`).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct HostArgs {
    /// Host config TOML file (flags override its values).
    #[arg(long = "config", value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// UDP bind address for incoming sessions.
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<SocketAddr>,
    /// Maximum frame rate.
    #[arg(long, value_name = "FPS")]
    pub max_fps: Option<u32>,
    /// Start/maximum video bitrate in kbit/s.
    #[arg(long = "bitrate", value_name = "KBPS")]
    pub bitrate_kbps: Option<u32>,
    /// Video codec: h264, h265, av1, vp9.
    #[arg(long, value_name = "CODEC")]
    pub codec: Option<String>,
    /// Force the software encoder.
    #[arg(long)]
    pub no_hw: bool,
    /// Force CPU capture frames (disable the zero-copy GPU path).
    #[arg(long)]
    pub cpu_capture: bool,
    /// Display index to stream (default: primary).
    #[arg(long, value_name = "INDEX")]
    pub display: Option<u32>,
    /// Refuse remote keyboard/mouse (view only).
    #[arg(long)]
    pub view_only: bool,
}

impl HostArgs {
    /// Defaults < `--config` file < flags.
    pub fn resolve(&self) -> anyhow::Result<HostConfig> {
        let mut cfg = match &self.config {
            Some(p) => HostConfig::from_toml_file(p)?,
            None => HostConfig::default(),
        };
        if let Some(v) = self.bind {
            cfg.bind = v;
        }
        if let Some(v) = self.max_fps {
            cfg.max_fps = v;
        }
        if let Some(v) = self.bitrate_kbps {
            cfg.bitrate_kbps = v;
            cfg.min_bitrate_kbps = cfg.min_bitrate_kbps.min(v);
        }
        if let Some(c) = &self.codec {
            cfg.codec = parse_codec(c)?;
        }
        if self.no_hw {
            cfg.prefer_hw = false;
        }
        if self.cpu_capture {
            cfg.prefer_gpu = false;
        }
        if let Some(d) = self.display {
            cfg.display = Some(d);
        }
        if self.view_only {
            cfg.allow_input = false;
        }
        cfg.validate()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Cli {
        #[arg(long)]
        host: bool,
        #[command(flatten)]
        h: HostArgs,
    }

    #[test]
    fn defaults_valid() {
        let c = HostConfig::default();
        c.validate().unwrap();
        assert_eq!(c.bind.port(), DEFAULT_DIRECT_PORT);
    }

    #[test]
    fn toml_overlay_and_roundtrip() {
        let c = HostConfig::from_toml_str(
            "bind = \"127.0.0.1:5000\"\nmax_fps = 120\ncodec = \"HEVC\"\ndisplay = 1\nprefer_hw = false\n",
        )
        .unwrap();
        assert_eq!(c.bind, "127.0.0.1:5000".parse().unwrap());
        assert_eq!(c.max_fps, 120);
        assert_eq!(c.codec, VideoCodec::H265);
        assert_eq!(c.display, Some(1));
        assert!(!c.prefer_hw);
        assert_eq!(c.bitrate_kbps, HostConfig::default().bitrate_kbps);
        let back = HostConfig::from_toml_str(&c.to_toml()).unwrap();
        assert_eq!(back, c);
        assert!(HostConfig::from_toml_str("bogus = 1").is_err());
        assert!(HostConfig::from_toml_str("codec = \"mpeg2\"").is_err());
        assert!(HostConfig::from_toml_str("max_fps = 0").is_err());
    }

    #[test]
    fn cli_overrides_file() {
        let dir = std::env::temp_dir().join(format!("vdk-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("host.toml");
        std::fs::write(&path, "max_fps = 30\nbitrate_kbps = 8000\ncodec = \"h265\"\n").unwrap();
        let cli = Cli::try_parse_from([
            "veloxdesk",
            "--host",
            "--config",
            path.to_str().unwrap(),
            "--max-fps",
            "144",
            "--no-hw",
            "--bind",
            "0.0.0.0:7000",
        ])
        .unwrap();
        assert!(cli.host);
        let c = cli.h.resolve().unwrap();
        assert_eq!(c.max_fps, 144);
        assert_eq!(c.bitrate_kbps, 8000);
        assert_eq!(c.codec, VideoCodec::H265);
        assert!(!c.prefer_hw);
        assert_eq!(c.bind.port(), 7000);
        std::fs::remove_dir_all(&dir).ok();
    }
}
