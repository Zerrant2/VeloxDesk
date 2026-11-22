// VeloxDesk video quad: NV12 / I420 / RGBA(BGRA) → RGB with a 3x4 YUV matrix.
// One pipeline variant draws the opaque video, another blends the local cursor.

struct Params {
    // NDC rectangle: left, top, right, bottom
    rect: vec4<f32>,
    // texture coordinates: u0, v0, u1, v1
    uv_rect: vec4<f32>,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // x: mode (0 = RGB opaque, 1 = NV12, 2 = I420, 3 = RGBA straight alpha)
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

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let mode = p.mode.x;
    let c0 = textureSampleLevel(t0, s, in.uv, 0.0);
    if (mode == 0u) {
        return vec4<f32>(c0.rgb, 1.0);
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
    let rgb = vec3<f32>(dot(p.m0, yuv), dot(p.m1, yuv), dot(p.m2, yuv));
    return vec4<f32>(clamp(rgb, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
