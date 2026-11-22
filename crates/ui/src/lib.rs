//! # veloxdesk-ui
//!
//! egui screens and widgets: connect screen (address + recent list), session toolbar
//! (Desktop/Gaming, display picker, quality, scaling, HUD), settings window, and the
//! persistent [`Settings`]. Rendering is done by `veloxdesk-render` in the video pass.

pub mod auth;
pub mod connect;
pub mod files;
pub mod session;
pub mod settings;
pub mod terminal;
pub mod tunnels;

pub use auth::{
    AcceptDecision, AcceptPrompt, IncomingSessionInfo, PasswordAction, PasswordPrompt, format_fingerprint,
};
pub use connect::{
    ConnectAction, ConnectScreen, ConnectTarget, format_device_id, parse_address, parse_device_id,
    parse_rendezvous_addr, parse_target,
};
pub use files::{FileAction, FileManagerWindow, TransferProgress, format_size, join_remote_path, parent_remote_path};
pub use session::{SessionInfo, SessionToolbar, ToolbarAction, settings_window};
pub use settings::{FpsCap, Mode, Present, Quality, Scale, Settings};
pub use terminal::{TerminalAction, TerminalWindow, strip_ansi};
pub use tunnels::{ActiveTunnel, TunnelAction, TunnelSpec, TunnelsWindow, parse_tunnel_spec};
