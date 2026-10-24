// Live brush stamps: the port of the CPU brush in `mmc_core::brush` (ADR-0025).

// Layout: `live::Params` and `live::Stamp`.
struct Params {
    box0: vec2i,
    size: vec2u,
    g0: vec2i,
    ridge: vec2f,
    level: u32,
    count: u32,
    sub: u32,
    detail: f32,
    octaves: u32,
    first: u32,
    reach: f32,
    pad: u32,
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

// `GRADIENTS`, picked by the top 3 bits of the corner hash.
const S = 0.70710678;
var<private> GRADIENTS: array<vec2f, 8> = array(
    vec2f(1.0, 0.0), vec2f(-1.0, 0.0), vec2f(0.0, 1.0), vec2f(0.0, -1.0),
    vec2f(S, S), vec2f(-S, S), vec2f(S, -S), vec2f(-S, -S),
);

// `VALUE_SPREAD`, `RIDGED_MID`, `RIDGED_SPREAD`.
const VALUE_SPREAD = 0.45;
const RIDGED_MID = 1.0;
const RIDGED_SPREAD = 0.6;

fn fade(t: vec2f) -> vec2f {
    return t * t * t * (t * (t * 6.0 - 15.0) + 10.0);
}

// `gradient`; corners wrap to 32 bits and hash as `rng::lattice`.
fn gradient(pre: vec2u, c: vec2u, f: vec2f) -> f32 {
    var d: array<f32, 4>;
    for (var k = 0u; k < 4u; k++) {
        let o = vec2u(k & 1u, k >> 1u);
        let g = GRADIENTS[mix64(pre ^ (c + o)).y >> 29u];
        d[k] = dot(g, f - vec2f(o));
    }
    let u = fade(f);
    let top = d[0] + (d[1] - d[0]) * u.x;
    let bottom = d[2] + (d[3] - d[2]) * u.x;
    return top + (bottom - top) * u.y;
}

// `combine` at node (kx, ky); column and row parts add without rounding, as `noise_at`.
fn node(kx: u32, ky: u32) -> vec2f {
    var value = 0.0;
    var ridged = 0.0;
    var weight = 1.0;
    for (var o = 0u; o < p.octaves; o++) {
        let h = o * 8u;
        let amp = bitcast<f32>(tables[h]);
        let pre = vec2u(tables[h + 1u], tables[h + 2u]);
        let xa = tables[h + 3u] + 4u * kx;
        let ya = tables[h + 4u] + 4u * ky;
        let fc = vec2u(tables[xa + 2u], tables[xa + 3u]);
        let fixed = fc + vec2u(tables[ya + 2u], tables[ya + 3u]);
        let carry = select(vec2u(0u), vec2u(1u), fixed < fc);
        let c = vec2u(tables[xa], tables[xa + 1u]) + vec2u(tables[ya], tables[ya + 1u]) + carry;
        let n = gradient(pre, c, vec2f(fixed) * 2.3283064e-10);
        value += amp * n;
        let s = 1.0 - abs(n * 1.4142135);
        let signal = s * s * weight;
        ridged += amp * signal;
        weight = signal * (2.0 - signal);
    }
    return vec2f(value / VALUE_SPREAD, (ridged - RIDGED_MID) / RIDGED_SPREAD);
}

// `spline`.
fn spline(a: vec2f, b: vec2f, c: vec2f, t: f32) -> vec2f {
    let s = 1.0 - t;
    return 0.5 * (s * s * a + (1.0 + 2.0 * t * s) * b + t * t * c);
}

// `detail_noise` at pixel g: value and ridged, splines of nodes every 4 pixels, in y, then x.
// Table node 0 is the one before the frame's first, so g's nodes are k..k + 2.
fn detail(g: vec2i) -> vec2f {
    let h = g + 2;
    let k = vec2u(h >> vec2u(2u));
    let t = vec2f(h & vec2i(3)) / 4.0;
    var col: array<vec2f, 3>;
    for (var i = 0u; i < 3u; i++) {
        col[i] = spline(node(k.x + i, k.y), node(k.x + i, k.y + 1u), node(k.x + i, k.y + 2u), t.y);
    }
    return spline(col[0], col[1], col[2], t.x);
}

// `RIDGE_GAIN`.
const RIDGE_GAIN = 2.0;

// `COAST_BAND`, `WATER_LEVEL`.
const COAST_BAND = 20.0;
const WATER_LEVEL = 0.0;

// `DetailNoise::at`: ridged on high ground when the stamp raises; faded at the water level.
fn noise_at(n: vec2f, height: f32, raise: bool) -> f32 {
    var v = n.x;
    if raise {
        let s = smoothstep(p.ridge.x, p.ridge.y, height);
        v = n.x * (1.0 - s) + RIDGE_GAIN * n.y * s;
    }
    return v * smoothstep(0.0, COAST_BAND, abs(height - WATER_LEVEL));
}

// `roughen`: at most the distance to the water level; a rounding that would cross it keeps `v`.
fn roughen(v: f32, d: f32) -> f32 {
    let a = abs(v - WATER_LEVEL);
    if a == 0.0 {
        return v;
    }
    let next = v + a * soft(d / a);
    return select(v, next, (next > WATER_LEVEL) == (v > WATER_LEVEL));
}

// `soft`.
fn soft(x: f32) -> f32 {
    return x / sqrt(1.0 + x * x);
}

// `weight_in`.
fn falloff(d2: f32, core: f32) -> f32 {
    if d2 <= core {
        return 1.0;
    }
    let t = (1.0 - d2) / (1.0 - core);
    return t * t * (3.0 - 2.0 * t);
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
    var noise: array<vec2f, 16>;
    // Bit k: noise[k] is set.
    var have = 0u;
    var changed = false;
    for (var i = p.first; i < p.first + p.count; i++) {
        let s = stamps[i];
        let rough = s.mode == ROUGHEN;
        let noisy = p.detail > 0.0 || rough;
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
            let start = v + dot(slope, vec2f(g) + 0.5 - centre);
            let u = start + change[k];
            var n = 0.0;
            if noisy {
                if (have & (1u << k)) == 0u {
                    noise[k] = detail(g);
                    have |= 1u << k;
                }
                n = noise_at(noise[k], u, s.mode == ADD && s.amount > 0.0);
            }
            let w = fall * (1.0 + soft(p.detail * n));
            var next = u;
            if rough {
                next = roughen(u, s.amount * fall * n);
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
