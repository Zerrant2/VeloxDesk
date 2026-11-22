//! Video frame types and the capture / encode / decode traits.

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;

use crate::{Result, Timestamp};

// ───────────────────────────── basic types ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// 8-bit B,G,R,A (DXGI / most desktop capture).
    Bgra8,
    Rgba8,
    /// 4:2:0 8-bit, Y plane + interleaved UV plane.
    Nv12,
    /// 4:2:0 10-bit in 16-bit containers (HDR10).
    P010,
    /// Planar 4:2:0 8-bit (Y, U, V).
    I420,
    /// Planar 4:4:4 8-bit.
    Yuv444,
    /// Planar 4:4:4 10-bit in 16-bit containers.
    Yuv444P10,
    /// 10:10:10:2 RGB (HDR desktop capture).
    Rgb10a2,
}

impl PixelFormat {
    pub fn is_yuv(self) -> bool {
        matches!(self, Self::Nv12 | Self::P010 | Self::I420 | Self::Yuv444 | Self::Yuv444P10)
    }
    pub fn is_10bit(self) -> bool {
        matches!(self, Self::P010 | Self::Yuv444P10 | Self::Rgb10a2)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorSpace {
    #[default]
    Bt709,
    Bt601,
    /// BT.2020 with PQ transfer (HDR10).
    Bt2020Pq,
    Srgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorRange {
    #[default]
    Limited,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Re-export of the protocol codec enum for the API surface.
pub use veloxdesk_protocol::control::{ChromaFormat, VideoCodec};

// ───────────────────────────── frame storage ─────────────────────────────

/// One CPU plane.
#[derive(Clone)]
pub struct Plane {
    pub data: Bytes,
    /// Bytes per row (may be > width * bpp).
    pub stride: usize,
}

impl fmt::Debug for Plane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plane").field("len", &self.data.len()).field("stride", &self.stride).finish()
    }
}

/// One DMA-BUF plane (Linux).
#[derive(Debug, Clone, Copy)]
pub struct DmaBufPlane {
    pub fd: i32,
    pub offset: u32,
    pub stride: u32,
}

/// Native GPU resource handle. Pointers are stored as `usize` so the type is
/// platform-neutral; each backend documents ownership in its own crate.
#[derive(Debug, Clone)]
pub enum GpuHandle {
    /// Windows: `ID3D11Texture2D*` (borrowed, AddRef'd by `keepalive`), its array slice and
    /// the adapter LUID; `shared_handle` is an NT handle usable by other devices (D3D12/wgpu).
    D3D11Texture { texture: usize, subresource: u32, adapter_luid: u64, shared_handle: Option<usize> },
    /// Linux: DMA-BUF planes with DRM fourcc + modifier.
    DmaBuf { planes: Vec<DmaBufPlane>, fourcc: u32, modifier: u64 },
    /// macOS/iOS: `IOSurfaceRef`.
    IoSurface { surface: usize },
    /// macOS/iOS: `CVPixelBufferRef`.
    CvPixelBuffer { buffer: usize },
    /// Vulkan: `VkImage` + `VkDeviceMemory` owned by the producing device.
    VulkanImage { image: u64, memory: u64, device: usize },
    /// CUDA device pointer (NVENC/NVDEC on Linux).
    CudaPtr { ptr: u64, pitch: usize, context: usize },
}

/// A GPU-resident frame. `keepalive` holds whatever object must stay alive while the
/// handle is in use (COM pointer wrapper, DMA-BUF fd owner, pooled surface, ...).
#[derive(Clone)]
pub struct GpuFrame {
    pub handle: GpuHandle,
    pub keepalive: Option<Arc<dyn Any + Send + Sync>>,
}

impl fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GpuFrame").field("handle", &self.handle).finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum FrameData {
    /// Planes in the order defined by the pixel format (packed formats have one plane).
    Cpu(Vec<Plane>),
    Gpu(GpuFrame),
}

/// A raw (uncompressed) video frame, either captured or decoded.
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub color_space: ColorSpace,
    pub color_range: ColorRange,
    pub data: FrameData,
    /// Host capture time (carried through encode → network → decode).
    pub capture_ts: Timestamp,
}

impl Frame {
    pub fn is_gpu(&self) -> bool {
        matches!(self.data, FrameData::Gpu(_))
    }
}

// ───────────────────────────── capture ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayId(pub String);

#[derive(Debug, Clone)]
pub struct Display {
    pub id: DisplayId,
    /// Stable index used on the wire (`DisplayInfo.index`).
    pub index: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub scale_factor: f32,
    pub primary: bool,
    pub hdr: bool,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub display: DisplayId,
    /// Upper bound; capture may deliver fewer frames when the screen is static.
    pub max_fps: u32,
    /// Preferred output format; backends return what they can (check `Frame::format`).
    pub preferred_format: PixelFormat,
    /// Prefer GPU-resident frames (zero-copy into the encoder).
    pub prefer_gpu: bool,
    /// Include the cursor in the image (desktop mode). Gaming mode sends it separately.
    pub draw_cursor: bool,
    pub hdr: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CursorState {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// Present only when the shape changed since the previous frame.
    pub shape: Option<CursorImage>,
}

#[derive(Debug, Clone)]
pub struct CursorImage {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub hotspot_x: u32,
    pub hotspot_y: u32,
    /// Straight-alpha RGBA8.
    pub rgba: Bytes,
}

#[derive(Debug, Clone)]
pub struct CapturedFrame {
    pub frame: Frame,
    /// Changed regions since the previous frame; empty = whole frame / unknown.
    pub dirty_rects: Vec<Rect>,
    pub cursor: Option<CursorState>,
    /// Time spent inside the capture API for this frame (µs).
    pub capture_us: u32,
    /// Time spent on color conversion inside the capture backend, if any (µs).
    pub convert_us: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CaptureCaps {
    pub backend: &'static str,
    pub gpu_frames: bool,
    pub dirty_rects: bool,
    pub separate_cursor: bool,
    pub hdr: bool,
    pub formats: Vec<PixelFormat>,
}

/// Screen capture backend (DXGI DD / WGC / PipeWire / X11 / ScreenCaptureKit / ...).
pub trait FrameCapture: Send {
    fn caps(&self) -> CaptureCaps;
    /// Enumerate capturable displays.
    fn displays(&self) -> Result<Vec<Display>>;
    /// Start (or restart) capturing with `cfg`.
    fn start(&mut self, cfg: &CaptureConfig) -> Result<()>;
    /// Wait up to `timeout` for the next frame. `Ok(None)` = nothing changed (static
    /// screen); callers may re-send the previous frame or idle. `Err(Reinit)` = recreate.
    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>>;
    fn stop(&mut self);
}

// ───────────────────────────── encode ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateControl {
    /// Constant bitrate with a VBV of about one frame — the low-latency default.
    Cbr,
    Vbr,
    /// Constant QP (testing).
    Cqp(u8),
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub rate_control: RateControl,
    pub chroma: ChromaFormat,
    pub bit_depth: u8,
    /// `None` = infinite GOP (IDR only on request / intra-refresh).
    pub gop: Option<u32>,
    /// Use rolling intra-refresh instead of periodic IDR frames.
    pub intra_refresh: bool,
    /// Input frames will be GPU-resident (`FrameData::Gpu`) of this format.
    pub input_format: PixelFormat,
    pub gpu_input: bool,
    pub color_space: ColorSpace,
}

#[derive(Debug, Clone, Default)]
pub struct EncoderCaps {
    /// e.g. "NVENC", "AMF", "QSV", "MediaFoundation", "VAAPI", "VideoToolbox", "openh264".
    pub name: &'static str,
    pub codecs: Vec<VideoCodec>,
    pub hardware: bool,
    /// Accepts `FrameData::Gpu` of these formats without CPU readback.
    pub gpu_input_formats: Vec<PixelFormat>,
    pub cpu_input_formats: Vec<PixelFormat>,
    pub yuv444: bool,
    pub ten_bit: bool,
    pub intra_refresh: bool,
    pub max_width: u32,
    pub max_height: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct EncodeOptions {
    pub force_keyframe: bool,
}

#[derive(Debug, Clone)]
pub struct EncodedPacket {
    /// Annex-B (H.264/H.265) or OBU (AV1) bitstream of exactly one frame.
    pub data: Bytes,
    pub codec: VideoCodec,
    pub keyframe: bool,
    /// Intra-refresh wave completed with this frame.
    pub recovery_point: bool,
    pub width: u32,
    pub height: u32,
    pub capture_ts: Timestamp,
    /// Monotonic per-stream frame counter assigned by the encoder wrapper.
    pub frame_id: u32,
    pub encode_us: u32,
    /// Color conversion time spent inside the encoder backend (µs).
    pub convert_us: u32,
}

/// Dynamic encoder parameters (ABR).
#[derive(Debug, Clone, Copy, Default)]
pub struct EncoderUpdate {
    pub bitrate_kbps: Option<u32>,
    pub fps: Option<u32>,
}

/// Video encoder. One instance per stream; calls are made from a single encode thread.
pub trait VideoEncoder: Send {
    fn caps(&self) -> EncoderCaps;
    fn config(&self) -> &EncoderConfig;
    /// Encode one frame. Low-latency encoders must return the packet for this frame
    /// synchronously (no B-frames, no lookahead) — `Ok(None)` only if the encoder
    /// intentionally dropped the frame (rate control).
    fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>>;
    /// Apply new bitrate / fps without a keyframe where the backend supports it.
    fn update(&mut self, update: EncoderUpdate) -> Result<()>;
}

// ───────────────────────────── decode ─────────────────────────────

#[derive(Debug, Clone)]
pub struct DecoderConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    /// Prefer output frames that stay on the GPU (zero-copy into the renderer).
    pub prefer_gpu_output: bool,
    /// Opaque native device to decode on (e.g. `ID3D11Device*` shared with the renderer).
    pub shared_device: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct DecoderCaps {
    /// e.g. "D3D11VA", "NVDEC", "VAAPI", "VideoToolbox", "openh264".
    pub name: &'static str,
    pub codecs: Vec<VideoCodec>,
    pub hardware: bool,
    pub gpu_output: bool,
    pub yuv444: bool,
    pub ten_bit: bool,
}

#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub frame: Frame,
    pub frame_id: u32,
    pub decode_us: u32,
}

/// Video decoder. One instance per stream, single decode thread.
pub trait VideoDecoder: Send {
    fn caps(&self) -> DecoderCaps;
    /// Decode one complete access unit. Must output the frame for this packet without
    /// reordering delay (`Ok(None)` only for non-displayable data, e.g. parameter sets).
    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<DecodedFrame>>;
    /// Drop references (after loss) so the next keyframe starts cleanly.
    fn reset(&mut self) -> Result<()>;
}
