//! Video placement inside the window (Fit / Stretch / Original) and the inverse mapping
//! used to translate local pointer positions into host display pixels.

use veloxdesk_core_traits::render::ScaleMode;

/// Video rectangle in window pixels (may extend beyond the window in `Original` mode).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl VideoRect {
    /// `[left, top, right, bottom]` in normalized device coordinates (+y up).
    pub fn to_ndc(&self, window: (u32, u32)) -> [f32; 4] {
        let (ww, wh) = (window.0.max(1) as f32, window.1.max(1) as f32);
        [
            self.x / ww * 2.0 - 1.0,
            1.0 - self.y / wh * 2.0,
            (self.x + self.w) / ww * 2.0 - 1.0,
            1.0 - (self.y + self.h) / wh * 2.0,
        ]
    }
}

/// Where a `video` sized image lands in a `window` sized surface.
pub fn compute_video_rect(mode: ScaleMode, video: (u32, u32), window: (u32, u32)) -> VideoRect {
    let (vw, vh) = (video.0.max(1) as f32, video.1.max(1) as f32);
    let (ww, wh) = (window.0 as f32, window.1 as f32);
    match mode {
        ScaleMode::Stretch => VideoRect { x: 0.0, y: 0.0, w: ww, h: wh },
        ScaleMode::Original => {
            VideoRect { x: ((ww - vw) / 2.0).round(), y: ((wh - vh) / 2.0).round(), w: vw, h: vh }
        }
        ScaleMode::Fit => {
            let s = (ww / vw).min(wh / vh);
            let (w, h) = ((vw * s).round(), (vh * s).round());
            VideoRect { x: ((ww - w) / 2.0).round(), y: ((wh - h) / 2.0).round(), w, h }
        }
    }
}

/// Map a window position (physical pixels) to video pixels. Returns `None` when outside
/// the video; use [`window_to_video_clamped`] for drags that leave the picture.
pub fn window_to_video(rect: &VideoRect, video: (u32, u32), pos: (f64, f64)) -> Option<(i32, i32)> {
    let (px, py) = (pos.0 as f32, pos.1 as f32);
    if px < rect.x || py < rect.y || px >= rect.x + rect.w || py >= rect.y + rect.h {
        return None;
    }
    Some(window_to_video_clamped(rect, video, pos))
}

/// Like [`window_to_video`] but clamps to the video bounds.
pub fn window_to_video_clamped(rect: &VideoRect, video: (u32, u32), pos: (f64, f64)) -> (i32, i32) {
    let sx = video.0 as f32 / rect.w.max(1.0);
    let sy = video.1 as f32 / rect.h.max(1.0);
    let x = ((pos.0 as f32 - rect.x) * sx).floor() as i32;
    let y = ((pos.1 as f32 - rect.y) * sy).floor() as i32;
    (x.clamp(0, video.0 as i32 - 1), y.clamp(0, video.1 as i32 - 1))
}

/// Map a video pixel to window pixels (cursor overlay placement).
pub fn video_to_window(rect: &VideoRect, video: (u32, u32), pos: (f32, f32)) -> (f32, f32) {
    let sx = rect.w / video.0.max(1) as f32;
    let sy = rect.h / video.1.max(1) as f32;
    (rect.x + pos.0 * sx, rect.y + pos.1 * sy)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_letterbox() {
        let r = compute_video_rect(ScaleMode::Fit, (1920, 1080), (1000, 1000));
        assert_eq!(r.w, 1000.0);
        assert_eq!(r.h, 563.0);
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 219.0);
    }

    #[test]
    fn original_centered_and_cropped() {
        let r = compute_video_rect(ScaleMode::Original, (2560, 1440), (1920, 1080));
        assert_eq!((r.x, r.y, r.w, r.h), (-320.0, -180.0, 2560.0, 1440.0));
        let ndc = r.to_ndc((1920, 1080));
        assert!(ndc[0] < -1.0 && ndc[2] > 1.0);
    }

    #[test]
    fn inverse_mapping() {
        let video = (1920, 1080);
        let r = compute_video_rect(ScaleMode::Fit, video, (960, 1000));
        assert_eq!(window_to_video(&r, video, (0.0, 0.0)), None);
        let (x, y) = window_to_video(&r, video, (480.0, 500.0)).unwrap();
        assert!((x - 960).abs() <= 2 && (y - 540).abs() <= 2, "{x},{y}");
        assert_eq!(window_to_video_clamped(&r, video, (5000.0, 5000.0)), (1919, 1079));
        let (wx, wy) = video_to_window(&r, video, (960.0, 540.0));
        assert!((wx - 480.0).abs() < 1.0 && (wy - 500.0).abs() < 1.0);
    }

    #[test]
    fn stretch_ndc_is_full() {
        let r = compute_video_rect(ScaleMode::Stretch, (640, 480), (800, 600));
        assert_eq!(r.to_ndc((800, 600)), [-1.0, 1.0, 1.0, -1.0]);
    }
}
