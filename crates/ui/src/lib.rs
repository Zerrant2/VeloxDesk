//! # veloxdesk-ui
//!
//! egui screens and widgets: connect screen (address + recent list), session toolbar
//! (Desktop/Gaming, display picker, quality, scaling, HUD), settings window, and the
//! persistent [`Settings`]. Rendering is done by `veloxdesk-render` in the video pass.

pub mod auth;
pub mod connect;
pub mod files;
pub mod i18n;
pub mod icons;
pub mod session;
pub mod settings;
pub mod terminal;
pub mod theme;
pub mod tunnels;

pub use auth::{
    AcceptDecision, AcceptPrompt, IncomingSessionInfo, PasswordAction, PasswordPrompt, format_fingerprint,
};
pub use connect::{
    ConnectAction, ConnectScreen, ConnectTarget, TailscaleInfo, TailscalePeer, UpdateUiState, format_device_id,
    parse_address, parse_address_i18n, parse_device_id, parse_rendezvous_addr, parse_rendezvous_addr_i18n,
    parse_target, parse_target_i18n,
};
pub use files::{
    FileAction, FileManagerWindow, TransferProgress, format_size, format_size_i18n, join_remote_path,
    parent_remote_path,
};
pub use i18n::{Strings, tr};
pub use session::{
    SessionInfo, SessionToolbar, ToolbarAction, settings_window, settings_window_at, settings_window_full,
    settings_window_with_security,
};
pub use settings::{
    FpsCap, Language, Mode, PasswordMode, Present, Quality, SavedTrustedPeer, Scale, Settings, StreamSettings,
    ThemePreference,
};
pub use terminal::{TerminalAction, TerminalWindow, strip_ansi};
pub use tunnels::{ActiveTunnel, TunnelAction, TunnelSpec, TunnelsWindow, parse_tunnel_spec, parse_tunnel_spec_i18n};
