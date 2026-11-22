//! Smoke test: list displays and grab a few frames (GPU and CPU paths). Tolerates
//! environments without an interactive desktop (CI, services, secure desktop).

use std::time::{Duration, Instant};

use veloxdesk_capture::{create_capture, is_cursor_only, list_displays};
use veloxdesk_core_traits::video::{CaptureConfig, DisplayId, FrameData, GpuHandle, PixelFormat};
use veloxdesk_core_traits::CoreError;

fn skippable(e: &CoreError) -> bool {
    matches!(e, CoreError::Unsupported(_) | CoreError::AccessDenied(_) | CoreError::Reinit(_))
}

fn grab(prefer_gpu: bool) {
    let displays = match list_displays() {
        Ok(d) if !d.is_empty() => d,
        Ok(_) => return println!("skip: no displays"),
        Err(e) if skippable(&e) || matches!(e, CoreError::Platform(_)) => return println!("skip: list_displays: {e}"),
        Err(e) => panic!("list_displays: {e}"),
    };
    for d in &displays {
        println!("display: {d:?}");
        assert!(d.width > 0 && d.height > 0);
        assert!(d.refresh_hz > 0);
    }
    assert!(displays.iter().filter(|d| d.primary).count() <= 1);
    let primary = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]);

    let mut cap = match create_capture(prefer_gpu) {
        Ok(c) => c,
        Err(e) if skippable(&e) => return println!("skip: create_capture: {e}"),
        Err(e) => panic!("create_capture: {e}"),
    };
    let caps = cap.caps();
    println!("caps: {caps:?}");
    let cfg = CaptureConfig {
        display: DisplayId(primary.id.0.clone()),
        max_fps: 120,
        preferred_format: PixelFormat::Bgra8,
        prefer_gpu,
        draw_cursor: false,
        hdr: false,
    };
    if let Err(e) = cap.start(&cfg) {
        if skippable(&e) {
            return println!("skip: start: {e}");
        }
        panic!("start: {e}");
    }

    // Static screens deliver nothing after the first frame; the first frame after start is
    // always delivered by DXGI DD, so expect >= 1 frame within a couple of seconds.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut frames = 0;
    let mut reinits = 0;
    while Instant::now() < deadline && frames < 5 {
        match cap.next_frame(Duration::from_millis(100)) {
            Ok(Some(f)) => {
                if is_cursor_only(&f.dirty_rects) {
                    continue;
                }
                frames += 1;
                let fr = &f.frame;
                assert_eq!(fr.format, PixelFormat::Bgra8);
                assert!(fr.width > 0 && fr.height > 0);
                match &fr.data {
                    FrameData::Cpu(planes) => {
                        assert!(!prefer_gpu || !caps.gpu_frames);
                        assert_eq!(planes.len(), 1);
                        assert!(planes[0].stride >= fr.width as usize * 4);
                        assert_eq!(planes[0].data.len(), planes[0].stride * fr.height as usize);
                    }
                    FrameData::Gpu(g) => {
                        assert!(prefer_gpu);
                        assert!(g.keepalive.is_some());
                        if let GpuHandle::D3D11Texture { texture, .. } = g.handle {
                            assert_ne!(texture, 0);
                        }
                    }
                }
                for r in &f.dirty_rects {
                    assert!(r.x + r.width <= fr.width && r.y + r.height <= fr.height);
                }
                println!(
                    "frame {frames}: {}x{} gpu={} dirty={} capture_us={} cursor={:?}",
                    fr.width,
                    fr.height,
                    fr.is_gpu(),
                    f.dirty_rects.len(),
                    f.capture_us,
                    f.cursor.as_ref().map(|c| (c.x, c.y, c.visible, c.shape.as_ref().map(|s| (s.id, s.width, s.height))))
                );
            }
            Ok(None) => {}
            Err(e @ CoreError::Reinit(_)) if reinits < 3 => {
                reinits += 1;
                println!("reinit: {e}; restarting");
                if let Err(e) = cap.start(&cfg) {
                    if skippable(&e) {
                        return println!("skip: restart: {e}");
                    }
                    panic!("restart: {e}");
                }
            }
            Err(e) if skippable(&e) => return println!("skip: next_frame: {e}"),
            Err(e) => panic!("next_frame: {e}"),
        }
    }
    cap.stop();
    assert!(frames >= 1, "no frame captured within 3 s");
}

#[test]
fn smoke_gpu() {
    grab(true);
}

#[test]
fn smoke_cpu() {
    grab(false);
}
