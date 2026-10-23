// World height draw pass: land above 0 m, sea at or below, darker with depth and lighter with
// height. Full shading is #25. The renderer prepends VIEW_WGSL.
@group(0) @binding(0) var<uniform> u: ViewUniform;
@group(0) @binding(1) var heights: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let t = vec2i(floor(u.origin_px + (u.tile_offset + pos.xy) * u.scale));
    if any(t < u.lo) || any(t > u.hi) {
        return vec4f(0.0);
    }
    let h = textureLoad(heights, t - u.tex_origin, 0).r;
    if h > 0.0 {
        let k = clamp(h / 3000.0, 0.0, 1.0);
        return vec4f(mix(vec3f(0.45, 0.62, 0.35), vec3f(0.80, 0.75, 0.62), k), 1.0);
    }
    let k = clamp(-h / 4000.0, 0.0, 1.0);
    return vec4f(mix(vec3f(0.30, 0.50, 0.72), vec3f(0.08, 0.16, 0.38), k), 1.0);
}
