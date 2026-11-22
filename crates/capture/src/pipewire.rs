//! Linux Wayland xdg-desktop-portal + PipeWire screen capture interface (`pipewire` feature).

use std::time::Duration;

use veloxdesk_core_traits::video::{CaptureCaps, CaptureConfig, CapturedFrame, Display, FrameCapture, PixelFormat};
use veloxdesk_core_traits::{CoreError, Result};

/// PipeWire / xdg-desktop-portal capture backend (DMA-BUF zero-copy capable on Wayland).
pub struct PipeWireCapture {
    prefer_gpu: bool,
}

impl PipeWireCapture {
    pub fn new(prefer_gpu: bool) -> Result<Self> {
        let _ = prefer_gpu;
        Err(CoreError::unsupported("PipeWire portal stream wiring scheduled for Linux M3"))
    }
}

impl FrameCapture for PipeWireCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps {
            backend: "pipewire",
            gpu_frames: self.prefer_gpu,
            dirty_rects: false,
            separate_cursor: true,
            hdr: false,
            formats: vec![PixelFormat::Bgra8],
        }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        Err(CoreError::unsupported("PipeWire portal display enumeration not active"))
    }

    fn start(&mut self, _cfg: &CaptureConfig) -> Result<()> {
        Err(CoreError::unsupported("PipeWire portal capture not active"))
    }

    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<CapturedFrame>> {
        Err(CoreError::unsupported("PipeWire portal capture not active"))
    }

    fn stop(&mut self) {}
}
