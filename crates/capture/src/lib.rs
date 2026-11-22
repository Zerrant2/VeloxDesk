//! # veloxdesk-capture
//!
//! Screen capture backends behind [`FrameCapture`]:
//!
//! | OS      | backend                                                    | `caps().backend` |
//! |---------|------------------------------------------------------------|------------------|
//! | Windows | DXGI Desktop Duplication (D3D11)                           | `"dxgi-dd"`      |
//! | Linux   | X11 MIT-SHM (always built)                                 | `"x11-shm"`      |
//! | Linux   | xdg-desktop-portal + PipeWire (`pipewire` feature, Wayland) | `"pipewire"`    |
//! | other   | — (`CoreError::Unsupported`)                               |                  |
//!
//! ## Frame contract (all backends)
//! * Format `Bgra8`, `ColorSpace::Srgb`, `ColorRange::Full`.
//! * `dirty_rects` empty = whole frame / unknown. A frame whose `dirty_rects` is exactly
//!   one **zero-sized** rect ([`CURSOR_ONLY`]) is a *cursor-only update*: the image is
//!   identical to the previous frame (same texture / same bytes) and only `cursor`
//!   changed. Encoders may skip it or encode a cheap repeat frame.
//! * `cursor` is always delivered separately (position each frame where known, shape only
//!   when it changed, with an incrementing `id`). `CaptureConfig::draw_cursor` is currently
//!   **not** composited into the image by any backend (DXGI DD never includes the cursor);
//!   the viewer draws the cursor from `CursorState`.
//! * `capture_ts` is the OS present time of the frame when known (DXGI `LastPresentTime`),
//!   otherwise the time the capture API returned.
//!
//! ## Windows GPU frames
//! With `prefer_gpu`, frames are `FrameData::Gpu(GpuFrame { handle:
//! GpuHandle::D3D11Texture { texture, subresource: 0, adapter_luid, shared_handle }, keepalive })`.
//! `texture` is a borrowed `ID3D11Texture2D*` (DEFAULT usage, `B8G8R8A8_UNORM`, bind
//! `SHADER_RESOURCE | RENDER_TARGET`), kept alive by `keepalive` (a
//! `D3D11TextureHolder`; downcast it to get a typed COM pointer). The texture belongs
//! to a pool: it is recycled only after every clone of the `GpuFrame` is dropped, so hold
//! the frame for as long as the GPU may read it.
//!
//! The encoder should reuse the capture device: `ID3D11DeviceChild::GetDevice` on the
//! texture. That device was created on the output's adapter with `BGRA_SUPPORT` and has
//! `ID3D11Multithread::SetMultithreadProtected(TRUE)`, so its immediate context may be
//! used from the encode thread. The copy into the texture is submitted (and flushed) on
//! that immediate context before the frame is returned, so same-device consumers need no
//! extra synchronisation. `shared_handle` (NT handle, when sharing succeeded) is for
//! other devices (D3D12 / wgpu); it is owned by the holder — duplicate it if you need it
//! beyond the frame's lifetime.

use veloxdesk_core_traits::video::{Display, FrameCapture, Rect};
#[allow(unused_imports)]
use veloxdesk_core_traits::CoreError;
use veloxdesk_core_traits::Result;

pub mod util;

#[cfg(windows)]
mod dxgi;
#[cfg(windows)]
pub use dxgi::{D3D11TextureHolder, DxgiCapture};

#[cfg(target_os = "linux")]
mod x11;
#[cfg(target_os = "linux")]
pub use x11::X11Capture;

#[cfg(all(target_os = "linux", feature = "pipewire"))]
mod pipewire;
#[cfg(all(target_os = "linux", feature = "pipewire"))]
pub use pipewire::PipeWireCapture;

/// Marker dirty-rect set meaning "image unchanged, cursor-only update".
pub const CURSOR_ONLY: Rect = Rect { x: 0, y: 0, width: 0, height: 0 };

/// True if `dirty_rects` marks a cursor-only update (see crate docs).
pub fn is_cursor_only(dirty_rects: &[Rect]) -> bool {
    dirty_rects.len() == 1 && dirty_rects[0] == CURSOR_ONLY
}

/// Create the best capture backend for this OS. `prefer_gpu` selects GPU-resident frames
/// where the backend supports them (`CaptureConfig::prefer_gpu` must also be set; both
/// have to agree for GPU output).
pub fn create_capture(prefer_gpu: bool) -> Result<Box<dyn FrameCapture>> {
    #[cfg(windows)]
    {
        Ok(Box::new(dxgi::DxgiCapture::new(prefer_gpu)?))
    }
    #[cfg(target_os = "linux")]
    {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        #[cfg(feature = "pipewire")]
        if wayland || std::env::var_os("DISPLAY").is_none() {
            match pipewire::PipeWireCapture::new(prefer_gpu) {
                Ok(c) => return Ok(Box::new(c)),
                Err(e) => tracing::warn!("pipewire capture unavailable: {e}; trying X11"),
            }
        }
        if wayland {
            tracing::debug!("Wayland session: X11 capture only sees XWayland content");
        }
        Ok(Box::new(x11::X11Capture::new(prefer_gpu)?))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = prefer_gpu;
        Err(CoreError::unsupported("screen capture is not implemented on this OS"))
    }
}

/// Enumerate capturable displays (desktop coordinates, size, refresh, scale).
pub fn list_displays() -> Result<Vec<Display>> {
    #[cfg(windows)]
    {
        dxgi::list_displays()
    }
    #[cfg(target_os = "linux")]
    {
        x11::list_displays()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(CoreError::unsupported("screen capture is not implemented on this OS"))
    }
}
