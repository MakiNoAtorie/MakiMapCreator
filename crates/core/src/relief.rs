//! Erosion relief (ADR-0026): gullies and ridges that run down the slope, written from
//! runevision's "Fast and Gorgeous Erosion Filter" (2026). `render/src/relief.wgsl` ports it.

use crate::{COAST_BAND, WATER_LEVEL, rng};

/// The rng stage of octave `o` is `relief.<o>`.
pub const RELIEF_STAGE: &str = "relief";
/// Cell of the coarsest octave, root-space metres; each next octave halves it.
pub const RELIEF_CELL: f64 = 640.0;
pub const RELIEF_OCTAVES: usize = 4;
/// Stripe cycles per cell.
pub const RELIEF_WAVES: f64 = 1.0;
/// How far a gully flank turns the next octave's slope, as a share of the slope.
pub const RELIEF_BEND: f64 = 0.7;
/// Side of the pyramid blocks whose slope the gullies follow, about, root-space metres.
pub const RELIEF_BLOCK: f64 = 160.0;

/// Sum of the octave amplitudes `0.5^o`.
pub const RELIEF_AMPLITUDES: f64 = 1.875;
/// Splat radius in cells: a 4 x 4 block of cells holds every splat that reaches a point.
pub const RELIEF_REACH2: f64 = 2.25;
/// Softness of the flank sign that bends the next octave.
pub const RELIEF_SIGN_SOFT2: f64 = 0.09;
/// Height above the water level whose fade target is a peak (1); the water level is a valley (-1).
pub const RELIEF_PEAK: f64 = 3000.0;
/// `|sin|` of a gully from which later octaves are full; less is near a ridge or crease line.
pub const RELIEF_CREASE: f64 = 0.5;

/// The map kind's relief options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReliefOptions {
    /// Gully depth at full mask, metres.
    pub strength: f64,
    /// Slope, metres per metre, from which gullies are full.
    pub slope: f64,
    /// Height above the water level, metres, from which gullies are full; none up to `COAST_BAND`.
    pub height: f64,
}

/// The rng prefix of each octave.
pub fn relief_prefixes(seed: u64) -> [u64; RELIEF_OCTAVES] {
    std::array::from_fn(|o| rng::prefix(seed, &format!("{RELIEF_STAGE}.{o}")))
}

/// The pyramid level whose slope the gullies follow at `mpp` metres per pixel: blocks of about
/// `RELIEF_BLOCK`, at least level 1.
pub fn relief_level(mpp: f64) -> u32 {
    (RELIEF_BLOCK / mpp).log2().round().clamp(1.0, 16.0) as u32
}

/// Pixels a relief pixel reads on each side: the level's blocks, their differences, bilinear.
pub fn relief_margin(mpp: f64) -> usize {
    3 << relief_level(mpp)
}

/// Metres the relief adds to height for shading, per pixel of `heights` (`w` x `h`, row by row),
/// whose pixel (0, 0) is global pixel `g0` at `mpp` metres per pixel. Reads past the edge take
/// the edge pixel, so only pixels `relief_margin` inside it are the map's.
pub fn relief(
    seed: u64,
    o: &ReliefOptions,
    mpp: f64,
    g0: (i64, i64),
    w: usize,
    h: usize,
    heights: &[f32],
) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    let low = COAST_BAND + WATER_LEVEL;
    if heights.iter().all(|&v| v <= low) {
        return out;
    }
    // Each octave's pivots over the cells the raster's points reach.
    let pivots: Vec<Pivots> = relief_prefixes(seed)
        .iter()
        .enumerate()
        .map(|(i, &prefix)| {
            let cell = RELIEF_CELL / (1u64 << i) as f64;
            let q = |g: i64| (g as f64 + 0.5) * mpp / cell;
            let lo = |g: i64| (q(g) - 0.5).floor() as i64 - 1;
            let hi = |g: i64| (q(g) - 0.5).floor() as i64 + 2;
            let (x0, y0) = (lo(g0.0), lo(g0.1));
            let (x1, y1) = (hi(g0.0 + w as i64 - 1), hi(g0.1 + h as i64 - 1));
            let pw = (x1 - x0 + 1) as usize;
            let mut at = Vec::with_capacity(pw * (y1 - y0 + 1) as usize);
            for y in y0..=y1 {
                at.extend((x0..=x1).map(|x| pivot(prefix, x, y)));
            }
            Pivots { x0, y0, w: pw, at }
        })
        .collect();
    let at = |x: i64, y: i64| {
        let (i, j) = (
            (x - g0.0).clamp(0, w as i64 - 1),
            (y - g0.1).clamp(0, h as i64 - 1),
        );
        heights[j as usize * w + i as usize] as f64
    };
    // Block means of `relief_level` on the global grid, blocks `c0 - 2 ..= c1 + 2`.
    let b = 1i64 << relief_level(mpp);
    let c0 = (g0.0.div_euclid(b) - 2, g0.1.div_euclid(b) - 2);
    let c1 = (
        (g0.0 + w as i64 - 1).div_euclid(b) + 2,
        (g0.1 + h as i64 - 1).div_euclid(b) + 2,
    );
    let cw = (c1.0 - c0.0 + 1) as usize;
    let ch = (c1.1 - c0.1 + 1) as usize;
    let mut coarse = vec![0.0; cw * ch];
    for j in 0..ch {
        for i in 0..cw {
            let (x, y) = (b * (c0.0 + i as i64), b * (c0.1 + j as i64));
            let sum: f64 = (y..y + b)
                .flat_map(|y| (x..x + b).map(move |x| (x, y)))
                .map(|(x, y)| at(x, y))
                .sum();
            coarse[j * cw + i] = sum / (b * b) as f64;
        }
    }
    let k = 1.0 / (2.0 * b as f64 * mpp);
    let grad = |cx: i64, cy: i64| {
        let (i, j) = ((cx - c0.0) as usize, (cy - c0.1) as usize);
        let c = |i: usize, j: usize| coarse[j * cw + i];
        (
            (c(i + 1, j) - c(i - 1, j)) * k,
            (c(i, j + 1) - c(i, j - 1)) * k,
        )
    };
    let lerp =
        |a: (f64, f64), b: (f64, f64), t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    for j in 0..h {
        for i in 0..w {
            let g = (g0.0 + i as i64, g0.1 + j as i64);
            let v = heights[j * w + i] as f64;
            let mask = smoothstep(COAST_BAND as f64, o.height, v - WATER_LEVEL as f64);
            if mask == 0.0 {
                continue;
            }
            // The block gradient, bilinear between block centres.
            let u = |g: i64| (g as f64 + 0.5) / b as f64 - 0.5;
            let (ux, uy) = (u(g.0), u(g.1));
            let (bx, by) = (ux.floor() as i64, uy.floor() as i64);
            let (fx, fy) = (ux - ux.floor(), uy - uy.floor());
            let top = lerp(grad(bx, by), grad(bx + 1, by), fx);
            let bottom = lerp(grad(bx, by + 1), grad(bx + 1, by + 1), fx);
            let s = lerp(top, bottom, fy);
            let at = ((g.0 as f64 + 0.5) * mpp, (g.1 as f64 + 0.5) * mpp);
            let fade = ((v - WATER_LEVEL as f64) / RELIEF_PEAK).clamp(0.0, 1.0) * 2.0 - 1.0;
            let d = filter(&pivots, o, mpp, at, s, fade);
            out[j * w + i] = (o.strength * mask * d / RELIEF_AMPLITUDES) as f32;
        }
    }
    out
}

/// Gullies at root point `at` (metres) on ground of slope `s` (metres per metre) and fade target
/// `fade` (-1 valley, 1 peak): the octave amplitudes times each faded octave less `fade`, so
/// flat ground gets 0.
fn filter(
    pivots: &[Pivots],
    o: &ReliefOptions,
    mpp: f64,
    at: (f64, f64),
    s: (f64, f64),
    fade: f64,
) -> f64 {
    let mut slope = s;
    let mut mask = ease_out(len(s) / o.slope);
    let mut target = fade;
    let (mut sum, mut rest) = (0.0, RELIEF_AMPLITUDES);
    let mut amp = 1.0;
    let mut cell = RELIEF_CELL;
    for p in pivots {
        // Octaves finer than 4 pixels fade out by 2, so they never alias.
        let lod = (cell / mpp / 2.0 - 1.0).clamp(0.0, 1.0);
        if mask == 0.0 || lod == 0.0 {
            break;
        }
        let l = len(slope);
        // Across the slope; gullies run along it, down toward the valley.
        let across = (-slope.1 / l, slope.0 / l);
        let (c, sn) = phacelle(p, (at.0 / cell, at.1 / cell), across);
        target += (c - target) * mask * lod;
        sum += amp * (target - fade);
        rest -= amp;
        // The flank's own slope turns the next octave toward the gully floor.
        let flank = -sn / (sn * sn + RELIEF_SIGN_SOFT2).sqrt() * l * RELIEF_BEND;
        slope = (slope.0 + across.0 * flank, slope.1 + across.1 * flank);
        mask = pow_inv(mask) * ease_out(sn.abs() / RELIEF_CREASE);
        amp *= 0.5;
        cell *= 0.5;
    }
    // The octaves left out equal the last target.
    sum + rest * (target - fade)
}

/// Phacelle-style noise at cell point `q`: stripes across `dir`, one splat per cell at a hashed
/// pivot, blended over the 4 x 4 cells around `q`. Returns the (cos, sin) pair, its length
/// doubled and capped at 1.
fn phacelle(pivots: &Pivots, q: (f64, f64), dir: (f64, f64)) -> (f64, f64) {
    let b = ((q.0 - 0.5).floor() as i64, (q.1 - 0.5).floor() as i64);
    let (mut c, mut s, mut wsum) = (0.0, 0.0, 0.0);
    for j in -1..=2 {
        for i in -1..=2 {
            let (x, y) = (
                (b.0 + i - pivots.x0) as usize,
                (b.1 + j - pivots.y0) as usize,
            );
            let pivot = pivots.at[y * pivots.w + x];
            let v = (q.0 - pivot.0, q.1 - pivot.1);
            let t = 1.0 - (v.0 * v.0 + v.1 * v.1) / RELIEF_REACH2;
            if t <= 0.0 {
                continue;
            }
            let wt = t * t * t;
            let (wc, ws) = wave((v.0 * dir.0 + v.1 * dir.1) * RELIEF_WAVES);
            c += wt * wc;
            s += wt * ws;
            wsum += wt;
        }
    }
    let (c, s) = (c / wsum, s / wsum);
    let l = (c * c + s * s).sqrt();
    if l == 0.0 {
        return (0.0, 0.0);
    }
    let k = (2.0 * l).min(1.0) / l;
    (c * k, s * k)
}

/// One octave's splat pivots, in cells, for cells `x0..x0 + w` and `y0..`.
struct Pivots {
    x0: i64,
    y0: i64,
    w: usize,
    at: Vec<(f64, f64)>,
}

/// The splat pivot of cell (`x`, `y`): the cell plus a hashed place in it.
fn pivot(prefix: u64, x: i64, y: i64) -> (f64, f64) {
    let hash = rng::lattice(prefix, x, y);
    (
        x as f64 + (hash as u32) as f64 * 2.0f64.powi(-32),
        y as f64 + ((hash >> 32) as u32) as f64 * 2.0f64.powi(-32),
    )
}

/// cos and sin of `2 pi t`, by Taylor series on the nearest half cycle, so a port needs no
/// `cos` or `sin` builtin.
pub(crate) fn wave(t: f64) -> (f64, f64) {
    let a = std::f64::consts::TAU * (t - (t + 0.5).floor());
    let a2 = a * a;
    let (mut c, mut s) = (1.0, 1.0);
    // Terms up to a^14 and a^15: off by at most 5e-6 within a half cycle.
    for k in (1..=7).rev() {
        let k = k as f64;
        c = 1.0 - a2 * c / ((2.0 * k) * (2.0 * k - 1.0));
        s = 1.0 - a2 * s / ((2.0 * k + 1.0) * (2.0 * k));
    }
    (c, a * s)
}

fn len(v: (f64, f64)) -> f64 {
    (v.0 * v.0 + v.1 * v.1).sqrt()
}

/// The article's `pow_inv(t, detail)` at detail 1.5, so the port needs only `sqrt`.
fn pow_inv(t: f64) -> f64 {
    let v = 1.0 - t.clamp(0.0, 1.0);
    1.0 - v * v.sqrt()
}

fn ease_out(t: f64) -> f64 {
    let v = 1.0 - t.clamp(0.0, 1.0);
    1.0 - v * v
}

fn smoothstep(a: f64, b: f64, x: f64) -> f64 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relief of a plane rising 0.6 m per metre along x from `base` metres, 64 x 64 at 10 m.
    fn plane(base: f32) -> Vec<f32> {
        let o = ReliefOptions {
            strength: 200.0,
            slope: 0.4,
            height: 1200.0,
        };
        let n = 64 + 2 * relief_margin(10.0);
        let h: Vec<f32> = (0..n * n).map(|i| base + (i % n) as f32 * 6.0).collect();
        let r = relief(7, &o, 10.0, (0, 0), n, n, &h);
        let m = relief_margin(10.0);
        (m..m + 64)
            .flat_map(|y| (m..m + 64).map(move |x| y * n + x))
            .map(|i| r[i])
            .collect()
    }

    #[test]
    fn peaks_only_cut_gullies_and_mid_slopes_cut_and_raise() {
        let high = plane(RELIEF_PEAK as f32 + 500.0);
        let (lo, hi) = high
            .iter()
            .fold((0f32, 0f32), |a, &v| (a.0.min(v), a.1.max(v)));
        assert!(hi <= 1e-3 && lo < -20.0, "{lo} {hi}");
        let mid = plane(RELIEF_PEAK as f32 / 2.0 - 200.0);
        let (lo, hi) = mid
            .iter()
            .fold((0f32, 0f32), |a, &v| (a.0.min(v), a.1.max(v)));
        assert!(hi > 20.0 && lo < -20.0, "{lo} {hi}");
    }

    #[test]
    fn wave_is_cos_and_sin() {
        for i in -200..200 {
            let t = i as f64 * 0.0137;
            let (c, s) = super::wave(t);
            let a = std::f64::consts::TAU * t;
            assert!(
                (c - a.cos()).abs() < 1e-5 && (s - a.sin()).abs() < 1e-5,
                "{t}"
            );
        }
    }
}
