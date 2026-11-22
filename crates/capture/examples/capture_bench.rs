//! Capture latency benchmark: p50/p99 `capture_us` over ~300 frames on the primary
//! display, GPU path then CPU path.
//!
//! `cargo run --release -p veloxdesk-capture --example capture_bench [frames]`
//!
//! DXGI DD only produces frames when the screen changes, so move a window / play a video
//! while it runs (static screens will hit the 10 s per-path time limit).

use std::time::{Duration, Instant};

use veloxdesk_capture::util::percentile;
use veloxdesk_capture::{create_capture, is_cursor_only, list_displays};
use veloxdesk_core_traits::video::{CaptureConfig, DisplayId, PixelFormat};

fn run(prefer_gpu: bool, display: &DisplayId, target: usize) {
    let label = if prefer_gpu { "GPU" } else { "CPU" };
    let mut cap = match create_capture(prefer_gpu) {
        Ok(c) => c,
        Err(e) => return println!("{label}: create_capture failed: {e}"),
    };
    let cfg = CaptureConfig {
        display: display.clone(),
        max_fps: 0, // unthrottled
        preferred_format: PixelFormat::Bgra8,
        prefer_gpu,
        draw_cursor: false,
        hdr: false,
    };
    if let Err(e) = cap.start(&cfg) {
        return println!("{label}: start failed: {e}");
    }
    let mut samples = Vec::with_capacity(target);
    let mut dirty = 0usize;
    let mut kept = Vec::new(); // hold a few frames like an encoder queue would
    let t0 = Instant::now();
    let deadline = t0 + Duration::from_secs(10);
    while samples.len() < target && Instant::now() < deadline {
        match cap.next_frame(Duration::from_millis(50)) {
            Ok(Some(f)) if !is_cursor_only(&f.dirty_rects) => {
                samples.push(f.capture_us);
                dirty += f.dirty_rects.len();
                kept.push(f);
                if kept.len() > 2 {
                    kept.remove(0);
                }
            }
            Ok(_) => {}
            Err(e) => {
                println!("{label}: next_frame error: {e}; restarting");
                if cap.start(&cfg).is_err() {
                    break;
                }
            }
        }
    }
    let secs = t0.elapsed().as_secs_f64();
    let n = samples.len();
    let (w, h) = kept.last().map(|f| (f.frame.width, f.frame.height)).unwrap_or((0, 0));
    println!(
        "{label}: {n} frames {w}x{h} in {secs:.1}s ({:.1} fps)  capture_us p50={} p99={} max={}  avg dirty rects={:.1}",
        n as f64 / secs,
        percentile(&mut samples, 50.0),
        percentile(&mut samples, 99.0),
        samples.last().copied().unwrap_or(0),
        dirty as f64 / n.max(1) as f64
    );
    cap.stop();
}

fn main() {
    let target: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let displays = match list_displays() {
        Ok(d) if !d.is_empty() => d,
        other => return println!("no displays: {other:?}"),
    };
    for d in &displays {
        println!("{d:?}");
    }
    let primary = displays.iter().find(|d| d.primary).unwrap_or(&displays[0]);
    run(true, &primary.id, target);
    run(false, &primary.id, target);
}
