//! # veloxdesk-codec
//!
//! GPU color conversion and hardware/software video encoders and decoders.
//!
//! ## Backends
//! * **Windows**: Media Foundation hardware encoders (NVENC / AMF / QSV) with D3D11
//!   `VideoProcessor` BGRA→NV12 GPU conversion, and D3D11VA hardware decoders.
//! * **Fallback (all OSes)**: Cisco OpenH264 software H.264 encoder & decoder.
//!
//! See `docs/ARCHITECTURE.md` (ADR-007) and `docs/SWARM.md` for the M1 public API.

pub mod annexb;
pub mod convert;
mod input;
pub mod sw;
#[cfg(windows)]
pub mod win;

use veloxdesk_core_traits::video::{DecoderCaps, DecoderConfig, EncoderCaps, EncoderConfig, VideoDecoder, VideoEncoder};
use veloxdesk_core_traits::Result;

/// Probe available video encoders on this machine, ordered from preferred hardware
/// backends to the always-available software fallback.
pub fn probe_encoders() -> Vec<EncoderCaps> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        out.extend(win::probe_encoders());
    }
    out.push(sw::encoder_caps());
    out
}

/// Probe available video decoders on this machine, ordered from preferred hardware
/// backends to the always-available software fallback.
pub fn probe_decoders() -> Vec<DecoderCaps> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        out.extend(win::probe_decoders());
    }
    out.push(sw::decoder_caps());
    out
}

/// Create a video encoder for `cfg`. When `prefer_hw` is `true`, tries hardware backends
/// first and falls back to software (`openh264`) if unavailable.
pub fn create_encoder(cfg: &EncoderConfig, prefer_hw: bool) -> Result<Box<dyn VideoEncoder>> {
    if prefer_hw {
        #[cfg(windows)]
        match win::mf_enc::MfEncoder::new(cfg) {
            Ok(enc) => return Ok(Box::new(enc)),
            Err(e) => {
                tracing::warn!(codec = ?cfg.codec, "hardware encoder unavailable ({e}); falling back to software");
            }
        }
    }
    Ok(Box::new(sw::OpenH264Encoder::new(cfg)?))
}

/// Create a video decoder for `cfg`. When `prefer_hw` is `true`, tries hardware backends
/// first and falls back to software (`openh264`) if unavailable.
pub fn create_decoder(cfg: &DecoderConfig, prefer_hw: bool) -> Result<Box<dyn VideoDecoder>> {
    if prefer_hw {
        #[cfg(windows)]
        match win::mf_dec::MfDecoder::new(cfg) {
            Ok(dec) => return Ok(Box::new(dec)),
            Err(e) => {
                tracing::warn!(codec = ?cfg.codec, "hardware decoder unavailable ({e}); falling back to software");
            }
        }
    }
    Ok(Box::new(sw::OpenH264Decoder::new(cfg)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use veloxdesk_core_traits::Timestamp;
    use veloxdesk_core_traits::video::{
        ChromaFormat, ColorRange, ColorSpace, EncodeOptions, EncoderUpdate, Frame, FrameData, PixelFormat, Plane,
        RateControl, VideoCodec,
    };

    fn make_bgra_frame(w: u32, h: u32, idx: u32) -> Frame {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let b = ((x + idx * 5) % 256) as u8;
                let g = ((y + idx * 3) % 256) as u8;
                let r = (((x + y) / 2 + idx * 7) % 256) as u8;
                buf[i] = b;
                buf[i + 1] = g;
                buf[i + 2] = r;
                buf[i + 3] = 255;
            }
        }
        Frame {
            width: w,
            height: h,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Srgb,
            color_range: ColorRange::Full,
            data: FrameData::Cpu(vec![Plane { data: Bytes::from(buf), stride: (w * 4) as usize }]),
            capture_ts: Timestamp(idx as u64 * 16_667),
        }
    }

    fn enc_cfg(w: u32, h: u32, codec: VideoCodec) -> EncoderConfig {
        EncoderConfig {
            codec,
            width: w,
            height: h,
            fps: 60,
            bitrate_kbps: 8_000,
            rate_control: RateControl::Cbr,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
            gop: None,
            intra_refresh: false,
            input_format: PixelFormat::Bgra8,
            gpu_input: false,
            color_space: ColorSpace::Bt709,
        }
    }

    fn dec_cfg(w: u32, h: u32, codec: VideoCodec, prefer_gpu_output: bool) -> DecoderConfig {
        DecoderConfig { codec, width: w, height: h, prefer_gpu_output, shared_device: None }
    }

    fn frame_psnr_against_bgra(src_bgra: &Frame, decoded_nv12: &Frame) -> f64 {
        assert_eq!(decoded_nv12.format, PixelFormat::Nv12);
        let FrameData::Cpu(ref sp) = src_bgra.data else { panic!("expected cpu src") };
        let FrameData::Cpu(ref dp) = decoded_nv12.data else { panic!("expected cpu decoded") };
        let m = convert::YuvMatrix::new(ColorSpace::Bt709, ColorRange::Limited);
        let (w, h) = (decoded_nv12.width, decoded_nv12.height);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        convert::nv12_to_rgba(&dp[0].data, dp[0].stride, &dp[1].data, dp[1].stride, w, h, &mut rgba, (w * 4) as usize, &m);
        let mut back_bgra = vec![0u8; (w * h * 4) as usize];
        for (o, p) in back_bgra.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            o.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        convert::psnr(&sp[0].data, &back_bgra)
    }

    #[test]
    fn probe_lists_backends() {
        let encs = probe_encoders();
        let decs = probe_decoders();
        eprintln!("probed encoders: {encs:#?}");
        eprintln!("probed decoders: {decs:#?}");
        assert!(encs.iter().any(|c| c.name == "openh264" && !c.hardware));
        assert!(decs.iter().any(|c| c.name == "openh264" && !c.hardware));
    }

    #[test]
    fn sw_roundtrip_and_controls() {
        let (w, h) = (320, 180);
        let mut enc = create_encoder(&enc_cfg(w, h, VideoCodec::H264), false).expect("sw enc");
        let mut dec = create_decoder(&dec_cfg(w, h, VideoCodec::H264, false), false).expect("sw dec");
        assert!(!enc.caps().hardware);
        assert!(!dec.caps().hardware);

        for i in 0..10u32 {
            let f = make_bgra_frame(w, h, i);
            let pkt = enc
                .encode(&f, EncodeOptions { force_keyframe: i == 5 })
                .expect("encode")
                .expect("packet");
            assert_eq!(pkt.frame_id, i);
            assert_eq!(pkt.width, w);
            assert_eq!(pkt.height, h);
            if i == 0 || i == 5 {
                assert!(pkt.keyframe, "frame {i} should be keyframe");
                assert!(annexb::is_keyframe(VideoCodec::H264, &pkt.data));
            }
            let df = dec.decode(&pkt).expect("decode").expect("decoded frame");
            assert_eq!(df.frame.width, w);
            assert_eq!(df.frame.height, h);
            assert_eq!(df.frame.format, PixelFormat::Nv12);
            let psnr = frame_psnr_against_bgra(&f, &df.frame);
            assert!(psnr > 28.0, "frame {i} psnr too low: {psnr:.2} dB");
            if i == 3 {
                enc.update(EncoderUpdate { bitrate_kbps: Some(4_000), fps: Some(30) }).expect("update");
            }
            if i == 4 {
                dec.reset().expect("reset");
            }
        }
    }

    #[test]
    fn prefer_hw_roundtrip_and_timings_1080p() {
        let (w, h) = (1920, 1080);
        let mut enc = create_encoder(&enc_cfg(w, h, VideoCodec::H264), true).expect("enc");
        let mut dec = create_decoder(&dec_cfg(w, h, VideoCodec::H264, false), true).expect("dec");
        let ecaps = enc.caps();
        let dcaps = dec.caps();

        let frames: Vec<Frame> = (0..12u32).map(|i| make_bgra_frame(w, h, i)).collect();
        let mut conv_us = Vec::new();
        let mut enc_us = Vec::new();
        let mut dec_us = Vec::new();
        let mut decoded = 0usize;
        let mut last_psnr = 0.0;

        for (i, f) in frames.iter().enumerate() {
            if let Some(pkt) = enc.encode(f, EncodeOptions { force_keyframe: i == 0 }).expect("encode") {
                if i >= 2 {
                    conv_us.push(pkt.convert_us);
                    enc_us.push(pkt.encode_us);
                }
                if let Some(df) = dec.decode(&pkt).expect("decode") {
                    assert_eq!(df.frame.width, w);
                    assert_eq!(df.frame.height, h);
                    assert_eq!(df.frame.format, PixelFormat::Nv12);
                    if i >= 2 {
                        dec_us.push(df.decode_us);
                    }
                    last_psnr = frame_psnr_against_bgra(f, &df.frame);
                    decoded += 1;
                }
            }
        }
        assert!(decoded >= 8, "expected >= 8 decoded frames, got {decoded}");
        assert!(last_psnr > 26.0, "1080p psnr too low: {last_psnr:.2} dB");
        let avg = |v: &[u32]| if v.is_empty() { 0 } else { (v.iter().map(|&x| x as u64).sum::<u64>() / v.len() as u64) as u32 };
        eprintln!(
            "1080p prefer_hw: enc={} (hw={}) dec={} (hw={}) decoded={} psnr={:.1}dB avg_convert={}us avg_encode={}us avg_decode={}us",
            ecaps.name,
            ecaps.hardware,
            dcaps.name,
            dcaps.hardware,
            decoded,
            last_psnr,
            avg(&conv_us),
            avg(&enc_us),
            avg(&dec_us)
        );
    }

    #[test]
    fn sw_timings_1080p() {
        let (w, h) = (1920, 1080);
        let mut enc = create_encoder(&enc_cfg(w, h, VideoCodec::H264), false).expect("sw enc");
        let mut dec = create_decoder(&dec_cfg(w, h, VideoCodec::H264, false), false).expect("sw dec");
        let frames: Vec<Frame> = (0..8u32).map(|i| make_bgra_frame(w, h, i)).collect();
        let mut conv_us = Vec::new();
        let mut enc_us = Vec::new();
        let mut dec_us = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            let pkt = enc.encode(f, EncodeOptions { force_keyframe: i == 0 }).expect("encode").expect("pkt");
            let df = dec.decode(&pkt).expect("decode").expect("frame");
            if i >= 1 {
                conv_us.push(pkt.convert_us);
                enc_us.push(pkt.encode_us);
                dec_us.push(df.decode_us);
            }
        }
        let avg = |v: &[u32]| (v.iter().map(|&x| x as u64).sum::<u64>() / v.len() as u64) as u32;
        eprintln!(
            "1080p SW (openh264): avg_convert={}us avg_encode={}us avg_decode={}us",
            avg(&conv_us),
            avg(&enc_us),
            avg(&dec_us)
        );
    }

    #[cfg(windows)]
    #[test]
    fn hw_gpu_zero_copy_encode_decode() {
        use windows::Win32::Graphics::Direct3D11::*;
        use windows::Win32::Graphics::Dxgi::Common::*;

        let enc_caps = probe_encoders();
        let dec_caps = probe_decoders();
        if !enc_caps.iter().any(|c| c.hardware && c.codecs.contains(&VideoCodec::H264)) {
            eprintln!("skipping GPU zero-copy test: no HW H.264 encoder");
            return;
        }
        let (w, h) = (1920u32, 1080u32);
        let dev = win::d3d::create_device(None).expect("d3d11 device");
        let luid = win::d3d::device_luid(&dev);
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut pool = win::d3d::TexturePool::new(desc, false, 2);
        let holder = pool.get(&dev).expect("pool texture");
        let cpu = make_bgra_frame(w, h, 1);
        let FrameData::Cpu(ref planes) = cpu.data else { unreachable!() };
        unsafe {
            let ctx = dev.GetImmediateContext().expect("ctx");
            ctx.UpdateSubresource(&holder.tex, 0, None, planes[0].data.as_ptr().cast(), planes[0].stride as u32, 0);
        }

        let mut cfg = enc_cfg(w, h, VideoCodec::H264);
        cfg.gpu_input = true;
        let mut enc = create_encoder(&cfg, true).expect("hw enc");
        assert!(enc.caps().hardware);

        let prefer_gpu_dec = dec_caps.iter().any(|c| c.hardware && c.gpu_output && c.codecs.contains(&VideoCodec::H264));
        let mut dec = create_decoder(&dec_cfg(w, h, VideoCodec::H264, prefer_gpu_dec), true).expect("dec");

        let mut conv_us = Vec::new();
        let mut enc_us = Vec::new();
        let mut dec_us = Vec::new();
        let mut got_gpu = false;
        for i in 0..12u32 {
            let gframe = Frame {
                width: w,
                height: h,
                format: PixelFormat::Bgra8,
                color_space: ColorSpace::Srgb,
                color_range: ColorRange::Full,
                data: FrameData::Gpu(win::d3d::gpu_frame(&holder, luid)),
                capture_ts: Timestamp(i as u64 * 16_667),
            };
            if i == 5 {
                let _ = enc.update(EncoderUpdate { bitrate_kbps: Some(12_000), fps: Some(60) });
                dec.reset().expect("hw dec reset");
            }
            if let Some(pkt) = enc.encode(&gframe, EncodeOptions { force_keyframe: i == 0 || i == 5 }).expect("gpu encode") {
                if i >= 2 {
                    conv_us.push(pkt.convert_us);
                    enc_us.push(pkt.encode_us);
                }
                if let Some(df) = dec.decode(&pkt).expect("decode") {
                    assert_eq!(df.frame.width, w);
                    assert_eq!(df.frame.height, h);
                    assert_eq!(df.frame.format, PixelFormat::Nv12);
                    if df.frame.is_gpu() {
                        got_gpu = true;
                    }
                    if i >= 2 {
                        dec_us.push(df.decode_us);
                    }
                }
            }
        }
        if prefer_gpu_dec {
            assert!(got_gpu, "expected GPU NV12 output from HW decoder");
        }
        let avg = |v: &[u32]| if v.is_empty() { 0 } else { (v.iter().map(|&x| x as u64).sum::<u64>() / v.len() as u64) as u32 };
        eprintln!(
            "1080p GPU zero-copy: enc={} dec={} gpu_out={} avg_convert={}us avg_encode={}us avg_decode={}us",
            enc.caps().name,
            dec.caps().name,
            got_gpu,
            avg(&conv_us),
            avg(&enc_us),
            avg(&dec_us)
        );
    }

    #[test]
    fn hw_sw_interop() {
        let (w, h) = (640, 360);
        // HW enc -> SW dec
        let mut hw_enc = create_encoder(&enc_cfg(w, h, VideoCodec::H264), true).expect("hw enc");
        let mut sw_dec = create_decoder(&dec_cfg(w, h, VideoCodec::H264, false), false).expect("sw dec");
        let mut decoded = 0;
        for i in 0..6u32 {
            let f = make_bgra_frame(w, h, i);
            if let Some(pkt) = hw_enc.encode(&f, EncodeOptions { force_keyframe: i == 0 }).expect("enc")
                && let Some(df) = sw_dec.decode(&pkt).expect("dec")
            {
                assert_eq!((df.frame.width, df.frame.height), (w, h));
                decoded += 1;
            }
        }
        assert!(decoded >= 4, "hw->sw decoded only {decoded}");

        // SW enc -> HW dec
        let mut sw_enc = create_encoder(&enc_cfg(w, h, VideoCodec::H264), false).expect("sw enc");
        let mut hw_dec = create_decoder(&dec_cfg(w, h, VideoCodec::H264, false), true).expect("hw dec");
        let mut decoded = 0;
        for i in 0..6u32 {
            let f = make_bgra_frame(w, h, i);
            if let Some(pkt) = sw_enc.encode(&f, EncodeOptions { force_keyframe: i == 0 }).expect("enc")
                && let Some(df) = hw_dec.decode(&pkt).expect("dec")
            {
                assert_eq!((df.frame.width, df.frame.height), (w, h));
                decoded += 1;
            }
        }
        assert!(decoded >= 4, "sw->hw decoded only {decoded}");
    }
}
