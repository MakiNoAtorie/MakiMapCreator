// Live brush stamps: the port of the CPU brush in `mmc_core::brush` (ADR-0025).

// Layout: `live::Params` and `live::Stamp`.
struct Params {
    box0: vec2i,
    size: vec2u,
    g0: vec2i,
    level: u32,
    count: u32,
    sub: u32,
    detail: f32,
    total: f32,
    octaves: u32,
    first: u32,
    reach: f32,
    _pad: vec2u,
}

struct Stamp {
    centre: vec2f,
    radius: f32,
    amount: f32,
    pressure: f32,
    core: f32,
    goal: f32,
    mode: u32,
}

const ADD = 0u;
const RAISE_TO = 1u;
const LOWER_TO = 2u;
const ROUGHEN = 3u;
const FLATTEN = 4u;
const SMOOTH = 5u;

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var heights: texture_storage_2d<r32float, read_write>;
@group(0) @binding(2) var<storage, read> stamps: array<Stamp>;
// Layout: `live::Live::tables`.
@group(0) @binding(3) var<storage, read> tables: array<u32>;
// The heights before this dispatch.
@group(0) @binding(4) var before: texture_2d<f32>;
// The flatten level of the stroke.
@group(0) @binding(5) var<storage, read_write> level: array<f32>;
// Smooth: box means along rows of the heights before the stamp.
@group(0) @binding(6) var rows: texture_storage_2d<r32float, read_write>;

fn before_at(t: vec2i) -> f32 {
    return textureLoad(before, clamp(t, vec2i(0), vec2i(textureDimensions(before)) - 1), 0).r;
}

fn height_at(t: vec2i) -> f32 {
    return textureLoad(heights, clamp(t, vec2i(0), vec2i(textureDimensions(heights)) - 1)).r;
}

// Flatten: the level is the pixel under the first stamp, on the plane of its texel `box0`.
@compute @workgroup_size(1)
fn sample() {
    let t = p.box0;
    let side = f32(1 << p.level);
    let slope = 0.5 * vec2f(
        height_at(t + vec2i(1, 0)) - height_at(t - vec2i(1, 0)),
        height_at(t + vec2i(0, 1)) - height_at(t - vec2i(0, 1)),
    ) / side;
    let centre = vec2f(p.g0 + (t << vec2u(p.level))) + 0.5 * side;
    let at = floor(stamps[0].centre) + 0.5;
    level[0] = height_at(t) + select(0.0, dot(slope, at - centre), p.level > 0u);
}

// Smooth: texels within `reach` of `c`, inside 0..n; the last ones count by the part in reach.
fn reach_weight(x: i32, c: i32) -> f32 {
    let k = i32(ceil(p.reach));
    return select(1.0, p.reach - f32(k - 1), abs(x - c) == k);
}

// Smooth: the row mean within `reach`, summed as differences so f32 keeps the precision.
@compute @workgroup_size(8, 8)
fn smooth_rows(@builtin(global_invocation_id) id: vec3u) {
    if any(id.xy >= p.size) {
        return;
    }
    let t = p.box0 + vec2i(id.xy);
    let k = i32(ceil(p.reach));
    let w = i32(textureDimensions(before).x);
    let v = before_at(t);
    var sum = 0.0;
    var n = 0.0;
    for (var x = max(t.x - k, 0); x <= min(t.x + k, w - 1); x++) {
        let f = reach_weight(x, t.x);
        sum += f * (before_at(vec2i(x, t.y)) - v);
        n += f;
    }
    textureStore(rows, t, vec4f(v + sum / n, 0.0, 0.0, 0.0));
}

// Smooth: the box mean around texel `t`, from the row means.
fn box_mean(t: vec2i) -> f32 {
    let k = i32(ceil(p.reach));
    let h = i32(textureDimensions(rows).y);
    let v = textureLoad(rows, t).r;
    var sum = 0.0;
    var n = 0.0;
    for (var y = max(t.y - k, 0); y <= min(t.y + k, h - 1); y++) {
        let f = reach_weight(y, t.y);
        sum += f * (textureLoad(rows, vec2i(t.x, y)).r - v);
        n += f;
    }
    return v + sum / n;
}

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
        let amp = bitcast<f32>(tables[h]);
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
        sum += amp * (2.0 * lerp(top, bottom, fy) - 1.0);
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

// A texel is the mean of its level-0 pixels (ADR-0025): it takes the mean change of `sub`² of them.
@compute @workgroup_size(8, 8)
fn stamp(@builtin(global_invocation_id) id: vec3u) {
    if any(id.xy >= p.size) {
        return;
    }
    let t = p.box0 + vec2i(id.xy);
    let first = p.g0 + (t << vec2u(p.level));
    let step = (1 << p.level) / i32(p.sub);
    let n_sub = p.sub * p.sub;
    let v = before_at(t);
    // Pixels start on the plane of the neighbours' slope, so their mean is v.
    var slope = vec2f(0.0);
    if p.level > 0u {
        slope = 0.5 * vec2f(
            before_at(t + vec2i(1, 0)) - before_at(t - vec2i(1, 0)),
            before_at(t + vec2i(0, 1)) - before_at(t - vec2i(0, 1)),
        ) / f32(1 << p.level);
    }
    let centre = vec2f(first) + 0.5 * f32(1 << p.level);
    var change: array<f32, 16>;
    var noise: array<f32, 16>;
    // Bit k: noise[k] is set.
    var have = 0u;
    var changed = false;
    for (var i = p.first; i < p.first + p.count; i++) {
        let s = stamps[i];
        let roughen = s.mode == ROUGHEN;
        let noisy = p.detail > 0.0 || roughen;
        var goal = s.goal;
        if s.mode == FLATTEN {
            goal = level[0];
        } else if s.mode == SMOOTH {
            goal = box_mean(t);
        }
        for (var k = 0u; k < n_sub; k++) {
            let g = first + step * vec2i(vec2u(k % p.sub, k / p.sub)) + step / 2;
            let d = vec2f(g) + 0.5 - s.centre;
            let d2 = dot(d, d) / (s.radius * s.radius);
            if d2 >= 1.0 {
                continue;
            }
            let fall = falloff(d2, s.core);
            var n = 0.0;
            if noisy {
                if (have & (1u << k)) == 0u {
                    noise[k] = detail(g);
                    have |= 1u << k;
                }
                n = noise[k];
            }
            let w = fall * max(0.0, 1.0 + p.detail * n);
            let start = v + dot(slope, vec2f(g) + 0.5 - centre);
            let u = start + change[k];
            var next = u;
            if roughen {
                next = u + s.amount * fall * n;
            } else if s.mode == ADD {
                next = u + s.amount * w;
            } else if s.mode == FLATTEN {
                next = u + (goal - u) * min(1.0, s.amount * w);
            } else if s.mode == SMOOTH {
                // The box mean lies on the same plane as the pixels.
                let m = goal + start - v;
                next = u + (m - u) * min(1.0, s.amount * w);
            } else if (u < goal) == (s.mode == RAISE_TO) && u != goal {
                next = u + (goal - u) * min(1.0, s.pressure * w);
            }
            if next != u {
                change[k] = next - start;
                changed = true;
            }
        }
    }
    if !changed {
        return;
    }
    var sum = 0.0;
    for (var k = 0u; k < n_sub; k++) {
        sum += change[k];
    }
    textureStore(heights, t, vec4f(v + sum / f32(n_sub), 0.0, 0.0, 0.0));
}
