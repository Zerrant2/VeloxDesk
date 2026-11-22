//! Persistent viewer settings (JSON in the user config dir).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use veloxdesk_core_traits::render::{PresentMode, ScaleMode};
use veloxdesk_protocol::control::SessionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Mode {
    #[default]
    Desktop,
    Gaming,
}

impl Mode {
    pub fn to_wire(self) -> SessionMode {
        match self {
            Mode::Desktop => SessionMode::Desktop,
            Mode::Gaming => SessionMode::Gaming,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Mode::Desktop => "Desktop",
            Mode::Gaming => "Gaming",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quality {
    Low,
    #[default]
    Balanced,
    High,
    Ultra,
}

impl Quality {
    pub const ALL: [Quality; 4] = [Quality::Low, Quality::Balanced, Quality::High, Quality::Ultra];
    /// Max bitrate requested from the host.
    pub fn bitrate_kbps(self) -> u32 {
        match self {
            Quality::Low => 5_000,
            Quality::Balanced => 20_000,
            Quality::High => 50_000,
            Quality::Ultra => 100_000,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Quality::Low => "Low (5 Mb/s)",
            Quality::Balanced => "Balanced (20 Mb/s)",
            Quality::High => "High (50 Mb/s)",
            Quality::Ultra => "Ultra (100 Mb/s)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Present {
    #[default]
    Mailbox,
    Immediate,
    Fifo,
}

impl Present {
    pub const ALL: [Present; 3] = [Present::Mailbox, Present::Immediate, Present::Fifo];
    pub fn to_core(self) -> PresentMode {
        match self {
            Present::Mailbox => PresentMode::Mailbox,
            Present::Immediate => PresentMode::Immediate,
            Present::Fifo => PresentMode::Fifo,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Present::Mailbox => "Mailbox (no tearing)",
            Present::Immediate => "Immediate (VSync off)",
            Present::Fifo => "VSync",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Scale {
    #[default]
    Fit,
    Stretch,
    Original,
}

impl Scale {
    pub const ALL: [Scale; 3] = [Scale::Fit, Scale::Stretch, Scale::Original];
    pub fn to_core(self) -> ScaleMode {
        match self {
            Scale::Fit => ScaleMode::Fit,
            Scale::Stretch => ScaleMode::Stretch,
            Scale::Original => ScaleMode::Original,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Scale::Fit => "Fit",
            Scale::Stretch => "Stretch",
            Scale::Original => "Original (1:1)",
        }
    }
}

/// Max entries kept in the recent-connections list.
pub const MAX_RECENT: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub recent: Vec<String>,
    /// Rendezvous server `host[:port]` (default port 21116) for connecting by 9-digit Device ID.
    pub rendezvous_server: String,
    pub mode: Mode,
    pub quality: Quality,
    pub present: Present,
    pub scale: Scale,
    pub show_hud: bool,
    pub prefer_hw_decode: bool,
    /// Gaming mode: lock the pointer and send relative motion.
    pub relative_mouse_in_gaming: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            recent: Vec::new(),
            rendezvous_server: String::new(),
            mode: Mode::Desktop,
            quality: Quality::Balanced,
            present: Present::Mailbox,
            scale: Scale::Fit,
            show_hud: false,
            prefer_hw_decode: true,
            relative_mouse_in_gaming: true,
        }
    }
}

impl Settings {
    /// `<config>/veloxdesk/settings.json` (`%APPDATA%`, `$XDG_CONFIG_HOME`, `~/.config`,
    /// `~/Library/Application Support`).
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("veloxdesk").join("settings.json"))
    }

    pub fn load_or_default() -> Self {
        Self::default_path().and_then(|p| Self::load(&p).ok()).unwrap_or_default()
    }

    pub fn load(path: &std::path::Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(std::io::Error::other)
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Save to the default path, logging failures.
    pub fn save_default(&self) {
        if let Some(p) = Self::default_path()
            && let Err(e) = self.save(&p)
        {
            tracing::warn!("saving settings to {}: {e}", p.display());
        }
    }

    /// Move/insert `addr` to the front of the recent list.
    pub fn push_recent(&mut self, addr: &str) {
        let addr = addr.trim();
        if addr.is_empty() {
            return;
        }
        self.recent.retain(|a| a != addr);
        self.recent.insert(0, addr.to_string());
        self.recent.truncate(MAX_RECENT);
    }
}

/// Per-user configuration directory.
pub fn config_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_list_mru() {
        let mut s = Settings::default();
        for i in 0..15 {
            s.push_recent(&format!("10.0.0.{i}:21118"));
        }
        s.push_recent("10.0.0.10:21118");
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], "10.0.0.10:21118");
        assert_eq!(s.recent.iter().filter(|a| *a == "10.0.0.10:21118").count(), 1);
    }

    #[test]
    fn roundtrip_and_partial_json() {
        let dir = std::env::temp_dir().join(format!("veloxdesk-ui-test-{}", std::process::id()));
        let path = dir.join("s.json");
        let mut s = Settings { mode: Mode::Gaming, ..Default::default() };
        s.push_recent("host:1");
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        // Unknown / missing fields fall back to defaults.
        let p: Settings = serde_json::from_str(r#"{"show_hud": true, "future": 1}"#).unwrap();
        assert!(p.show_hud);
        assert_eq!(p.quality, Quality::Balanced);
        let _ = std::fs::remove_dir_all(dir);
    }
}
