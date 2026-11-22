//! Platform-neutral helpers shared by the capture backends (pure, unit-tested).

use veloxdesk_core_traits::video::Rect;

/// Above this many rects we collapse the set to its bounding box: encoders and the
/// wire only benefit from a handful of regions, and merging is O(n²).
pub const MAX_DIRTY_RECTS: usize = 32;

/// Signed rectangle as reported by OS APIs (`left, top, right, bottom`, exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Clip `r` to `width x height` and convert; `None` if empty after clipping.
pub fn clip_rect(r: IRect, width: u32, height: u32) -> Option<Rect> {
    let l = r.left.clamp(0, width as i32);
    let t = r.top.clamp(0, height as i32);
    let rr = r.right.clamp(0, width as i32);
    let b = r.bottom.clamp(0, height as i32);
    if rr <= l || b <= t {
        return None;
    }
    Some(Rect { x: l as u32, y: t as u32, width: (rr - l) as u32, height: (b - t) as u32 })
}

fn touches(a: &Rect, b: &Rect) -> bool {
    // Overlapping or edge-adjacent rects are merged (adjacent strips are common in DD).
    a.x <= b.x + b.width && b.x <= a.x + a.width && a.y <= b.y + b.height && b.y <= a.y + a.height
}

fn union(a: &Rect, b: &Rect) -> Rect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = (a.x + a.width).max(b.x + b.width);
    let y1 = (a.y + a.height).max(b.y + b.height);
    Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
}

/// Merge overlapping / touching rects in place until no two touch; if more than
/// [`MAX_DIRTY_RECTS`] remain, collapse to the bounding box. Empty rects are dropped.
pub fn merge_rects(rects: &mut Vec<Rect>) {
    rects.retain(|r| r.width > 0 && r.height > 0);
    let mut changed = true;
    while changed {
        changed = false;
        let mut i = 0;
        while i < rects.len() {
            let mut j = i + 1;
            while j < rects.len() {
                if touches(&rects[i], &rects[j]) {
                    let m = union(&rects[i], &rects[j]);
                    rects[i] = m;
                    rects.swap_remove(j);
                    changed = true;
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }
    if rects.len() > MAX_DIRTY_RECTS {
        let bb = rects.iter().skip(1).fold(rects[0], |acc, r| union(&acc, r));
        rects.clear();
        rects.push(bb);
    }
    rects.sort_by_key(|r| (r.y, r.x));
}

/// Is the rect set equivalent to "the whole frame"?
pub fn covers_frame(rects: &[Rect], width: u32, height: u32) -> bool {
    rects.len() == 1 && rects[0] == Rect { x: 0, y: 0, width, height }
}

// ───────────────────────────── cursor shapes ─────────────────────────────

/// Kind of cursor bitmap delivered by the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    /// 32bpp BGRA with straight alpha (DXGI `COLOR`).
    Color,
    /// 32bpp BGRX where the top byte is a mask: 0x00 = opaque RGB, 0xFF = XOR with the
    /// screen (DXGI `MASKED_COLOR`).
    MaskedColor,
    /// 1bpp AND mask (top half) followed by 1bpp XOR mask (bottom half); `height` is the
    /// doubled height (DXGI `MONOCHROME`).
    Monochrome,
}

/// Convert an OS cursor bitmap into straight-alpha RGBA8 (tightly packed).
/// Returns `(width, height, rgba)`. XOR ("invert screen") pixels cannot be represented
/// in RGBA: they are approximated as opaque black (monochrome) or opaque colour
/// (masked colour) so I-beams stay visible on light backgrounds; XOR with black (no-op)
/// becomes transparent.
pub fn cursor_to_rgba(kind: CursorKind, width: u32, height: u32, pitch: u32, src: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let w = width as usize;
    let pitch = pitch as usize;
    match kind {
        CursorKind::Color | CursorKind::MaskedColor => {
            let h = height as usize;
            if pitch < w * 4 || src.len() < pitch * h.saturating_sub(1) + w * 4 {
                return None;
            }
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                let row = &src[y * pitch..y * pitch + w * 4];
                for px in row.chunks_exact(4) {
                    let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
                    if kind == CursorKind::Color {
                        out.extend_from_slice(&[r, g, b, a]);
                    } else if a == 0 {
                        out.extend_from_slice(&[r, g, b, 0xFF]);
                    } else if (r | g | b) == 0 {
                        out.extend_from_slice(&[0, 0, 0, 0]);
                    } else {
                        out.extend_from_slice(&[r, g, b, 0xFF]);
                    }
                }
            }
            Some((width, height, out))
        }
        CursorKind::Monochrome => {
            let h = (height / 2) as usize;
            let row_bytes = w.div_ceil(8);
            if pitch < row_bytes || src.len() < pitch * (2 * h).saturating_sub(1) + row_bytes {
                return None;
            }
            let mut out = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                let and_row = &src[y * pitch..];
                let xor_row = &src[(y + h) * pitch..];
                for x in 0..w {
                    let bit = 0x80u8 >> (x % 8);
                    let and = and_row[x / 8] & bit != 0;
                    let xor = xor_row[x / 8] & bit != 0;
                    let px = match (and, xor) {
                        (false, false) => [0, 0, 0, 0xFF],       // black
                        (false, true) => [0xFF, 0xFF, 0xFF, 0xFF], // white
                        (true, false) => [0, 0, 0, 0],           // transparent
                        (true, true) => [0, 0, 0, 0xFF],         // invert → black
                    };
                    out.extend_from_slice(&px);
                }
            }
            Some((width, h as u32, out))
        }
    }
}

/// Percentile (0..=100) of an unsorted sample set (nearest-rank). Used by tests/benches.
pub fn percentile(samples: &mut [u32], p: f64) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    samples.sort_unstable();
    let rank = ((p / 100.0) * samples.len() as f64).ceil() as usize;
    samples[rank.clamp(1, samples.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn clip() {
        assert_eq!(clip_rect(IRect { left: -10, top: -5, right: 20, bottom: 10 }, 100, 100), Some(r(0, 0, 20, 10)));
        assert_eq!(clip_rect(IRect { left: 90, top: 90, right: 120, bottom: 130 }, 100, 100), Some(r(90, 90, 10, 10)));
        assert_eq!(clip_rect(IRect { left: 100, top: 0, right: 120, bottom: 10 }, 100, 100), None);
        assert_eq!(clip_rect(IRect { left: 5, top: 5, right: 5, bottom: 10 }, 100, 100), None);
    }

    #[test]
    fn merge_overlapping_and_adjacent() {
        let mut v = vec![r(0, 0, 10, 10), r(5, 5, 10, 10), r(100, 100, 5, 5), r(15, 0, 5, 5), r(0, 0, 0, 7)];
        merge_rects(&mut v);
        assert_eq!(v, vec![r(0, 0, 20, 15), r(100, 100, 5, 5)]);
    }

    #[test]
    fn merge_chain_needs_multiple_passes() {
        // a touches c only after b merged into a
        let mut v = vec![r(0, 0, 10, 10), r(50, 0, 10, 10), r(10, 0, 40, 2)];
        merge_rects(&mut v);
        assert_eq!(v, vec![r(0, 0, 60, 10)]);
    }

    #[test]
    fn merge_collapses_many() {
        let mut v: Vec<Rect> = (0..40).map(|i| r(i * 20, i * 20, 5, 5)).collect();
        merge_rects(&mut v);
        assert_eq!(v, vec![r(0, 0, 39 * 20 + 5, 39 * 20 + 5)]);
        assert!(covers_frame(&[r(0, 0, 8, 8)], 8, 8));
    }

    #[test]
    fn color_cursor() {
        // 2x1, pitch 12 (padding)
        let src = [1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0];
        let (w, h, px) = cursor_to_rgba(CursorKind::Color, 2, 1, 12, &src).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, vec![3, 2, 1, 4, 7, 6, 5, 8]);
        assert!(cursor_to_rgba(CursorKind::Color, 4, 1, 12, &src).is_none());
    }

    #[test]
    fn masked_color_cursor() {
        let src = [10, 20, 30, 0x00, 0, 0, 0, 0xFF, 1, 1, 1, 0xFF];
        let (_, _, px) = cursor_to_rgba(CursorKind::MaskedColor, 3, 1, 12, &src).unwrap();
        assert_eq!(px, vec![30, 20, 10, 255, 0, 0, 0, 0, 1, 1, 1, 255]);
    }

    #[test]
    fn monochrome_cursor() {
        // width 4, real height 1 → mask height 2, pitch 1 byte.
        // AND: 0b0011_0000 (pixels 2,3 have AND=1); XOR: 0b0101_0000 (pixels 1,3 XOR=1)
        let src = [0b0011_0000, 0b0101_0000];
        let (w, h, px) = cursor_to_rgba(CursorKind::Monochrome, 4, 2, 1, &src).unwrap();
        assert_eq!((w, h), (4, 1));
        assert_eq!(px, vec![0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 255]);
    }

    #[test]
    fn pct() {
        let mut v: Vec<u32> = (1..=100).collect();
        assert_eq!(percentile(&mut v, 50.0), 50);
        assert_eq!(percentile(&mut v, 99.0), 99);
        assert_eq!(percentile(&mut [], 50.0), 0);
    }
}
