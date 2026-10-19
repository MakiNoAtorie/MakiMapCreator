// Height draw pass: hypsometric colours with hillshade. The renderer prepends VIEW_WGSL.
@group(0) @binding(0) var<uniform> u: ViewUniform;
@group(0) @binding(1) var heights: texture_2d<f32>;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4f {
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4f(p * 2.0 - 1.0, 0.0, 1.0);
}

fn height(t: vec2i) -> f32 {
    return textureLoad(heights, clamp(t, u.lo, u.hi) - u.tex_origin, 0).r;
}

// (height in metres, colour); linear between stops.
const STOPS = array<vec4f, 9>(
    vec4f(-4000.0, 0.05, 0.10, 0.35),
    vec4f(-200.0, 0.15, 0.35, 0.65),
    // Sea level (0) is water.
    vec4f(0.0, 0.35, 0.60, 0.80),
    vec4f(0.001, 0.30, 0.55, 0.25),
    vec4f(300.0, 0.55, 0.70, 0.35),
    vec4f(1000.0, 0.75, 0.65, 0.40),
    vec4f(2500.0, 0.55, 0.45, 0.35),
    vec4f(4000.0, 0.95, 0.95, 0.95),
    vec4f(100000.0, 0.95, 0.95, 0.95),
);

fn ramp(h: f32) -> vec3f {
    if h <= STOPS[0].x {
        return STOPS[0].yzw;
    }
    for (var i = 1; i < 9; i++) {
        if h <= STOPS[i].x {
            let a = STOPS[i - 1];
            let b = STOPS[i];
            return mix(a.yzw, b.yzw, (h - a.x) / (b.x - a.x));
        }
    }
    return STOPS[8].yzw;
}

@fragment
fn fs(@builtin(position) pos: vec4f) -> @location(0) vec4f {
    let src = u.origin_px + (u.tile_offset + pos.xy) * u.scale;
    let t = vec2i(floor(src));
    if any(t < u.lo) || any(t > u.hi) {
        return vec4f(0.0);
    }
    let dx = (height(t + vec2i(1, 0)) - height(t - vec2i(1, 0))) / (2.0 * u.metres_per_pixel);
    let dy = (height(t + vec2i(0, 1)) - height(t - vec2i(0, 1))) / (2.0 * u.metres_per_pixel);
    let n = normalize(vec3f(-dx, -dy, 1.0));
    // Light from the north-west (image up-left), 45 degrees high. Flat ground keeps its ramp colour.
    let light = normalize(vec3f(-1.0, -1.0, 1.4142135));
    let shade = clamp(0.5 + 0.5 * dot(n, light) / light.z, 0.3, 1.3);
    return vec4f(clamp(ramp(height(t)) * shade, vec3f(0.0), vec3f(1.0)), 1.0);
}
