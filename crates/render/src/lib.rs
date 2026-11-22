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
