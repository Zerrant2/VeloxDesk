//! Zero-copy GPU frame import (design; implementation scheduled for M3).
//!
//! # Windows (D3D11VA decoder → wgpu DX12)
//!
//! 1. The decoder (media-worker) outputs `GpuHandle::D3D11Texture` whose texture was
//!    created with `D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED` and
//!    exposes `shared_handle` (from `IDXGIResource1::CreateSharedHandle`). Decoders that
//!    render into a texture *array* must copy the slice into a pooled shared NV12 texture
//!    first (D3D11 cannot share array slices).
//! 2. The renderer must run on the **same adapter** (`adapter_luid` vs
//!    `wgpu::AdapterInfo` / DXGI LUID); otherwise fall back to CPU.
//! 3. `ID3D12Device::OpenSharedHandle(shared_handle)` → `ID3D12Resource` (NV12,
//!    `DXGI_FORMAT_NV12`), obtained through `device.as_hal::<wgpu::hal::api::Dx12>()`.
//! 4. `hal::dx12::Device::texture_from_raw(resource, TextureFormat::NV12, D2, size, 1, 1)`
//!    → `device.create_texture_from_hal::<Dx12>(hal_tex, &desc)` with
//!    `Features::TEXTURE_FORMAT_NV12`; two views with `TextureAspect::Plane0` (R8) and
//!    `Plane1` (RG8) bind straight into the existing NV12 shader path (mode 1).
//! 5. Synchronisation: the decoder signals a shared `ID3D11Fence` (or uses keyed mutex)
//!    that the D3D12 queue waits on before sampling; the frame's `keepalive` is held
//!    until the GPU work that samples it has completed (`queue.on_submitted_work_done`).
//! 6. Imported textures are cached by `shared_handle` (decoders recycle a small pool),
//!    so steady state is: one fence wait + draw, no copies, no allocations.
//!
//! # Linux / macOS
//! VA-API DMA-BUF → Vulkan `VK_EXT_external_memory_dma_buf` + `VK_EXT_image_drm_format_modifier`
//! via `wgpu::hal::vulkan::Device::texture_from_raw`; IOSurface → `MTLTexture`
//! (`newTextureWithDescriptor:iosurface:plane:`) via the Metal hal.
//!
//! Until then [`GpuImporter::supported_kinds`] is empty, `RendererCaps::zero_copy_import`
//! is empty, and the viewer asks decoders for CPU output (`prefer_gpu_output = false`).

use veloxdesk_core_traits::video::GpuFrame;
use veloxdesk_core_traits::{CoreError, Result};

/// Imports native GPU frames into wgpu textures usable by the NV12 shader path.
pub trait GpuImporter {
    /// `GpuHandle` variant names this importer handles (reported in `RendererCaps`).
    fn supported_kinds(&self) -> Vec<&'static str>;
    /// Import (or fetch from cache) a frame; returns `(luma, chroma)` views.
    fn import(&mut self, frame: &GpuFrame, width: u32, height: u32) -> Result<(wgpu::TextureView, wgpu::TextureView)>;
}

/// Importer used until the platform paths land: supports nothing.
#[derive(Debug, Default)]
pub struct NoImport;

impl GpuImporter for NoImport {
    fn supported_kinds(&self) -> Vec<&'static str> {
        Vec::new()
    }
    fn import(&mut self, _frame: &GpuFrame, _w: u32, _h: u32) -> Result<(wgpu::TextureView, wgpu::TextureView)> {
        Err(CoreError::unsupported("zero-copy import not implemented on this platform yet (M3)"))
    }
}
