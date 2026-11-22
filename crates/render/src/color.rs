//! YUV → RGB conversion matrices (shared by the WGSL shader and CPU reference code).

use veloxdesk_core_traits::video::{ColorRange, ColorSpace, PixelFormat};

/// 3×4 row-major affine matrix: `rgb[i] = dot(m[i], [y, u, v, 1])` with `y,u,v` in 0..=1
/// (normalized code values as sampled from `R8Unorm`/`Rg8Unorm` or `R16Unorm`/`Rg16Unorm`).
pub type YuvMatrix = [[f32; 4]; 3];

/// Luma coefficients `(Kr, Kb)` of a color space.
fn coefficients(cs: ColorSpace) -> (f32, f32) {
    match cs {
        ColorSpace::Bt709 | ColorSpace::Srgb => (0.2126, 0.0722),
        ColorSpace::Bt601 => (0.299, 0.114),
        ColorSpace::Bt2020Pq => (0.2627, 0.0593),
    }
}

/// Matrix converting normalized 8-bit Y'CbCr (as sampled) to non-linear R'G'B' in 0..=1.
pub fn yuv_to_rgb_matrix(cs: ColorSpace, range: ColorRange) -> YuvMatrix {
    yuv_to_rgb_matrix_for(PixelFormat::Nv12, cs, range)
}

/// Matrix converting normalized Y'CbCr for `format` (8-bit or 10-bit P010) to non-linear R'G'B'.
pub fn yuv_to_rgb_matrix_for(format: PixelFormat, cs: ColorSpace, range: ColorRange) -> YuvMatrix {
    let (kr, kb) = coefficients(cs);
    let kg = 1.0 - kr - kb;
    // P010 stores 10-bit samples in the top bits of 16-bit unorm (shifted left by 6):
    // limited Y = [64..=940] << 6 = [4096..=60160] / 65535; C = 512 << 6 = 32768 / 65535, span 896 << 6 = 57344.
    let (ys, yo, csc, co) = if format.is_10bit() {
        match range {
            ColorRange::Limited => (
                65535.0 / 56064.0,
                -4096.0 / 56064.0,
                65535.0 / 57344.0,
                -32768.0 / 57344.0,
            ),
            ColorRange::Full => (
                65535.0 / 65472.0,
                0.0,
                65535.0 / 65472.0,
                -32768.0 / 65472.0,
            ),
        }
    } else {
        match range {
            ColorRange::Limited => (255.0 / 219.0, -16.0 / 219.0, 255.0 / 224.0, -128.0 / 224.0),
            ColorRange::Full => (1.0, 0.0, 1.0, -128.0 / 255.0),
        }
    };
    let cr_r = 2.0 * (1.0 - kr);
    let cb_g = -2.0 * kb * (1.0 - kb) / kg;
    let cr_g = -2.0 * kr * (1.0 - kr) / kg;
    let cb_b = 2.0 * (1.0 - kb);
    [
        [ys, 0.0, cr_r * csc, yo + cr_r * co],
        [ys, cb_g * csc, cr_g * csc, yo + (cb_g + cr_g) * co],
        [ys, cb_b * csc, 0.0, yo + cb_b * co],
    ]
}

/// CPU reference conversion of one pixel (8-bit in, 8-bit out). Used by tests.
pub fn yuv_to_rgb8(m: &YuvMatrix, y: u8, u: u8, v: u8) -> [u8; 3] {
    let p = [y as f32 / 255.0, u as f32 / 255.0, v as f32 / 255.0, 1.0];
    let mut out = [0u8; 3];
    for (o, row) in out.iter_mut().zip(m.iter()) {
        let c: f32 = row.iter().zip(p.iter()).map(|(a, b)| a * b).sum();
        *o = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    out
}

/// CPU reference BT.709/601 RGB → YUV for one pixel (used by the test pattern & tests).
pub fn rgb8_to_yuv(cs: ColorSpace, range: ColorRange, r: u8, g: u8, b: u8) -> [u8; 3] {
    let (kr, kb) = coefficients(cs);
    let kg = 1.0 - kr - kb;
    let (r, g, b) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let y = kr * r + kg * g + kb * b;
    let cb = (b - y) / (2.0 * (1.0 - kb));
    let cr = (r - y) / (2.0 * (1.0 - kr));
    let (yv, cbv, crv) = match range {
        ColorRange::Limited => (16.0 + 219.0 * y, 128.0 + 224.0 * cb, 128.0 + 224.0 * cr),
        ColorRange::Full => (255.0 * y, 128.0 + 255.0 * cb, 128.0 + 255.0 * cr),
    };
    [
        yv.round().clamp(0.0, 255.0) as u8,
        cbv.round().clamp(0.0, 255.0) as u8,
        crv.round().clamp(0.0, 255.0) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_black_white() {
        let m = yuv_to_rgb_matrix(ColorSpace::Bt709, ColorRange::Limited);
        assert_eq!(yuv_to_rgb8(&m, 16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb8(&m, 235, 128, 128), [255, 255, 255]);
    }

    #[test]
    fn full_gray() {
        let m = yuv_to_rgb_matrix(ColorSpace::Bt709, ColorRange::Full);
        assert_eq!(yuv_to_rgb8(&m, 128, 128, 128), [128, 128, 128]);
    }

    #[test]
    fn roundtrip_primaries() {
        for cs in [ColorSpace::Bt709, ColorSpace::Bt601] {
            for range in [ColorRange::Limited, ColorRange::Full] {
                let m = yuv_to_rgb_matrix(cs, range);
                for rgb in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [200, 120, 40], [10, 20, 30]] {
                    let [y, u, v] = rgb8_to_yuv(cs, range, rgb[0], rgb[1], rgb[2]);
                    let back = yuv_to_rgb8(&m, y, u, v);
                    for i in 0..3 {
                        assert!(
                            (back[i] as i32 - rgb[i] as i32).abs() <= 3,
                            "{cs:?} {range:?} {rgb:?} -> {back:?}"
                        );
                    }
                }
            }
        }
    }
}
