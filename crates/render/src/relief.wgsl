// Erosion relief: the port of `mmc_core::relief` (ADR-0026). `hash.wgsl` comes first.

// Layout: `live::ReliefOctave` and `live::ReliefParams`.
struct Octave {
    prefix: vec2u,
    // The lattice cell of texel (0, 0)'s centre and the centre's place in it.
    base: vec2i,
    frac: vec2f,
    // Cells per texel.
    step: f32,
    lod: f32,
}

struct Params {
    // Texels of this dispatch: copy texels for `relief`, block texels for `blocks`.
    box0: vec2i,
    size: vec2u,
    // Global pixel of texel (0, 0) at the copy's level.
    g0: vec2i,
    // Global block of block texel (0, 0).
    b0: vec2i,
    // Metres per texel.
    mpp: f32,
    strength: f32,
    slope: f32,
    height: f32,
    // Texels per block side: `relief_level` from the copy's level.
    block: i32,
    pad0: u32,
    pad1: vec2u,
    octaves: array<Octave, 4>,
}

// `COAST_BAND`, `WATER_LEVEL` and the `RELIEF_` constants of `mmc_core::relief`.
const COAST_BAND = 20.0;
const WATER_LEVEL = 0.0;
const RELIEF_WAVES = 1.0;
const RELIEF_BEND = 0.7;
const RELIEF_AMPLITUDES = 1.875;
const RELIEF_REACH2 = 2.25;
const RELIEF_SIGN_SOFT2 = 0.09;
const RELIEF_PEAK = 3000.0;
const RELIEF_CREASE = 0.5;
const TAU = 6.283185307179586;

@group(0) @binding(0) var<uniform> r: Params;
@group(0) @binding(1) var heights: texture_2d<f32>;
@group(0) @binding(2) var shaded: texture_storage_2d<r32float, write>;
@group(0) @binding(3) var blocks_out: texture_storage_2d<r32float, write>;
@group(0) @binding(4) var blocks_in: texture_2d<f32>;

fn height_at(t: vec2i) -> f32 {
    return textureLoad(heights, clamp(t, vec2i(0), vec2i(textureDimensions(heights)) - 1), 0).r;
}

// Block means: summed as differences from the first texel, so f32 keeps the precision.
@compute @workgroup_size(8, 8)
fn blocks(@builtin(global_invocation_id) id: vec3u) {
    if any(id.xy >= r.size) {
        return;
    }
    let c = r.box0 + vec2i(id.xy);
    let t0 = (r.b0 + c) * r.block - r.g0;
    let first = height_at(t0);
    var sum = 0.0;
    for (var y = 0; y < r.block; y++) {
        for (var x = 0; x < r.block; x++) {
            sum += height_at(t0 + vec2i(x, y)) - first;
        }
    }
    textureStore(blocks_out, c, vec4f(first + sum / f32(r.block * r.block), 0.0, 0.0, 0.0));
}

fn block_at(c: vec2i) -> f32 {
    let n = vec2i(textureDimensions(blocks_in)) - 1;
    return textureLoad(blocks_in, clamp(c - r.b0, vec2i(0), n), 0).r;
}

fn grad(c: vec2i) -> vec2f {
    let x = vec2i(1, 0);
    let y = vec2i(0, 1);
    let d = vec2f(block_at(c + x) - block_at(c - x), block_at(c + y) - block_at(c - y));
    return d / (2.0 * f32(r.block) * r.mpp);
}

fn ease_out(t: f32) -> f32 {
    let v = 1.0 - clamp(t, 0.0, 1.0);
    return 1.0 - v * v;
}

fn pow_inv(t: f32) -> f32 {
    let v = 1.0 - clamp(t, 0.0, 1.0);
    return 1.0 - v * sqrt(v);
}

fn ramp(a: f32, b: f32, x: f32) -> f32 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

// `wave`: cos and sin of 2 pi t by the same Taylor series.
fn wave(t: f32) -> vec2f {
    let a = TAU * (t - floor(t + 0.5));
    let a2 = a * a;
    var c = 1.0;
    var s = 1.0;
    for (var k = 7; k >= 1; k--) {
        let f = f32(k);
        c = 1.0 - a2 * c / ((2.0 * f) * (2.0 * f - 1.0));
        s = 1.0 - a2 * s / ((2.0 * f + 1.0) * (2.0 * f));
    }
    return vec2f(c, a * s);
}

// `phacelle` at cell point `q` (the octave's frame), stripes across `dir`.
fn phacelle(o: Octave, q: vec2f, dir: vec2f) -> vec2f {
    let b = vec2i(floor(q - 0.5));
    var sum = vec2f(0.0);
    var wsum = 0.0;
    for (var j = -1; j <= 2; j++) {
        for (var i = -1; i <= 2; i++) {
            let cell = b + vec2i(i, j);
            let h = mix64(o.prefix ^ vec2u(o.base + cell));
            let pivot = vec2f(cell) + vec2f(h) * 2.3283064e-10;
            let v = q - pivot;
            let t = 1.0 - dot(v, v) / RELIEF_REACH2;
            if t <= 0.0 {
                continue;
            }
            let w = t * t * t;
            sum += w * wave(dot(v, dir) * RELIEF_WAVES);
            wsum += w;
        }
    }
    let cs = sum / wsum;
    let l = length(cs);
    if l == 0.0 {
        return vec2f(0.0);
    }
    return cs * (min(2.0 * l, 1.0) / l);
}

// `filter` (CPU) at texel `t` on ground of slope `s` and fade target `fade`.
fn gullies(t: vec2i, s: vec2f, fade: f32) -> f32 {
    var slope = s;
    var mask = ease_out(length(s) / r.slope);
    var aim = fade;
    var sum = 0.0;
    var rest = RELIEF_AMPLITUDES;
    var amp = 1.0;
    for (var k = 0; k < 4; k++) {
        let o = r.octaves[k];
        if mask == 0.0 || o.lod == 0.0 {
            break;
        }
        let l = length(slope);
        let across = vec2f(-slope.y, slope.x) / l;
        let q = o.frac + vec2f(t) * o.step;
        let cs = phacelle(o, q, across);
        aim += (cs.x - aim) * mask * o.lod;
        sum += amp * (aim - fade);
        rest -= amp;
        let flank = -cs.y / sqrt(cs.y * cs.y + RELIEF_SIGN_SOFT2) * l * RELIEF_BEND;
        slope += across * flank;
        mask = pow_inv(mask) * ease_out(abs(cs.y) / RELIEF_CREASE);
        amp *= 0.5;
    }
    return sum + rest * (aim - fade);
}

@compute @workgroup_size(8, 8)
fn relief(@builtin(global_invocation_id) id: vec3u) {
    if any(id.xy >= r.size) {
        return;
    }
    let t = r.box0 + vec2i(id.xy);
    let v = height_at(t);
    let mask = ramp(COAST_BAND, r.height, v - WATER_LEVEL);
    var d = 0.0;
    if mask > 0.0 {
        // The block gradient, bilinear between block centres, as `relief`.
        let u = (vec2f(r.g0 + t) + 0.5) / f32(r.block) - 0.5;
        let b = vec2i(floor(u));
        let f = u - floor(u);
        let top = mix(grad(b), grad(b + vec2i(1, 0)), f.x);
        let bottom = mix(grad(b + vec2i(0, 1)), grad(b + vec2i(1, 1)), f.x);
        let fade = clamp((v - WATER_LEVEL) / RELIEF_PEAK, 0.0, 1.0) * 2.0 - 1.0;
        d = r.strength * mask * gullies(t, mix(top, bottom, f.y), fade) / RELIEF_AMPLITUDES;
    }
    textureStore(shaded, t, vec4f(v + d, 0.0, 0.0, 0.0));
}
