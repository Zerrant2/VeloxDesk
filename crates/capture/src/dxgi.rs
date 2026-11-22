//! Windows backend: DXGI Desktop Duplication on a D3D11 device created on the output's
//! adapter. See the crate docs for the GPU frame / device-sharing contract.

use std::any::Any;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED, HANDLE, HMODULE, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R10G10B10A2_UNORM,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutput5, IDXGIOutput6,
    IDXGIOutputDuplication, IDXGIResource, IDXGIResource1, DXGI_ERROR_ACCESS_DENIED, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_DEVICE_HUNG, DXGI_ERROR_DEVICE_REMOVED, DXGI_ERROR_DEVICE_RESET, DXGI_ERROR_INVALID_CALL,
    DXGI_ERROR_MODE_CHANGE_IN_PROGRESS, DXGI_ERROR_NOT_FOUND, DXGI_ERROR_SESSION_DISCONNECTED, DXGI_ERROR_UNSUPPORTED,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_MOVE_RECT, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR, DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR,
    DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME, DXGI_OUTPUT_DESC, DXGI_SHARED_RESOURCE_READ,
    DXGI_SHARED_RESOURCE_WRITE,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, DISPLAY_DEVICEW, ENUM_CURRENT_SETTINGS,
    MONITORINFO,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, CursorImage, CursorState, Display, DisplayId,
    Frame, FrameCapture, FrameData, GpuFrame, GpuHandle, PixelFormat, Plane, Rect,
};
use veloxdesk_core_traits::{CoreError, Result, Timestamp};

use crate::util::{clip_rect, covers_frame, cursor_to_rgba, merge_rects, CursorKind, IRect};

/// Pool size above which busy textures are allocated transiently instead of pooled.
const MAX_POOL: usize = 16;
/// CPU buffers kept for reuse.
const MAX_CPU_POOL: usize = 4;

// ───────────────────────────── texture holder ─────────────────────────────

/// Owner of one pooled capture texture; the `keepalive` of every GPU frame.
/// Downcast `GpuFrame::keepalive` to this type to get a typed COM pointer.
pub struct D3D11TextureHolder {
    texture: ID3D11Texture2D,
    shared: Option<HANDLE>,
}

// SAFETY: D3D11 resources are free-threaded COM objects; the device that owns them is
// multithread-protected (ID3D11Multithread), and the NT handle is just a kernel handle.
unsafe impl Send for D3D11TextureHolder {}
unsafe impl Sync for D3D11TextureHolder {}

impl D3D11TextureHolder {
    /// The pooled texture (`B8G8R8A8_UNORM`, DEFAULT usage, SRV|RTV bind).
    pub fn texture(&self) -> &ID3D11Texture2D {
        &self.texture
    }
    /// Shared NT handle (owned by the holder) if the texture was created shareable.
    pub fn shared_handle(&self) -> Option<usize> {
        self.shared.map(|h| h.0 as usize)
    }
    /// The capture device (same as `ID3D11DeviceChild::GetDevice` on the texture).
    pub fn device(&self) -> Option<ID3D11Device> {
        // SAFETY: valid COM object.
        unsafe { self.texture.GetDevice().ok() }
    }
}

impl Drop for D3D11TextureHolder {
    fn drop(&mut self) {
        if let Some(h) = self.shared.take() {
            // SAFETY: we own this handle (created by CreateSharedHandle).
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

// ───────────────────────────── errors ─────────────────────────────

fn map_err(e: windows::core::Error, what: &'static str) -> CoreError {
    let code = e.code();
    if code == DXGI_ERROR_ACCESS_LOST
        || code == DXGI_ERROR_DEVICE_REMOVED
        || code == DXGI_ERROR_DEVICE_RESET
        || code == DXGI_ERROR_DEVICE_HUNG
        || code == DXGI_ERROR_MODE_CHANGE_IN_PROGRESS
        || code == DXGI_ERROR_SESSION_DISCONNECTED
        || code == DXGI_ERROR_INVALID_CALL
    {
        CoreError::reinit(format!("{what}: {e}"))
    } else if code == E_ACCESSDENIED || code == DXGI_ERROR_ACCESS_DENIED {
        CoreError::AccessDenied(format!("{what}: {e}").into())
    } else if code == DXGI_ERROR_UNSUPPORTED {
        CoreError::unsupported(format!("{what}: {e}"))
    } else {
        CoreError::platform(format!("{what}: {e}"))
    }
}

fn is_device_lost(e: &CoreError) -> bool {
    matches!(e, CoreError::Reinit(m) if m.contains("0x887A0005") || m.contains("0x887A0006") || m.contains("0x887A0007"))
}

// ───────────────────────────── enumeration ─────────────────────────────

struct OutputInfo {
    adapter: IDXGIAdapter1,
    output: IDXGIOutput,
    desc: DXGI_OUTPUT_DESC,
    luid: u64,
    display: Display,
}

fn wide_to_string(w: &[u16]) -> String {
    let n = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..n])
}

fn enumerate() -> Result<Vec<OutputInfo>> {
    // SAFETY: plain DXGI/GDI calls with valid out-pointers.
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().map_err(|e| map_err(e, "CreateDXGIFactory1"))?;
        let mut out = Vec::new();
        let mut ai = 0;
        loop {
            let adapter = match factory.EnumAdapters1(ai) {
                Ok(a) => a,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(map_err(e, "EnumAdapters1")),
            };
            ai += 1;
            let luid = adapter
                .GetDesc1()
                .map(|d| ((d.AdapterLuid.HighPart as u32 as u64) << 32) | d.AdapterLuid.LowPart as u64)
                .unwrap_or(0);
            let mut oi = 0;
            loop {
                let output = match adapter.EnumOutputs(oi) {
                    Ok(o) => o,
                    Err(_) => break,
                };
                oi += 1;
                let Ok(desc) = output.GetDesc() else { continue };
                if !desc.AttachedToDesktop.as_bool() {
                    continue;
                }
                let display = describe(&output, &desc, out.len() as u32);
                out.push(OutputInfo { adapter: adapter.clone(), output, desc, luid, display });
            }
        }
        Ok(out)
    }
}

/// SAFETY: caller passes a valid output and its desc.
unsafe fn describe(output: &IDXGIOutput, desc: &DXGI_OUTPUT_DESC, index: u32) -> Display {
    let dev_name = wide_to_string(&desc.DeviceName);
    let name_ptr = PCWSTR(desc.DeviceName.as_ptr());
    let r = desc.DesktopCoordinates;

    let mut dm = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: valid, NUL-terminated device name and DEVMODEW with dmSize set.
    let refresh_hz = if unsafe { EnumDisplaySettingsW(name_ptr, ENUM_CURRENT_SETTINGS, &mut dm) }.as_bool() {
        dm.dmDisplayFrequency.max(1)
    } else {
        60
    };

    let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
    // SAFETY: as above.
    let friendly = if unsafe { EnumDisplayDevicesW(name_ptr, 0, &mut dd, 0) }.as_bool() {
        wide_to_string(&dd.DeviceString)
    } else {
        String::new()
    };
    let short = dev_name.trim_start_matches(r"\\.\").to_string();
    let name = if friendly.is_empty() { short } else { format!("{friendly} ({short})") };

    let (mut dx, mut dy) = (96u32, 96u32);
    // SAFETY: valid HMONITOR from DXGI.
    let scale_factor = if unsafe { GetDpiForMonitor(desc.Monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_ok() {
        dx as f32 / 96.0
    } else {
        1.0
    };

    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: valid HMONITOR, cbSize set.
    let primary = if unsafe { GetMonitorInfoW(desc.Monitor, &mut mi) }.as_bool() {
        mi.dwFlags & 1 != 0 // MONITORINFOF_PRIMARY
    } else {
        r.left == 0 && r.top == 0
    };

    let hdr = output
        .cast::<IDXGIOutput6>()
        .ok()
        // SAFETY: valid COM object.
        .and_then(|o6| unsafe { o6.GetDesc1() }.ok())
        .is_some_and(|d| d.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020);

    Display {
        id: DisplayId(dev_name),
        index,
        name,
        x: r.left,
        y: r.top,
        width: (r.right - r.left).max(0) as u32,
        height: (r.bottom - r.top).max(0) as u32,
        refresh_hz,
        scale_factor,
        primary,
        hdr,
    }
}

pub(crate) fn list_displays() -> Result<Vec<Display>> {
    Ok(enumerate()?.into_iter().map(|o| o.display).collect())
}

// ───────────────────────────── timing ─────────────────────────────

fn qpc_freq() -> i64 {
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        // SAFETY: valid out pointer.
        let _ = unsafe { QueryPerformanceFrequency(&mut f) };
        f.max(1)
    })
}

/// Map a QPC present time to our monotonic `Timestamp` clock.
fn qpc_to_timestamp(present_qpc: i64) -> Timestamp {
    let now_ts = Timestamp::now();
    let mut now = 0i64;
    // SAFETY: valid out pointer.
    if present_qpc <= 0 || unsafe { QueryPerformanceCounter(&mut now) }.is_err() {
        return now_ts;
    }
    let age_us = ((now - present_qpc).max(0) as i128 * 1_000_000 / qpc_freq() as i128) as u64;
    if age_us > 1_000_000 {
        now_ts
    } else {
        Timestamp(now_ts.0.saturating_sub(age_us))
    }
}

// ───────────────────────────── session ─────────────────────────────

struct Device {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    luid: u64,
}

fn create_device(adapter: &IDXGIAdapter1, luid: u64) -> Result<Device> {
    const LEVELS: [D3D_FEATURE_LEVEL; 4] =
        [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0];
    let mut device = None;
    let mut ctx = None;
    // Prefer VIDEO_SUPPORT so zero-copy MF encoders/VideoProcessor can reuse this device directly.
    let attempts = [
        (D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, &LEVELS[..]),
        (D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT, &LEVELS[1..]),
        (D3D11_CREATE_DEVICE_BGRA_SUPPORT, &LEVELS[..]),
        (D3D11_CREATE_DEVICE_BGRA_SUPPORT, &LEVELS[1..]),
    ];
    // SAFETY: valid adapter; out-pointers to Option<Interface>.
    unsafe {
        let mut last_err = None;
        for (flags, lvls) in attempts {
            device = None;
            ctx = None;
            match D3D11CreateDevice(
                adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                Some(lvls),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            ) {
                Ok(()) => {
                    last_err = None;
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(e) = last_err {
            return Err(map_err(e, "D3D11CreateDevice"));
        }
        let (Some(device), Some(ctx)) = (device, ctx) else {
            return Err(CoreError::platform("D3D11CreateDevice returned no device"));
        };
        let mt: ID3D11Multithread = ctx.cast().map_err(|e| map_err(e, "ID3D11Multithread"))?;
        let _ = mt.SetMultithreadProtected(true);
        Ok(Device { device, ctx, luid })
    }
}

/// SAFETY: valid output and device.
unsafe fn duplicate(output: &IDXGIOutput, device: &ID3D11Device, want_hdr: bool) -> Result<IDXGIOutputDuplication> {
    // DuplicateOutput1 (Win10 1703+, per-monitor-DPI-aware processes) lets us request
    // 10-bit RGB10A2 (HDR10) or BGRA explicitly; fall back to DuplicateOutput.
    if let Ok(o5) = output.cast::<IDXGIOutput5>() {
        let fmts: &[DXGI_FORMAT] = if want_hdr {
            &[DXGI_FORMAT_R10G10B10A2_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM]
        } else {
            &[DXGI_FORMAT_B8G8R8A8_UNORM]
        };
        // SAFETY: valid objects.
        match unsafe { o5.DuplicateOutput1(device, 0, fmts) } {
            Ok(d) => return Ok(d),
            Err(e) if e.code() == E_ACCESSDENIED || e.code() == DXGI_ERROR_ACCESS_DENIED => {
                return Err(map_err(e, "DuplicateOutput1"))
            }
            Err(e) => tracing::debug!("DuplicateOutput1 failed ({e}); falling back to DuplicateOutput"),
        }
    }
    let o1: IDXGIOutput1 = output.cast().map_err(|e| map_err(e, "IDXGIOutput1"))?;
    // SAFETY: valid objects.
    unsafe { o1.DuplicateOutput(device) }.map_err(|e| map_err(e, "DuplicateOutput"))
}

/// High-precision wait used for 120/144/240 FPS pacing without Windows' 15.6 ms sleep jitter.
fn pace_until(target: Instant) {
    loop {
        let now = Instant::now();
        if now >= target {
            return;
        }
        let left = target - now;
        if left > Duration::from_millis(2) {
            std::thread::sleep(left - Duration::from_millis(2));
        } else {
            std::thread::yield_now();
        }
    }
}

struct Session {
    dupl: IDXGIOutputDuplication,
    width: u32,
    height: u32,
    gpu: bool,
    min_interval: Duration,
}

/// DXGI Desktop Duplication capture (`caps().backend == "dxgi-dd"`).
pub struct DxgiCapture {
    prefer_gpu: bool,
    device: Option<Device>,
    session: Option<Session>,
    pool: Vec<Arc<D3D11TextureHolder>>,
    share_ok: bool,
    staging: Option<ID3D11Texture2D>,
    cpu_pool: Vec<Bytes>,
    last_frame: Option<Frame>,
    last_delivery: Option<Instant>,
    cursor: CursorState,
    cursor_known: bool,
    cursor_id: u64,
    shape_buf: Vec<u8>,
    meta_buf: Vec<u8>,
    rects: Vec<Rect>,
}

// SAFETY: all COM objects used here are free-threaded (D3D11 device is
// multithread-protected); the capture object is only used from one thread at a time.
unsafe impl Send for DxgiCapture {}

impl DxgiCapture {
    pub fn new(prefer_gpu: bool) -> Result<Self> {
        // Fail early on machines without any desktop output (services, headless CI).
        let outputs = enumerate()?;
        if outputs.is_empty() {
            return Err(CoreError::unsupported("no DXGI outputs attached to the desktop"));
        }
        Ok(Self::empty(prefer_gpu))
    }

    fn empty(prefer_gpu: bool) -> Self {
        Self {
            prefer_gpu,
            device: None,
            session: None,
            pool: Vec::new(),
            share_ok: true,
            staging: None,
            cpu_pool: Vec::new(),
            last_frame: None,
            last_delivery: None,
            cursor: CursorState::default(),
            cursor_known: false,
            cursor_id: 0,
            shape_buf: Vec::new(),
            meta_buf: Vec::new(),
            rects: Vec::new(),
        }
    }

    /// The D3D11 device frames are produced on (after `start`).
    pub fn device(&self) -> Option<&ID3D11Device> {
        self.device.as_ref().map(|d| &d.device)
    }

    fn drop_session(&mut self, err: &CoreError) {
        self.session = None;
        self.last_frame = None;
        if is_device_lost(err) {
            self.device = None;
            self.pool.clear();
            self.staging = None;
        }
    }

    fn new_texture(&mut self, w: u32, h: u32, format: DXGI_FORMAT) -> Result<Arc<D3D11TextureHolder>> {
        let dev = &self.device.as_ref().ok_or_else(|| CoreError::reinit("no device"))?.device;
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex = None;
        // SAFETY: valid device and desc; out-pointer to Option<Interface>.
        unsafe {
            if self.share_ok {
                desc.MiscFlags = (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32;
                if dev.CreateTexture2D(&desc, None, Some(&mut tex)).is_err() {
                    tracing::debug!("shared capture textures unsupported; using plain textures");
                    self.share_ok = false;
                    desc.MiscFlags = 0;
                    tex = None;
                }
            }
            if tex.is_none() {
                dev.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(|e| map_err(e, "CreateTexture2D"))?;
            }
            let texture = tex.ok_or_else(|| CoreError::platform("CreateTexture2D returned null"))?;
            let shared = if desc.MiscFlags != 0 {
                texture.cast::<IDXGIResource1>().ok().and_then(|r| {
                    r.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0, PCWSTR::null())
                        .ok()
                })
            } else {
                None
            };
            Ok(Arc::new(D3D11TextureHolder { texture, shared }))
        }
    }

    fn pooled_texture(&mut self, w: u32, h: u32, format: DXGI_FORMAT) -> Result<Arc<D3D11TextureHolder>> {
        let matching = |t: &Arc<D3D11TextureHolder>| {
            if Arc::strong_count(t) != 1 {
                return false;
            }
            let mut d = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: valid texture.
            unsafe { t.texture.GetDesc(&mut d) };
            d.Width == w && d.Height == h && d.Format == format
        };
        if let Some(t) = self.pool.iter().find(|t| matching(t)) {
            return Ok(t.clone());
        }
        let t = self.new_texture(w, h, format)?;
        if self.pool.len() < MAX_POOL {
            self.pool.push(t.clone());
        } else {
            tracing::debug!("capture texture pool exhausted ({MAX_POOL} in flight); allocating transient texture");
        }
        Ok(t)
    }

    fn ensure_staging(&mut self, w: u32, h: u32, format: DXGI_FORMAT) -> Result<ID3D11Texture2D> {
        if let Some(s) = &self.staging {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: valid texture.
            unsafe { s.GetDesc(&mut d) };
            if d.Width == w && d.Height == h && d.Format == format {
                return Ok(s.clone());
            }
            self.staging = None;
        }
        let dev = &self.device.as_ref().ok_or_else(|| CoreError::reinit("no device"))?.device;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut tex = None;
        // SAFETY: valid device and desc.
        unsafe { dev.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(|e| map_err(e, "CreateTexture2D(staging)"))?;
        let tex = tex.ok_or_else(|| CoreError::platform("CreateTexture2D returned null"))?;
        self.staging = Some(tex.clone());
        Ok(tex)
    }

    fn take_cpu_buffer(&mut self, len: usize) -> BytesMut {
        let mut found = None;
        for b in std::mem::take(&mut self.cpu_pool) {
            if found.is_none() {
                match b.try_into_mut() {
                    Ok(mut m) if m.capacity() >= len => {
                        m.clear();
                        found = Some(m);
                    }
                    Ok(_) => {} // too small (mode change): drop it
                    Err(b) => self.cpu_pool.push(b),
                }
            } else {
                self.cpu_pool.push(b);
            }
        }
        found.unwrap_or_else(|| BytesMut::with_capacity(len))
    }

    fn ctx(&self) -> Result<ID3D11DeviceContext> {
        Ok(self.device.as_ref().ok_or_else(|| CoreError::reinit("no device"))?.ctx.clone())
    }

    /// GPU path: copy `src` (same device, `w x h`) into a free pool texture.
    fn copy_to_pool(&mut self, src: &ID3D11Texture2D, w: u32, h: u32) -> Result<Arc<D3D11TextureHolder>> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid texture.
        unsafe { src.GetDesc(&mut desc) };
        let ctx = self.ctx()?;
        let holder = self.pooled_texture(w, h, desc.Format)?;
        // SAFETY: same device, same size/format. Flush so other devices (shared handle)
        // observe the copy promptly; same-device users are ordered by the context.
        unsafe {
            ctx.CopyResource(&holder.texture, src);
            ctx.Flush();
        }
        Ok(holder)
    }

    /// CPU path, step 1 (frame still acquired): queue the copy into the staging texture.
    fn copy_to_staging(&mut self, src: &ID3D11Texture2D, w: u32, h: u32) -> Result<ID3D11Texture2D> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: valid texture.
        unsafe { src.GetDesc(&mut desc) };
        let ctx = self.ctx()?;
        let staging = self.ensure_staging(w, h, desc.Format)?;
        // SAFETY: same device, same size/format.
        unsafe { ctx.CopyResource(&staging, src) };
        Ok(staging)
    }

    /// CPU path, step 2 (after ReleaseFrame): map and pack rows tightly (stride `w*4`).
    fn read_staging(&mut self, staging: &ID3D11Texture2D, w: u32, h: u32) -> Result<Bytes> {
        let ctx = self.ctx()?;
        let row = w as usize * 4;
        let mut buf = self.take_cpu_buffer(row * h as usize);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging texture with CPU read access, `h` rows of at least `row` bytes
        // each at `RowPitch` spacing; unmapped before returning.
        unsafe {
            ctx.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)).map_err(|e| map_err(e, "Map(staging)"))?;
            let base = mapped.pData as *const u8;
            let pitch = mapped.RowPitch as usize;
            for y in 0..h as usize {
                buf.extend_from_slice(std::slice::from_raw_parts(base.add(y * pitch), row));
            }
            ctx.Unmap(staging, 0);
        }
        let bytes = buf.freeze();
        if self.cpu_pool.len() < MAX_CPU_POOL {
            self.cpu_pool.push(bytes.clone());
        }
        Ok(bytes)
    }

    /// Read the frame's dirty + move rects into `self.rects` (empty = whole frame).
    ///
    /// SAFETY: a frame must currently be acquired on `dupl`.
    unsafe fn read_dirty_rects(&mut self, dupl: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO, w: u32, h: u32) {
        self.rects.clear();
        let total = info.TotalMetadataBufferSize as usize;
        if total == 0 {
            return;
        }
        if self.meta_buf.len() < total {
            self.meta_buf.resize(total, 0);
        }
        let mut ok = true;
        let mut needed = 0u32;
        // Move rects: the destination region changed.
        let move_ptr = self.meta_buf.as_mut_ptr() as *mut DXGI_OUTDUPL_MOVE_RECT;
        // SAFETY: buffer has `total` bytes; DXGI writes at most that many. Vec<u8> is not
        // necessarily aligned for the structs, so we read them unaligned below.
        match unsafe { dupl.GetFrameMoveRects(total as u32, move_ptr, &mut needed) } {
            Ok(()) => {
                let n = needed as usize / std::mem::size_of::<DXGI_OUTDUPL_MOVE_RECT>();
                for i in 0..n {
                    // SAFETY: i < n, within written bytes.
                    let m = unsafe { std::ptr::read_unaligned(move_ptr.add(i)) };
                    let d = m.DestinationRect;
                    if let Some(r) = clip_rect(IRect { left: d.left, top: d.top, right: d.right, bottom: d.bottom }, w, h) {
                        self.rects.push(r);
                    }
                }
            }
            Err(_) => ok = false,
        }
        let rect_ptr = self.meta_buf.as_mut_ptr() as *mut RECT;
        // SAFETY: as above.
        match unsafe { dupl.GetFrameDirtyRects(total as u32, rect_ptr, &mut needed) } {
            Ok(()) => {
                let n = needed as usize / std::mem::size_of::<RECT>();
                for i in 0..n {
                    // SAFETY: i < n.
                    let d = unsafe { std::ptr::read_unaligned(rect_ptr.add(i)) };
                    if let Some(r) = clip_rect(IRect { left: d.left, top: d.top, right: d.right, bottom: d.bottom }, w, h) {
                        self.rects.push(r);
                    }
                }
            }
            Err(_) => ok = false,
        }
        if !ok {
            self.rects.clear();
            return;
        }
        merge_rects(&mut self.rects);
        if covers_frame(&self.rects, w, h) {
            self.rects.clear();
        }
    }

    /// Update `self.cursor` from frame info; returns whether anything cursor-related changed.
    ///
    /// SAFETY: a frame must currently be acquired on `dupl`.
    unsafe fn read_cursor(&mut self, dupl: &IDXGIOutputDuplication, info: &DXGI_OUTDUPL_FRAME_INFO) -> bool {
        self.cursor.shape = None;
        if info.LastMouseUpdateTime == 0 {
            return false;
        }
        let p = info.PointerPosition;
        self.cursor.visible = p.Visible.as_bool();
        if self.cursor.visible {
            self.cursor.x = p.Position.x;
            self.cursor.y = p.Position.y;
        }
        self.cursor_known = true;
        let size = info.PointerShapeBufferSize as usize;
        if size > 0 {
            if self.shape_buf.len() < size {
                self.shape_buf.resize(size, 0);
            }
            let mut needed = 0u32;
            let mut si = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
            // SAFETY: buffer has `size` bytes.
            let r = unsafe {
                dupl.GetFramePointerShape(size as u32, self.shape_buf.as_mut_ptr().cast(), &mut needed, &mut si)
            };
            if r.is_ok() {
                let kind = match si.Type as i32 {
                    t if t == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR.0 => Some(CursorKind::Color),
                    t if t == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MASKED_COLOR.0 => Some(CursorKind::MaskedColor),
                    t if t == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME.0 => Some(CursorKind::Monochrome),
                    _ => None,
                };
                let src = &self.shape_buf[..(needed as usize).min(size)];
                if let Some((w, h, rgba)) = kind.and_then(|k| cursor_to_rgba(k, si.Width, si.Height, si.Pitch, src)) {
                    self.cursor_id += 1;
                    self.cursor.shape = Some(CursorImage {
                        id: self.cursor_id,
                        width: w,
                        height: h,
                        hotspot_x: si.HotSpot.x.max(0) as u32,
                        hotspot_y: si.HotSpot.y.max(0) as u32,
                        rgba: Bytes::from(rgba),
                    });
                }
            }
        }
        true
    }

    fn try_next(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        let Some(sess) = &self.session else {
            return Err(CoreError::reinit("capture not started"));
        };
        let dupl = sess.dupl.clone();
        let (exp_w, exp_h, gpu, min_interval) = (sess.width, sess.height, sess.gpu, sess.min_interval);

        // Rate limit (max_fps): wait out the remaining interval with sub-ms precision (120/144/240 FPS).
        let deadline = Instant::now() + timeout;
        if let Some(last) = self.last_delivery {
            let next = last + min_interval;
            let now = Instant::now();
            if next > now {
                if next >= deadline {
                    pace_until(deadline);
                    return Ok(None);
                }
                pace_until(next);
            }
        }
        let wait_ms = deadline.saturating_duration_since(Instant::now()).as_millis().min(u32::MAX as u128) as u32;

        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // SAFETY: valid duplication; out-pointers valid.
        match unsafe { dupl.AcquireNextFrame(wait_ms, &mut info, &mut resource) } {
            Ok(()) => {}
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
            Err(e) => return Err(map_err(e, "AcquireNextFrame")),
        }
        let t0 = Instant::now();

        // Everything that needs the acquired frame; ReleaseFrame right after.
        struct Acquired {
            w: u32,
            h: u32,
            fmt: PixelFormat,
            cs: ColorSpace,
            pending: Pending,
        }
        let acquired = (|| -> Result<Option<Acquired>> {
            // SAFETY: frame acquired above.
            let cursor_changed = unsafe { self.read_cursor(&dupl, &info) };
            if info.LastPresentTime == 0 {
                return Ok(if cursor_changed {
                    Some(Acquired {
                        w: exp_w,
                        h: exp_h,
                        fmt: PixelFormat::Bgra8,
                        cs: ColorSpace::Srgb,
                        pending: Pending::CursorOnly,
                    })
                } else {
                    None
                });
            }
            let res = resource.as_ref().ok_or_else(|| CoreError::platform("AcquireNextFrame returned no resource"))?;
            let src: ID3D11Texture2D = res.cast().map_err(|e| map_err(e, "IDXGIResource→ID3D11Texture2D"))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: valid texture.
            unsafe { src.GetDesc(&mut desc) };
            if desc.Width != exp_w || desc.Height != exp_h {
                return Err(CoreError::reinit(format!(
                    "desktop size changed {exp_w}x{exp_h} → {}x{}",
                    desc.Width, desc.Height
                )));
            }
            let (fmt, cs) = match desc.Format {
                DXGI_FORMAT_B8G8R8A8_UNORM => (PixelFormat::Bgra8, ColorSpace::Srgb),
                DXGI_FORMAT_R10G10B10A2_UNORM => (PixelFormat::Rgb10a2, ColorSpace::Bt2020Pq),
                other => return Err(CoreError::unsupported(format!("unexpected desktop format {other:?}"))),
            };
            // SAFETY: frame acquired.
            unsafe { self.read_dirty_rects(&dupl, &info, exp_w, exp_h) };
            let pending = if gpu {
                Pending::Gpu(self.copy_to_pool(&src, exp_w, exp_h)?)
            } else {
                Pending::Cpu(self.copy_to_staging(&src, exp_w, exp_h)?)
            };
            Ok(Some(Acquired { w: exp_w, h: exp_h, fmt, cs, pending }))
        })();
        drop(resource);
        // SAFETY: a frame is acquired.
        let rel = unsafe { dupl.ReleaseFrame() };
        let acquired = acquired?;
        if let Err(e) = rel {
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                return Err(map_err(e, "ReleaseFrame"));
            }
        }
        let Some(Acquired { w, h, fmt: pix_fmt, cs: color_space, pending }) = acquired else { return Ok(None) };

        let capture_ts = if matches!(pending, Pending::CursorOnly) {
            Timestamp::now()
        } else {
            qpc_to_timestamp(info.LastPresentTime)
        };
        let (data, dirty_rects, final_fmt, final_cs) = match pending {
            Pending::CursorOnly => match &self.last_frame {
                Some(f) => (f.data.clone(), vec![crate::CURSOR_ONLY], f.format, f.color_space),
                None => return Ok(None),
            },
            Pending::Gpu(holder) => {
                let luid = self.device.as_ref().map(|d| d.luid).unwrap_or(0);
                let handle = GpuHandle::D3D11Texture {
                    texture: holder.texture.as_raw() as usize,
                    subresource: 0,
                    adapter_luid: luid,
                    shared_handle: holder.shared_handle(),
                };
                let keepalive: Arc<dyn Any + Send + Sync> = holder;
                (
                    FrameData::Gpu(GpuFrame { handle, keepalive: Some(keepalive) }),
                    std::mem::take(&mut self.rects),
                    pix_fmt,
                    color_space,
                )
            }
            Pending::Cpu(staging) => {
                let bytes = self.read_staging(&staging, w, h)?;
                (
                    FrameData::Cpu(vec![Plane { data: bytes, stride: w as usize * 4 }]),
                    std::mem::take(&mut self.rects),
                    pix_fmt,
                    color_space,
                )
            }
        };
        let capture_us = t0.elapsed().as_micros().min(u32::MAX as u128) as u32;

        let frame = Frame {
            width: w,
            height: h,
            format: final_fmt,
            color_space: final_cs,
            color_range: ColorRange::Full,
            data,
            capture_ts,
        };
        self.last_frame = Some(frame.clone());
        self.last_delivery = Some(Instant::now());
        let cursor = self.cursor_known.then(|| self.cursor.clone());
        self.cursor.shape = None;
        Ok(Some(CapturedFrame { frame, dirty_rects, cursor, capture_us, convert_us: 0 }))
    }
}

enum Pending {
    CursorOnly,
    Gpu(Arc<D3D11TextureHolder>),
    Cpu(ID3D11Texture2D),
}

impl FrameCapture for DxgiCapture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps {
            backend: "dxgi-dd",
            gpu_frames: true,
            dirty_rects: true,
            separate_cursor: true,
            hdr: true,
            formats: vec![PixelFormat::Bgra8, PixelFormat::Rgb10a2],
        }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        list_displays()
    }

    fn start(&mut self, cfg: &CaptureConfig) -> Result<()> {
        self.stop();
        let outputs = enumerate()?;
        let out = if cfg.display.0.is_empty() {
            outputs.iter().find(|o| o.display.primary).or(outputs.first())
        } else {
            outputs.iter().find(|o| o.display.id == cfg.display)
        }
        .ok_or_else(|| CoreError::invalid(format!("display {:?} not found", cfg.display.0)))?;

        // Reuse the device across restarts when possible so encoders keep their resources.
        let reuse = self.device.as_ref().is_some_and(|d| {
            // SAFETY: valid device.
            d.luid == out.luid && unsafe { d.device.GetDeviceRemovedReason() }.is_ok()
        });
        if !reuse {
            self.pool.clear();
            self.staging = None;
            self.device = Some(create_device(&out.adapter, out.luid)?);
        }
        let device = self.device.as_ref().map(|d| d.device.clone()).ok_or_else(|| CoreError::reinit("no device"))?;
        // SAFETY: valid output and device.
        let dupl = unsafe { duplicate(&out.output, &device, cfg.hdr && out.display.hdr) }?;
        // SAFETY: valid duplication.
        let dd = unsafe { dupl.GetDesc() };
        let (width, height) = (dd.ModeDesc.Width, dd.ModeDesc.Height);
        let (dw, dh) = (
            (out.desc.DesktopCoordinates.right - out.desc.DesktopCoordinates.left) as u32,
            (out.desc.DesktopCoordinates.bottom - out.desc.DesktopCoordinates.top) as u32,
        );
        if (dw, dh) != (width, height) && (dh, dw) != (width, height) {
            tracing::debug!("duplication mode {width}x{height} differs from desktop rect {dw}x{dh}");
        }
        // Drop pooled resources of a different size.
        if let Some(s) = &self.staging {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: valid texture.
            unsafe { s.GetDesc(&mut d) };
            if (d.Width, d.Height) != (width, height) {
                self.staging = None;
                self.pool.clear();
            }
        }
        if cfg.draw_cursor {
            tracing::debug!("draw_cursor requested: DXGI DD delivers the cursor separately (not composited)");
        }
        let gpu = self.prefer_gpu && cfg.prefer_gpu;
        let min_interval = if cfg.max_fps == 0 {
            Duration::ZERO
        } else {
            // Slightly under the period so a 60 Hz display isn't throttled to 30 at max_fps=60.
            Duration::from_micros(1_000_000 / cfg.max_fps as u64).mul_f32(0.9)
        };
        tracing::info!(
            display = %out.display.name, width, height, gpu, luid = out.luid,
            "DXGI desktop duplication started"
        );
        self.session = Some(Session { dupl, width, height, gpu, min_interval });
        Ok(())
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        match self.try_next(timeout) {
            Ok(f) => Ok(f),
            Err(e) => {
                if matches!(e, CoreError::Reinit(_)) {
                    self.drop_session(&e);
                }
                Err(e)
            }
        }
    }

    fn stop(&mut self) {
        self.session = None;
        self.last_frame = None;
        self.last_delivery = None;
        self.cursor = CursorState::default();
        self.cursor_known = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Direct3D11::D3D11_SUBRESOURCE_DATA;

    fn test_capture() -> Option<DxgiCapture> {
        // SAFETY: plain DXGI calls.
        let adapter = unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
            factory.EnumAdapters1(0).ok()?
        };
        let dev = create_device(&adapter, 7).ok()?;
        let mut c = DxgiCapture::empty(true);
        c.device = Some(dev);
        Some(c)
    }

    fn px(x: u32, y: u32) -> [u8; 4] {
        [x as u8, y as u8, (x ^ y) as u8, 0xFF]
    }

    fn pattern_texture(dev: &ID3D11Device, w: u32, h: u32) -> ID3D11Texture2D {
        let data: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| px(x, y))).collect();
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let init = D3D11_SUBRESOURCE_DATA { pSysMem: data.as_ptr().cast(), SysMemPitch: w * 4, SysMemSlicePitch: 0 };
        let mut tex = None;
        // SAFETY: valid desc and init data that outlives the call.
        unsafe { dev.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }.unwrap();
        tex.unwrap()
    }

    fn check(bytes: &[u8], w: u32, h: u32) {
        assert_eq!(bytes.len(), (w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let o = ((y * w + x) * 4) as usize;
                assert_eq!(&bytes[o..o + 4], &px(x, y), "pixel {x},{y}");
            }
        }
    }

    /// Exercises the copy / pool / readback machinery on a real D3D11 device without
    /// needing Desktop Duplication (which requires the interactive input desktop).
    #[test]
    fn pool_and_readback() {
        let Some(mut c) = test_capture() else {
            println!("skip: no D3D11 device");
            return;
        };
        let (w, h) = (67, 33); // odd width: RowPitch padding must be stripped
        let dev = c.device().unwrap().clone();
        let src = pattern_texture(&dev, w, h);

        // GPU pool: a held frame is never recycled; a released one is.
        let a = c.copy_to_pool(&src, w, h).unwrap();
        let pa = a.texture().as_raw();
        let b = c.copy_to_pool(&src, w, h).unwrap();
        assert_ne!(pa, b.texture().as_raw());
        drop(a);
        let a2 = c.copy_to_pool(&src, w, h).unwrap();
        assert_eq!(a2.texture().as_raw(), pa);
        assert_eq!(c.pool.len(), 2);
        println!("shared handle: {:?}", a2.shared_handle());

        // The texture hands out the capture device, which is multithread-protected.
        let d = a2.device().unwrap();
        assert_eq!(d.as_raw(), dev.as_raw());
        // SAFETY: valid device/context.
        unsafe {
            let mt: ID3D11Multithread = c.ctx().unwrap().cast().unwrap();
            assert!(mt.GetMultithreadProtected().as_bool());
        }

        // Content of the pooled GPU texture, read back through the CPU path.
        let st = c.copy_to_staging(a2.texture(), w, h).unwrap();
        let bytes = c.read_staging(&st, w, h).unwrap();
        check(&bytes, w, h);

        // CPU buffers are recycled once the consumer drops them, never while held.
        let p1 = bytes.as_ptr();
        let st = c.copy_to_staging(&src, w, h).unwrap();
        let held = c.read_staging(&st, w, h).unwrap();
        assert_ne!(held.as_ptr(), p1);
        check(&held, w, h);
        drop(bytes);
        let st = c.copy_to_staging(&src, w, h).unwrap();
        let again = c.read_staging(&st, w, h).unwrap();
        assert_eq!(again.as_ptr(), p1);
        check(&again, w, h);
    }

    #[test]
    fn error_mapping() {
        let e = |hr| windows::core::Error::from_hresult(hr);
        assert!(matches!(map_err(e(DXGI_ERROR_ACCESS_LOST), "x"), CoreError::Reinit(_)));
        assert!(is_device_lost(&map_err(e(DXGI_ERROR_DEVICE_REMOVED), "x")));
        assert!(!is_device_lost(&map_err(e(DXGI_ERROR_ACCESS_LOST), "x")));
        assert!(matches!(map_err(e(E_ACCESSDENIED), "x"), CoreError::AccessDenied(_)));
        assert!(matches!(map_err(e(DXGI_ERROR_UNSUPPORTED), "x"), CoreError::Unsupported(_)));
    }
}
