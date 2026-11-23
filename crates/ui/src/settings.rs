//! Persistent viewer settings (JSON in the user config dir).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use veloxdesk_core_traits::render::{PresentMode, ScaleMode};
use veloxdesk_protocol::control::{Permissions, SessionMode};

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

/// Opus audio frame duration preference for low-latency audio streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioFrameLatency {
    /// Automatic: 5 ms in Gaming mode, 10 ms in Desktop mode.
    #[default]
    Auto,
    /// Ultra-low 2.5 ms Opus frames.
    Ms2_5,
    /// Low-latency 5 ms Opus frames.
    Ms5,
    /// Balanced 10 ms Opus frames.
    Ms10,
    /// Bandwidth-efficient 20 ms Opus frames.
    Ms20,
}

impl AudioFrameLatency {
    pub const ALL: [AudioFrameLatency; 5] = [
        AudioFrameLatency::Auto,
        AudioFrameLatency::Ms2_5,
        AudioFrameLatency::Ms5,
        AudioFrameLatency::Ms10,
        AudioFrameLatency::Ms20,
    ];

    /// Wire representation in microseconds (`0` for `Auto`, else `2500`/`5000`/`10000`/`20000`).
    pub const fn to_wire_us(self) -> u32 {
        match self {
            AudioFrameLatency::Auto => 0,
            AudioFrameLatency::Ms2_5 => 2_500,
            AudioFrameLatency::Ms5 => 5_000,
            AudioFrameLatency::Ms10 => 10_000,
            AudioFrameLatency::Ms20 => 20_000,
        }
    }

    /// Effective Opus frame duration in microseconds for `mode` (`Auto` resolves to 5 ms Gaming / 10 ms Desktop).
    pub const fn effective_us(self, mode: Mode) -> u32 {
        match self {
            AudioFrameLatency::Auto => match mode {
                Mode::Gaming => 5_000,
                Mode::Desktop => 10_000,
            },
            AudioFrameLatency::Ms2_5 => 2_500,
            AudioFrameLatency::Ms5 => 5_000,
            AudioFrameLatency::Ms10 => 10_000,
            AudioFrameLatency::Ms20 => 20_000,
        }
    }

    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            AudioFrameLatency::Auto => t.audio_latency_auto,
            AudioFrameLatency::Ms2_5 => t.audio_latency_2_5ms,
            AudioFrameLatency::Ms5 => t.audio_latency_5ms,
            AudioFrameLatency::Ms10 => t.audio_latency_10ms,
            AudioFrameLatency::Ms20 => t.audio_latency_20ms,
        }
    }

    pub fn label(self) -> &'static str {
        self.label_for(Language::En)
    }
}

/// Windows physical display blanking strategy used by Host Privacy Mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HostPrivacyMode {
    /// Fullscreen topmost curtain overlay with `WDA_EXCLUDEFROMCAPTURE` (works without VDD).
    #[default]
    CurtainOverlay,
    /// Curtain overlay plus `SC_MONITORPOWER` monitor power-off when a Virtual Display (VDD) is active.
    VddMonitorOff,
}

impl HostPrivacyMode {
    pub const ALL: [HostPrivacyMode; 2] = [HostPrivacyMode::CurtainOverlay, HostPrivacyMode::VddMonitorOff];

    pub fn label_for(self, lang: Language) -> &'static str {
        let t = crate::i18n::tr(lang);
        match self {
            HostPrivacyMode::CurtainOverlay => t.privacy_mode_curtain,
            HostPrivacyMode::VddMonitorOff => t.privacy_mode_vdd,
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
    /// Enable adaptive NLMS acoustic echo cancellation (AEC) on viewer microphone.
    #[serde(default = "default_true")]
    pub echo_cancellation: bool,
    /// Enable pure-Rust microphone background noise suppression & VAD gate.
    #[serde(default = "default_true")]
    pub noise_suppression: bool,
    /// Preferred Opus audio frame duration (`Auto` = 5 ms Gaming / 10 ms Desktop).
    #[serde(default)]
    pub audio_frame_latency: AudioFrameLatency,
}

impl StreamSettings {
    pub fn default_for(mode: Mode) -> Self {
        match mode {
            Mode::Desktop => Self {
                quality: Quality::Balanced,
                fps_cap: FpsCap::Fps60,
                present: Present::Fifo,
                scale: Scale::Fit,
                show_hud: false,
                prefer_hw_decode: true,
                relative_mouse_in_gaming: false,
                hdr_10bit: false,
                yuv444: true,
                audio_enabled: true,
                mic_enabled: false,
                clipboard_sync: true,
                echo_cancellation: true,
                noise_suppression: true,
                audio_frame_latency: AudioFrameLatency::Auto,
            },
            Mode::Gaming => Self {
                quality: Quality::High,
                fps_cap: FpsCap::Fps120,
                present: Present::Mailbox,
                scale: Scale::Fit,
                show_hud: true,
                prefer_hw_decode: true,
                relative_mouse_in_gaming: true,
                hdr_10bit: false,
                yuv444: false,
                audio_enabled: true,
                mic_enabled: false,
                clipboard_sync: true,
                echo_cancellation: true,
                noise_suppression: true,
                audio_frame_latency: AudioFrameLatency::Auto,
            },
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
    /// Custom folder for saved `.mkv` session recordings (empty = default `Videos\VeloxDesk`).
    #[serde(default)]
    pub recordings_dir: String,
    /// Automatically retry reconnecting after a remote host reboot (up to 5 minutes).
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Default host permission: allow remote keyboard and mouse input.
    #[serde(default = "default_true")]
    pub host_allow_input: bool,
    /// Default host permission: allow clipboard synchronization.
    #[serde(default = "default_true")]
    pub host_allow_clipboard: bool,
    /// Default host permission: allow file transfer.
    #[serde(default = "default_true")]
    pub host_allow_file_transfer: bool,
    /// Default host permission: allow remote audio and microphone streaming.
    #[serde(default = "default_true")]
    pub host_allow_audio: bool,
    /// Default host permission: allow in-session text chat.
    #[serde(default = "default_true")]
    pub host_allow_chat: bool,
    /// Default host permission: allow remote terminal and TCP port forwarding tunnels.
    #[serde(default = "default_true")]
    pub host_allow_terminal_tunnel: bool,
    /// Default host permission: allow remote host power actions (reboot/lock/sleep).
    #[serde(default)]
    pub host_allow_power: bool,
    /// Exclude VeloxDesk's own playback from host loopback audio capture (Windows Process Loopback Exclusion).
    #[serde(default = "default_true")]
    pub host_echo_exclusion: bool,
    /// Windows physical monitor blanking strategy for Host Privacy Mode.
    #[serde(default)]
    pub host_privacy_mode: HostPrivacyMode,
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
            recordings_dir: String::new(),
            auto_reconnect: true,
            host_allow_input: true,
            host_allow_clipboard: true,
            host_allow_file_transfer: true,
            host_allow_audio: true,
            host_allow_chat: true,
            host_allow_terminal_tunnel: true,
            host_allow_power: false,
            host_echo_exclusion: true,
            host_privacy_mode: HostPrivacyMode::CurtainOverlay,
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
    echo_cancellation: Option<bool>,
    noise_suppression: Option<bool>,
    audio_frame_latency: Option<AudioFrameLatency>,
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
        if let Some(v) = self.echo_cancellation {
            dst.echo_cancellation = v;
        }
        if let Some(v) = self.noise_suppression {
            dst.noise_suppression = v;
        }
        if let Some(v) = self.audio_frame_latency {
            dst.audio_frame_latency = v;
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
    recordings_dir: Option<String>,
    auto_reconnect: Option<bool>,
    host_allow_input: Option<bool>,
    host_allow_clipboard: Option<bool>,
    host_allow_file_transfer: Option<bool>,
    host_allow_audio: Option<bool>,
    host_allow_chat: Option<bool>,
    host_allow_terminal_tunnel: Option<bool>,
    host_allow_power: Option<bool>,
    host_echo_exclusion: Option<bool>,
    host_privacy_mode: Option<HostPrivacyMode>,
    /// Captures pre-R4 flat stream fields and v0.5.0 top-level `echo_cancellation`.
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
        let legacy_top_aec = raw.legacy.echo_cancellation;
        let has_desktop = raw.desktop.is_some();
        let has_gaming = raw.gaming.is_some();
        let desktop_had_aec = raw.desktop.as_ref().is_some_and(|d| d.echo_cancellation.is_some());
        let gaming_had_aec = raw.gaming.as_ref().is_some_and(|g| g.echo_cancellation.is_some());

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
        // Backward-compat migration from v0.5.0 top-level `echo_cancellation`.
        if let Some(aec) = legacy_top_aec {
            if !desktop_had_aec {
                desktop.echo_cancellation = aec;
            }
            if !gaming_had_aec {
                gaming.echo_cancellation = aec;
            }
        }

        let host_echo_exclusion = raw.host_echo_exclusion.or(legacy_top_aec).unwrap_or(true);

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
            recordings_dir: raw.recordings_dir.unwrap_or_default(),
            auto_reconnect: raw.auto_reconnect.unwrap_or(true),
            host_allow_input: raw.host_allow_input.unwrap_or(true),
            host_allow_clipboard: raw.host_allow_clipboard.unwrap_or(true),
            host_allow_file_transfer: raw.host_allow_file_transfer.unwrap_or(true),
            host_allow_audio: raw.host_allow_audio.unwrap_or(true),
            host_allow_chat: raw.host_allow_chat.unwrap_or(true),
            host_allow_terminal_tunnel: raw.host_allow_terminal_tunnel.unwrap_or(true),
            host_allow_power: raw.host_allow_power.unwrap_or(false),
            host_echo_exclusion,
            host_privacy_mode: raw.host_privacy_mode.unwrap_or_default(),
        })
    }
}

impl Settings {
    /// Whether echo cancellation is active for the currently selected session profile.
    pub fn effective_echo_cancellation(&self) -> bool {
        self.active().echo_cancellation
    }

    /// Switch the active session mode.
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// Reset the specified profile (`Desktop` or `Gaming`) back to its built-in defaults.
    pub fn reset_profile_defaults(&mut self, mode: Mode) {
        *self.profile_mut(mode) = StreamSettings::default_for(mode);
    }

    /// Build the default host [`Permissions`] from the user's Host Access & Security toggles.
    pub fn host_default_permissions(&self) -> Permissions {
        Permissions {
            view: true,
            input: self.host_allow_input,
            clipboard: self.host_allow_clipboard,
            file_transfer: self.host_allow_file_transfer,
            audio: self.host_allow_audio,
            tunnel: self.host_allow_terminal_tunnel,
            terminal: self.host_allow_terminal_tunnel,
            restart: self.host_allow_power,
            privacy_mode: true,
            allow_chat: self.host_allow_chat,
            allow_power: self.host_allow_power,
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
                echo_cancellation: false,
                noise_suppression: false,
                audio_frame_latency: AudioFrameLatency::Ms20,
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
                echo_cancellation: true,
                noise_suppression: true,
                audio_frame_latency: AudioFrameLatency::Ms2_5,
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
        assert_eq!(empty.desktop.fps_cap, FpsCap::Fps60);
        assert_eq!(empty.gaming.fps_cap, FpsCap::Fps120);
        assert!(!empty.desktop.relative_mouse_in_gaming);
        assert!(empty.gaming.relative_mouse_in_gaming);

        let flat_desktop: Settings = serde_json::from_str(r#"{"show_hud": true, "future": 1}"#).unwrap();
        assert!(flat_desktop.desktop.show_hud);
        assert!(flat_desktop.gaming.show_hud);
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

        let partial_nested: Settings = serde_json::from_str(r#"{"gaming": {"quality": "Ultra"}}"#).unwrap();
        assert_eq!(partial_nested.gaming.quality, Quality::Ultra);
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
        assert!(def.desktop.echo_cancellation);
        assert!(def.gaming.echo_cancellation);
        assert!(def.effective_echo_cancellation());
        assert!(def.recordings_dir.is_empty());

        // Legacy v0.4.0 JSON without echo_cancellation / recordings_dir / allow_chat / allow_power.
        let legacy: Settings = serde_json::from_str(
            r#"{"mode":"Desktop","trusted_peers":[{"public_key_hex":"aa","device_id":"123456789","name":"Peer","fingerprint":"fp"}]}"#,
        )
        .unwrap();
        assert!(legacy.desktop.echo_cancellation);
        assert!(legacy.gaming.echo_cancellation);
        assert!(legacy.host_echo_exclusion);
        assert!(legacy.recordings_dir.is_empty());
        assert_eq!(legacy.trusted_peers.len(), 1);
        assert!(legacy.trusted_peers[0].allow_chat);
        assert!(!legacy.trusted_peers[0].allow_power);

        // v0.5.0 top-level `echo_cancellation: false` migrates seamlessly into both profiles & host_echo_exclusion.
        let v050_json: Settings = serde_json::from_str(
            r#"{"mode":"Gaming","echo_cancellation":false,"recordings_dir":"C:\\Rec","desktop":{"quality":"Low"}}"#,
        )
        .unwrap();
        assert!(!v050_json.desktop.echo_cancellation);
        assert!(!v050_json.gaming.echo_cancellation);
        assert!(!v050_json.host_echo_exclusion);
        assert!(!v050_json.effective_echo_cancellation());
        assert_eq!(v050_json.desktop.quality, Quality::Low);
        assert_eq!(v050_json.recordings_dir, "C:\\Rec");

        // v0.6.0 echo_cancellation is independently toggleable per profile (no longer forced on in Gaming).
        let mut s = Settings::default();
        s.gaming.echo_cancellation = false;
        s.set_mode(Mode::Gaming);
        assert!(!s.gaming.echo_cancellation);
        assert!(!s.effective_echo_cancellation());
        assert!(s.desktop.echo_cancellation);
    }

    #[test]
    fn v060_deep_profile_defaults_audio_latency_reset_and_host_policy() {
        let desktop = StreamSettings::default_for(Mode::Desktop);
        assert_eq!(desktop.quality, Quality::Balanced);
        assert_eq!(desktop.fps_cap, FpsCap::Fps60);
        assert_eq!(desktop.present, Present::Fifo);
        assert!(!desktop.show_hud);
        assert!(!desktop.relative_mouse_in_gaming);
        assert!(desktop.yuv444);
        assert!(desktop.audio_enabled);
        assert!(!desktop.mic_enabled);
        assert!(desktop.clipboard_sync);
        assert!(desktop.echo_cancellation);
        assert!(desktop.noise_suppression);
        assert_eq!(desktop.audio_frame_latency, AudioFrameLatency::Auto);

        let gaming = StreamSettings::default_for(Mode::Gaming);
        assert_eq!(gaming.quality, Quality::High);
        assert_eq!(gaming.fps_cap, FpsCap::Fps120);
        assert_eq!(gaming.present, Present::Mailbox);
        assert!(gaming.show_hud);
        assert!(gaming.relative_mouse_in_gaming);
        assert!(!gaming.yuv444);
        assert!(gaming.audio_enabled);
        assert!(!gaming.mic_enabled);
        assert!(gaming.clipboard_sync);
        assert!(gaming.echo_cancellation);
        assert!(gaming.noise_suppression);
        assert_eq!(gaming.audio_frame_latency, AudioFrameLatency::Auto);

        // AudioFrameLatency wire and effective durations.
        assert_eq!(AudioFrameLatency::Auto.to_wire_us(), 0);
        assert_eq!(AudioFrameLatency::Auto.effective_us(Mode::Gaming), 5_000);
        assert_eq!(AudioFrameLatency::Auto.effective_us(Mode::Desktop), 10_000);
        assert_eq!(AudioFrameLatency::Ms2_5.to_wire_us(), 2_500);
        assert_eq!(AudioFrameLatency::Ms5.to_wire_us(), 5_000);
        assert_eq!(AudioFrameLatency::Ms10.to_wire_us(), 10_000);
        assert_eq!(AudioFrameLatency::Ms20.to_wire_us(), 20_000);
        assert_eq!(AudioFrameLatency::Ms2_5.effective_us(Mode::Desktop), 2_500);
        assert_eq!(AudioFrameLatency::Ms20.effective_us(Mode::Gaming), 20_000);

        // reset_profile_defaults restores only the requested profile.
        let mut s = Settings::default();
        s.desktop.quality = Quality::Ultra;
        s.desktop.noise_suppression = false;
        s.desktop.audio_frame_latency = AudioFrameLatency::Ms20;
        s.gaming.quality = Quality::Low;
        s.gaming.echo_cancellation = false;
        s.reset_profile_defaults(Mode::Desktop);
        assert_eq!(s.desktop, StreamSettings::default_for(Mode::Desktop));
        assert_eq!(s.gaming.quality, Quality::Low);
        assert!(!s.gaming.echo_cancellation);
        s.reset_profile_defaults(Mode::Gaming);
        assert_eq!(s.gaming, StreamSettings::default_for(Mode::Gaming));

        // Host policy defaults match Permissions::interactive_default() and roundtrip via JSON.
        let mut def = Settings::default();
        assert!(def.auto_reconnect);
        assert!(def.host_echo_exclusion);
        assert_eq!(def.host_privacy_mode, HostPrivacyMode::CurtainOverlay);
        assert_eq!(def.host_default_permissions(), Permissions::interactive_default());

        def.auto_reconnect = false;
        def.host_allow_input = false;
        def.host_allow_clipboard = false;
        def.host_allow_file_transfer = false;
        def.host_allow_audio = false;
        def.host_allow_chat = false;
        def.host_allow_terminal_tunnel = false;
        def.host_allow_power = true;
        def.host_echo_exclusion = false;
        def.host_privacy_mode = HostPrivacyMode::VddMonitorOff;
        let perms = def.host_default_permissions();
        assert!(perms.view);
        assert!(!perms.input);
        assert!(!perms.clipboard);
        assert!(!perms.file_transfer);
        assert!(!perms.audio);
        assert!(!perms.allow_chat);
        assert!(!perms.terminal);
        assert!(!perms.tunnel);
        assert!(perms.allow_power);
        assert!(perms.restart);

        let json = serde_json::to_string_pretty(&def).unwrap();
        let reloaded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(reloaded, def);
    }
}
