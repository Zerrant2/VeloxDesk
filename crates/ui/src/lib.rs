//! # veloxdesk-ui
//!
//! egui screens and widgets: connect screen (address + recent list), session toolbar
//! (Desktop/Gaming, display picker, quality, scaling, HUD), settings window, and the
//! persistent [`Settings`]. Rendering is done by `veloxdesk-render` in the video pass.

pub mod connect;
pub mod session;
pub mod settings;

pub use connect::{ConnectAction, ConnectScreen, parse_address};
pub use session::{SessionInfo, SessionToolbar, ToolbarAction, settings_window};
pub use settings::{Mode, Present, Quality, Scale, Settings};
