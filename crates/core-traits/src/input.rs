//! Host-side input injection (keyboard, mouse, gamepads, touch).

use crate::Result;

pub use veloxdesk_protocol::control::{
    input_event::Event as InputEventKind, GamepadState, InputEvent, KeyEvent, MouseButton, Rumble,
};

#[derive(Debug, Clone, Default)]
pub struct InputCaps {
    /// e.g. "SendInput", "uinput", "libei", "CGEvent".
    pub backend: &'static str,
    pub keyboard: bool,
    pub mouse_absolute: bool,
    pub mouse_relative: bool,
    pub gamepads: bool,
    /// e.g. "ViGEmBus", "uinput".
    pub gamepad_backend: Option<&'static str>,
    pub max_gamepads: u32,
    pub touch: bool,
}

/// Feedback produced by the host (e.g. a game asked the virtual pad to rumble).
#[derive(Debug, Clone, PartialEq)]
pub enum InputFeedback {
    Rumble(Rumble),
}

/// Geometry used to map absolute mouse coordinates to the virtual desktop.
#[derive(Debug, Clone, Copy, Default)]
pub struct DisplayGeometry {
    pub index: u32,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Injects remote input into the host OS. Called from one input thread.
pub trait InputInjector: Send {
    fn caps(&self) -> InputCaps;
    /// Inform the injector about display layout (for absolute coordinates).
    fn set_displays(&mut self, displays: &[DisplayGeometry]);
    fn inject(&mut self, event: &InputEvent) -> Result<()>;
    /// Release every pressed key/button and disconnect virtual pads (session end/focus loss).
    fn release_all(&mut self) -> Result<()>;
    /// Drain pending feedback (rumble) to be sent to the viewer.
    fn poll_feedback(&mut self) -> Vec<InputFeedback>;
}
