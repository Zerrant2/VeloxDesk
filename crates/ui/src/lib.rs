//! # veloxdesk-ui
//!
//! egui screens and widgets: connect screen (address + recent list), session toolbar
//! (Desktop/Gaming, display picker, quality, scaling, HUD), settings window, and the
//! persistent [`Settings`]. Rendering is done by `veloxdesk-render` in the video pass.

pub mod auth;
pub mod connect;
pub mod session;
pub mod settings;

pub use auth::{
    AcceptDecision, AcceptPrompt, IncomingSessionInfo, PasswordAction, PasswordPrompt, format_fingerprint,
};
pub use connect::{
    ConnectAction, ConnectScreen, ConnectTarget, format_device_id, parse_address, parse_device_id,
    parse_rendezvous_addr, parse_target,
};
pub use session::{SessionInfo, SessionToolbar, ToolbarAction, settings_window};
pub use settings::{Mode, Present, Quality, Scale, Settings};
