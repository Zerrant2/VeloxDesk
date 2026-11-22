//! Zero-copy GPU frame import into wgpu textures (ARCHITECTURE §5, M3).
//!
//! # Windows (D3D11VA decoder → wgpu DX12)
//!
//! 1. The decoder (`veloxdesk-codec` `MfDecoder`) outputs `GpuHandle::D3D11Texture` backed by
//!    a single-slice pooled `NV12` or `P010` texture created with
//!    `D3D11_RESOURCE_MISC_SHARED_NTHANDLE | D3D11_RESOURCE_MISC_SHARED` and exposes its NT
//!    `shared_handle` alongside `adapter_luid`, having flushed/synced GPU writes.
//! 2. [`Dx12Importer`] verifies `adapter_luid` matches the wgpu DX12 adapter, opens the NT
//!    handle via `ID3D12Device::OpenSharedHandle`, wraps the `ID3D12Resource` with
//!    `wgpu::hal::dx12::Device::texture_from_raw` + `wgpu::Device::create_texture_from_hal`
//!    (`TextureFormat::NV12` / `TextureFormat::P010`), and creates `Plane0` (luma) and `Plane1`
//!    (chroma) views.
//! 3. Opened textures are cached by `shared_handle` and validated with a `Weak` reference to
//!    the producer's `keepalive` object so recycled Windows HANDLE values never alias and the
//!    decoder's texture pool is never starved.

use veloxdesk_core_traits::video::{GpuFrame, PixelFormat};
use veloxdesk_core_traits::{CoreError, Result};

/// Imported GPU plane views ready to bind into [`crate::pipeline::VideoPipeline`].
pub struct ImportedGpuViews {
    /// Stable cache key for bind-group caching (`shared_handle` + generation).
    pub cache_key: u64,
    pub y_view: wgpu::TextureView,
    pub uv_view: wgpu::TextureView,
    /// Backing GPU texture dimensions (may be macroblock-aligned larger than visible size).
    pub tex_size: (u32, u32),
}

/// Optional wgpu device features needed for zero-copy NV12/P010 and 10-bit CPU uploads.
pub fn requested_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    let want = wgpu::Features::TEXTURE_FORMAT_NV12
        | wgpu::Features::TEXTURE_FORMAT_P010
        | wgpu::Features::TEXTURE_FORMAT_16BIT_NORM;
    adapter.features() & want
}

/// Imports native GPU frames into wgpu textures usable by the YUV shader path.
pub trait GpuImporter: Send {
    /// `GpuHandle` variant names this importer handles (reported in `RendererCaps`).
    fn supported_kinds(&self) -> Vec<&'static str>;
    /// Adapter LUID on Windows, if known.
    fn adapter_luid(&self) -> Option<u64> {
        None
    }
    /// Import (or fetch from cache) a frame; returns luma/chroma views and backing texture size.
    fn import(
        &mut self,
        frame: &GpuFrame,
        format: PixelFormat,
        width: u32,
        height: u32,
    ) -> Result<ImportedGpuViews>;
}

/// Fallback importer when the active backend does not support zero-copy import.
#[derive(Debug, Default)]
pub struct NoImport;

impl GpuImporter for NoImport {
    fn supported_kinds(&self) -> Vec<&'static str> {
        Vec::new()
    }
    fn import(
        &mut self,
        _frame: &GpuFrame,
        _format: PixelFormat,
        _w: u32,
        _h: u32,
    ) -> Result<ImportedGpuViews> {
        Err(CoreError::unsupported("zero-copy GPU import not available on this backend"))
    }
}

/// Create the best available [`GpuImporter`] for `(adapter, device)`.
pub fn create_importer(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Box<dyn GpuImporter> {
    #[cfg(windows)]
    if let Some(imp) = win_dx12::Dx12Importer::try_new(adapter, device) {
        return Box::new(imp);
    }
    let _ = (adapter, device);
    Box::new(NoImport)
}

#[cfg(windows)]
mod win_dx12 {
    use std::any::Any;
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::sync::{Arc, Weak};

    use veloxdesk_core_traits::video::{GpuFrame, GpuHandle, PixelFormat};
    use veloxdesk_core_traits::{CoreError, Result};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Direct3D12::ID3D12Resource;

    use super::{GpuImporter, ImportedGpuViews};

    struct CachedTexture {
        cache_key: u64,
        format: PixelFormat,
        tex_size: (u32, u32),
        /// Weak reference to producer's `keepalive` (`TextureHolder`) to detect pool teardown
        /// without preventing `TexturePool` from recycling slots (`strong_count == 1`).
        keepalive_ptr: Option<usize>,
        keepalive_weak: Option<Weak<dyn Any + Send + Sync>>,
        _texture: wgpu::Texture,
        y_view: wgpu::TextureView,
        uv_view: wgpu::TextureView,
    }

    impl CachedTexture {
        fn matches(&self, format: PixelFormat, keepalive: Option<&Arc<dyn Any + Send + Sync>>) -> bool {
            if self.format != format {
                return false;
            }
            match (keepalive, self.keepalive_ptr, &self.keepalive_weak) {
                (Some(arc), Some(ptr), Some(weak)) => {
                    let cur_ptr = Arc::as_ptr(arc) as *const () as usize;
                    cur_ptr == ptr && weak.upgrade().is_some()
                }
                (None, None, None) => true,
                _ => false,
            }
        }

        fn is_dead(&self) -> bool {
            self.keepalive_weak.as_ref().is_some_and(|w| w.upgrade().is_none())
        }
    }

    pub struct Dx12Importer {
        device: wgpu::Device,
        adapter_luid: u64,
        supports_p010: bool,
        next_key: u64,
        cache: HashMap<usize, CachedTexture>,
    }

    impl Dx12Importer {
        pub fn try_new(adapter: &wgpu::Adapter, device: &wgpu::Device) -> Option<Self> {
            if adapter.get_info().backend != wgpu::Backend::Dx12 {
                return None;
            }
            if !device.features().contains(wgpu::Features::TEXTURE_FORMAT_NV12) {
                tracing::debug!("DX12 adapter lacks TEXTURE_FORMAT_NV12; zero-copy import disabled");
                return None;
            }
            let adapter_luid = query_dx12_luid(adapter).unwrap_or(0);
            let supports_p010 = device.features().contains(wgpu::Features::TEXTURE_FORMAT_P010);
            tracing::info!(
                luid = format_args!("{adapter_luid:#x}"),
                p010 = supports_p010,
                "DX12 zero-copy GPU importer enabled"
            );
            Some(Self {
                device: device.clone(),
                adapter_luid,
                supports_p010,
                next_key: 1,
                cache: HashMap::new(),
            })
        }
    }

    fn query_dx12_luid(adapter: &wgpu::Adapter) -> Option<u64> {
        // SAFETY: read-only DXGI GetDesc1 on the live DX12 HAL adapter.
        unsafe {
            let hal_adapter = adapter.as_hal::<wgpu::hal::api::Dx12>()?;
            let desc = hal_adapter.raw_adapter().GetDesc1().ok()?;
            Some(((desc.AdapterLuid.HighPart as u32 as u64) << 32) | desc.AdapterLuid.LowPart as u64)
        }
    }

    impl GpuImporter for Dx12Importer {
        fn supported_kinds(&self) -> Vec<&'static str> {
            vec!["D3D11Texture"]
        }

        fn adapter_luid(&self) -> Option<u64> {
            (self.adapter_luid != 0).then_some(self.adapter_luid)
        }

        fn import(
            &mut self,
            frame: &GpuFrame,
            format: PixelFormat,
            width: u32,
            height: u32,
        ) -> Result<ImportedGpuViews> {
            let (raw_handle, frame_luid) = match frame.handle {
                GpuHandle::D3D11Texture { shared_handle: Some(h), adapter_luid, .. } if h != 0 => (h, adapter_luid),
                GpuHandle::D3D11Texture { shared_handle: None | Some(0), .. } => {
                    return Err(CoreError::unsupported(
                        "D3D11Texture has no shared NT handle for DX12 zero-copy import",
                    ));
                }
                _ => return Err(CoreError::unsupported("DX12 importer only supports GpuHandle::D3D11Texture")),
            };

            if frame_luid != 0 && self.adapter_luid != 0 && frame_luid != self.adapter_luid {
                return Err(CoreError::unsupported(format!(
                    "GPU adapter LUID mismatch (frame {frame_luid:#x} != renderer {:#x})",
                    self.adapter_luid
                )));
            }

            let (wgpu_fmt, y_fmt, uv_fmt) = match format {
                PixelFormat::Nv12 => (
                    wgpu::TextureFormat::NV12,
                    wgpu::TextureFormat::R8Unorm,
                    wgpu::TextureFormat::Rg8Unorm,
                ),
                PixelFormat::P010 if self.supports_p010 => (
                    wgpu::TextureFormat::P010,
                    wgpu::TextureFormat::R16Unorm,
                    wgpu::TextureFormat::Rg16Unorm,
                ),
                other => {
                    return Err(CoreError::unsupported(format!(
                        "DX12 zero-copy import does not support {other:?}"
                    )));
                }
            };

            if let Some(entry) = self.cache.get(&raw_handle)
                && entry.matches(format, frame.keepalive.as_ref())
            {
                return Ok(ImportedGpuViews {
                    cache_key: entry.cache_key,
                    y_view: entry.y_view.clone(),
                    uv_view: entry.uv_view.clone(),
                    tex_size: entry.tex_size,
                });
            }

            // Evict any textures whose producer keepalive was dropped.
            self.cache.retain(|_, v| !v.is_dead());
            self.cache.remove(&raw_handle);

            // SAFETY: `raw_handle` is a valid D3D11 NT shared handle kept alive by `frame.keepalive`.
            let (resource, tex_w, tex_h) = unsafe {
                let hal_dev = self
                    .device
                    .as_hal::<wgpu::hal::api::Dx12>()
                    .ok_or_else(|| CoreError::unsupported("wgpu device is not DX12"))?;
                let mut out: Option<ID3D12Resource> = None;
                hal_dev
                    .raw_device()
                    .OpenSharedHandle(HANDLE(raw_handle as *mut c_void), &mut out)
                    .map_err(|e| CoreError::platform(format!("ID3D12Device::OpenSharedHandle: {e}")))?;
                let res = out.ok_or_else(|| CoreError::platform("OpenSharedHandle returned null resource"))?;
                let rdesc = res.GetDesc();
                (res, (rdesc.Width as u32).max(width), rdesc.Height.max(height))
            };

            let size = wgpu::Extent3d { width: tex_w, height: tex_h, depth_or_array_layers: 1 };
            // SAFETY: `resource` is a single-slice 2D NV12/P010 D3D12 texture opened on this device;
            // newly opened shared D3D11 resources begin in D3D12_RESOURCE_STATE_COMMON (`UNINITIALIZED`).
            let texture = unsafe {
                let hal_tex = wgpu::hal::dx12::Device::texture_from_raw(
                    resource,
                    wgpu_fmt,
                    wgpu::TextureDimension::D2,
                    size,
                    1,
                    1,
                );
                let desc = wgpu::TextureDescriptor {
                    label: Some("veloxdesk-dx12-shared"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu_fmt,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                };
                self.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(
                    hal_tex,
                    &desc,
                    wgpu::wgt::TextureUses::UNINITIALIZED,
                )
            };

            let y_view = texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("veloxdesk-dx12-y"),
                format: Some(y_fmt),
                aspect: wgpu::TextureAspect::Plane0,
                ..Default::default()
            });
            let uv_view = texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("veloxdesk-dx12-uv"),
                format: Some(uv_fmt),
                aspect: wgpu::TextureAspect::Plane1,
                ..Default::default()
            });

            let cache_key = self.next_key;
            self.next_key = self.next_key.wrapping_add(1).max(1);
            let (keepalive_ptr, keepalive_weak) = match &frame.keepalive {
                Some(arc) => (Some(Arc::as_ptr(arc) as *const () as usize), Some(Arc::downgrade(arc))),
                None => (None, None),
            };

            self.cache.insert(
                raw_handle,
                CachedTexture {
                    cache_key,
                    format,
                    tex_size: (tex_w, tex_h),
                    keepalive_ptr,
                    keepalive_weak,
                    _texture: texture,
                    y_view: y_view.clone(),
                    uv_view: uv_view.clone(),
                },
            );

            tracing::debug!(
                handle = format_args!("{raw_handle:#x}"),
                ?format,
                tex_w,
                tex_h,
                visible_w = width,
                visible_h = height,
                "DX12 zero-copy shared texture imported"
            );

            Ok(ImportedGpuViews { cache_key, y_view, uv_view, tex_size: (tex_w, tex_h) })
        }
    }
}
