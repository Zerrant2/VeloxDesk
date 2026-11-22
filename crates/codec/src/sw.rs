//! Software H.264 via Cisco OpenH264 (always available, used by CI and as the last fallback).

use std::time::Instant;

use bytes::Bytes;
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig as OhDecoderConfig, Flush};
use openh264::encoder::{
    Complexity, Encoder, EncoderConfig as OhEncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile, QpRange,
    RateControlMode, SpsPpsStrategy, UsageType, VuiConfig,
};
use openh264::formats::{YUVSlices, YUVSource};
use veloxdesk_core_traits::video::{
    ColorRange, ColorSpace, DecodedFrame, DecoderCaps, DecoderConfig, EncodeOptions, EncodedPacket, EncoderCaps,
    EncoderConfig, EncoderUpdate, Frame, FrameData, PixelFormat, Plane, RateControl, VideoCodec,
};
use veloxdesk_core_traits::{CoreError, Result, VideoDecoder, VideoEncoder};

use crate::convert::{self, YuvMatrix};
use crate::input::CpuYuvInput;

pub const NAME: &str = "openh264";

pub fn encoder_caps() -> EncoderCaps {
    EncoderCaps {
        name: NAME,
        codecs: vec![VideoCodec::H264],
        hardware: false,
        gpu_input_formats: vec![],
        cpu_input_formats: vec![PixelFormat::Bgra8, PixelFormat::Nv12, PixelFormat::I420],
        yuv444: false,
        ten_bit: false,
        intra_refresh: false,
        max_width: 3840,
        max_height: 2160,
    }
}

pub fn decoder_caps() -> DecoderCaps {
    DecoderCaps {
        name: NAME,
        codecs: vec![VideoCodec::H264],
        hardware: false,
        gpu_output: false,
        yuv444: false,
        ten_bit: false,
    }
}

/// OpenH264 encoder configured for real-time screen content: no B-frames, no frame skipping,
/// single spatial/temporal layer, infinite GOP unless `gop` is set, IDR on request.
pub struct OpenH264Encoder {
    cfg: EncoderConfig,
    enc: Encoder,
    input: CpuYuvInput,
    frame_id: u32,
    initialized: bool,
    out: Vec<u8>,
}

fn oh_err(e: openh264::Error) -> CoreError {
    CoreError::platform(format!("openh264: {e}"))
}

fn build_oh_config(cfg: &EncoderConfig) -> OhEncoderConfig {
    let threads = if cfg.width * cfg.height >= 1920 * 1080 { 4 } else { 2 };
    let mut c = OhEncoderConfig::new()
        .usage_type(UsageType::ScreenContentRealTime)
        .skip_frames(false)
        .max_frame_rate(FrameRate::from_hz(cfg.fps.max(1) as f32))
        .bitrate(openh264::encoder::BitRate::from_bps(cfg.bitrate_kbps.saturating_mul(1000)))
        .complexity(Complexity::Low)
        .scene_change_detect(false)
        .background_detection(false)
        .adaptive_quantization(false)
        .long_term_reference(false)
        .sps_pps_strategy(SpsPpsStrategy::ConstantId)
        .profile(Profile::Baseline)
        .num_threads(threads)
        .intra_frame_period(IntraFramePeriod::from_num_frames(cfg.gop.unwrap_or(0)))
        .vui(match cfg.color_space {
            ColorSpace::Bt601 => VuiConfig::bt601(),
            _ => VuiConfig::bt709(),
        });
    c = match cfg.rate_control {
        RateControl::Cbr | RateControl::Vbr => c.rate_control_mode(RateControlMode::Bitrate),
        RateControl::Cqp(qp) => {
            let qp = qp.min(51);
            c.rate_control_mode(RateControlMode::Off).qp(QpRange::new(qp, qp))
        }
    };
    c
}

impl OpenH264Encoder {
    pub fn new(cfg: &EncoderConfig) -> Result<Self> {
        if cfg.codec != VideoCodec::H264 {
            return Err(CoreError::unsupported("openh264 only encodes H.264"));
        }
        if cfg.width < 16 || cfg.height < 16 || cfg.width > 3840 || cfg.height > 2160 && cfg.width > 2160 {
            return Err(CoreError::invalid(format!("openh264: unsupported size {}x{}", cfg.width, cfg.height)));
        }
        if cfg.bit_depth > 8 || cfg.chroma != veloxdesk_core_traits::video::ChromaFormat::Yuv420 {
            return Err(CoreError::unsupported("openh264: 8-bit 4:2:0 only"));
        }
        let enc = Encoder::with_api_config(OpenH264API::from_source(), build_oh_config(cfg)).map_err(oh_err)?;
        // H.264 needs even dimensions; odd ones are cropped by one pixel.
        let (w, h) = (cfg.width & !1, cfg.height & !1);
        Ok(Self {
            cfg: cfg.clone(),
            enc,
            input: CpuYuvInput::new(w, h, YuvMatrix::new(ColorSpace::Bt709, ColorRange::Limited)),
            frame_id: 0,
            initialized: false,
            out: Vec::with_capacity(256 * 1024),
        })
    }

    fn set_bitrate(&mut self, kbps: u32) -> Result<()> {
        use openh264_sys2::{ENCODER_OPTION_BITRATE, ENCODER_OPTION_MAX_BITRATE, SBitrateInfo, SPATIAL_LAYER_ALL};
        let mut info = SBitrateInfo { iLayer: SPATIAL_LAYER_ALL, iBitrate: (kbps.saturating_mul(1000)).min(i32::MAX as u32) as i32 };
        // SAFETY: the encoder is initialised (first frame encoded); the option struct lives on
        // the stack for the duration of the call and matches the documented C layout.
        unsafe {
            let api = self.enc.raw_api();
            let r1 = api.set_option(ENCODER_OPTION_MAX_BITRATE, (&raw mut info).cast());
            let r2 = api.set_option(ENCODER_OPTION_BITRATE, (&raw mut info).cast());
            if r1 != 0 || r2 != 0 {
                return Err(CoreError::platform(format!("openh264 set bitrate failed ({r1},{r2})")));
            }
        }
        Ok(())
    }

    fn set_fps(&mut self, fps: u32) -> Result<()> {
        use openh264_sys2::ENCODER_OPTION_FRAME_RATE;
        let mut f = fps.max(1) as f32;
        // SAFETY: as above; option takes a float*.
        let r = unsafe { self.enc.raw_api().set_option(ENCODER_OPTION_FRAME_RATE, (&raw mut f).cast()) };
        if r != 0 {
            return Err(CoreError::platform(format!("openh264 set fps failed ({r})")));
        }
        Ok(())
    }
}

impl VideoEncoder for OpenH264Encoder {
    fn caps(&self) -> EncoderCaps {
        encoder_caps()
    }

    fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    fn encode(&mut self, frame: &Frame, opts: EncodeOptions) -> Result<Option<EncodedPacket>> {
        let t0 = Instant::now();
        let convert_us = self.input.load(frame)?;
        let t1 = Instant::now();
        let (w, h) = self.input.dims();
        let (y, u, v) = self.input.planes();
        let src = YUVSlices::new((y, u, v), (w as usize, h as usize), self.input.strides());
        debug_assert_eq!(src.dimensions(), (w as usize, h as usize));
        if opts.force_keyframe && self.initialized {
            self.enc.force_intra_frame();
        }
        let ts = openh264::Timestamp::from_millis(frame.capture_ts.as_micros() / 1000);
        let bs = self.enc.encode_at(&src, ts).map_err(oh_err)?;
        self.initialized = true;
        let ft = bs.frame_type();
        if matches!(ft, FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        self.out.clear();
        bs.write_vec(&mut self.out);
        let keyframe = matches!(ft, FrameType::IDR | FrameType::I);
        let encode_us = t1.elapsed().as_micros() as u32;
        let _ = t0;
        let id = self.frame_id;
        self.frame_id = self.frame_id.wrapping_add(1);
        Ok(Some(EncodedPacket {
            data: Bytes::copy_from_slice(&self.out),
            codec: VideoCodec::H264,
            keyframe,
            recovery_point: keyframe,
            width: w,
            height: h,
            capture_ts: frame.capture_ts,
            frame_id: id,
            encode_us,
            convert_us,
        }))
    }

    fn update(&mut self, update: EncoderUpdate) -> Result<()> {
        if let Some(kbps) = update.bitrate_kbps {
            self.cfg.bitrate_kbps = kbps;
        }
        if let Some(fps) = update.fps {
            self.cfg.fps = fps;
        }
        if !self.initialized {
            // Not initialised yet: rebuild with the new parameters (no keyframe cost).
            self.enc = Encoder::with_api_config(OpenH264API::from_source(), build_oh_config(&self.cfg)).map_err(oh_err)?;
            return Ok(());
        }
        if let Some(kbps) = update.bitrate_kbps {
            self.set_bitrate(kbps)?;
        }
        if let Some(fps) = update.fps {
            self.set_fps(fps)?;
        }
        Ok(())
    }
}

/// OpenH264 decoder using `DecodeFrameNoDelay` (no reordering delay). Output: CPU NV12.
pub struct OpenH264Decoder {
    dec: Decoder,
    matrix: YuvMatrix,
}

impl OpenH264Decoder {
    pub fn new(cfg: &DecoderConfig) -> Result<Self> {
        if cfg.codec != VideoCodec::H264 {
            return Err(CoreError::unsupported("openh264 only decodes H.264"));
        }
        Ok(Self { dec: Self::make()?, matrix: YuvMatrix::new(ColorSpace::Bt709, ColorRange::Limited) })
    }

    fn make() -> Result<Decoder> {
        Decoder::with_api_config(OpenH264API::from_source(), OhDecoderConfig::new().flush_after_decode(Flush::NoFlush))
            .map_err(oh_err)
    }

    /// Matrix used when callers convert our NV12 output to RGB.
    pub fn matrix(&self) -> &YuvMatrix {
        &self.matrix
    }
}

impl VideoDecoder for OpenH264Decoder {
    fn caps(&self) -> DecoderCaps {
        decoder_caps()
    }

    fn decode(&mut self, packet: &EncodedPacket) -> Result<Option<DecodedFrame>> {
        let t0 = Instant::now();
        let yuv = match self.dec.decode(&packet.data) {
            Ok(Some(y)) => y,
            Ok(None) => return Ok(None),
            Err(e) => return Err(CoreError::platform(format!("openh264 decode: {e}"))),
        };
        let (w, h) = yuv.dimensions();
        let (ys, us, vs) = yuv.strides();
        let (cw, ch) = convert::chroma_dims(w as u32, h as u32);
        let mut yb = vec![0u8; w * h];
        convert::copy_plane(yuv.y(), ys, &mut yb, w, w, h);
        let mut uv = vec![0u8; cw * 2 * ch];
        convert::i420_chroma_to_nv12(yuv.u(), us, yuv.v(), vs, cw, ch, &mut uv, cw * 2);
        let decode_us = t0.elapsed().as_micros() as u32;
        Ok(Some(DecodedFrame {
            frame: Frame {
                width: w as u32,
                height: h as u32,
                format: PixelFormat::Nv12,
                color_space: ColorSpace::Bt709,
                color_range: ColorRange::Limited,
                data: FrameData::Cpu(vec![
                    Plane { data: Bytes::from(yb), stride: w },
                    Plane { data: Bytes::from(uv), stride: cw * 2 },
                ]),
                capture_ts: packet.capture_ts,
            },
            frame_id: packet.frame_id,
            decode_us,
        }))
    }

    fn reset(&mut self) -> Result<()> {
        self.dec = Self::make()?;
        Ok(())
    }
}
