//! Persistent viewer settings (JSON in the user config dir).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use veloxdesk_core_traits::render::{PresentMode, ScaleMode};
use veloxdesk_protocol::control::SessionMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    #[default]
    En,
    Ru,
}

impl Language {
    pub const ALL: [Language; 2] = [Language::En, Language::Ru];

    /// Native autonym displayed in language switchers regardless of active UI language.
    pub fn label(self) -> &'static str {
        match self {
            Language::En => "English",
            Language::Ru => "Русский",
        }
    }

    /// Compact code for narrow header switchers (`EN` / `RU`).
    pub fn short_label(self) -> &'static str {
        match self {
            Language::En => "EN",
            Language::Ru => "RU",
        }
    }

    pub const fn to_u8(self) -> u8 {
        match self {
            Language::En => 0,
            Language::Ru => 1,
        }
    }

    pub const fn from_u8(v: u8) -> Self {
        match v {
            1 => Language::Ru,
            _ => Language::En,
        }
    }
}

/// Desktop UI color theme preference (`System` follows the OS dark/light mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ThemePreference {
    #[default]
    System,
    Dark,
    Light,
}

impl ThemePreference {
    pub const ALL: [ThemePreference; 3] = [
        ThemePreference::System,
        ThemePreference::Dark,
        ThemePreference::Light,
    ];

    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            ThemePreference::System => t.theme_system,
            ThemePreference::Dark => t.theme_dark,
            ThemePreference::Light => t.theme_light,
        }
    }

    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
    }

    /// Cycle through `System -> Dark -> Light -> System`.
    pub const fn next(self) -> Self {
        match self {
            ThemePreference::System => ThemePreference::Dark,
            ThemePreference::Dark => ThemePreference::Light,
            ThemePreference::Light => ThemePreference::System,
        }
    }

    /// Convert to [`egui::ThemePreference`].
    pub const fn to_egui(self) -> egui::ThemePreference {
        match self {
            ThemePreference::System => egui::ThemePreference::System,
            ThemePreference::Dark => egui::ThemePreference::Dark,
            ThemePreference::Light => egui::ThemePreference::Light,
        }
    }
}

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
    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            Mode::Desktop => t.mode_desktop,
            Mode::Gaming => t.mode_gaming,
        }
    }
    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
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
    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            Quality::Low => t.quality_low,
            Quality::Balanced => t.quality_balanced,
            Quality::High => t.quality_high,
            Quality::Ultra => t.quality_ultra,
        }
    }
    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
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
    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            Present::Mailbox => t.present_mailbox,
            Present::Immediate => t.present_immediate,
            Present::Fifo => t.present_fifo,
        }
    }
    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
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
    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            Scale::Fit => t.scale_fit,
            Scale::Stretch => t.scale_stretch,
            Scale::Original => t.scale_original,
        }
    }
    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FpsCap {
    #[default]
    Fps60,
    Fps120,
    Fps144,
    Fps240,
}

impl FpsCap {
    pub const ALL: [FpsCap; 4] = [FpsCap::Fps60, FpsCap::Fps120, FpsCap::Fps144, FpsCap::Fps240];
    pub fn fps(self) -> u32 {
        match self {
            FpsCap::Fps60 => 60,
            FpsCap::Fps120 => 120,
            FpsCap::Fps144 => 144,
            FpsCap::Fps240 => 240,
        }
    }
    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            FpsCap::Fps60 => t.fps_60,
            FpsCap::Fps120 => t.fps_120,
            FpsCap::Fps144 => t.fps_144,
            FpsCap::Fps240 => t.fps_240,
        }
    }
    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
    }
}

/// How incoming connections to this desk authenticate with passwords.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PasswordMode {
    /// Only the rotating one-time password (OTP) is accepted.
    #[default]
    OneTime,
    /// Only the configured permanent password is accepted (falls back to OTP if none is set).
    Permanent,
    /// Both the permanent password and the rotating OTP are accepted.
    Both,
}

impl PasswordMode {
    pub const ALL: [PasswordMode; 3] = [PasswordMode::OneTime, PasswordMode::Permanent, PasswordMode::Both];

    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            PasswordMode::OneTime => t.pw_mode_onetime,
            PasswordMode::Permanent => t.pw_mode_permanent,
            PasswordMode::Both => t.pw_mode_both,
        }
    }

    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
    }
}

fn default_true() -> bool {
    true
}

/// Persisted trusted viewer identity remembered from an interactive Accept dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedTrustedPeer {
    pub public_key_hex: String,
    pub device_id: String,
    pub name: String,
    pub fingerprint: String,
    pub allow_input: bool,
    pub allow_clipboard: bool,
    pub allow_file_transfer: bool,
    pub allow_audio: bool,
    pub allow_terminal: bool,
    pub allow_tunnel: bool,
    pub allow_privacy_mode: bool,
    #[serde(default = "default_true")]
    pub allow_chat: bool,
    #[serde(default)]
    pub allow_power: bool,
}

impl Default for SavedTrustedPeer {
    fn default() -> Self {
        Self {
            public_key_hex: String::new(),
            device_id: String::new(),
            name: String::new(),
            fingerprint: String::new(),
            allow_input: true,
            allow_clipboard: true,
            allow_file_transfer: true,
            allow_audio: true,
            allow_terminal: true,
            allow_tunnel: true,
            allow_privacy_mode: false,
            allow_chat: true,
            allow_power: false,
        }
    }
}

/// Max entries kept in the recent-connections list.
pub const MAX_RECENT: usize = 10;

/// Per-mode streaming and input preferences (stored independently for Desktop and Gaming).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamSettings {
    pub quality: Quality,
    pub fps_cap: FpsCap,
    pub present: Present,
    pub scale: Scale,
    pub show_hud: bool,
    pub prefer_hw_decode: bool,
    /// Gaming mode: lock the pointer and send relative motion (`Ctrl+Alt+G`).
    pub relative_mouse_in_gaming: bool,
    /// Request 10-bit HDR (P010 / HEVC Main10 / AV1 10-bit) when supported.
    pub hdr_10bit: bool,
    /// Request YUV 4:4:4 full chroma when supported.
    pub yuv444: bool,
    /// Play incoming remote desktop audio (Opus low-delay).
    pub audio_enabled: bool,
    /// Stream local microphone to the host (track 1).
    pub mic_enabled: bool,
    /// Synchronize clipboard (text, images, file lists) with the remote host.
    pub clipboard_sync: bool,
}

impl StreamSettings {
    pub fn default_for(mode: Mode) -> Self {
        Self {
            quality: Quality::Balanced,
            fps_cap: FpsCap::Fps120,
            present: Present::Mailbox,
            scale: Scale::Fit,
            show_hud: false,
            prefer_hw_decode: true,
            relative_mouse_in_gaming: matches!(mode, Mode::Gaming),
            hdr_10bit: false,
            yuv444: false,
            audio_enabled: true,
            mic_enabled: false,
            clipboard_sync: true,
        }
    }
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self::default_for(Mode::Desktop)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    pub language: Language,
    pub theme: ThemePreference,
    pub recent: Vec<String>,
    /// Rendezvous server `host[:port]` (default port 21116) for connecting by 9-digit Device ID.
    pub rendezvous_server: String,
    pub mode: Mode,
    pub desktop: StreamSettings,
    pub gaming: StreamSettings,
    /// Require interactive Accept/Reject approval dialog for incoming connections to this desk.
    pub require_accept: bool,
    /// Host password authentication mode (OTP only, Permanent only, or Both).
    pub password_mode: PasswordMode,
    /// Argon2id PHC hash (`$argon2id$...`) of the permanent host password (never plaintext).
    pub permanent_password_hash: Option<String>,
    /// Persisted trusted viewer devices allowed passwordless Ed25519 reconnect.
    pub trusted_peers: Vec<SavedTrustedPeer>,
    /// Enable echo cancellation (Windows process loopback exclusion + NLMS AEC; forced on in Gaming mode).
    #[serde(default = "default_true")]
    pub echo_cancellation: bool,
    /// Custom folder for saved `.mkv` session recordings (empty = default `Videos\VeloxDesk`).
    #[serde(default)]
    pub recordings_dir: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::default(),
            theme: ThemePreference::default(),
            recent: Vec::new(),
            rendezvous_server: String::new(),
            mode: Mode::Desktop,
            desktop: StreamSettings::default_for(Mode::Desktop),
            gaming: StreamSettings::default_for(Mode::Gaming),
            require_accept: true,
            password_mode: PasswordMode::default(),
            permanent_password_hash: None,
            trusted_peers: Vec::new(),
            echo_cancellation: true,
            recordings_dir: String::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawStreamSettings {
    quality: Option<Quality>,
    fps_cap: Option<FpsCap>,
    present: Option<Present>,
    scale: Option<Scale>,
    show_hud: Option<bool>,
    prefer_hw_decode: Option<bool>,
    relative_mouse_in_gaming: Option<bool>,
    hdr_10bit: Option<bool>,
    yuv444: Option<bool>,
    audio_enabled: Option<bool>,
    mic_enabled: Option<bool>,
    clipboard_sync: Option<bool>,
}

impl RawStreamSettings {
    fn apply_to(self, dst: &mut StreamSettings) {
        if let Some(v) = self.quality {
            dst.quality = v;
        }
        if let Some(v) = self.fps_cap {
            dst.fps_cap = v;
        }
        if let Some(v) = self.present {
            dst.present = v;
        }
        if let Some(v) = self.scale {
            dst.scale = v;
        }
        if let Some(v) = self.show_hud {
            dst.show_hud = v;
        }
        if let Some(v) = self.prefer_hw_decode {
            dst.prefer_hw_decode = v;
        }
        if let Some(v) = self.relative_mouse_in_gaming {
            dst.relative_mouse_in_gaming = v;
        }
        if let Some(v) = self.hdr_10bit {
            dst.hdr_10bit = v;
        }
        if let Some(v) = self.yuv444 {
            dst.yuv444 = v;
        }
        if let Some(v) = self.audio_enabled {
            dst.audio_enabled = v;
        }
        if let Some(v) = self.mic_enabled {
            dst.mic_enabled = v;
        }
        if let Some(v) = self.clipboard_sync {
            dst.clipboard_sync = v;
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RawSettings {
    language: Option<Language>,
    theme: Option<ThemePreference>,
    recent: Option<Vec<String>>,
    rendezvous_server: Option<String>,
    mode: Option<Mode>,
    desktop: Option<RawStreamSettings>,
    gaming: Option<RawStreamSettings>,
    require_accept: Option<bool>,
    password_mode: Option<PasswordMode>,
    permanent_password_hash: Option<String>,
    trusted_peers: Option<Vec<SavedTrustedPeer>>,
    echo_cancellation: Option<bool>,
    recordings_dir: Option<String>,
    #[serde(flatten)]
    legacy: RawStreamSettings,
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = RawSettings::deserialize(deserializer)?;
        let mode = raw.mode.unwrap_or_default();
        let mut desktop = StreamSettings::default_for(Mode::Desktop);
        let mut gaming = StreamSettings::default_for(Mode::Gaming);

        let legacy_rel_mouse = raw.legacy.relative_mouse_in_gaming;
        let has_desktop = raw.desktop.is_some();
        let has_gaming = raw.gaming.is_some();

        if let Some(d) = raw.desktop {
            d.apply_to(&mut desktop);
        }
        if let Some(g) = raw.gaming {
            g.apply_to(&mut gaming);
        }
        match mode {
            Mode::Desktop if !has_desktop => raw.legacy.apply_to(&mut desktop),
            Mode::Gaming if !has_gaming => raw.legacy.apply_to(&mut gaming),
            _ => {}
        }
        if !has_gaming && let Some(rel) = legacy_rel_mouse {
            gaming.relative_mouse_in_gaming = rel;
        }

        let echo_cancellation = if mode == Mode::Gaming {
            true
        } else {
            raw.echo_cancellation.unwrap_or(true)
        };

        Ok(Self {
            language: raw.language.unwrap_or_default(),
            theme: raw.theme.unwrap_or_default(),
            recent: raw.recent.unwrap_or_default(),
            rendezvous_server: raw.rendezvous_server.unwrap_or_default(),
            mode,
            desktop,
            gaming,
            require_accept: raw.require_accept.unwrap_or(true),
            password_mode: raw.password_mode.unwrap_or_default(),
            permanent_password_hash: raw.permanent_password_hash.filter(|s| !s.trim().is_empty()),
            trusted_peers: raw.trusted_peers.unwrap_or_default(),
            echo_cancellation,
            recordings_dir: raw.recordings_dir.unwrap_or_default(),
        })
    }
}

impl Settings {
    /// Whether echo cancellation is effectively active (always `true` in Gaming mode).
    pub fn effective_echo_cancellation(&self) -> bool {
        self.echo_cancellation || self.mode == Mode::Gaming
    }

    /// Switch the active session mode, forcing `echo_cancellation = true` when selecting Gaming.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if mode == Mode::Gaming {
            self.echo_cancellation = true;
        }
    }
    /// Whether a permanent host password hash is currently configured.
    pub fn has_permanent_password(&self) -> bool {
        self.permanent_password_hash.as_deref().is_some_and(|s| !s.trim().is_empty())
    }

    /// Whether the embedded host should keep a rotating OTP enabled (safety invariant:
    /// never leaves the host with neither OTP nor a permanent password).
    pub fn effective_otp_enabled(&self) -> bool {
        self.password_mode != PasswordMode::Permanent || !self.has_permanent_password()
    }

    /// Insert or update a remembered trusted peer matched by `public_key_hex`.
    pub fn upsert_trusted_peer(&mut self, peer: SavedTrustedPeer) {
        if let Some(existing) = self
            .trusted_peers
            .iter_mut()
            .find(|p| p.public_key_hex.eq_ignore_ascii_case(&peer.public_key_hex))
        {
            *existing = peer;
        } else {
            self.trusted_peers.push(peer);
        }
    }

    /// Remove a remembered trusted peer by `public_key_hex`. Returns `true` if removed.
    pub fn remove_trusted_peer(&mut self, public_key_hex: &str) -> bool {
        let before = self.trusted_peers.len();
        self.trusted_peers
            .retain(|p| !p.public_key_hex.eq_ignore_ascii_case(public_key_hex));
        self.trusted_peers.len() < before
    }
    pub fn profile(&self, mode: Mode) -> &StreamSettings {
        match mode {
            Mode::Desktop => &self.desktop,
            Mode::Gaming => &self.gaming,
        }
    }

    pub fn profile_mut(&mut self, mode: Mode) -> &mut StreamSettings {
        match mode {
            Mode::Desktop => &mut self.desktop,
            Mode::Gaming => &mut self.gaming,
        }
    }

    pub fn active(&self) -> &StreamSettings {
        self.profile(self.mode)
    }

    pub fn active_mut(&mut self) -> &mut StreamSettings {
        self.profile_mut(self.mode)
    }

    /// `<config>/veloxdesk/settings.json` (`%APPDATA%`, `$XDG_CONFIG_HOME`, `~/.config`,
    /// `~/Library/Application Support`).
    pub fn default_path() -> Option<PathBuf> {
        config_dir().map(|d| d.join("veloxdesk").join("settings.json"))
    }

    pub fn load_or_default() -> Self {
        Self::default_path().and_then(|p| Self::load(&p).ok()).unwrap_or_default()
    }

    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text).map_err(std::io::Error::other)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
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

    /// Save to `path` if provided (e.g. `--portable`), otherwise to [`Self::default_path`], logging failures.
    pub fn save_to_or_default(&self, path: Option<&Path>) {
        match path {
            Some(p) => {
                if let Err(e) = self.save(p) {
                    tracing::warn!("saving settings to {}: {e}", p.display());
                }
            }
            None => self.save_default(),
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
        assert!(p.active().show_hud);
        assert_eq!(p.active().quality, Quality::Balanced);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn language_json_roundtrip_and_default() {
        assert_eq!(Settings::default().language, Language::En);
        assert_eq!(Language::from_u8(Language::En.to_u8()), Language::En);
        assert_eq!(Language::from_u8(Language::Ru.to_u8()), Language::Ru);
        assert_eq!(Language::from_u8(255), Language::En);

        let dir = std::env::temp_dir().join(format!("veloxdesk-ui-lang-test-{}", std::process::id()));
        let path = dir.join("lang.json");
        let s = Settings { language: Language::Ru, ..Default::default() };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap().language, Language::Ru);

        let legacy: Settings = serde_json::from_str(r#"{"mode": "Desktop"}"#).unwrap();
        assert_eq!(legacy.language, Language::En);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn profiles_are_independent_and_roundtrip() {
        let mut s = Settings {
            language: Language::Ru,
            mode: Mode::Gaming,
            desktop: StreamSettings {
                quality: Quality::Low,
                fps_cap: FpsCap::Fps60,
                present: Present::Fifo,
                scale: Scale::Stretch,
                show_hud: false,
                prefer_hw_decode: false,
                relative_mouse_in_gaming: false,
                hdr_10bit: false,
                yuv444: true,
                audio_enabled: false,
                mic_enabled: false,
                clipboard_sync: false,
            },
            gaming: StreamSettings {
                quality: Quality::Ultra,
                fps_cap: FpsCap::Fps240,
                present: Present::Immediate,
                scale: Scale::Original,
                show_hud: true,
                prefer_hw_decode: true,
                relative_mouse_in_gaming: true,
                hdr_10bit: true,
                yuv444: false,
                audio_enabled: true,
                mic_enabled: true,
                clipboard_sync: true,
            },
            ..Default::default()
        };

        let desktop_before = s.desktop;
        s.active_mut().quality = Quality::High;
        s.active_mut().hdr_10bit = false;
        assert_eq!(s.desktop, desktop_before);
        assert_eq!(s.gaming.quality, Quality::High);

        s.mode = Mode::Desktop;
        let gaming_before = s.gaming;
        s.active_mut().audio_enabled = true;
        assert_eq!(s.gaming, gaming_before);

        let dir = std::env::temp_dir().join(format!("veloxdesk-ui-profiles-{}", std::process::id()));
        let path = dir.join("profiles.json");
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_flat_json_migration_and_defaults() {
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert!(empty.require_accept);
        assert_eq!(empty.desktop.fps_cap, FpsCap::Fps120);
        assert!(!empty.desktop.relative_mouse_in_gaming);
        assert!(empty.gaming.relative_mouse_in_gaming);

        let flat_desktop: Settings = serde_json::from_str(r#"{"show_hud": true, "future": 1}"#).unwrap();
        assert!(flat_desktop.desktop.show_hud);
        assert!(!flat_desktop.gaming.show_hud);
        assert!(flat_desktop.require_accept);

        let flat_gaming: Settings =
            serde_json::from_str(r#"{"mode": "Gaming", "quality": "Ultra", "audio_enabled": false}"#).unwrap();
        assert_eq!(flat_gaming.mode, Mode::Gaming);
        assert_eq!(flat_gaming.gaming.quality, Quality::Ultra);
        assert!(!flat_gaming.gaming.audio_enabled);
        assert!(flat_gaming.gaming.relative_mouse_in_gaming);
        assert_eq!(flat_gaming.desktop.quality, Quality::Balanced);
        assert!(flat_gaming.desktop.audio_enabled);

        let flat_rel_false: Settings = serde_json::from_str(r#"{"relative_mouse_in_gaming": false}"#).unwrap();
        assert!(!flat_rel_false.gaming.relative_mouse_in_gaming);

        let partial_nested: Settings = serde_json::from_str(r#"{"gaming": {"quality": "High"}}"#).unwrap();
        assert_eq!(partial_nested.gaming.quality, Quality::High);
        assert!(partial_nested.gaming.relative_mouse_in_gaming);
    }

    #[test]
    fn save_to_or_default_writes_portable_path() {
        let dir = std::env::temp_dir().join(format!("veloxdesk-ui-portable-{}", std::process::id()));
        let path = dir.join("veloxdesk-settings.json");
        let s = Settings { language: Language::Ru, ..Default::default() };
        s.save_to_or_default(Some(&path));
        assert!(path.is_file());
        assert_eq!(Settings::load(&path).unwrap(), s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn password_mode_and_trusted_peers_roundtrip_and_v020_compat() {
        // Legacy v0.2.0 JSON without password_mode / permanent_password_hash / trusted_peers
        let v020: Settings = serde_json::from_str(
            r#"{"language":"Ru","mode":"Desktop","require_accept":true,"desktop":{"quality":"High"}}"#,
        )
        .unwrap();
        assert_eq!(v020.password_mode, PasswordMode::OneTime);
        assert!(v020.permanent_password_hash.is_none());
        assert!(v020.trusted_peers.is_empty());
        assert!(v020.effective_otp_enabled());

        // Full v0.3.0 roundtrip
        let mut s = Settings {
            password_mode: PasswordMode::Permanent,
            permanent_password_hash: Some("$argon2id$v=19$m=65536,t=3,p=1$salt$hash".into()),
            require_accept: false,
            ..Default::default()
        };
        assert!(s.has_permanent_password());
        assert!(!s.effective_otp_enabled());

        let peer = SavedTrustedPeer {
            public_key_hex: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            device_id: "987654321".into(),
            name: "Work-Laptop".into(),
            fingerprint: "abcdef0123456789abcdef0123456789".into(),
            allow_input: true,
            allow_clipboard: true,
            allow_file_transfer: false,
            allow_audio: true,
            allow_terminal: false,
            allow_tunnel: false,
            allow_privacy_mode: true,
            allow_chat: true,
            allow_power: true,
        };
        s.upsert_trusted_peer(peer.clone());
        assert_eq!(s.trusted_peers.len(), 1);

        // Case-insensitive upsert updates in place
        let mut updated = peer.clone();
        updated.public_key_hex = updated.public_key_hex.to_uppercase();
        updated.name = "Renamed-Laptop".into();
        s.upsert_trusted_peer(updated);
        assert_eq!(s.trusted_peers.len(), 1);
        assert_eq!(s.trusted_peers[0].name, "Renamed-Laptop");

        let dir = std::env::temp_dir().join(format!("veloxdesk-ui-v030-{}", std::process::id()));
        let path = dir.join("v030.json");
        s.save(&path).unwrap();
        let reloaded = Settings::load(&path).unwrap();
        assert_eq!(reloaded, s);
        let _ = std::fs::remove_dir_all(dir);

        // Remove trusted peer (case-insensitive)
        assert!(s.remove_trusted_peer(&peer.public_key_hex));
        assert!(s.trusted_peers.is_empty());
        assert!(!s.remove_trusted_peer(&peer.public_key_hex));

        // Permanent mode with no hash falls back to OTP enabled
        s.permanent_password_hash = None;
        assert!(s.effective_otp_enabled());
    }

    #[test]
    fn theme_preference_roundtrip_and_v030_compat() {
        assert_eq!(Settings::default().theme, ThemePreference::System);
        assert_eq!(ThemePreference::System.next(), ThemePreference::Dark);
        assert_eq!(ThemePreference::Dark.next(), ThemePreference::Light);
        assert_eq!(ThemePreference::Light.next(), ThemePreference::System);
        assert_eq!(ThemePreference::System.to_egui(), egui::ThemePreference::System);
        assert_eq!(ThemePreference::Dark.to_egui(), egui::ThemePreference::Dark);
        assert_eq!(ThemePreference::Light.to_egui(), egui::ThemePreference::Light);

        // Legacy JSON without `theme` field defaults to `ThemePreference::System`.
        let legacy: Settings = serde_json::from_str(r#"{"language":"En","mode":"Desktop"}"#).unwrap();
        assert_eq!(legacy.theme, ThemePreference::System);

        let dir = std::env::temp_dir().join(format!("veloxdesk-ui-theme-{}", std::process::id()));
        for th in ThemePreference::ALL {
            let path = dir.join(format!("theme-{th:?}.json"));
            let s = Settings { theme: th, ..Default::default() };
            s.save(&path).unwrap();
            let reloaded = Settings::load(&path).unwrap();
            assert_eq!(reloaded.theme, th);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn v050_echo_cancellation_recordings_dir_and_trusted_peer_compat() {
        let def = Settings::default();
        assert!(def.echo_cancellation);
        assert!(def.effective_echo_cancellation());
        assert!(def.recordings_dir.is_empty());

        // Legacy v0.4.0 JSON without echo_cancellation / recordings_dir / allow_chat / allow_power.
        let legacy: Settings = serde_json::from_str(
            r#"{"mode":"Desktop","trusted_peers":[{"public_key_hex":"aa","device_id":"123456789","name":"Peer","fingerprint":"fp"}]}"#,
        )
        .unwrap();
        assert!(legacy.echo_cancellation);
        assert!(legacy.recordings_dir.is_empty());
        assert_eq!(legacy.trusted_peers.len(), 1);
        assert!(legacy.trusted_peers[0].allow_chat);
        assert!(!legacy.trusted_peers[0].allow_power);

        // Gaming mode forces echo_cancellation = true.
        let mut s = Settings { echo_cancellation: false, recordings_dir: "D:\\Captures".into(), ..Default::default() };
        assert!(!s.effective_echo_cancellation());
        s.set_mode(Mode::Gaming);
        assert!(s.echo_cancellation);
        assert!(s.effective_echo_cancellation());

        // JSON with Gaming mode and echo_cancellation=false still forces true on deserialize.
        let gaming_json: Settings =
            serde_json::from_str(r#"{"mode":"Gaming","echo_cancellation":false,"recordings_dir":"C:\\Rec"}"#).unwrap();
        assert!(gaming_json.echo_cancellation);
        assert_eq!(gaming_json.recordings_dir, "C:\\Rec");
    }
}
