//! Synthetic NV12 test pattern (`veloxdesk --test-pattern`, render tests, benches).

use bytes::Bytes;
use veloxdesk_core_traits::Timestamp;
use veloxdesk_core_traits::video::{ColorRange, ColorSpace, Frame, FrameData, PixelFormat, Plane};

use crate::color::rgb8_to_yuv;

const BARS: [[u8; 3]; 8] = [
    [235, 235, 235],
    [235, 235, 16],
    [16, 235, 235],
    [16, 235, 16],
    [235, 16, 235],
    [235, 16, 16],
    [16, 16, 235],
    [16, 16, 16],
];

/// Build one NV12 (BT.709 limited) frame: 75% color bars plus a moving white block and a
/// horizontal gray ramp, offset by `phase` pixels so successive frames visibly animate.
pub fn nv12_test_frame(width: u32, height: u32, phase: u32) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let y_stride = w.next_multiple_of(64);
    let uv_stride = (cw * 2).next_multiple_of(64);
    let mut y_plane = vec![16u8; y_stride * h];
    let mut uv_plane = vec![128u8; uv_stride * ch];
    let cs = ColorSpace::Bt709;
    let range = ColorRange::Limited;
    let bar_yuv: Vec<[u8; 3]> = BARS.iter().map(|c| rgb8_to_yuv(cs, range, c[0], c[1], c[2])).collect();
    let bars_h = h * 2 / 3;
    let block = (h / 6).max(8);
    let bx = (phase as usize * 8) % w.max(1);
    let by = h / 2 - block / 2;

    for yy in 0..h {
        let row = &mut y_plane[yy * y_stride..yy * y_stride + w];
        for (xx, px) in row.iter_mut().enumerate() {
            *px = if yy < bars_h {
                bar_yuv[xx * 8 / w.max(1)][0]
            } else {
                (16 + xx * 219 / w.max(1)) as u8
            };
            let in_block = xx >= bx && xx < bx + block && yy >= by && yy < by + block;
            if in_block {
                *px = 235;
            }
        }
    }
    for cy in 0..ch {
        let row = &mut uv_plane[cy * uv_stride..cy * uv_stride + cw * 2];
        for cx in 0..cw {
            let (xx, yy) = (cx * 2, cy * 2);
            let in_block = xx + 1 >= bx && xx < bx + block && yy + 1 >= by && yy < by + block;
            let (u, v) = if yy < bars_h && !in_block {
                let c = bar_yuv[xx * 8 / w.max(1)];
                (c[1], c[2])
            } else {
                (128, 128)
            };
            row[cx * 2] = u;
            row[cx * 2 + 1] = v;
        }
    }
    Frame {
        width,
        height,
        format: PixelFormat::Nv12,
        color_space: cs,
        color_range: range,
        data: FrameData::Cpu(vec![
            Plane { data: Bytes::from(y_plane), stride: y_stride },
            Plane { data: Bytes::from(uv_plane), stride: uv_stride },
        ]),
        capture_ts: Timestamp::now(),
    }
}

/// Solid-color NV12 frame (tests).
pub fn nv12_solid(width: u32, height: u32, y: u8, u: u8, v: u8, range: ColorRange) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut uv = vec![0u8; cw * 2 * ch];
    for p in uv.chunks_exact_mut(2) {
        p[0] = u;
        p[1] = v;
    }
    Frame {
        width,
        height,
        format: PixelFormat::Nv12,
        color_space: ColorSpace::Bt709,
        color_range: range,
        data: FrameData::Cpu(vec![
            Plane { data: Bytes::from(vec![y; w * h]), stride: w },
            Plane { data: Bytes::from(uv), stride: cw * 2 },
        ]),
        capture_ts: Timestamp::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_shapes() {
        let f = nv12_test_frame(1921, 1081, 3);
        let FrameData::Cpu(planes) = &f.data else { panic!() };
        assert_eq!(planes.len(), 2);
        assert!(planes[0].data.len() >= planes[0].stride * 1081);
        assert!(planes[1].data.len() >= planes[1].stride * 541);
        assert!(planes[1].stride >= 961 * 2);
    }
}
