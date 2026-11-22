//! # veloxdesk-render
//!
//! wgpu video presentation and the Performance HUD (ARCHITECTURE ADR-002, §5, §8).
//!
//! * [`VideoSurface`] implements [`veloxdesk_core_traits::VideoRenderer`]: CPU frames
//!   (NV12 / I420 / BGRA / RGBA) are uploaded with `queue.write_texture` into plane
//!   textures and converted to RGB in the fragment shader (BT.709/601/2020, limited/full);
//!   the local cursor and egui (UI + HUD) are drawn in the **same render pass**, then one
//!   present. Swapchain: Mailbox/Immediate/Fifo, `desired_maximum_frame_latency = 1`.
//! * [`OffscreenRenderer`] runs the same pipeline headless (tests, benches).
//! * [`zero_copy`] documents/hosts the GPU import hook (D3D11 shared handle → DX12), M3.

pub mod color;
pub mod hud;
pub mod layout;
pub mod offscreen;
pub mod pattern;
pub mod pipeline;
pub mod surface;
pub mod zero_copy;

pub use offscreen::OffscreenRenderer;
pub use surface::{RenderBreakdown, SurfaceOptions, VideoSurface};

/// Re-exports so dependents use the exact same versions.
pub use {egui, egui_winit, wgpu, winit};

#[cfg(test)]
mod gpu_tests {
    use veloxdesk_core_traits::render::ScaleMode;
    use veloxdesk_core_traits::video::ColorRange;

    use crate::OffscreenRenderer;
    use crate::color::{yuv_to_rgb8, yuv_to_rgb_matrix};
    use crate::pattern::{nv12_solid, nv12_test_frame};

    fn renderer(w: u32, h: u32) -> Option<OffscreenRenderer> {
        match OffscreenRenderer::new(w, h) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("skipping GPU test: {e}");
                None
            }
        }
    }

    #[test]
    fn nv12_solid_color_matches_reference() {
        let Some(mut r) = renderer(64, 32) else { return };
        for (y, u, v, range) in [(81u8, 90u8, 240u8, ColorRange::Limited), (145, 54, 34, ColorRange::Full)] {
            let f = nv12_solid(64, 32, y, u, v, range);
            let px = r.render_rgba(&f, ScaleMode::Stretch).unwrap();
            let m = yuv_to_rgb_matrix(f.color_space, range);
            let want = yuv_to_rgb8(&m, y, u, v);
            let got = &px[(16 * 64 + 32) * 4..(16 * 64 + 32) * 4 + 4];
            for i in 0..3 {
                assert!((got[i] as i32 - want[i] as i32).abs() <= 2, "{range:?}: got {got:?} want {want:?}");
            }
            assert_eq!(got[3], 255);
        }
    }

    #[test]
    fn fit_letterboxes_black() {
        let Some(mut r) = renderer(64, 64) else { return };
        let f = nv12_solid(64, 32, 235, 128, 128, ColorRange::Limited); // white 2:1
        let px = r.render_rgba(&f, ScaleMode::Fit).unwrap();
        let at = |x: usize, y: usize| px[(y * 64 + x) * 4];
        assert!(at(32, 2) < 5, "top bar should be black");
        assert!(at(32, 32) > 250, "center should be white");
    }

    #[test]
    fn yuv444_and_p010_render_cleanly() {
        use bytes::Bytes;
        use veloxdesk_core_traits::Timestamp;
        use veloxdesk_core_traits::video::{ColorSpace, Frame, FrameData, PixelFormat, Plane};

        let Some(mut r) = renderer(64, 32) else { return };

        // YUV 4:4:4 limited-range white (Y=235, U=128, V=128) -> [255, 255, 255].
        let yuv444 = Frame {
            width: 64,
            height: 32,
            format: PixelFormat::Yuv444,
            color_space: ColorSpace::Bt709,
            color_range: ColorRange::Limited,
            data: FrameData::Cpu(vec![
                Plane { data: Bytes::from(vec![235u8; 64 * 32]), stride: 64 },
                Plane { data: Bytes::from(vec![128u8; 64 * 32]), stride: 64 },
                Plane { data: Bytes::from(vec![128u8; 64 * 32]), stride: 64 },
            ]),
            capture_ts: Timestamp::now(),
        };
        let px = r.render_rgba(&yuv444, ScaleMode::Stretch).unwrap();
        let mid = &px[(16 * 64 + 32) * 4..(16 * 64 + 32) * 4 + 4];
        assert!(mid[0] > 250 && mid[1] > 250 && mid[2] > 250, "YUV444 white got {mid:?}");

        // P010 10-bit HDR10 PQ mid-gray (Y = 512 << 6 = 32768, U=V=512 << 6 = 32768) -> neutral gray after PQ tone-map.
        let mut y10 = Vec::with_capacity(64 * 32 * 2);
        for _ in 0..(64 * 32) {
            y10.extend_from_slice(&(512u16 << 6).to_le_bytes());
        }
        let mut uv10 = Vec::with_capacity(32 * 16 * 4);
        for _ in 0..(32 * 16 * 2) {
            uv10.extend_from_slice(&(512u16 << 6).to_le_bytes());
        }
        let p010 = Frame {
            width: 64,
            height: 32,
            format: PixelFormat::P010,
            color_space: ColorSpace::Bt2020Pq,
            color_range: ColorRange::Limited,
            data: FrameData::Cpu(vec![
                Plane { data: Bytes::from(y10), stride: 64 * 2 },
                Plane { data: Bytes::from(uv10), stride: 32 * 4 },
            ]),
            capture_ts: Timestamp::now(),
        };
        let px = r.render_rgba(&p010, ScaleMode::Stretch).unwrap();
        let mid = &px[(16 * 64 + 32) * 4..(16 * 64 + 32) * 4 + 4];
        assert!(
            mid[0] > 100 && mid[0].abs_diff(mid[1]) <= 3 && mid[1].abs_diff(mid[2]) <= 3,
            "P010 HDR10 PQ mid-gray should tone-map to neutral gray, got {mid:?}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn dx12_zero_copy_from_d3d11va_decoder_1080p() {
        use veloxdesk_core_traits::Timestamp;
        use veloxdesk_core_traits::video::{
            ChromaFormat, ColorSpace, DecoderConfig, EncodeOptions, EncoderConfig, Frame, FrameData, PixelFormat,
            Plane, RateControl, VideoCodec,
        };

        let Ok(mut r) = OffscreenRenderer::new_with_backends(1920, 1080, wgpu::Backends::DX12) else {
            eprintln!("skipping DX12 zero-copy test: no DX12 adapter");
            return;
        };
        if !r.caps().zero_copy_import.contains(&"D3D11Texture") {
            eprintln!("skipping DX12 zero-copy test: adapter lacks TEXTURE_FORMAT_NV12");
            return;
        }
        let enc_cfg = EncoderConfig {
            codec: VideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 20_000,
            rate_control: RateControl::Cbr,
            chroma: ChromaFormat::Yuv420,
            bit_depth: 8,
            gop: None,
            intra_refresh: false,
            input_format: PixelFormat::Bgra8,
            gpu_input: false,
            color_space: ColorSpace::Bt709,
        };
        let Ok(mut enc) = veloxdesk_codec::create_encoder(&enc_cfg, true) else {
            eprintln!("skipping DX12 zero-copy test: no HW encoder");
            return;
        };
        let dec_cfg = DecoderConfig {
            codec: VideoCodec::H264,
            width: 1920,
            height: 1080,
            prefer_gpu_output: true,
            shared_device: None,
        };
        let Ok(mut dec) = veloxdesk_codec::create_decoder(&dec_cfg, true) else {
            eprintln!("skipping DX12 zero-copy test: no HW decoder");
            return;
        };
        if !dec.caps().gpu_output {
            eprintln!("skipping DX12 zero-copy test: decoder does not output GPU frames");
            return;
        }

        // Solid green-ish BGRA8 frame (B=40, G=200, R=60, A=255).
        let mut bgra = vec![0u8; 1920 * 1080 * 4];
        for px in bgra.chunks_exact_mut(4) {
            px.copy_from_slice(&[40, 200, 60, 255]);
        }
        let src = Frame {
            width: 1920,
            height: 1080,
            format: PixelFormat::Bgra8,
            color_space: ColorSpace::Bt709,
            color_range: ColorRange::Limited,
            data: FrameData::Cpu(vec![Plane { data: bytes::Bytes::from(bgra), stride: 1920 * 4 }]),
            capture_ts: Timestamp::now(),
        };

        let mut samples = veloxdesk_core_traits::metrics::LatencySamples::default();
        let mut verified_pixels = false;
        for i in 0..35u32 {
            let pkt = match enc.encode(&src, EncodeOptions { force_keyframe: i == 0 }).unwrap() {
                Some(p) => p,
                None => continue,
            };
            let Some(decoded) = dec.decode(&pkt).unwrap() else { continue };
            assert!(decoded.frame.is_gpu(), "expected GPU frame from D3D11VA decoder");
            let (us, zc) = r.draw_frame(&decoded.frame, ScaleMode::Stretch).unwrap();
            assert!(zc, "expected DX12 zero-copy import to succeed");
            if i >= 5 {
                samples.push(us);
            }
            if !verified_pixels {
                let rgba = r.render_rgba(&decoded.frame, ScaleMode::Stretch).unwrap();
                let mid = &rgba[(540 * 1920 + 960) * 4..(540 * 1920 + 960) * 4 + 4];
                assert!(
                    mid[1] > 170 && mid[0] < 95 && mid[2] < 80,
                    "DX12 zero-copy rendered color mismatch (want ~[60,200,40], got {mid:?})"
                );
                verified_pixels = true;
            }
        }
        assert!(verified_pixels, "no frames decoded");
        eprintln!(
            "DX12 zero-copy 1080p NV12 import+draw+wait on {} ({:?}): p50 {} us p95 {} us p99 {} us ({} frames)",
            r.adapter_name,
            r.backend,
            samples.percentile(50.0).unwrap_or(0),
            samples.percentile(95.0).unwrap_or(0),
            samples.percentile(99.0).unwrap_or(0),
            samples.len(),
        );
    }

    #[test]
    fn render_us_1080p_offscreen() {
        let Some(mut r) = renderer(1920, 1080) else { return };
        let frames: Vec<_> = (0..4).map(|i| nv12_test_frame(1920, 1080, i)).collect();
        let mut s = veloxdesk_core_traits::metrics::LatencySamples::default();
        for i in 0..60 {
            s.push(r.draw(&frames[i % 4], ScaleMode::Fit).unwrap());
        }
        eprintln!(
            "offscreen 1080p NV12 upload+draw+wait on {} ({:?}): p50 {} us p99 {} us",
            r.adapter_name,
            r.backend,
            s.percentile(50.0).unwrap(),
            s.percentile(99.0).unwrap()
        );
    }
}
