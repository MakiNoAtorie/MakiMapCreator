// Terrain shading of a height pass. Uses the pass's `u: ViewUniform`; `PALETTE` and `COAST` come
// from the Rust tables in terrain.rs.

// Hillshade strength on land and under the sea; the darkest a slope turned away gets.
const LAND_SHADE = 0.6;
const SEA_SHADE = 0.3;
const DARKEST = 0.45;
// Height of the rock stop, metres; slopes in metres per metre from rock to steep rock.
const ROCK = 3500.0;
const STEEP = vec2f(0.6, 1.2);
// Metres the highest texel near a 0 m crossing must reach for a full coastline.
const COAST_LAND = 0.5;

fn palette(h: f32) -> vec3f {
    let n = i32(PALETTE_LEN);
    if h <= PALETTE[0].x {
        return PALETTE[0].yzw;
    }
    for (var i = 1; i < n; i++) {
        if h <= PALETTE[i].x {
            let a = PALETTE[i - 1];
            let b = PALETTE[i];
            return mix(a.yzw, b.yzw, (h - a.x) / (b.x - a.x));
        }
    }
    return PALETTE[n - 1].yzw;
}

fn texel(hm: texture_2d<f32>, t: vec2i) -> f32 {
    return textureLoad(hm, clamp(t, u.lo, u.hi) - u.tex_origin, 0).r;
}

// Central difference at texel `t`, metres per map pixel.
fn slope(hm: texture_2d<f32>, t: vec2i) -> vec2f {
    let x = vec2i(1, 0);
    let y = vec2i(0, 1);
    return 0.5 * vec2f(texel(hm, t + x) - texel(hm, t - x), texel(hm, t + y) - texel(hm, t - y));
}

// Shaded terrain at image pixel `pos` of this target; transparent outside the map.
fn terrain(hm: texture_2d<f32>, pos: vec2f) -> vec4f {
    let src = u.origin_px + (u.tile_offset + pos) * u.scale;
    let inside = vec2i(floor(src));
    if any(inside < u.lo) || any(inside > u.hi) {
        return vec4f(0.0);
    }
    // Bilinear between texel centres, so the coast and the shade are smooth when zoomed in.
    let q = src - 0.5;
    let t = vec2i(floor(q));
    let f = q - floor(q);
    let x = vec2i(1, 0);
    let y = vec2i(0, 1);
    let h00 = texel(hm, t);
    let h10 = texel(hm, t + x);
    let h01 = texel(hm, t + y);
    let h11 = texel(hm, t + x + y);
    let h = mix(mix(h00, h10, f.x), mix(h01, h11, f.x), f.y);

    // Slope in metres per metre, so relief is the same at every zoom and pyramid level.
    let s = mix(
        mix(slope(hm, t), slope(hm, t + x), f.x),
        mix(slope(hm, t + y), slope(hm, t + x + y), f.x),
        f.y,
    ) / u.metres_per_pixel;
    let n = normalize(vec3f(-s, 1.0));
    // Light from the north-west (image up-left), 45 degrees high. Flat ground gives 1.
    let light = normalize(vec3f(-1.0, -1.0, 1.4142135));
    let lit = mix(1.0, dot(n, light) / light.z, select(LAND_SHADE, SEA_SHADE, h <= 0.0));
    // Snow stays on gentle high ground; steep high slopes show rock.
    var c = mix(palette(h), palette(min(h, ROCK)), smoothstep(STEEP.x, STEEP.y, length(s)));
    if lit > 1.0 {
        c = mix(c, vec3f(1.0), lit - 1.0);
    } else {
        // Shade eases toward DARKEST, so steep slopes keep their relief.
        c = c * (DARKEST + (1.0 - DARKEST) * exp((lit - 1.0) / (1.0 - DARKEST)));
    }

    // Coast: distance to the 0 m crossing in image pixels, from the cell's bilinear gradient.
    // Only where land rises clearly above the sea nearby: sea floor at 0 m next to deeper sea
    // is no coast.
    var top = -1e30;
    for (var j = -1; j <= 2; j++) {
        for (var i = -1; i <= 2; i++) {
            top = max(top, texel(hm, t + vec2i(i, j)));
        }
    }
    let g = vec2f(mix(h10 - h00, h11 - h01, f.y), mix(h01 - h00, h11 - h10, f.x)) * u.scale;
    let len = length(g);
    if len > 1e-6 {
        let ink = clamp(1.0 - abs(h) / len, 0.0, 1.0) * smoothstep(0.0, COAST_LAND, top);
        c = mix(c, COAST, ink);
    }
    return vec4f(c, 1.0);
}
