//! macOS ScreenCaptureKit backend interface (`IOSurface` zero-copy path).

use std::time::Duration;

use veloxdesk_core_traits::video::{CaptureCaps, CaptureConfig, CapturedFrame, Display, FrameCapture, PixelFormat};
use veloxdesk_core_traits::{CoreError, Result};

/// macOS ScreenCaptureKit screen capture (`IOSurface`-backed zero-copy frames).
pub struct ScreenCaptureKitCapture {
    prefer_gpu: bool,
}

impl ScreenCaptureKitCapture {
    pub fn new(prefer_gpu: bool) -> Result<Self> {
        let _ = prefer_gpu;
        Err(CoreError::unsupported("macOS ScreenCaptureKit capture not yet implemented"))
    }
}

pub(crate) fn list_displays() -> Result<Vec<Display>> {
    Err(CoreError::unsupported("macOS ScreenCaptureKit display enumeration not yet implemented"))
}

impl FrameCapture for ScreenCaptureKitCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps {
            backend: "screencapturekit",
            gpu_frames: self.prefer_gpu,
            dirty_rects: false,
            separate_cursor: true,
            hdr: true,
            formats: vec![PixelFormat::Bgra8, PixelFormat::Rgb10a2],
        }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        list_displays()
    }

    fn start(&mut self, _cfg: &CaptureConfig) -> Result<()> {
        Err(CoreError::unsupported("macOS ScreenCaptureKit capture not yet implemented"))
    }

    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<CapturedFrame>> {
        Err(CoreError::unsupported("macOS ScreenCaptureKit capture not yet implemented"))
    }

    fn stop(&mut self) {}
}
