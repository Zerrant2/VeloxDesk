//! D3D11 helpers shared by the Media Foundation encoder/decoder: device creation, texture
//! pools with keepalive, GPU BGRA→NV12 (D3D11 VideoProcessor), GPU timers and CPU readback.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;

use veloxdesk_core_traits::video::{GpuFrame, GpuHandle, PixelFormat};
use veloxdesk_core_traits::{CoreError, Result};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

pub(crate) fn win_err(ctx: &'static str) -> impl Fn(windows::core::Error) -> CoreError {
    move |e| {
        let code = e.code();
        if code == DXGI_ERROR_DEVICE_REMOVED || code == DXGI_ERROR_DEVICE_RESET {
            CoreError::reinit(format!("{ctx}: device lost ({e})"))
        } else {
            CoreError::platform(format!("{ctx}: {e}"))
        }
    }
}

/// Owner of an AddRef'd texture (+ optional shared NT handle); used as `GpuFrame::keepalive`.
pub struct TextureHolder {
    pub tex: ID3D11Texture2D,
    pub shared: Option<HANDLE>,
}

// SAFETY: D3D11 resources are free-threaded COM objects; the device is created
// multithread-protected so cross-thread use of the immediate context is serialised.
unsafe impl Send for TextureHolder {}
unsafe impl Sync for TextureHolder {}

impl Drop for TextureHolder {
    fn drop(&mut self) {
        if let Some(h) = self.shared.take() {
            // SAFETY: we own the handle returned by CreateSharedHandle.
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

pub fn luid_u64(l: LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

pub fn device_luid(dev: &ID3D11Device) -> u64 {
    // SAFETY: plain COM queries on a valid device.
    unsafe {
        dev.cast::<IDXGIDevice>()
            .and_then(|d| d.GetAdapter())
            .and_then(|a| a.GetDesc())
            .map(|d| luid_u64(d.AdapterLuid))
            .unwrap_or(0)
    }
}

/// Borrow the texture behind a `GpuHandle::D3D11Texture` (AddRef'd clone).
pub fn texture_of(g: &GpuFrame) -> Result<(ID3D11Texture2D, u32)> {
    match g.handle {
        GpuHandle::D3D11Texture { texture, subresource, .. } if texture != 0 => {
            let raw = texture as *mut c_void;
            // SAFETY: producer guarantees `texture` is a live ID3D11Texture2D* kept alive by
            // `keepalive` for at least the duration of this call.
            let t = unsafe { ID3D11Texture2D::from_raw_borrowed(&raw) }
                .ok_or_else(|| CoreError::invalid("null D3D11 texture"))?
                .clone();
            Ok((t, subresource))
        }
        _ => Err(CoreError::unsupported("GPU frame is not a D3D11 texture")),
    }
}

pub fn tex_desc(t: &ID3D11Texture2D) -> D3D11_TEXTURE2D_DESC {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    // SAFETY: valid texture, out-param on the stack.
    unsafe { t.GetDesc(&mut d) };
    d
}

pub fn device_of(t: &ID3D11Texture2D) -> Result<ID3D11Device> {
    // SAFETY: valid texture.
    unsafe { t.GetDevice() }.map_err(win_err("GetDevice"))
}

pub fn enable_multithread(dev: &ID3D11Device) {
    // SAFETY: COM calls on a valid device.
    unsafe {
        if let Ok(mt) = dev.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        } else if let Ok(ctx) = dev.GetImmediateContext()
            && let Ok(mt) = ctx.cast::<ID3D11Multithread>()
        {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
}

/// Create a video-capable, multithread-protected D3D11 device on the adapter with `luid`
/// (or the default adapter).
pub fn create_device(luid: Option<u64>) -> Result<ID3D11Device> {
    // SAFETY: standard D3D11/DXGI creation calls with stack out-params.
    unsafe {
        let adapter: Option<IDXGIAdapter> = match luid {
            Some(l) if l != 0 => {
                let f: IDXGIFactory4 = CreateDXGIFactory1().map_err(win_err("CreateDXGIFactory1"))?;
                f.EnumAdapterByLuid::<IDXGIAdapter>(LUID { LowPart: l as u32, HighPart: (l >> 32) as i32 }).ok()
            }
            _ => None,
        };
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let mut dev = None;
        let driver = if adapter.is_some() { D3D_DRIVER_TYPE_UNKNOWN } else { D3D_DRIVER_TYPE_HARDWARE };
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            flags,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut dev),
            None,
            None,
        )
        .map_err(|e| CoreError::unsupported(format!("D3D11CreateDevice: {e}")))?;
        let dev = dev.ok_or_else(|| CoreError::platform("D3D11CreateDevice returned null"))?;
        enable_multithread(&dev);
        Ok(dev)
    }
}

/// Pool of same-sized textures handed out with an `Arc` keepalive. A slot is reused only
/// when nobody else holds its `Arc` (strong_count == 1).
pub struct TexturePool {
    desc: D3D11_TEXTURE2D_DESC,
    shared: bool,
    slots: Vec<Arc<TextureHolder>>,
    max: usize,
}

impl TexturePool {
    pub fn new(desc: D3D11_TEXTURE2D_DESC, want_shared: bool, max: usize) -> Self {
        Self { desc, shared: want_shared, slots: Vec::new(), max }
    }

    pub fn desc(&self) -> &D3D11_TEXTURE2D_DESC {
        &self.desc
    }

    pub fn get(&mut self, dev: &ID3D11Device) -> Result<Arc<TextureHolder>> {
        if let Some(s) = self.slots.iter().find(|s| Arc::strong_count(s) == 1) {
            return Ok(s.clone());
        }
        let h = Arc::new(self.create(dev)?);
        if self.slots.len() < self.max {
            self.slots.push(h.clone());
        }
        Ok(h)
    }

    fn create(&mut self, dev: &ID3D11Device) -> Result<TextureHolder> {
        // SAFETY: creating resources on a valid device.
        unsafe {
            if self.shared {
                let mut d = self.desc;
                d.MiscFlags |= (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 | D3D11_RESOURCE_MISC_SHARED.0) as u32;
                let mut t = None;
                if dev.CreateTexture2D(&d, None, Some(&mut t)).is_ok()
                    && let Some(t) = t
                {
                    let handle = t
                        .cast::<IDXGIResource1>()
                        .and_then(|r| r.CreateSharedHandle(None, GENERIC_ALL.0, None))
                        .ok();
                    return Ok(TextureHolder { tex: t, shared: handle });
                }
                // Driver refused shareable textures of this format: stop trying.
                self.shared = false;
            }
            let mut t = None;
            dev.CreateTexture2D(&self.desc, None, Some(&mut t)).map_err(win_err("CreateTexture2D"))?;
            Ok(TextureHolder { tex: t.ok_or_else(|| CoreError::platform("null texture"))?, shared: None })
        }
    }
}

pub fn gpu_frame(h: &Arc<TextureHolder>, luid: u64) -> GpuFrame {
    GpuFrame {
        handle: GpuHandle::D3D11Texture {
            texture: h.tex.as_raw() as usize,
            subresource: 0,
            adapter_luid: luid,
            shared_handle: h.shared.map(|s| s.0 as usize),
        },
        keepalive: Some(h.clone() as Arc<dyn std::any::Any + Send + Sync>),
    }
}

/// GPU timestamp queries, read back asynchronously (value lags 1–3 frames, never blocks).
pub struct GpuTimer {
    ring: Vec<[ID3D11Query; 3]>,
    pending: Vec<bool>,
    idx: usize,
    pub last_us: u32,
}

impl GpuTimer {
    pub fn new(dev: &ID3D11Device) -> Option<Self> {
        let mk = |q: D3D11_QUERY| -> Option<ID3D11Query> {
            let mut out = None;
            // SAFETY: valid device, stack desc.
            unsafe { dev.CreateQuery(&D3D11_QUERY_DESC { Query: q, MiscFlags: 0 }, Some(&mut out)).ok()? };
            out
        };
        let mut ring = Vec::new();
        for _ in 0..4 {
            ring.push([mk(D3D11_QUERY_TIMESTAMP_DISJOINT)?, mk(D3D11_QUERY_TIMESTAMP)?, mk(D3D11_QUERY_TIMESTAMP)?]);
        }
        Some(Self { pending: vec![false; ring.len()], ring, idx: 0, last_us: 0 })
    }

    fn poll(&mut self, ctx: &ID3D11DeviceContext) {
        for i in 0..self.ring.len() {
            if !self.pending[i] {
                continue;
            }
            let [dj, s, e] = &self.ring[i];
            let mut d = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            let (mut t0, mut t1) = (0u64, 0u64);
            // SAFETY: queries belong to `ctx`'s device; sizes match the query types.
            unsafe {
                let flag = D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32;
                if ctx.GetData(dj, Some((&raw mut d).cast()), size_of_val(&d) as u32, flag).is_err()
                    || ctx.GetData(s, Some((&raw mut t0).cast()), 8, flag).is_err()
                    || ctx.GetData(e, Some((&raw mut t1).cast()), 8, flag).is_err()
                {
                    continue;
                }
            }
            // GetData returns S_FALSE (Ok) when not ready; the data then stays zeroed.
            if d.Frequency == 0 {
                continue;
            }
            self.pending[i] = false;
            if !d.Disjoint.as_bool() && t1 >= t0 {
                self.last_us = ((t1 - t0) * 1_000_000 / d.Frequency) as u32;
            }
        }
    }

    pub fn begin(&mut self, ctx: &ID3D11DeviceContext) {
        self.poll(ctx);
        let i = self.idx;
        if self.pending[i] {
            return; // still in flight; skip timing this frame
        }
        // SAFETY: valid queries.
        unsafe {
            ctx.Begin(&self.ring[i][0]);
            ctx.End(&self.ring[i][1]);
        }
    }

    pub fn end(&mut self, ctx: &ID3D11DeviceContext) {
        let i = self.idx;
        if self.pending[i] {
            return;
        }
        // SAFETY: valid queries.
        unsafe {
            ctx.End(&self.ring[i][2]);
            ctx.End(&self.ring[i][0]);
        }
        self.pending[i] = true;
        self.idx = (i + 1) % self.ring.len();
    }
}

/// BGRA (or RGBA) → NV12 colour conversion on the GPU with the fixed-function video
/// processor. Input/output views are cached per texture.
pub struct Nv12Converter {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    vctx: ID3D11VideoContext,
    vdev: ID3D11VideoDevice,
    vpe: ID3D11VideoProcessorEnumerator,
    vp: ID3D11VideoProcessor,
    in_views: HashMap<usize, (ID3D11Texture2D, ID3D11VideoProcessorInputView)>,
    out_views: HashMap<usize, ID3D11VideoProcessorOutputView>,
    pool: TexturePool,
    timer: Option<GpuTimer>,
    in_w: u32,
    in_h: u32,
}

// SAFETY: see TextureHolder; all calls happen on the single encode thread.
unsafe impl Send for Nv12Converter {}

impl Nv12Converter {
    pub fn new(dev: &ID3D11Device, in_w: u32, in_h: u32, out_w: u32, out_h: u32) -> Result<Self> {
        // SAFETY: COM calls on a valid device; descriptors live on the stack.
        unsafe {
            let ctx = dev.GetImmediateContext().map_err(win_err("GetImmediateContext"))?;
            let vdev: ID3D11VideoDevice = dev.cast().map_err(|e| CoreError::unsupported(format!("no video device: {e}")))?;
            let vctx: ID3D11VideoContext = ctx.cast().map_err(|e| CoreError::unsupported(format!("no video ctx: {e}")))?;
            let cd = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: in_w,
                InputHeight: in_h,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: out_w,
                OutputHeight: out_h,
                Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
            };
            let vpe = vdev.CreateVideoProcessorEnumerator(&cd).map_err(win_err("CreateVideoProcessorEnumerator"))?;
            let fl = vpe.CheckVideoProcessorFormat(DXGI_FORMAT_NV12).unwrap_or(0);
            if fl & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                return Err(CoreError::unsupported("video processor cannot output NV12"));
            }
            let vp = vdev.CreateVideoProcessor(&vpe, 0).map_err(win_err("CreateVideoProcessor"))?;
            vctx.VideoProcessorSetStreamFrameFormat(&vp, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            vctx.VideoProcessorSetStreamAutoProcessingMode(&vp, 0, false);
            if let Ok(v1) = vctx.cast::<ID3D11VideoContext1>() {
                v1.VideoProcessorSetStreamColorSpace1(&vp, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                v1.VideoProcessorSetOutputColorSpace1(&vp, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709);
            }
            let desc = D3D11_TEXTURE2D_DESC {
                Width: out_w,
                Height: out_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            Ok(Self {
                dev: dev.clone(),
                timer: GpuTimer::new(dev),
                ctx,
                vctx,
                vdev,
                vpe,
                vp,
                in_views: HashMap::new(),
                out_views: HashMap::new(),
                pool: TexturePool::new(desc, false, 6),
                in_w,
                in_h,
            })
        }
    }

    pub fn input_size(&self) -> (u32, u32) {
        (self.in_w, self.in_h)
    }

    /// Last measured GPU time of the conversion (µs, lags a few frames).
    pub fn gpu_us(&self) -> u32 {
        self.timer.as_ref().map(|t| t.last_us).unwrap_or(0)
    }

    pub fn convert(&mut self, src: &ID3D11Texture2D, subresource: u32) -> Result<Arc<TextureHolder>> {
        let out = self.pool.get(&self.dev)?;
        // SAFETY: all objects belong to `self.dev`; views keep their resources alive.
        unsafe {
            let key = src.as_raw() as usize;
            if !self.in_views.contains_key(&key) {
                if self.in_views.len() > 16 {
                    self.in_views.clear();
                }
                let d = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    FourCC: 0,
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: subresource },
                    },
                };
                let mut v = None;
                self.vdev
                    .CreateVideoProcessorInputView(src, &self.vpe, &d, Some(&mut v))
                    .map_err(win_err("CreateVideoProcessorInputView"))?;
                self.in_views.insert(key, (src.clone(), v.ok_or_else(|| CoreError::platform("null input view"))?));
            }
            let okey = out.tex.as_raw() as usize;
            if !self.out_views.contains_key(&okey) {
                let d = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
                };
                let mut v = None;
                self.vdev
                    .CreateVideoProcessorOutputView(&out.tex, &self.vpe, &d, Some(&mut v))
                    .map_err(win_err("CreateVideoProcessorOutputView"))?;
                self.out_views.insert(okey, v.ok_or_else(|| CoreError::platform("null output view"))?);
            }
            let in_view = self.in_views[&key].1.clone();
            let out_view = &self.out_views[&okey];
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(in_view)),
                ..Default::default()
            };
            let mut streams = [stream];
            if let Some(t) = self.timer.as_mut() {
                t.begin(&self.ctx);
            }
            let r = self.vctx.VideoProcessorBlt(&self.vp, out_view, 0, &streams);
            if let Some(t) = self.timer.as_mut() {
                t.end(&self.ctx);
            }
            std::mem::ManuallyDrop::drop(&mut streams[0].pInputSurface);
            r.map_err(win_err("VideoProcessorBlt"))?;
        }
        Ok(out)
    }
}

/// CPU readback of BGRA / NV12 textures through a cached staging texture.
#[derive(Default)]
pub struct Readback {
    staging: Option<(ID3D11Texture2D, D3D11_TEXTURE2D_DESC)>,
    buf: Vec<u8>,
}

// SAFETY: see TextureHolder.
unsafe impl Send for Readback {}

/// Pixel format and borrowed plane `(data, stride)` slices returned by [`Readback`].
pub type ReadbackPlanes<'a> = (PixelFormat, Vec<(&'a [u8], usize)>);

impl Readback {
    /// Copy the frame's texture to CPU memory. Returns the pixel format and plane slices
    /// (borrowed from an internal buffer valid until the next call).
    pub fn read(&mut self, g: &GpuFrame) -> Result<ReadbackPlanes<'_>> {
        let (tex, sub) = texture_of(g)?;
        self.read_texture(&tex, sub)
    }

    pub fn read_texture(&mut self, tex: &ID3D11Texture2D, sub: u32) -> Result<ReadbackPlanes<'_>> {
        let src = tex_desc(tex);
        let fmt = match src.Format {
            DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB | DXGI_FORMAT_B8G8R8X8_UNORM => PixelFormat::Bgra8,
            DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => PixelFormat::Rgba8,
            DXGI_FORMAT_NV12 => PixelFormat::Nv12,
            f => return Err(CoreError::unsupported(format!("readback of DXGI format {}", f.0))),
        };
        let dev = device_of(tex)?;
        // SAFETY: staging texture created on the source's device; Map/Unmap paired.
        unsafe {
            let ctx = dev.GetImmediateContext().map_err(win_err("GetImmediateContext"))?;
            let reuse = matches!(&self.staging, Some((t, d))
                if d.Width == src.Width && d.Height == src.Height && d.Format == src.Format
                    && device_of(t).map(|x| x.as_raw() == dev.as_raw()).unwrap_or(false));
            if !reuse {
                let d = D3D11_TEXTURE2D_DESC {
                    Width: src.Width,
                    Height: src.Height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: src.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut t = None;
                dev.CreateTexture2D(&d, None, Some(&mut t)).map_err(win_err("CreateTexture2D(staging)"))?;
                self.staging = Some((t.ok_or_else(|| CoreError::platform("null staging"))?, d));
            }
            let Some((st, _)) = &self.staging else { unreachable!() };
            ctx.CopySubresourceRegion(st, 0, 0, 0, 0, tex, sub, None);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(st, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(win_err("Map"))?;
            let pitch = m.RowPitch as usize;
            let h = src.Height as usize;
            let rows = if fmt == PixelFormat::Nv12 { h + h.div_ceil(2) } else { h };
            let total = pitch * rows;
            self.buf.resize(total, 0);
            std::ptr::copy_nonoverlapping(m.pData as *const u8, self.buf.as_mut_ptr(), total);
            ctx.Unmap(st, 0);
            let planes = if fmt == PixelFormat::Nv12 {
                let (y, uv) = self.buf.split_at(pitch * h);
                vec![(y, pitch), (uv, pitch)]
            } else {
                vec![(&self.buf[..], pitch)]
            };
            Ok((fmt, planes))
        }
    }
}
