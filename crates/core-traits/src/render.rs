//! Viewer-side video presentation.

use crate::video::{CursorImage, Frame};
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PresentMode {
    /// Tear-free, lowest latency without tearing (preferred).
    #[default]
    Mailbox,
    /// Tearing allowed, absolute lowest latency (gaming, VSync off).
    Immediate,
    /// Classic VSync.
    Fifo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleMode {
    /// Keep aspect ratio, letterbox.
    Fit,
    /// Stretch to window.
    Stretch,
    /// 1:1 pixels.
    Original,
}

#[derive(Debug, Clone, Default)]
pub struct RendererCaps {
    pub backend: &'static str,
    /// Native GPU frame kinds that can be imported without copy.
    pub zero_copy_import: Vec<&'static str>,
    pub hdr: bool,
    pub present_modes: Vec<PresentMode>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PresentStats {
    /// Upload/import + draw + present call duration (µs).
    pub render_us: u32,
    /// Whether the frame was imported zero-copy.
    pub zero_copy: bool,
}

/// Presents decoded frames (and an optional local cursor) to a window surface.
/// Implemented by `crates/render` on `wgpu`; UI/HUD is composed in the same pass.
pub trait VideoRenderer {
    fn caps(&self) -> RendererCaps;
    /// Upload/import the frame and present it immediately.
    fn present(&mut self, frame: &Frame) -> Result<PresentStats>;
    /// Re-present the last frame (e.g. on window expose / UI change).
    fn redraw(&mut self) -> Result<PresentStats>;
    fn resize(&mut self, width: u32, height: u32);
    fn set_present_mode(&mut self, mode: PresentMode) -> Result<()>;
    fn set_scale_mode(&mut self, mode: ScaleMode);
    /// Locally rendered cursor (gaming mode / separate-cursor capture).
    fn set_cursor(&mut self, image: Option<&CursorImage>, x: i32, y: i32, visible: bool);
}
