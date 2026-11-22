// VeloxDesk video quad: NV12 / P010 (HDR10 PQ→SDR) / I420 / YUV444 / RGBA(BGRA) → RGB.
// One pipeline variant draws the opaque video, another blends the local cursor.

struct Params {
    // NDC rectangle: left, top, right, bottom
    rect: vec4<f32>,
    // texture coordinates: u0, v0, u1, v1
    uv_rect: vec4<f32>,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // x: mode (0 = RGB opaque, 1 = 2-plane Y+UV [NV12/P010], 2 = 3-plane Y+U+V [I420/YUV444], 3 = RGBA straight alpha)
    // y: transfer (0 = SDR passthrough, 1 = HDR10 ST.2084 PQ BT.2020 → tone-mapped SDR sRGB)
    mode: vec4<u32>,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var t0: texture_2d<f32>;
@group(0) @binding(2) var t1: texture_2d<f32>;
@group(0) @binding(3) var t2: texture_2d<f32>;
@group(0) @binding(4) var s: sampler;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> VOut {
    // triangle strip: 0 (0,0) 1 (1,0) 2 (0,1) 3 (1,1)
    let fx = f32(vi & 1u);
    let fy = f32((vi >> 1u) & 1u);
    var out: VOut;
    out.pos = vec4<f32>(mix(p.rect.x, p.rect.z, fx), mix(p.rect.y, p.rect.w, fy), 0.0, 1.0);
    out.uv = vec2<f32>(mix(p.uv_rect.x, p.uv_rect.z, fx), mix(p.uv_rect.y, p.uv_rect.w, fy));
    return out;
}

fn pq_to_linear_nits(v: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let vp = pow(max(v, vec3<f32>(0.0)), vec3<f32>(1.0 / m2));
    let num = max(vp - vec3<f32>(c1), vec3<f32>(0.0));
    let den = max(vec3<f32>(c2) - vec3<f32>(c3) * vp, vec3<f32>(1e-6));
    return pow(num / den, vec3<f32>(1.0 / m1)) * 10000.0;
}

fn linear_to_srgb_ch(x: f32) -> f32 {
    if (x <= 0.0031308) {
        return 12.92 * x;
    }
    return 1.055 * pow(x, 1.0 / 2.4) - 0.055;
}

fn hdr10_pq_to_sdr(pq_rgb: vec3<f32>) -> vec3<f32> {
    let lin2020 = pq_to_linear_nits(clamp(pq_rgb, vec3<f32>(0.0), vec3<f32>(1.0))) / 203.0;
    // BT.2020 linear → BT.709 linear
    let r709 = 1.6605 * lin2020.r - 0.5876 * lin2020.g - 0.0728 * lin2020.b;
    let g709 = -0.1246 * lin2020.r + 1.1329 * lin2020.g - 0.0083 * lin2020.b;
    let b709 = -0.0182 * lin2020.r - 0.1006 * lin2020.g + 1.1187 * lin2020.b;
    let lin = max(vec3<f32>(r709, g709, b709), vec3<f32>(0.0));
    // Smooth extended Reinhard tone-mapping (peak ~5x SDR white = ~1000 nits).
    let white2 = 25.0;
    let mapped = (lin * (vec3<f32>(1.0) + lin / white2)) / (vec3<f32>(1.0) + lin);
    let clamped = clamp(mapped, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec3<f32>(
        linear_to_srgb_ch(clamped.r),
        linear_to_srgb_ch(clamped.g),
        linear_to_srgb_ch(clamped.b),
    );
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let mode = p.mode.x;
    let transfer = p.mode.y;
    let c0 = textureSampleLevel(t0, s, in.uv, 0.0);
    if (mode == 0u) {
        let rgb = select(c0.rgb, hdr10_pq_to_sdr(c0.rgb), transfer == 1u);
        return vec4<f32>(rgb, 1.0);
    }
    if (mode == 3u) {
        return c0;
    }
    var yuv = vec4<f32>(c0.r, 0.5, 0.5, 1.0);
    if (mode == 1u) {
        let c = textureSampleLevel(t1, s, in.uv, 0.0);
        yuv = vec4<f32>(c0.r, c.r, c.g, 1.0);
    } else {
        let u = textureSampleLevel(t1, s, in.uv, 0.0).r;
        let v = textureSampleLevel(t2, s, in.uv, 0.0).r;
        yuv = vec4<f32>(c0.r, u, v, 1.0);
    }
    var rgb = vec3<f32>(dot(p.m0, yuv), dot(p.m1, yuv), dot(p.m2, yuv));
    if (transfer == 1u) {
        rgb = hdr10_pq_to_sdr(rgb);
    }
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
