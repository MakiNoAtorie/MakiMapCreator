// Live brush stamps (ADR-0025): the port of `mmc_core::stamp_weight`, `detail_noise` and the add
// and roughen modes of `brush_into`. Pixel coordinates are global level-0 pixels minus a multiple
// of 4 (the detail tables' first node), so they stay small. u64 is vec2u (lo, hi).

struct Params {
    // Texels this dispatch covers.
    box0: vec2i,
    size: vec2u,
    // Pixel sampled for texel (0, 0); texel t samples g0 + (t << level).
    g0: vec2i,
    level: u32,
    count: u32,
    // Detail amount (0 is none), sum of octave amplitudes, octaves in the tables.
    detail: f32,
    total: f32,
    octaves: u32,
    _pad: u32,
}

struct Stamp {
    centre: vec2f,
    radius: f32,
    // strength * pressure.
    amount: f32,
    pressure: f32,
    // Squared radius fraction of the hard core.
    core: f32,
    goal: f32,
    // 1: has a target, 2: raises, 4: roughen.
    flags: u32,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var heights: texture_storage_2d<r32float, read_write>;
@group(0) @binding(2) var<storage, read> stamps: array<Stamp>;
// Per octave 8 words: amp (f32 bits), offsets of node x, node y, cols, rows. Node entries are
// (lattice index, smoothstep f32 bits); cols and rows are u64.
@group(0) @binding(3) var<storage, read> tables: array<u32>;

fn mul_wide(a: u32, b: u32) -> vec2u {
    let a0 = a & 0xffffu;
    let a1 = a >> 16u;
    let b0 = b & 0xffffu;
    let b1 = b >> 16u;
    let p00 = a0 * b0;
    let p01 = a0 * b1;
    let p10 = a1 * b0;
    let mid = (p00 >> 16u) + (p01 & 0xffffu) + (p10 & 0xffffu);
    return vec2u((p00 & 0xffffu) | (mid << 16u), a1 * b1 + (p01 >> 16u) + (p10 >> 16u) + (mid >> 16u));
}

fn mul64(a: vec2u, b: vec2u) -> vec2u {
    let w = mul_wide(a.x, b.x);
    return vec2u(w.x, w.y + a.x * b.y + a.y * b.x);
}

fn shr64(a: vec2u, s: u32) -> vec2u {
    return vec2u((a.x >> s) | (a.y << (32u - s)), a.y >> s);
}

// splitmix64 finaliser, as `rng::mix`.
fn mix64(z0: vec2u) -> vec2u {
    var z = mul64(z0 ^ shr64(z0, 30u), vec2u(0x1CE4E5B9u, 0xBF58476Du));
    z = mul64(z ^ shr64(z, 27u), vec2u(0x133111EBu, 0x94D049BBu));
    return z ^ shr64(z, 31u);
}

// `rng::to_unit`: the top 53 bits over 2^53.
fn unit(h: vec2u) -> f32 {
    return f32(h.y) * 2.3283064e-10 + f32(h.x >> 11u) * 1.110223e-16;
}

fn word64(at: u32) -> vec2u {
    return vec2u(tables[at], tables[at + 1u]);
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    return a + (b - a) * t;
}

// Node (kx, ky) of the tables: the octave sum over the total, as `fractal`.
fn node(kx: u32, ky: u32) -> f32 {
    var sum = 0.0;
    for (var o = 0u; o < p.octaves; o++) {
        let h = o * 8u;
        let nx = h + 0u;
        let xa = tables[h + 1u] + 2u * kx;
        let ya = tables[h + 2u] + 2u * ky;
        let ix = tables[xa];
        let fx = bitcast<f32>(tables[xa + 1u]);
        let iy = tables[ya];
        let fy = bitcast<f32>(tables[ya + 1u]);
        let cols = tables[h + 3u];
        let rows = tables[h + 4u];
        let x0 = word64(cols + 2u * ix);
        let x1 = word64(cols + 2u * ix + 2u);
        let y0 = word64(rows + 2u * iy);
        let y1 = word64(rows + 2u * iy + 2u);
        let a = unit(mix64(x0 ^ y0));
        let b = unit(mix64(x1 ^ y0));
        let c = unit(mix64(x0 ^ y1));
        let d = unit(mix64(x1 ^ y1));
        let top = lerp(a, b, fx);
        let bottom = lerp(c, d, fx);
        sum += bitcast<f32>(tables[nx]) * (2.0 * lerp(top, bottom, fy) - 1.0);
    }
    return sum / p.total;
}

// `detail_noise` at pixel g: nodes every 4 pixels, mixed in y, then x.
fn detail(g: vec2i) -> f32 {
    let n = vec2u(g >> vec2u(2u));
    let f = vec2f(g & vec2i(3)) / 4.0;
    let left = lerp(node(n.x, n.y), node(n.x, n.y + 1u), f.y);
    let right = lerp(node(n.x + 1u, n.y), node(n.x + 1u, n.y + 1u), f.y);
    return lerp(left, right, f.x);
}

// `weight_in`.
fn falloff(d2: f32, core: f32) -> f32 {
    if d2 <= core {
        return 1.0;
    }
    let t = (1.0 - d2) / (1.0 - core);
    return t * t;
}

@compute @workgroup_size(8, 8)
fn stamp(@builtin(global_invocation_id) id: vec3u) {
    if any(id.xy >= p.size) {
        return;
    }
    let t = p.box0 + vec2i(id.xy);
    let g = p.g0 + (t << vec2u(p.level));
    let at = vec2f(g) + select(0.5, 0.0, p.level > 0u);
    var v = textureLoad(heights, t).r;
    let old = v;
    var n = 0.0;
    var have_n = false;
    for (var i = 0u; i < p.count; i++) {
        let s = stamps[i];
        let d = at - s.centre;
        let d2 = dot(d, d) / (s.radius * s.radius);
        if d2 >= 1.0 {
            continue;
        }
        let roughen = (s.flags & 4u) != 0u;
        if !have_n && (p.detail > 0.0 || roughen) {
            n = detail(g);
            have_n = true;
        }
        let fall = falloff(d2, s.core);
        if roughen {
            v += s.amount * fall * n;
            continue;
        }
        let w = fall * max(0.0, 1.0 + p.detail * n);
        if (s.flags & 1u) == 0u {
            v += s.amount * w;
        } else if (v < s.goal) == ((s.flags & 2u) != 0u) && v != s.goal {
            v += (s.goal - v) * min(1.0, s.pressure * w);
        }
    }
    if v != old {
        textureStore(heights, t, vec4f(v, 0.0, 0.0, 0.0));
    }
}
