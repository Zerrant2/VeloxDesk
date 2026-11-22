//! CPU colour conversion helpers (fallback path, tests, SW codec glue).
//!
//! All routines are scalar, branch-free in the inner loop and work on contiguous row slices
//! so LLVM auto-vectorises them (SSE2/AVX2 with `target-cpu=native`). Fixed-point Q16
//! arithmetic; chroma is computed from the 2×2 RGB average (correct 4:2:0 siting for
//! MPEG-2/H.264 "left" chroma is a half-pixel shift we ignore — invisible at desktop sizes).
//!
//! Odd widths/heights are supported: the chroma plane is `ceil(w/2) × ceil(h/2)` and the
//! last column/row is replicated.

use veloxdesk_core_traits::video::{ColorRange, ColorSpace};

/// Fixed-point (Q16) RGB↔YCbCr matrix for one colour space + range.
#[derive(Debug, Clone, Copy)]
pub struct YuvMatrix {
    // forward
    yr: i32,
    yg: i32,
    yb: i32,
    ur: i32,
    ug: i32,
    ub: i32,
    vr: i32,
    vg: i32,
    vb: i32,
    y_off: i32,
    // inverse
    iy: i32,
    rv: i32,
    gu: i32,
    gv: i32,
    bu: i32,
}

const Q: i32 = 16;
const HALF: i32 = 1 << (Q - 1);

impl YuvMatrix {
    pub fn new(space: ColorSpace, range: ColorRange) -> Self {
        let (kr, kb) = match space {
            ColorSpace::Bt709 | ColorSpace::Srgb => (0.2126f64, 0.0722f64),
            ColorSpace::Bt601 => (0.299, 0.114),
            ColorSpace::Bt2020Pq => (0.2627, 0.0593),
        };
        let kg = 1.0 - kr - kb;
        let (ys, cs, yo) = match range {
            ColorRange::Limited => (219.0 / 255.0, 224.0 / 255.0, 16),
            ColorRange::Full => (1.0, 1.0, 0),
        };
        let q = (1 << Q) as f64;
        let f = |v: f64| (v * q).round() as i32;
        let u_den = 2.0 * (1.0 - kb);
        let v_den = 2.0 * (1.0 - kr);
        Self {
            yr: f(kr * ys),
            yg: f(kg * ys),
            yb: f(kb * ys),
            ur: f(-kr / u_den * cs),
            ug: f(-kg / u_den * cs),
            ub: f((1.0 - kb) / u_den * cs),
            vr: f((1.0 - kr) / v_den * cs),
            vg: f(-kg / v_den * cs),
            vb: f(-kb / v_den * cs),
            y_off: yo,
            iy: f(1.0 / ys),
            rv: f(v_den / cs),
            gu: f(2.0 * kb * (1.0 - kb) / kg / cs),
            gv: f(2.0 * kr * (1.0 - kr) / kg / cs),
            bu: f(u_den / cs),
        }
    }

    #[inline(always)]
    fn y(&self, r: i32, g: i32, b: i32) -> u8 {
        (((self.yr * r + self.yg * g + self.yb * b + HALF) >> Q) + self.y_off).clamp(0, 255) as u8
    }

    /// `r,g,b` are sums of 4 samples (×4).
    #[inline(always)]
    fn uv4(&self, r: i32, g: i32, b: i32) -> (u8, u8) {
        let u = ((self.ur * r + self.ug * g + self.ub * b + (HALF << 2)) >> (Q + 2)) + 128;
        let v = ((self.vr * r + self.vg * g + self.vb * b + (HALF << 2)) >> (Q + 2)) + 128;
        (u.clamp(0, 255) as u8, v.clamp(0, 255) as u8)
    }

    #[inline(always)]
    fn rgb(&self, y: u8, u: u8, v: u8) -> (u8, u8, u8) {
        let c = self.iy * (y as i32 - self.y_off);
        let d = u as i32 - 128;
        let e = v as i32 - 128;
        let r = (c + self.rv * e + HALF) >> Q;
        let g = (c - self.gu * d - self.gv * e + HALF) >> Q;
        let b = (c + self.bu * d + HALF) >> Q;
        (r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8)
    }
}

/// Size of NV12 / I420 chroma planes (in samples per row, rows).
#[inline]
pub fn chroma_dims(width: u32, height: u32) -> (usize, usize) {
    (width.div_ceil(2) as usize, height.div_ceil(2) as usize)
}

/// Destination planes of a 4:2:0 conversion.
pub enum Yuv420Out<'a> {
    /// `(y, y_stride, uv, uv_stride)`, UV interleaved.
    Nv12(&'a mut [u8], usize, &'a mut [u8], usize),
    /// `(y, y_stride, u, u_stride, v, v_stride)`.
    I420(&'a mut [u8], usize, &'a mut [u8], usize, &'a mut [u8], usize),
}

/// BGRA8 → NV12. `src_stride` in bytes. Panics if a buffer is too small.
#[allow(clippy::too_many_arguments)]
pub fn bgra_to_nv12(
    src: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
    dst_y: &mut [u8],
    y_stride: usize,
    dst_uv: &mut [u8],
    uv_stride: usize,
    m: &YuvMatrix,
) {
    bgra_to_yuv420(src, src_stride, width, height, Yuv420Out::Nv12(dst_y, y_stride, dst_uv, uv_stride), m);
}

/// BGRA8 → I420 (planar).
#[allow(clippy::too_many_arguments)]
pub fn bgra_to_i420(
    src: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
    dst_y: &mut [u8],
    y_stride: usize,
    dst_u: &mut [u8],
    u_stride: usize,
    dst_v: &mut [u8],
    v_stride: usize,
    m: &YuvMatrix,
) {
    bgra_to_yuv420(
        src,
        src_stride,
        width,
        height,
        Yuv420Out::I420(dst_y, y_stride, dst_u, u_stride, dst_v, v_stride),
        m,
    );
}

/// Generic BGRA8 → 4:2:0 conversion.
pub fn bgra_to_yuv420(src: &[u8], src_stride: usize, width: u32, height: u32, out: Yuv420Out<'_>, m: &YuvMatrix) {
    let w = width as usize;
    let h = height as usize;
    let (cw, ch) = chroma_dims(width, height);
    assert!(src_stride >= w * 4 && src.len() >= src_stride * (h.max(1) - 1) + w * 4, "src too small");
    let (dst_y, y_stride) = match &out {
        Yuv420Out::Nv12(y, ys, ..) | Yuv420Out::I420(y, ys, ..) => (y.len(), *ys),
    };
    assert!(y_stride >= w && dst_y >= y_stride * (h.max(1) - 1) + w, "y plane too small");
    let (y_plane, mut cout) = match out {
        Yuv420Out::Nv12(y, _, uv, uvs) => {
            assert!(uvs >= cw * 2 && uv.len() >= uvs * (ch.max(1) - 1) + cw * 2, "uv plane too small");
            (y, ChromaOut::Nv12(uv, uvs))
        }
        Yuv420Out::I420(y, _, u, us, v, vs) => {
            assert!(us >= cw && u.len() >= us * (ch.max(1) - 1) + cw, "u plane too small");
            assert!(vs >= cw && v.len() >= vs * (ch.max(1) - 1) + cw, "v plane too small");
            (y, ChromaOut::I420(u, us, v, vs))
        }
    };

    for cy in 0..ch {
        let r0 = cy * 2;
        let r1 = (r0 + 1).min(h - 1);
        let s0 = &src[r0 * src_stride..r0 * src_stride + w * 4];
        let s1 = &src[r1 * src_stride..r1 * src_stride + w * 4];
        // Luma rows.
        {
            let y0 = &mut y_plane[r0 * y_stride..r0 * y_stride + w];
            for (d, p) in y0.iter_mut().zip(s0.chunks_exact(4)) {
                *d = m.y(p[2] as i32, p[1] as i32, p[0] as i32);
            }
        }
        if r1 != r0 {
            let y1 = &mut y_plane[r1 * y_stride..r1 * y_stride + w];
            for (d, p) in y1.iter_mut().zip(s1.chunks_exact(4)) {
                *d = m.y(p[2] as i32, p[1] as i32, p[0] as i32);
            }
        }
        // Chroma row.
        let chroma = |cx: usize| {
            let x0 = cx * 2;
            let x1 = (x0 + 1).min(w - 1);
            let (a, b, c, d) = (&s0[x0 * 4..], &s0[x1 * 4..], &s1[x0 * 4..], &s1[x1 * 4..]);
            let bs = a[0] as i32 + b[0] as i32 + c[0] as i32 + d[0] as i32;
            let gs = a[1] as i32 + b[1] as i32 + c[1] as i32 + d[1] as i32;
            let rs = a[2] as i32 + b[2] as i32 + c[2] as i32 + d[2] as i32;
            m.uv4(rs, gs, bs)
        };
        match &mut cout {
            ChromaOut::Nv12(uv, uvs) => {
                let row = &mut uv[cy * *uvs..cy * *uvs + cw * 2];
                for (cx, o) in row.chunks_exact_mut(2).enumerate() {
                    let (u, v) = chroma(cx);
                    o[0] = u;
                    o[1] = v;
                }
            }
            ChromaOut::I420(u, us, v, vs) => {
                let urow = &mut u[cy * *us..cy * *us + cw];
                let vrow = &mut v[cy * *vs..cy * *vs + cw];
                for (cx, (uo, vo)) in urow.iter_mut().zip(vrow.iter_mut()).enumerate() {
                    let (cu, cv) = chroma(cx);
                    *uo = cu;
                    *vo = cv;
                }
            }
        }
    }
}

enum ChromaOut<'a> {
    Nv12(&'a mut [u8], usize),
    I420(&'a mut [u8], usize, &'a mut [u8], usize),
}

/// Output byte order for YUV → RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbOrder {
    Rgba,
    Bgra,
}

/// NV12 → RGBA8 (alpha = 255).
#[allow(clippy::too_many_arguments)]
pub fn nv12_to_rgba(
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
    m: &YuvMatrix,
) {
    yuv420_to_rgb(y, y_stride, ChromaIn::Nv12(uv, uv_stride), width, height, dst, dst_stride, RgbOrder::Rgba, m);
}

/// NV12 → BGRA8 / RGBA8 with selectable order.
#[allow(clippy::too_many_arguments)]
pub fn nv12_to_rgb(
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
    order: RgbOrder,
    m: &YuvMatrix,
) {
    yuv420_to_rgb(y, y_stride, ChromaIn::Nv12(uv, uv_stride), width, height, dst, dst_stride, order, m);
}

/// I420 → RGBA8 (alpha = 255).
#[allow(clippy::too_many_arguments)]
pub fn i420_to_rgba(
    y: &[u8],
    y_stride: usize,
    u: &[u8],
    u_stride: usize,
    v: &[u8],
    v_stride: usize,
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
    m: &YuvMatrix,
) {
    yuv420_to_rgb(
        y,
        y_stride,
        ChromaIn::I420(u, u_stride, v, v_stride),
        width,
        height,
        dst,
        dst_stride,
        RgbOrder::Rgba,
        m,
    );
}

enum ChromaIn<'a> {
    Nv12(&'a [u8], usize),
    I420(&'a [u8], usize, &'a [u8], usize),
}

#[allow(clippy::too_many_arguments)]
fn yuv420_to_rgb(
    y: &[u8],
    y_stride: usize,
    c: ChromaIn<'_>,
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
    order: RgbOrder,
    m: &YuvMatrix,
) {
    let w = width as usize;
    let h = height as usize;
    assert!(dst_stride >= w * 4 && dst.len() >= dst_stride * (h.max(1) - 1) + w * 4, "dst too small");
    assert!(y.len() >= y_stride * (h.max(1) - 1) + w, "y too small");
    let (ri, bi) = match order {
        RgbOrder::Rgba => (0, 2),
        RgbOrder::Bgra => (2, 0),
    };
    for row in 0..h {
        let yr = &y[row * y_stride..row * y_stride + w];
        let d = &mut dst[row * dst_stride..row * dst_stride + w * 4];
        let cy = row / 2;
        match c {
            ChromaIn::Nv12(uv, uvs) => {
                let uvr = &uv[cy * uvs..];
                for (x, (o, &yy)) in d.chunks_exact_mut(4).zip(yr.iter()).enumerate() {
                    let ci = (x / 2) * 2;
                    let (r, g, b) = m.rgb(yy, uvr[ci], uvr[ci + 1]);
                    o[ri] = r;
                    o[1] = g;
                    o[bi] = b;
                    o[3] = 255;
                }
            }
            ChromaIn::I420(u, us, v, vs) => {
                let ur = &u[cy * us..];
                let vr = &v[cy * vs..];
                for (x, (o, &yy)) in d.chunks_exact_mut(4).zip(yr.iter()).enumerate() {
                    let (r, g, b) = m.rgb(yy, ur[x / 2], vr[x / 2]);
                    o[ri] = r;
                    o[1] = g;
                    o[bi] = b;
                    o[3] = 255;
                }
            }
        }
    }
}

/// Interleave planar U/V into an NV12 UV plane.
#[allow(clippy::too_many_arguments)]
pub fn i420_chroma_to_nv12(
    u: &[u8],
    u_stride: usize,
    v: &[u8],
    v_stride: usize,
    cw: usize,
    ch: usize,
    dst_uv: &mut [u8],
    uv_stride: usize,
) {
    for row in 0..ch {
        let ur = &u[row * u_stride..row * u_stride + cw];
        let vr = &v[row * v_stride..row * v_stride + cw];
        let d = &mut dst_uv[row * uv_stride..row * uv_stride + cw * 2];
        for ((o, &a), &b) in d.chunks_exact_mut(2).zip(ur).zip(vr) {
            o[0] = a;
            o[1] = b;
        }
    }
}

/// De-interleave an NV12 UV plane into planar U/V.
#[allow(clippy::too_many_arguments)]
pub fn nv12_chroma_to_i420(
    uv: &[u8],
    uv_stride: usize,
    cw: usize,
    ch: usize,
    dst_u: &mut [u8],
    u_stride: usize,
    dst_v: &mut [u8],
    v_stride: usize,
) {
    for row in 0..ch {
        let s = &uv[row * uv_stride..row * uv_stride + cw * 2];
        let ur = &mut dst_u[row * u_stride..row * u_stride + cw];
        let vr = &mut dst_v[row * v_stride..row * v_stride + cw];
        for ((p, a), b) in s.chunks_exact(2).zip(ur.iter_mut()).zip(vr.iter_mut()) {
            *a = p[0];
            *b = p[1];
        }
    }
}

/// BGRA8 → planar YUV 4:4:4 (8-bit, full chroma resolution for sharp desktop text).
#[allow(clippy::too_many_arguments)]
pub fn bgra_to_yuv444(
    src: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
    dst_y: &mut [u8],
    y_stride: usize,
    dst_u: &mut [u8],
    u_stride: usize,
    dst_v: &mut [u8],
    v_stride: usize,
    m: &YuvMatrix,
) {
    let w = width as usize;
    let h = height as usize;
    assert!(src_stride >= w * 4 && src.len() >= src_stride * (h.max(1) - 1) + w * 4, "src too small");
    assert!(y_stride >= w && dst_y.len() >= y_stride * (h.max(1) - 1) + w, "y too small");
    assert!(u_stride >= w && dst_u.len() >= u_stride * (h.max(1) - 1) + w, "u too small");
    assert!(v_stride >= w && dst_v.len() >= v_stride * (h.max(1) - 1) + w, "v too small");
    for row in 0..h {
        let s = &src[row * src_stride..row * src_stride + w * 4];
        let yr = &mut dst_y[row * y_stride..row * y_stride + w];
        let ur = &mut dst_u[row * u_stride..row * u_stride + w];
        let vr = &mut dst_v[row * v_stride..row * v_stride + w];
        for (((p, y), u), v) in s.chunks_exact(4).zip(yr.iter_mut()).zip(ur.iter_mut()).zip(vr.iter_mut()) {
            let (r, g, b) = (p[2] as i32, p[1] as i32, p[0] as i32);
            *y = m.y(r, g, b);
            let (cu, cv) = m.uv4(r * 4, g * 4, b * 4);
            *u = cu;
            *v = cv;
        }
    }
}

/// Planar YUV 4:4:4 (8-bit) → RGBA8 (alpha = 255).
#[allow(clippy::too_many_arguments)]
pub fn yuv444_to_rgba(
    y: &[u8],
    y_stride: usize,
    u: &[u8],
    u_stride: usize,
    v: &[u8],
    v_stride: usize,
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
    m: &YuvMatrix,
) {
    let w = width as usize;
    let h = height as usize;
    assert!(dst_stride >= w * 4 && dst.len() >= dst_stride * (h.max(1) - 1) + w * 4, "dst too small");
    for row in 0..h {
        let yr = &y[row * y_stride..row * y_stride + w];
        let ur = &u[row * u_stride..row * u_stride + w];
        let vr = &v[row * v_stride..row * v_stride + w];
        let d = &mut dst[row * dst_stride..row * dst_stride + w * 4];
        for (((o, &yy), &uu), &vv) in d.chunks_exact_mut(4).zip(yr).zip(ur).zip(vr) {
            let (r, g, b) = m.rgb(yy, uu, vv);
            o[0] = r;
            o[1] = g;
            o[2] = b;
            o[3] = 255;
        }
    }
}

/// BGRA8 → 10-bit P010 (4:2:0, MSB-aligned 10-bit samples in little-endian `u16`, matching `DXGI_FORMAT_P010`).
/// `y_stride` and `uv_stride` are in **bytes** (must be >= `w * 2` and `cw * 4`).
#[allow(clippy::too_many_arguments)]
pub fn bgra_to_p010(
    src: &[u8],
    src_stride: usize,
    width: u32,
    height: u32,
    dst_y: &mut [u8],
    y_stride: usize,
    dst_uv: &mut [u8],
    uv_stride: usize,
    m: &YuvMatrix,
) {
    let w = width as usize;
    let h = height as usize;
    let (cw, ch) = chroma_dims(width, height);
    assert!(src_stride >= w * 4 && src.len() >= src_stride * (h.max(1) - 1) + w * 4, "src too small");
    assert!(y_stride >= w * 2 && dst_y.len() >= y_stride * (h.max(1) - 1) + w * 2, "p010 y too small");
    assert!(uv_stride >= cw * 4 && dst_uv.len() >= uv_stride * (ch.max(1) - 1) + cw * 4, "p010 uv too small");
    // Promote 8-bit sample to MSB-aligned 16-bit P010 container (`v << 8`).
    for cy in 0..ch {
        let r0 = cy * 2;
        let r1 = (r0 + 1).min(h - 1);
        let s0 = &src[r0 * src_stride..r0 * src_stride + w * 4];
        let s1 = &src[r1 * src_stride..r1 * src_stride + w * 4];
        for (d, p) in dst_y[r0 * y_stride..r0 * y_stride + w * 2].chunks_exact_mut(2).zip(s0.chunks_exact(4)) {
            let v16 = (m.y(p[2] as i32, p[1] as i32, p[0] as i32) as u16) << 8;
            d.copy_from_slice(&v16.to_le_bytes());
        }
        if r1 != r0 {
            for (d, p) in dst_y[r1 * y_stride..r1 * y_stride + w * 2].chunks_exact_mut(2).zip(s1.chunks_exact(4)) {
                let v16 = (m.y(p[2] as i32, p[1] as i32, p[0] as i32) as u16) << 8;
                d.copy_from_slice(&v16.to_le_bytes());
            }
        }
        let uv_row = &mut dst_uv[cy * uv_stride..cy * uv_stride + cw * 4];
        for (cx, o) in uv_row.chunks_exact_mut(4).enumerate() {
            let x0 = cx * 2;
            let x1 = (x0 + 1).min(w - 1);
            let (a, b, c, d) = (&s0[x0 * 4..], &s0[x1 * 4..], &s1[x0 * 4..], &s1[x1 * 4..]);
            let bs = a[0] as i32 + b[0] as i32 + c[0] as i32 + d[0] as i32;
            let gs = a[1] as i32 + b[1] as i32 + c[1] as i32 + d[1] as i32;
            let rs = a[2] as i32 + b[2] as i32 + c[2] as i32 + d[2] as i32;
            let (u8v, v8v) = m.uv4(rs, gs, bs);
            o[0..2].copy_from_slice(&((u8v as u16) << 8).to_le_bytes());
            o[2..4].copy_from_slice(&((v8v as u16) << 8).to_le_bytes());
        }
    }
}

/// 10-bit P010 → RGBA8 (alpha = 255). `y_stride` and `uv_stride` are in bytes.
#[allow(clippy::too_many_arguments)]
pub fn p010_to_rgba(
    y: &[u8],
    y_stride: usize,
    uv: &[u8],
    uv_stride: usize,
    width: u32,
    height: u32,
    dst: &mut [u8],
    dst_stride: usize,
    m: &YuvMatrix,
) {
    let w = width as usize;
    let h = height as usize;
    assert!(dst_stride >= w * 4 && dst.len() >= dst_stride * (h.max(1) - 1) + w * 4, "dst too small");
    for row in 0..h {
        let yr = &y[row * y_stride..row * y_stride + w * 2];
        let uvr = &uv[(row / 2) * uv_stride..];
        let d = &mut dst[row * dst_stride..row * dst_stride + w * 4];
        for (x, (o, yp)) in d.chunks_exact_mut(4).zip(yr.chunks_exact(2)).enumerate() {
            let yy = (u16::from_le_bytes([yp[0], yp[1]]) >> 8) as u8;
            let ci = (x / 2) * 4;
            let uu = (u16::from_le_bytes([uvr[ci], uvr[ci + 1]]) >> 8) as u8;
            let vv = (u16::from_le_bytes([uvr[ci + 2], uvr[ci + 3]]) >> 8) as u8;
            let (r, g, b) = m.rgb(yy, uu, vv);
            o[0] = r;
            o[1] = g;
            o[2] = b;
            o[3] = 255;
        }
    }
}

/// Copy `rows` rows of `row_bytes` between buffers with different strides.
pub fn copy_plane(src: &[u8], src_stride: usize, dst: &mut [u8], dst_stride: usize, row_bytes: usize, rows: usize) {
    if src_stride == row_bytes && dst_stride == row_bytes {
        let n = row_bytes * rows;
        dst[..n].copy_from_slice(&src[..n]);
        return;
    }
    for r in 0..rows {
        dst[r * dst_stride..r * dst_stride + row_bytes].copy_from_slice(&src[r * src_stride..r * src_stride + row_bytes]);
    }
}

/// Peak signal-to-noise ratio between two equally sized 8-bit planes (dB, capped at 99).
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    if a.is_empty() {
        return 99.0;
    }
    let mse: f64 = a.iter().zip(b).map(|(&x, &y)| ((x as i32 - y as i32) * (x as i32 - y as i32)) as f64).sum::<f64>()
        / a.len() as f64;
    if mse == 0.0 { 99.0 } else { (10.0 * (255.0f64 * 255.0 / mse).log10()).min(99.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let p = &mut v[(y * w + x) * 4..];
                p[0] = (x * 255 / w.max(1)) as u8; // B
                p[1] = (y * 255 / h.max(1)) as u8; // G
                p[2] = ((x + y) * 127 / (w + h).max(1)) as u8 + 64; // R
                p[3] = 255;
            }
        }
        v
    }

    #[test]
    fn known_values_bt709_limited() {
        let m = YuvMatrix::new(ColorSpace::Bt709, ColorRange::Limited);
        // white / black / pure red (BT.709 limited: Y=63, U=102, V=240)
        assert_eq!(m.y(255, 255, 255), 235);
        assert_eq!(m.y(0, 0, 0), 16);
        assert_eq!(m.uv4(0, 0, 0), (128, 128));
        assert_eq!(m.y(255, 0, 0), 63);
        let (u, v) = m.uv4(255 * 4, 0, 0);
        assert!((u as i32 - 102).abs() <= 1 && (v as i32 - 240).abs() <= 1, "{u} {v}");
        assert_eq!(m.rgb(235, 128, 128), (255, 255, 255));
        assert_eq!(m.rgb(16, 128, 128), (0, 0, 0));
    }

    #[test]
    fn roundtrip_nv12_and_i420() {
        for &(w, h) in &[(64usize, 48usize), (33, 17), (1, 1), (2, 3)] {
            for range in [ColorRange::Limited, ColorRange::Full] {
                let m = YuvMatrix::new(ColorSpace::Bt709, range);
                let src = gradient(w, h);
                let (cw, ch) = chroma_dims(w as u32, h as u32);
                let mut y = vec![0u8; w * h];
                let mut uv = vec![0u8; cw * 2 * ch];
                bgra_to_nv12(&src, w * 4, w as u32, h as u32, &mut y, w, &mut uv, cw * 2, &m);
                let mut rgba = vec![0u8; w * h * 4];
                nv12_to_rgba(&y, w, &uv, cw * 2, w as u32, h as u32, &mut rgba, w * 4, &m);
                // compare as BGRA
                let mut back = vec![0u8; w * h * 4];
                for (o, p) in back.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
                    o.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
                }
                let p = psnr(&src, &back);
                if w >= 16 || (w == 1 && h == 1) {
                    assert!(p > 33.0, "{w}x{h} {range:?} psnr {p}");
                } else {
                    // Tiny 2x3 full-span gradient has ~127-level step within a single 2x2 chroma block.
                    assert!(p > 17.0, "{w}x{h} {range:?} psnr {p}");
                    let solid: Vec<u8> = [40u8, 120, 200, 255].repeat(w * h);
                    let mut sy = vec![0u8; w * h];
                    let mut suv = vec![0u8; cw * 2 * ch];
                    bgra_to_nv12(&solid, w * 4, w as u32, h as u32, &mut sy, w, &mut suv, cw * 2, &m);
                    let mut srgba = vec![0u8; w * h * 4];
                    nv12_to_rgba(&sy, w, &suv, cw * 2, w as u32, h as u32, &mut srgba, w * 4, &m);
                    let mut sback = vec![0u8; w * h * 4];
                    for (o, px) in sback.chunks_exact_mut(4).zip(srgba.chunks_exact(4)) {
                        o.copy_from_slice(&[px[2], px[1], px[0], px[3]]);
                    }
                    let sp = psnr(&solid, &sback);
                    assert!(sp > 42.0, "solid {w}x{h} {range:?} psnr {sp}");
                }

                // I420 path gives the same planes.
                let mut y2 = vec![0u8; w * h];
                let mut u2 = vec![0u8; cw * ch];
                let mut v2 = vec![0u8; cw * ch];
                bgra_to_i420(&src, w * 4, w as u32, h as u32, &mut y2, w, &mut u2, cw, &mut v2, cw, &m);
                assert_eq!(y, y2);
                let mut uv2 = vec![0u8; cw * 2 * ch];
                i420_chroma_to_nv12(&u2, cw, &v2, cw, cw, ch, &mut uv2, cw * 2);
                assert_eq!(uv, uv2);
                let (mut u3, mut v3) = (vec![0u8; cw * ch], vec![0u8; cw * ch]);
                nv12_chroma_to_i420(&uv, cw * 2, cw, ch, &mut u3, cw, &mut v3, cw);
                assert_eq!((u2, v2), (u3, v3));
            }
        }
    }

    #[test]
    fn strided_buffers() {
        let (w, h) = (10usize, 6usize);
        let src_stride = w * 4 + 12;
        let mut src = vec![0u8; src_stride * h];
        for r in 0..h {
            src[r * src_stride..r * src_stride + w * 4].copy_from_slice(&gradient(w, h)[r * w * 4..(r + 1) * w * 4]);
        }
        let m = YuvMatrix::new(ColorSpace::Bt601, ColorRange::Limited);
        let mut y = vec![0u8; 16 * h];
        let mut uv = vec![0u8; 16 * 3];
        bgra_to_nv12(&src, src_stride, w as u32, h as u32, &mut y, 16, &mut uv, 16, &m);
        let mut rgba = vec![0u8; 48 * h];
        nv12_to_rgba(&y, 16, &uv, 16, w as u32, h as u32, &mut rgba, 48, &m);
        assert_eq!(rgba[3], 255);
    }

    #[test]
    fn roundtrip_yuv444_and_p010() {
        let (w, h) = (64usize, 48usize);
        let src = gradient(w, h);
        let m = YuvMatrix::new(ColorSpace::Bt709, ColorRange::Limited);

        // YUV 4:4:4 roundtrip (higher PSNR than 4:2:0 because chroma is unsubsampled).
        let (mut y, mut u, mut v) = (vec![0u8; w * h], vec![0u8; w * h], vec![0u8; w * h]);
        bgra_to_yuv444(&src, w * 4, w as u32, h as u32, &mut y, w, &mut u, w, &mut v, w, &m);
        let mut rgba = vec![0u8; w * h * 4];
        yuv444_to_rgba(&y, w, &u, w, &v, w, w as u32, h as u32, &mut rgba, w * 4, &m);
        let mut back = vec![0u8; w * h * 4];
        for (o, p) in back.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            o.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        assert!(psnr(&src, &back) > 42.0);

        // 10-bit P010 roundtrip.
        let (cw, ch) = chroma_dims(w as u32, h as u32);
        let mut py = vec![0u8; w * 2 * h];
        let mut puv = vec![0u8; cw * 4 * ch];
        bgra_to_p010(&src, w * 4, w as u32, h as u32, &mut py, w * 2, &mut puv, cw * 4, &m);
        let mut prgba = vec![0u8; w * h * 4];
        p010_to_rgba(&py, w * 2, &puv, cw * 4, w as u32, h as u32, &mut prgba, w * 4, &m);
        for (o, p) in back.chunks_exact_mut(4).zip(prgba.chunks_exact(4)) {
            o.copy_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
        assert!(psnr(&src, &back) > 35.0);
    }
}
