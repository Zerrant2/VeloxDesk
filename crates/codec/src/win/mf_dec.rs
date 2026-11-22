//! Hardware H.264 / HEVC / AV1 decoding through Media Foundation D3D11VA MFTs.
//!
//! Uses a D3D11 device manager (`IMFDXGIDeviceManager`) with low-latency mode enabled so
//! frames are emitted without B-frame reordering delay. Outputs NV12 either as a pooled
//! GPU `ID3D11Texture2D` (when `prefer_gpu_output` is set) or as CPU `Plane`s.

use std::ffi::c_void;
use std::time::Instant;

use bytes::Bytes;
use veloxdesk_core_traits::video::{
    ColorRange, ColorSpace, DecodedFrame, DecoderCaps, DecoderConfig, EncodedPacket, Frame, FrameData, PixelFormat,
    Plane, VideoCodec,
};
use veloxdesk_core_traits::{CoreError, Result, VideoDecoder};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::VARIANT;
use windows::core::Interface;

use super::d3d::{self, GpuSync, Readback, TexturePool, win_err};
use super::mf::{self, ensure_mf, mf_err};
use crate::convert;

pub const NAME: &str = "D3D11VA";

fn codec_set(api: &Option<ICodecAPI>, key: &windows::core::GUID, v: VARIANT) -> bool {
    match api {
        // SAFETY: ICodecAPI::SetValue with a stack VARIANT.
        Some(a) => unsafe { a.SetValue(key, &v).is_ok() },
        None => false,
    }
}

/// Select the decoder's native YUV 4:2:0 output format (`NV12` for 8-bit, `P010` for 10-bit).
/// Because `MFT_DECODER_EXPOSE_OUTPUT_TYPES_IN_NATIVE_ORDER` is set, the first matching
/// NV12/P010 format matches the bitstream bit depth.
fn select_yuv_output(transform: &IMFTransform) -> Result<(IMFMediaType, PixelFormat)> {
    // SAFETY: GetOutputAvailableType / SetOutputType on a valid MFT.
    unsafe {
        let mut idx = 0u32;
        loop {
            let t = match transform.GetOutputAvailableType(0, idx) {
                Ok(t) => t,
                Err(e) if e.code() == MF_E_NO_MORE_TYPES => {
                    return Err(CoreError::unsupported("MF decoder does not offer NV12 or P010 output"));
                }
                Err(e) => return Err(mf_err("GetOutputAvailableType")(e)),
            };
            match t.GetGUID(&MF_MT_SUBTYPE).ok() {
                Some(g) if g == MFVideoFormat_NV12 => {
                    transform.SetOutputType(0, &t, 0).map_err(mf_err("SetOutputType(NV12)"))?;
                    return Ok((t, PixelFormat::Nv12));
                }
                Some(g) if g == MFVideoFormat_P010 => {
                    transform.SetOutputType(0, &t, 0).map_err(mf_err("SetOutputType(P010)"))?;
                    return Ok((t, PixelFormat::P010));
                }
                _ => {}
            }
            idx += 1;
        }
    }
}

fn output_dimensions(t: &IMFMediaType, fallback_w: u32, fallback_h: u32) -> (u32, u32) {
    // SAFETY: MF attribute getters with stack buffers.
    unsafe {
        let mut area = MFVideoArea::default();
        let bytes = std::slice::from_raw_parts_mut((&raw mut area).cast::<u8>(), std::mem::size_of::<MFVideoArea>());
        if t.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
            return (area.Area.cx as u32, area.Area.cy as u32);
        }
        if let Ok(sz) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
            let w = (sz >> 32) as u32;
            let h = (sz & 0xFFFF_FFFF) as u32;
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
    }
    (fallback_w, fallback_h)
}

fn resolve_device(shared_device: Option<usize>) -> Result<ID3D11Device> {
    if let Some(raw) = shared_device.filter(|&p| p != 0) {
        let ptr = raw as *mut c_void;
        // SAFETY: caller contract on DecoderConfig::shared_device guarantees a live ID3D11Device*.
        let dev = unsafe { ID3D11Device::from_raw_borrowed(&ptr) }
            .ok_or_else(|| CoreError::invalid("null shared_device"))?
            .clone();
        d3d::enable_multithread(&dev);
        return Ok(dev);
    }
    d3d::create_device(None)
}

pub fn caps_for(codecs: Vec<VideoCodec>, ten_bit: bool) -> DecoderCaps {
    DecoderCaps {
        name: NAME,
        codecs,
        hardware: true,
        gpu_output: true,
        yuv444: false,
        ten_bit,
    }
}

/// Media Foundation D3D11VA video decoder.
pub struct MfDecoder {
    codec: VideoCodec,
    width: u32,
    height: u32,
    output_format: PixelFormat,
    prefer_gpu_output: bool,
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    luid: u64,
    _manager: IMFDXGIDeviceManager,
    transform: IMFTransform,
    provides_samples: bool,
    out_size: u32,
    pool: Option<TexturePool>,
    gpu_sync: Option<GpuSync>,
    readback: Readback,
    last_ts_hns: i64,
}

// SAFETY: MF/D3D11 objects are free-threaded; the device is multithread-protected and
// VideoDecoder is driven from one decode thread at a time.
unsafe impl Send for MfDecoder {}

impl MfDecoder {
    pub fn new(cfg: &DecoderConfig) -> Result<Self> {
        ensure_mf()?;
        let sub = mf::subtype(cfg.codec)?;
        let dev = resolve_device(cfg.shared_device)?;
        let luid = d3d::device_luid(&dev);

        let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
        let mfts = mf::enum_mfts(MFT_CATEGORY_VIDEO_DECODER, flags, sub, MFVideoFormat_NV12, None);
        if mfts.is_empty() {
            return Err(CoreError::unsupported(format!("no MF decoder for {:?}", cfg.codec)));
        }

        let mut last = CoreError::unsupported("no D3D11VA-capable MF decoder");
        for m in mfts {
            match Self::try_mft(cfg, &dev, luid, &m) {
                Ok(dec) => {
                    tracing::info!(name = %m.name, codec = ?cfg.codec, gpu_out = cfg.prefer_gpu_output, "MF D3D11VA decoder ready");
                    return Ok(dec);
                }
                Err(e) => {
                    tracing::debug!(name = %m.name, "decoder MFT rejected: {e}");
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn try_mft(cfg: &DecoderConfig, dev: &ID3D11Device, luid: u64, m: &mf::MftInfo) -> Result<Self> {
        // SAFETY: COM activation and setup; ShutdownObject called on error.
        unsafe {
            let transform: IMFTransform = m.activate.ActivateObject().map_err(mf_err("ActivateObject"))?;
            let res = Self::setup(cfg, dev, luid, transform);
            if res.is_err() {
                let _ = m.activate.ShutdownObject();
            }
            res
        }
    }

    unsafe fn setup(cfg: &DecoderConfig, dev: &ID3D11Device, luid: u64, transform: IMFTransform) -> Result<Self> {
        // SAFETY: caller contract (valid COM objects).
        unsafe {
            let attrs = transform.GetAttributes().map_err(mf_err("GetAttributes"))?;
            if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) != 0 {
                return Err(CoreError::unsupported("async decoder MFT not yet wired; skipping"));
            }
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) == 0 {
                return Err(CoreError::unsupported("decoder MFT is not D3D11-aware"));
            }
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            let _ = attrs.SetUINT32(&MFT_DECODER_EXPOSE_OUTPUT_TYPES_IN_NATIVE_ORDER, 1);

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(mf_err("MFCreateDXGIDeviceManager"))?;
            let manager = manager.ok_or_else(|| CoreError::platform("null device manager"))?;
            manager.ResetDevice(dev, token).map_err(mf_err("ResetDevice"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(|e| CoreError::unsupported(format!("SET_D3D_MANAGER: {e}")))?;

            let api = transform.cast::<ICodecAPI>().ok();
            codec_set(&api, &CODECAPI_AVLowLatencyMode, VARIANT::from(true));
            if cfg.codec == VideoCodec::H264 {
                codec_set(&api, &CODECAPI_AVDecVideoAcceleration_H264, VARIANT::from(true));
            }

            let w = cfg.width.max(16);
            let h = cfg.height.max(16);
            let sub = mf::subtype(cfg.codec)?;
            let inp = mf::video_type(&sub, w, h, 60)?;
            transform.SetInputType(0, &inp, 0).map_err(mf_err("SetInputType"))?;
            let (out_type, output_format) = select_yuv_output(&transform)?;
            let (out_w, out_h) = output_dimensions(&out_type, w, h);

            let info = transform.GetOutputStreamInfo(0).map_err(mf_err("GetOutputStreamInfo"))?;
            let provides =
                info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 != 0;

            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(mf_err("BEGIN_STREAMING"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(mf_err("START_OF_STREAM"))?;

            Ok(Self {
                codec: cfg.codec,
                width: out_w,
                height: out_h,
                output_format,
                prefer_gpu_output: cfg.prefer_gpu_output,
                dev: dev.clone(),
                ctx: dev.GetImmediateContext().map_err(win_err("GetImmediateContext"))?,
                luid,
                _manager: manager,
                transform,
                provides_samples: provides,
                out_size: info.cbSize.max(w * h * 3),
                pool: None,
                gpu_sync: GpuSync::new(dev),
                readback: Readback::default(),
                last_ts_hns: -1,
            })
        }
    }

    fn refresh_output_info(&mut self) -> Result<()> {
        // SAFETY: valid MFT.
        unsafe {
            let (t, fmt) = select_yuv_output(&self.transform)?;
            let (w, h) = output_dimensions(&t, self.width, self.height);
            self.width = w;
            self.height = h;
            self.output_format = fmt;
            let info = self.transform.GetOutputStreamInfo(0).map_err(mf_err("GetOutputStreamInfo"))?;
            self.provides_samples =
                info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 != 0;
            self.out_size = info.cbSize.max(w * h * 3);
        }
        Ok(())
    }

    fn pull_sample(&mut self) -> Result<Option<IMFSample>> {
        // SAFETY: ProcessOutput contract; ManuallyDrop fields taken and dropped cleanly.
        unsafe {
            for _ in 0..4 {
                let own_sample = if self.provides_samples {
                    None
                } else {
                    let s = MFCreateSample().map_err(mf_err("MFCreateSample"))?;
                    let b = MFCreateMemoryBuffer(self.out_size).map_err(mf_err("MFCreateMemoryBuffer"))?;
                    s.AddBuffer(&b).map_err(mf_err("AddBuffer"))?;
                    Some(s)
                };
                let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(own_sample),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let r = self.transform.ProcessOutput(0, &mut buf, &mut status);
                let sample = std::mem::ManuallyDrop::take(&mut buf[0].pSample);
                drop(std::mem::ManuallyDrop::take(&mut buf[0].pEvents));
                match r {
                    Ok(()) => return Ok(sample),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.refresh_output_info()?;
                        continue;
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                    Err(e) => return Err(CoreError::platform(format!("MF decoder ProcessOutput: {e}"))),
                }
            }
            Ok(None)
        }
    }

    fn sample_to_frame(&mut self, sample: &IMFSample, capture_ts: veloxdesk_core_traits::Timestamp) -> Result<Frame> {
        let w = self.width;
        let h = self.height;
        // SAFETY: COM calls on a valid output IMFSample.
        unsafe {
            let buf = sample.GetBufferByIndex(0).map_err(mf_err("GetBufferByIndex"))?;
            if let Ok(dxgi_buf) = buf.cast::<IMFDXGIBuffer>() {
                let mut raw: *mut c_void = std::ptr::null_mut();
                dxgi_buf.GetResource(&ID3D11Texture2D::IID, &mut raw).map_err(mf_err("IMFDXGIBuffer::GetResource"))?;
                let src_tex = ID3D11Texture2D::from_raw(raw);
                let sub = dxgi_buf.GetSubresourceIndex().map_err(mf_err("GetSubresourceIndex"))?;
                if self.prefer_gpu_output {
                    return self.export_gpu(&src_tex, sub, capture_ts);
                }
                let (fmt, planes) = self.readback.read_texture(&src_tex, sub)?;
                if !matches!(fmt, PixelFormat::Nv12 | PixelFormat::P010) || planes.len() < 2 {
                    return Err(CoreError::platform(format!("unexpected MF decoder texture format {fmt:?}")));
                }
                return Ok(Self::pack_yuv420(fmt, w, h, planes[0].0, planes[0].1, planes[1].0, planes[1].1, capture_ts));
            }
            // Fallback: CPU buffer emitted by decoder.
            let fmt = self.output_format;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>() {
                let mut p = std::ptr::null_mut();
                let mut pitch = 0i32;
                b2.Lock2D(&mut p, &mut pitch).map_err(mf_err("Lock2D"))?;
                let res = if pitch > 0 {
                    let stride = pitch as usize;
                    let total_rows = h as usize + (h as usize).div_ceil(2);
                    let slice = std::slice::from_raw_parts(p, stride * total_rows);
                    let (y, uv) = slice.split_at(stride * h as usize);
                    Ok(Self::pack_yuv420(fmt, w, h, y, stride, uv, stride, capture_ts))
                } else {
                    Err(CoreError::platform("MF decoder returned negative stride"))
                };
                let _ = b2.Unlock2D();
                return res;
            }
            let mut bytes = Vec::new();
            mf::sample_bytes(sample, &mut bytes)?;
            let bps = if fmt == PixelFormat::P010 { 2 } else { 1 };
            let stride = w as usize * bps;
            let y_len = stride * h as usize;
            let (cw, ch) = convert::chroma_dims(w, h);
            let uv_stride = cw * 2 * bps;
            if bytes.len() < y_len + uv_stride * ch {
                return Err(CoreError::platform("MF decoder CPU buffer too small"));
            }
            Ok(Self::pack_yuv420(fmt, w, h, &bytes[..y_len], stride, &bytes[y_len..], uv_stride, capture_ts))
        }
    }

    fn export_gpu(&mut self, src: &ID3D11Texture2D, sub: u32, capture_ts: veloxdesk_core_traits::Timestamp) -> Result<Frame> {
        let src_desc = d3d::tex_desc(src);
        let w = (self.width.min(src_desc.Width)) & !1;
        let h = (self.height.min(src_desc.Height)) & !1;
        let (dxgi_fmt, pix_fmt, color_space) = if src_desc.Format == DXGI_FORMAT_P010 {
            (DXGI_FORMAT_P010, PixelFormat::P010, ColorSpace::Bt2020Pq)
        } else {
            (DXGI_FORMAT_NV12, PixelFormat::Nv12, ColorSpace::Bt709)
        };
        let need_pool = match &self.pool {
            Some(p) => p.desc().Width != w || p.desc().Height != h || p.desc().Format != dxgi_fmt,
            None => true,
        };
        if need_pool {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: dxgi_fmt,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            self.pool = Some(TexturePool::new(desc, true, 6));
        }
        let pool = self.pool.as_mut().expect("pool");
        let dst = pool.get(&self.dev)?;
        let region = D3D11_BOX { left: 0, top: 0, front: 0, right: w, bottom: h, back: 1 };
        // SAFETY: both textures belong to `self.dev`, region is even-aligned and within bounds.
        unsafe {
            self.ctx.CopySubresourceRegion(&dst.tex, 0, 0, 0, 0, src, sub, Some(&region));
            if let Some(sync) = &self.gpu_sync
                && dst.shared.is_some()
            {
                // Retire copy before handing the shared NT handle to another device (DX12/wgpu).
                sync.flush_and_wait(&self.ctx);
            } else {
                self.ctx.Flush();
            }
        }
        Ok(Frame {
            width: w,
            height: h,
            format: pix_fmt,
            color_space,
            color_range: ColorRange::Limited,
            data: FrameData::Gpu(d3d::gpu_frame(&dst, self.luid)),
            capture_ts,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn pack_yuv420(
        fmt: PixelFormat,
        w: u32,
        h: u32,
        y: &[u8],
        y_stride: usize,
        uv: &[u8],
        uv_stride: usize,
        capture_ts: veloxdesk_core_traits::Timestamp,
    ) -> Frame {
        let bps = if fmt == PixelFormat::P010 { 2 } else { 1 };
        let wu = w as usize * bps;
        let hu = h as usize;
        let (cw, ch) = convert::chroma_dims(w, h);
        let uv_row = cw * 2 * bps;
        let mut yb = vec![0u8; wu * hu];
        convert::copy_plane(y, y_stride, &mut yb, wu, wu, hu);
        let mut uvb = vec![0u8; uv_row * ch];
        convert::copy_plane(uv, uv_stride, &mut uvb, uv_row, uv_row, ch);
        Frame {
            width: w,
            height: h,
            format: fmt,
            color_space: if fmt == PixelFormat::P010 { ColorSpace::Bt2020Pq } else { ColorSpace::Bt709 },
            color_range: ColorRange::Limited,
            data: FrameData::Cpu(vec![
                Plane { data: Bytes::from(yb), stride: wu },
                Plane { data: Bytes::from(uvb), stride: uv_row },
            ]),
            capture_ts,
        }
    }
}

impl VideoDecoder for MfDecoder {
    fn caps(&self) -> DecoderCaps {
        caps_for(vec![self.codec], self.output_format == PixelFormat::P010 || matches!(self.codec, VideoCodec::H265 | VideoCodec::Av1))
    }

    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<DecodedFrame>> {
        ensure_mf()?;
        let t0 = Instant::now();
        let mut ts = packet.capture_ts.as_micros() as i64 * 10;
        if ts <= self.last_ts_hns {
            ts = self.last_ts_hns + 1;
        }
        self.last_ts_hns = ts;
        let sample = mf::memory_sample(&packet.data, ts, 0)?;
        if packet.keyframe {
            // SAFETY: valid sample attribute setter.
            unsafe {
                let _ = sample.SetUINT32(&MFSampleExtension_CleanPoint, 1);
            }
        }

        // Feed input; if MFT is full, drain one output first and retry.
        // SAFETY: valid MFT and sample.
        let mut drained: Option<IMFSample> = None;
        unsafe {
            match self.transform.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    drained = self.pull_sample()?;
                    self.transform.ProcessInput(0, &sample, 0).map_err(mf_err("ProcessInput(retry)"))?;
                }
                Err(e) => return Err(CoreError::platform(format!("MF decoder ProcessInput: {e}"))),
            }
        }

        // Pull any ready output (keep latest if multiple became available).
        let mut latest = drained;
        while let Some(s) = self.pull_sample()? {
            latest = Some(s);
        }
        let Some(out_sample) = latest else {
            return Ok(None);
        };
        let frame = self.sample_to_frame(&out_sample, packet.capture_ts)?;
        let decode_us = t0.elapsed().as_micros() as u32;
        Ok(Some(DecodedFrame { frame, frame_id: packet.frame_id, decode_us }))
    }

    fn reset(&mut self) -> Result<()> {
        // SAFETY: valid MFT.
        unsafe {
            self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0).map_err(mf_err("FLUSH"))?;
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
        }
        Ok(())
    }
}

impl Drop for MfDecoder {
    fn drop(&mut self) {
        // SAFETY: clean MFT teardown.
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.transform.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
    }
}

/// Probe result for a D3D11VA hardware decoder MFT.
pub struct ProbeInfo {
    pub ten_bit: bool,
}

/// Probe whether a D3D11VA hardware decoder MFT is registered for `codec` (and whether it supports P010 10-bit).
pub fn probe(codec: VideoCodec) -> Option<ProbeInfo> {
    ensure_mf().ok()?;
    let sub = mf::subtype(codec).ok()?;
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER;
    let mfts = mf::enum_mfts(MFT_CATEGORY_VIDEO_DECODER, flags, sub, MFVideoFormat_NV12, None);
    let mut found = false;
    for m in mfts {
        // SAFETY: activate briefly to inspect MF_SA_D3D11_AWARE, then shut down.
        let ok = unsafe {
            let r = m
                .activate
                .ActivateObject::<IMFTransform>()
                .ok()
                .and_then(|t| t.GetAttributes().ok())
                .map(|a| a.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 && a.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0)
                .unwrap_or(false);
            let _ = m.activate.ShutdownObject();
            r
        };
        if ok {
            found = true;
            break;
        }
    }
    if !found {
        return None;
    }
    let ten_bit = matches!(codec, VideoCodec::H265 | VideoCodec::Av1)
        && !mf::enum_mfts(MFT_CATEGORY_VIDEO_DECODER, flags, sub, MFVideoFormat_P010, None).is_empty();
    Some(ProbeInfo { ten_bit })
}
