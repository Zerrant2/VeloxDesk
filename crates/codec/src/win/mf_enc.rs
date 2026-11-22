//! Hardware H.264 / HEVC encoding through Media Foundation hardware MFTs (NVENC / AMF / QSV
//! drivers expose themselves as async MFTs). GPU textures are fed through the DXGI device
//! manager without CPU copies; BGRA input is converted to NV12 on the GPU first.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use veloxdesk_core_traits::video::{
    ChromaFormat, ColorSpace, EncodeOptions, EncodedPacket, EncoderCaps, EncoderConfig, EncoderUpdate, Frame,
    FrameData, PixelFormat, RateControl, VideoCodec,
};
use veloxdesk_core_traits::{CoreError, Result, Timestamp, VideoEncoder};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::VARIANT;
use windows::core::{IUnknown, Interface, Ref, implement};

use super::d3d::{self, Nv12Converter, TextureHolder, win_err};
use super::mf::{self, ensure_mf, mf_err};
use crate::annexb;

/// Event forwarded from the MF work-queue thread: (event type, status).
type EvMsg = std::result::Result<(u32, windows::core::HRESULT), windows::core::HRESULT>;

#[implement(IMFAsyncCallback)]
struct EventCallback {
    gen_: IMFMediaEventGenerator,
    tx: Mutex<Sender<EvMsg>>,
}

impl IMFAsyncCallback_Impl for EventCallback_Impl {
    fn GetParameters(&self, _flags: *mut u32, _queue: *mut u32) -> windows::core::Result<()> {
        Err(windows::Win32::Foundation::E_NOTIMPL.into())
    }

    fn Invoke(&self, result: Ref<IMFAsyncResult>) -> windows::core::Result<()> {
        // SAFETY: EndGetEvent with the result passed to us by MF.
        let msg = match unsafe { self.gen_.EndGetEvent(result.ok()?) } {
            Ok(ev) => unsafe {
                let ty = ev.GetType().unwrap_or(0);
                let st = ev.GetStatus().unwrap_or(windows::core::HRESULT(0));
                Ok((ty, st))
            },
            Err(e) => Err(e.code()),
        };
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(msg);
        }
        Ok(())
    }
}

struct Pending {
    capture_ts: Timestamp,
    convert_us: u32,
    submitted: Instant,
    _input: Option<Arc<TextureHolder>>,
}

/// One MFT instance bound to one D3D11 device.
struct Session {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
    _manager: IMFDXGIDeviceManager,
    transform: IMFTransform,
    codec_api: Option<ICodecAPI>,
    gen_: IMFMediaEventGenerator,
    cb: IMFAsyncCallback,
    rx: Receiver<EvMsg>,
    armed: bool,
    need_input: u32,
    provides_samples: bool,
    out_size: u32,
    vendor: &'static str,
    luid: u64,
    conv: Option<Nv12Converter>,
    upload: Option<(ID3D11Texture2D, u32, u32)>,
    seq_header: Option<Vec<u8>>,
    frames_out: u64,
}

fn codec_set(api: &Option<ICodecAPI>, key: &windows::core::GUID, v: VARIANT) -> bool {
    match api {
        // SAFETY: ICodecAPI::SetValue with a stack VARIANT.
        Some(a) => unsafe { a.SetValue(key, &v).is_ok() },
        None => false,
    }
}

fn mf_input_subtype(cfg: &EncoderConfig) -> Result<(windows::core::GUID, DXGI_FORMAT)> {
    match (cfg.chroma, cfg.bit_depth) {
        (ChromaFormat::Yuv420, 8) => Ok((MFVideoFormat_NV12, DXGI_FORMAT_NV12)),
        (ChromaFormat::Yuv420, 10) => {
            if !matches!(cfg.codec, VideoCodec::H265 | VideoCodec::Av1) {
                return Err(CoreError::unsupported("10-bit MF hardware encode requires HEVC or AV1"));
            }
            Ok((MFVideoFormat_P010, DXGI_FORMAT_P010))
        }
        (ChromaFormat::Yuv444, 8) => Ok((MFVideoFormat_AYUV, DXGI_FORMAT_AYUV)),
        _ => Err(CoreError::unsupported(format!(
            "MF encoder unsupported chroma/depth: {:?} {}-bit",
            cfg.chroma, cfg.bit_depth
        ))),
    }
}

impl Session {
    fn new(cfg: &EncoderConfig, dev: &ID3D11Device) -> Result<Self> {
        ensure_mf()?;
        d3d::enable_multithread(dev);
        let luid = d3d::device_luid(dev);
        let sub = mf::subtype(cfg.codec)?;
        let (in_sub, _) = mf_input_subtype(cfg)?;
        let mfts = mf::enum_mfts(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            in_sub,
            sub,
            Some(luid),
        );
        if mfts.is_empty() {
            return Err(CoreError::unsupported(format!(
                "no hardware {:?} ({:?} {}-bit) MFT encoder",
                cfg.codec, cfg.chroma, cfg.bit_depth
            )));
        }
        let mut last = CoreError::unsupported("no usable hardware MFT");
        for m in mfts {
            match Self::try_mft(cfg, dev, &m, luid) {
                Ok(s) => {
                    tracing::info!(name = %m.name, vendor = m.vendor, "MF hardware encoder ready");
                    return Ok(s);
                }
                Err(e) => {
                    tracing::debug!(name = %m.name, "MFT rejected: {e}");
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn try_mft(cfg: &EncoderConfig, dev: &ID3D11Device, m: &mf::MftInfo, luid: u64) -> Result<Self> {
        // SAFETY: Media Foundation / D3D11 COM calls on valid objects; out-params on the stack.
        unsafe {
            let transform: IMFTransform = m.activate.ActivateObject().map_err(mf_err("ActivateObject"))?;
            let res = Self::setup(cfg, dev, transform.clone(), m.vendor, luid);
            if res.is_err() {
                let _ = m.activate.ShutdownObject();
            }
            res
        }
    }

    unsafe fn setup(
        cfg: &EncoderConfig,
        dev: &ID3D11Device,
        transform: IMFTransform,
        vendor: &'static str,
        luid: u64,
    ) -> Result<Self> {
        // SAFETY: caller contract (valid COM objects).
        unsafe {
            let attrs = transform.GetAttributes().map_err(mf_err("GetAttributes"))?;
            if attrs.GetUINT32(&MF_TRANSFORM_ASYNC).unwrap_or(0) == 0 {
                return Err(CoreError::unsupported("synchronous encoder MFT (expected async hardware MFT)"));
            }
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1).map_err(mf_err("ASYNC_UNLOCK"))?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);

            let mut token = 0u32;
            let mut manager = None;
            MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(mf_err("MFCreateDXGIDeviceManager"))?;
            let manager = manager.ok_or_else(|| CoreError::platform("null device manager"))?;
            manager.ResetDevice(dev, token).map_err(mf_err("ResetDevice"))?;
            transform
                .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
                .map_err(mf_err("SET_D3D_MANAGER"))?;

            let api = transform.cast::<ICodecAPI>().ok();
            let bps = cfg.bitrate_kbps.saturating_mul(1000);
            let fps = cfg.fps.max(1);
            codec_set(&api, &CODECAPI_AVLowLatencyMode, VARIANT::from(true));
            match cfg.rate_control {
                RateControl::Cqp(qp) => {
                    codec_set(&api, &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_Quality.0 as u32));
                    codec_set(&api, &CODECAPI_AVEncVideoEncodeQP, VARIANT::from(qp as u64));
                }
                RateControl::Vbr => {
                    codec_set(&api, &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32));
                    codec_set(&api, &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps));
                    codec_set(&api, &CODECAPI_AVEncCommonMaxBitRate, VARIANT::from(bps.saturating_mul(3) / 2));
                }
                RateControl::Cbr => {
                    codec_set(&api, &CODECAPI_AVEncCommonRateControlMode, VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32));
                    codec_set(&api, &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps));
                }
            }
            // VBV ≈ one frame (clamped so high-FPS 120/144/240 Hz doesn't underflow driver limits).
            codec_set(&api, &CODECAPI_AVEncCommonBufferSize, VARIANT::from((bps / fps).max(16_384)));
            codec_set(&api, &CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32));
            codec_set(&api, &CODECAPI_AVEncMPVGOPSize, VARIANT::from(cfg.gop.unwrap_or(0x7FFF_FFFF)));
            codec_set(&api, &CODECAPI_AVEncNumWorkerThreads, VARIANT::from(0u32));

            let sub = mf::subtype(cfg.codec)?;
            let (in_sub, _) = mf_input_subtype(cfg)?;
            let out = mf::video_type(&sub, cfg.width, cfg.height, fps)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bps.max(1)).map_err(mf_err("bitrate"))?;
            let profile = match (cfg.codec, cfg.chroma, cfg.bit_depth) {
                (VideoCodec::H264, ChromaFormat::Yuv420, 8) => eAVEncH264VProfile_Main.0 as u32,
                (VideoCodec::H264, ChromaFormat::Yuv444, 8) => eAVEncH264VProfile_444.0 as u32,
                (VideoCodec::H265, ChromaFormat::Yuv420, 8) => eAVEncH265VProfile_Main_420_8.0 as u32,
                (VideoCodec::H265, ChromaFormat::Yuv420, 10) => eAVEncH265VProfile_Main_420_10.0 as u32,
                (VideoCodec::H265, ChromaFormat::Yuv444, 8) => eAVEncH265VProfile_Main_444_8.0 as u32,
                (VideoCodec::Av1, ChromaFormat::Yuv420, 8) => eAVEncAV1VProfile_Main_420_8.0 as u32,
                (VideoCodec::Av1, ChromaFormat::Yuv420, 10) => eAVEncAV1VProfile_Main_420_10.0 as u32,
                _ => 0,
            };
            if profile != 0 {
                let _ = out.SetUINT32(&MF_MT_MPEG2_PROFILE, profile);
            }
            let (yuv_matrix, primaries, transfer) = match cfg.color_space {
                ColorSpace::Bt2020Pq => (
                    MFVideoTransferMatrix_BT2020_10.0 as u32,
                    MFVideoPrimaries_BT2020.0 as u32,
                    MFVideoTransFunc_2084.0 as u32,
                ),
                ColorSpace::Bt601 => (
                    MFVideoTransferMatrix_BT601.0 as u32,
                    MFVideoPrimaries_SMPTE170M.0 as u32,
                    MFVideoTransFunc_709.0 as u32,
                ),
                _ => (
                    MFVideoTransferMatrix_BT709.0 as u32,
                    MFVideoPrimaries_BT709.0 as u32,
                    MFVideoTransFunc_709.0 as u32,
                ),
            };
            let _ = out.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            let _ = out.SetUINT32(&MF_MT_YUV_MATRIX, yuv_matrix);
            let _ = out.SetUINT32(&MF_MT_VIDEO_PRIMARIES, primaries);
            let _ = out.SetUINT32(&MF_MT_TRANSFER_FUNCTION, transfer);
            transform.SetOutputType(0, &out, 0).map_err(mf_err("SetOutputType"))?;

            let inp = mf::video_type(&in_sub, cfg.width, cfg.height, fps)?;
            let _ = inp.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32);
            let _ = inp.SetUINT32(&MF_MT_YUV_MATRIX, yuv_matrix);
            let _ = inp.SetUINT32(&MF_MT_VIDEO_PRIMARIES, primaries);
            let _ = inp.SetUINT32(&MF_MT_TRANSFER_FUNCTION, transfer);
            transform.SetInputType(0, &inp, 0).map_err(mf_err("SetInputType"))?;

            // Some drivers only honour these after the types are set.
            codec_set(&api, &CODECAPI_AVLowLatencyMode, VARIANT::from(true));

            let info = transform.GetOutputStreamInfo(0).map_err(mf_err("GetOutputStreamInfo"))?;
            let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32 != 0;

            let gen_: IMFMediaEventGenerator = transform.cast().map_err(mf_err("IMFMediaEventGenerator"))?;
            let (tx, rx) = channel();
            let cb: IMFAsyncCallback = EventCallback { gen_: gen_.clone(), tx: Mutex::new(tx) }.into();

            transform.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0).map_err(mf_err("FLUSH"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0).map_err(mf_err("BEGIN_STREAMING"))?;
            transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0).map_err(mf_err("START_OF_STREAM"))?;

            Ok(Self {
                dev: dev.clone(),
                ctx: dev.GetImmediateContext().map_err(win_err("GetImmediateContext"))?,
                _manager: manager,
                transform,
                codec_api: api,
                gen_,
                cb,
                rx,
                armed: false,
                need_input: 0,
                provides_samples: provides,
                out_size: info.cbSize.max(1 << 20),
                vendor,
                luid,
                conv: None,
                upload: None,
                seq_header: None,
                frames_out: 0,
            })
        }
    }

    /// Wait up to `timeout` for one MFT event; returns false on timeout.
    fn pump(&mut self, timeout: Duration, ready: &mut VecDeque<(Vec<u8>, bool)>) -> Result<bool> {
        if !self.armed {
            // SAFETY: one outstanding BeginGetEvent at a time (guarded by `armed`).
            unsafe { self.gen_.BeginGetEvent(&self.cb, None::<&IUnknown>) }.map_err(mf_err("BeginGetEvent"))?;
            self.armed = true;
        }
        let msg = match self.rx.recv_timeout(timeout) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => return Ok(false),
            Err(RecvTimeoutError::Disconnected) => return Err(CoreError::reinit("MF event channel closed")),
        };
        self.armed = false;
        let (ty, status) = msg.map_err(|hr| CoreError::reinit(format!("MF event error {hr:?}")))?;
        if status.is_err() {
            return Err(CoreError::reinit(format!("MFT event {ty} failed: {status:?}")));
        }
        if ty == METransformNeedInput.0 as u32 {
            self.need_input += 1;
        } else if ty == METransformHaveOutput.0 as u32 {
            self.collect_output(ready)?;
        }
        Ok(true)
    }

    fn collect_output(&mut self, ready: &mut VecDeque<(Vec<u8>, bool)>) -> Result<()> {
        // SAFETY: ProcessOutput contract — we own any sample returned and release the
        // ManuallyDrop fields exactly once.
        unsafe {
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
            if let Err(e) = r {
                if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    let t = self.transform.GetOutputAvailableType(0, 0).map_err(mf_err("GetOutputAvailableType"))?;
                    self.transform.SetOutputType(0, &t, 0).map_err(mf_err("SetOutputType(renegotiate)"))?;
                    return Ok(());
                }
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(());
                }
                return Err(CoreError::platform(format!("ProcessOutput: {e}")));
            }
            let Some(sample) = sample else { return Ok(()) };
            let mut data = Vec::new();
            mf::sample_bytes(&sample, &mut data)?;
            let clean = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            ready.push_back((data, clean));
            self.frames_out += 1;
        }
        Ok(())
    }

    fn sequence_header(&mut self) -> Option<&[u8]> {
        if self.seq_header.is_none() {
            // SAFETY: attribute reads on the current output type.
            unsafe {
                let t = self.transform.GetOutputCurrentType(0).ok()?;
                let n = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
                let mut v = vec![0u8; n as usize];
                t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut v, None).ok()?;
                self.seq_header = Some(v);
            }
        }
        self.seq_header.as_deref()
    }

    /// Upload a CPU BGRA frame into a GPU texture.
    fn upload_bgra(&mut self, data: &[u8], stride: usize, w: u32, h: u32) -> Result<ID3D11Texture2D> {
        let reuse = matches!(&self.upload, Some((_, uw, uh)) if *uw == w && *uh == h);
        if !reuse {
            let d = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32 | D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut t = None;
            // SAFETY: valid device + desc.
            unsafe { self.dev.CreateTexture2D(&d, None, Some(&mut t)) }.map_err(win_err("CreateTexture2D(upload)"))?;
            self.upload = Some((t.ok_or_else(|| CoreError::platform("null texture"))?, w, h));
        }
        let Some((t, _, _)) = &self.upload else { unreachable!() };
        if data.len() < stride * (h as usize - 1) + w as usize * 4 {
            return Err(CoreError::invalid("BGRA plane too small"));
        }
        // SAFETY: `data` covers `h` rows of `stride` bytes (checked above).
        unsafe { self.ctx.UpdateSubresource(t, 0, None, data.as_ptr().cast(), stride as u32, 0) };
        Ok(t.clone())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: shutting down the MFT releases the pending BeginGetEvent (and its callback).
        unsafe {
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(s) = self.transform.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
            if let Ok(s) = self.gen_.cast::<IMFShutdown>() {
                let _ = s.Shutdown();
            }
        }
    }
}

/// Media Foundation hardware encoder (see module docs).
pub struct MfEncoder {
    cfg: EncoderConfig,
    session: Option<Session>,
    vendor: &'static str,
    pending: VecDeque<Pending>,
    ready: VecDeque<(Vec<u8>, bool)>,
    frame_id: u32,
    last_ts_hns: i64,
    warned_delay: bool,
}

// SAFETY: MF/D3D11 objects used here are free-threaded; the encoder is driven from one
// thread at a time (VideoEncoder contract) and the device is multithread-protected.
unsafe impl Send for MfEncoder {}

pub fn caps_for(vendor: &'static str, codecs: Vec<VideoCodec>, ten_bit: bool, yuv444: bool) -> EncoderCaps {
    let mut gpu_input_formats = vec![PixelFormat::Bgra8, PixelFormat::Nv12];
    if ten_bit {
        gpu_input_formats.push(PixelFormat::Rgb10a2);
        gpu_input_formats.push(PixelFormat::P010);
    }
    EncoderCaps {
        name: vendor,
        codecs,
        hardware: true,
        gpu_input_formats,
        cpu_input_formats: vec![PixelFormat::Bgra8],
        yuv444,
        ten_bit,
        intra_refresh: false,
        max_width: 4096,
        max_height: 4096,
    }
}

impl MfEncoder {
    /// Creates the encoder and validates that a hardware MFT accepts `cfg` on the default
    /// adapter. The real session is (re)built lazily on the device of the first GPU frame.
    pub fn new(cfg: &EncoderConfig) -> Result<Self> {
        let _ = mf_input_subtype(cfg)?;
        if cfg.width % 2 != 0 || cfg.height % 2 != 0 {
            return Err(CoreError::invalid("MF encoder needs even dimensions"));
        }
        let dev = d3d::create_device(None)?;
        let s = Session::new(cfg, &dev)?;
        let vendor = s.vendor;
        Ok(Self {
            cfg: cfg.clone(),
            session: Some(s),
            vendor,
            pending: VecDeque::new(),
            ready: VecDeque::new(),
            frame_id: 0,
            last_ts_hns: -1,
            warned_delay: false,
        })
    }

    fn ensure_session(&mut self, dev: Option<&ID3D11Device>) -> Result<()> {
        let rebuild = match (&self.session, dev) {
            (None, _) => true,
            (Some(s), Some(d)) => s.dev.as_raw() != d.as_raw(),
            (Some(_), None) => false,
        };
        if rebuild {
            self.session = None;
            self.pending.clear();
            self.ready.clear();
            let dev = match dev {
                Some(d) => d.clone(),
                None => d3d::create_device(None)?,
            };
            let s = Session::new(&self.cfg, &dev)?;
            self.vendor = s.vendor;
            self.session = Some(s);
        }
        Ok(())
    }

    fn encode_inner(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
        let t0 = Instant::now();
        // Resolve the source texture (GPU) or upload (CPU).
        let (src_tex, sub, dev) = match &frame.data {
            FrameData::Gpu(g) => {
                let (t, sub) = d3d::texture_of(g)?;
                let dev = d3d::device_of(&t)?;
                (Some(t), sub, Some(dev))
            }
            FrameData::Cpu(_) => (None, 0, None),
        };
        let (w, h) = (self.cfg.width, self.cfg.height);
        self.ensure_session(dev.as_ref())?;
        let s = self.session.as_mut().expect("session");
        let (tex, sub) = match (src_tex, &frame.data) {
            (Some(t), _) => (t, sub),
            (None, FrameData::Cpu(planes)) => {
                if frame.format != PixelFormat::Bgra8 || planes.is_empty() {
                    return Err(CoreError::unsupported(format!("MF encoder CPU input {:?}", frame.format)));
                }
                (s.upload_bgra(&planes[0].data, planes[0].stride, frame.width, frame.height)?, 0)
            }
            _ => unreachable!(),
        };
        let td = d3d::tex_desc(&tex);
        let (_, target_dxgi) = mf_input_subtype(&self.cfg)?;
        let direct = td.Format == target_dxgi && td.Width == w && td.Height == h && td.ArraySize == 1;
        let (input_tex, holder, convert_us) = if direct {
            (tex, None, 0)
        } else {
            let need_new = s
                .conv
                .as_ref()
                .map(|c| c.input_size() != (td.Width, td.Height) || c.out_format() != target_dxgi)
                .unwrap_or(true);
            if need_new {
                let in_cs = match frame.color_space {
                    ColorSpace::Bt2020Pq => DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020,
                    _ => DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                };
                let out_cs = match self.cfg.color_space {
                    ColorSpace::Bt2020Pq => DXGI_COLOR_SPACE_YCBCR_STUDIO_G2084_LEFT_P2020,
                    _ => DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                };
                s.conv = Some(Nv12Converter::new_for_format(
                    &s.dev,
                    td.Width,
                    td.Height,
                    w,
                    h,
                    target_dxgi,
                    in_cs,
                    out_cs,
                )?);
            }
            let tc = Instant::now();
            let conv = s.conv.as_mut().expect("converter");
            let out = conv.convert(&tex, sub)?;
            let cpu_us = tc.elapsed().as_micros() as u32;
            let gpu = conv.gpu_us();
            (out.tex.clone(), Some(out), if gpu > 0 { gpu } else { cpu_us })
        };

        if opts.force_keyframe {
            codec_set(&s.codec_api, &CODECAPI_AVEncVideoForceKeyFrame, VARIANT::from(1u32));
        }

        // Wait for the MFT to ask for input.
        let first = s.frames_out == 0;
        let deadline = Instant::now() + if first { Duration::from_secs(3) } else { Duration::from_millis(500) };
        while s.need_input == 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !s.pump(left, &mut self.ready)? {
                return Err(CoreError::reinit("MF encoder did not request input"));
            }
        }

        // Wrap the NV12 texture in a sample (no copy).
        let fps = self.cfg.fps.max(1) as i64;
        let mut ts = frame.capture_ts.as_micros() as i64 * 10;
        if ts <= self.last_ts_hns {
            ts = self.last_ts_hns + 1;
        }
        self.last_ts_hns = ts;
        // SAFETY: MF sample creation around a live texture kept alive by `holder` / pending.
        unsafe {
            let buf = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &input_tex, 0, false)
                .map_err(mf_err("MFCreateDXGISurfaceBuffer"))?;
            if let Ok(b2) = buf.cast::<IMF2DBuffer>()
                && let Ok(len) = b2.GetContiguousLength()
            {
                let _ = buf.SetCurrentLength(len);
            }
            let sample = MFCreateSample().map_err(mf_err("MFCreateSample"))?;
            sample.AddBuffer(&buf).map_err(mf_err("AddBuffer"))?;
            sample.SetSampleTime(ts).map_err(mf_err("SetSampleTime"))?;
            let _ = sample.SetSampleDuration(10_000_000 / fps);
            s.transform.ProcessInput(0, &sample, 0).map_err(|e| {
                if e.code() == MF_E_NOTACCEPTING {
                    CoreError::reinit("MF encoder not accepting input")
                } else if e.code() == MF_E_UNSUPPORTED_D3D_TYPE {
                    CoreError::unsupported(format!("MF encoder D3D input type unsupported: {e}"))
                } else {
                    CoreError::platform(format!("ProcessInput: {e}"))
                }
            })?;
        }
        s.need_input -= 1;
        self.pending.push_back(Pending { capture_ts: frame.capture_ts, convert_us, submitted: t0, _input: holder });

        // Wait for this frame's output (low-latency MFTs emit one output per input).
        let s = self.session.as_mut().expect("session");
        let deadline = Instant::now() + if first { Duration::from_secs(3) } else { Duration::from_millis(100) };
        while self.ready.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !s.pump(left, &mut self.ready)? {
                if !self.warned_delay {
                    tracing::warn!(vendor = self.vendor, "MF encoder buffered a frame (pipeline delay)");
                    self.warned_delay = true;
                }
                return Ok(None);
            }
        }
        let (mut data, clean) = self.ready.pop_front().expect("ready");
        let meta = self.pending.pop_front();
        let keyframe = clean || annexb::is_keyframe(self.cfg.codec, &data);
        if keyframe
            && !annexb::has_sps(self.cfg.codec, &data)
            && let Some(hdr) = s.sequence_header()
        {
            let mut v = hdr.to_vec();
            v.extend_from_slice(&data);
            data = v;
        }
        let (capture_ts, convert_us, encode_us) = match meta {
            Some(m) => (m.capture_ts, m.convert_us, m.submitted.elapsed().as_micros() as u32),
            None => (frame.capture_ts, convert_us, t0.elapsed().as_micros() as u32),
        };
        let id = self.frame_id;
        self.frame_id = self.frame_id.wrapping_add(1);
        Ok(Some(EncodedPacket {
            data: Bytes::from(data),
            codec: self.cfg.codec,
            keyframe,
            recovery_point: keyframe,
            width: w,
            height: h,
            capture_ts,
            frame_id: id,
            encode_us,
            convert_us,
        }))
    }
}

impl VideoEncoder for MfEncoder {
    fn caps(&self) -> EncoderCaps {
        caps_for(
            self.vendor,
            vec![self.cfg.codec],
            self.cfg.bit_depth == 10,
            self.cfg.chroma == ChromaFormat::Yuv444,
        )
    }

    fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
        ensure_mf()?;
        let r = self.encode_inner(frame, opts);
        if matches!(r, Err(CoreError::Reinit(_))) {
            // Force a fresh MFT next time.
            self.session = None;
            self.pending.clear();
            self.ready.clear();
        }
        r
    }

    fn update(&mut self, update: EncoderUpdate) -> Result<()> {
        if let Some(fps) = update.fps {
            self.cfg.fps = fps.max(1);
        }
        if let Some(kbps) = update.bitrate_kbps {
            self.cfg.bitrate_kbps = kbps;
            if let Some(s) = &self.session {
                let bps = kbps.saturating_mul(1000);
                let ok = codec_set(&s.codec_api, &CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps));
                codec_set(&s.codec_api, &CODECAPI_AVEncCommonBufferSize, VARIANT::from(bps / self.cfg.fps.max(1)));
                if !ok {
                    return Err(CoreError::unsupported("MF encoder rejected dynamic bitrate"));
                }
            }
        }
        Ok(())
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        self.session = None;
        let _ = d3d::luid_u64;
    }
}

/// Hardware encoder probe result for one codec.
pub struct ProbeInfo {
    pub vendor: &'static str,
    pub ten_bit: bool,
    pub yuv444: bool,
}

/// Which hardware encoder (vendor + 10-bit/4:4:4 support) is available for `codec`, without creating it.
pub fn probe(codec: VideoCodec) -> Option<ProbeInfo> {
    ensure_mf().ok()?;
    let sub = mf::subtype(codec).ok()?;
    let flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;
    let vendor = mf::enum_mfts(MFT_CATEGORY_VIDEO_ENCODER, flags, MFVideoFormat_NV12, sub, None)
        .first()
        .map(|m| m.vendor)?;
    let ten_bit = matches!(codec, VideoCodec::H265 | VideoCodec::Av1)
        && !mf::enum_mfts(MFT_CATEGORY_VIDEO_ENCODER, flags, MFVideoFormat_P010, sub, None).is_empty();
    let yuv444 = !mf::enum_mfts(MFT_CATEGORY_VIDEO_ENCODER, flags, MFVideoFormat_AYUV, sub, None).is_empty();
    Some(ProbeInfo { vendor, ten_bit, yuv444 })
}

#[allow(dead_code)]
fn _assert_luid(s: &Session) -> u64 {
    s.luid
}
