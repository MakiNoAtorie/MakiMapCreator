//! Brush stamps: falloff with root-space detail (ADR-0008), and the add, smooth, flatten and
//! roughen modes. `stamp_weight` and `detail_noise` are the reference maths the GPU brush ports.

use std::collections::BTreeSet;

use crate::command::{BrushStroke, EditTiles, brush_core, weight_in};
use crate::layer::{RootPoint, TILE_SIZE};
use crate::rng::{self, Key};
use crate::{Map, Project, Result};

/// What a brush stamp does to each pixel, with `w` = `stamp_weight` and `p` the pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BrushMode {
    /// `v += strength * p * w`, or with `target` `v += (target - v) * min(1, p * w)`.
    #[default]
    Add,
    /// `v += (mean - v) * min(1, strength * p * w)`; `mean` is the box mean of the
    /// `(2k + 1)²` pixels around, `k = max(1, ceil(radius px / 4))`, read before the stamp.
    Smooth,
    /// `v += (level - v) * min(1, strength * p * w)`; `level` is the layer under the stroke's
    /// first stamp centre before the stroke.
    Flatten,
    /// `v += strength * p * falloff weight * detail_noise`: no net raise, the noise has mean 0.
    Roughen,
}

/// Root-space noise on a brush's stamp weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detail {
    /// 0 is the smooth falloff; 1 scales the weight by 0 to 2. Clamped to 0..=1.
    pub amount: f32,
    /// Cell of the coarsest octave, metres.
    pub scale: f64,
}

impl Detail {
    pub const NONE: Detail = Detail {
        amount: 0.0,
        scale: 1000.0,
    };
}

/// `rng` stage of the brush detail noise; the seed is the project seed.
pub const DETAIL_STAGE: &str = "brush-detail";
const OCTAVES: u32 = 4;

/// Fractal value noise in -1..1 at root-space `at`:
/// `sum(0.5^o * (2 * rng::noise(seed, DETAIL_STAGE, at, scale / 2^o) - 1)) / sum(0.5^o)` for
/// octaves `o` in 0..4, skipping octaves whose cell is under 2 `mpp` (no sub-pixel detail; this
/// also bounds the work per stamp). `rng::noise` is smoothstep-interpolated value noise on lattice
/// corners `unit(seed, stage, Key::At(corner))`, a splitmix64 hash of the corner's f64 bits (`rng.rs`).
/// A port with f32 or another hash agrees within its rounding, not bit for bit.
pub fn detail_noise(seed: u64, at: RootPoint, scale: f64, mpp: f64) -> f64 {
    fractal(scale, mpp, |_, cell| {
        rng::noise(seed, DETAIL_STAGE, at, cell)
    })
}

fn fractal(scale: f64, mpp: f64, noise: impl Fn(usize, f64) -> f64) -> f64 {
    let (mut sum, mut total, mut amp, mut cell) = (0.0, 0.0, 1.0, scale);
    for o in 0..OCTAVES as usize {
        if cell >= 2.0 * mpp {
            sum += amp * (2.0 * noise(o, cell) - 1.0);
        }
        total += amp;
        amp *= 0.5;
        cell *= 0.5;
    }
    sum / total
}

/// Squared distance from `at` to `centre` as a fraction of `radius`, all root-space metres.
fn dist2(at: RootPoint, centre: RootPoint, radius: f64) -> f64 {
    let (dx, dy) = (at.x - centre.x, at.y - centre.y);
    (dx * dx + dy * dy) / (radius * radius)
}

/// `w = falloff(d²) * max(0, 1 + amount * n)` in f64, then to f32.
fn modulate(w: f32, amount: f32, n: f64) -> f32 {
    (w as f64 * (1.0 + amount.clamp(0.0, 1.0) as f64 * n).max(0.0)) as f32
}

/// Weight of a stamp at `centre` with `radius` (metres) at root-space `at`, 0 outside the disc:
/// `brush_weight(d², falloff) * max(0, 1 + amount * detail_noise(seed, at, scale, mpp))`, with
/// `d² = |at - centre|² / radius²`. The weight never goes below 0, so detail never turns a
/// stamp's effect around. `at` is a pixel centre `((origin / mpp).round() + i + 0.5) * mpp`.
pub fn stamp_weight(
    at: RootPoint,
    centre: RootPoint,
    radius: f64,
    falloff: f32,
    detail: Detail,
    seed: u64,
    mpp: f64,
) -> f32 {
    let d2 = dist2(at, centre, radius);
    if d2 >= 1.0 {
        return 0.0;
    }
    let n = if detail.amount > 0.0 {
        detail_noise(seed, at, detail.scale, mpp)
    } else {
        0.0
    };
    modulate(weight_in(d2, brush_core(falloff)), detail.amount, n)
}

/// The detail noise's lattice corners over one stamp, so pixels interpolate without hashing.
/// Gives the same bits as `detail_noise`.
struct Lattice {
    octaves: Vec<(f64, i64, i64, usize, Vec<f64>)>,
    scale: f64,
    mpp: f64,
}

impl Lattice {
    fn new(seed: u64, scale: f64, mpp: f64, lo: RootPoint, hi: RootPoint) -> Lattice {
        let prefix = rng::prefix(seed, DETAIL_STAGE);
        let mut octaves = vec![];
        let mut cell = scale;
        for _ in 0..OCTAVES {
            let (ix0, iy0) = (
                (lo.x / cell).floor() as i64 - 1,
                (lo.y / cell).floor() as i64 - 1,
            );
            let (ix1, iy1) = (
                (hi.x / cell).floor() as i64 + 2,
                (hi.y / cell).floor() as i64 + 2,
            );
            let w = (ix1 - ix0 + 1).max(0) as usize;
            let h = (iy1 - iy0 + 1).max(0) as usize;
            let mut v = vec![];
            if cell >= 2.0 * mpp && w > 0 && h > 0 {
                v = vec![0.0; w * h];
                let row = |iy: i64, out: &mut [f64]| {
                    for (ix, o) in (ix0..).zip(out) {
                        let p = RootPoint {
                            x: ix as f64 * cell,
                            y: iy as f64 * cell,
                        };
                        *o = rng::to_unit(rng::hash_from(prefix, Key::At(p)));
                    }
                };
                // A fine octave under a large stamp has as many corners as the stamp has pixels.
                let threads = if w * h >= 1 << 14 { threads() } else { 1 };
                let per = h.div_ceil(threads);
                std::thread::scope(|sc| {
                    for (c, chunk) in v.chunks_mut(per * w).enumerate() {
                        sc.spawn(move || {
                            for (r, out) in chunk.chunks_mut(w).enumerate() {
                                row(iy0 + (c * per + r) as i64, out);
                            }
                        });
                    }
                });
            }
            octaves.push((cell, ix0, iy0, w, v));
            cell *= 0.5;
        }
        Lattice {
            octaves,
            scale,
            mpp,
        }
    }

    fn noise(&self, at: RootPoint) -> f64 {
        fractal(self.scale, self.mpp, |o, cell| {
            let (_, ix0, iy0, w, v) = &self.octaves[o];
            rng::noise_with(at, cell, |cx, cy| {
                v[(cy as i64 - iy0) as usize * w + (cx as i64 - ix0) as usize]
            })
        })
    }
}

fn threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Prefix sums over a pixel box of a layer, for box means. Values are fixed point (`FIXED`
/// per metre) so sums are exact: a mean does not depend on where the box starts, so
/// neighbouring maps agree.
struct Sums {
    x0: i64,
    y0: i64,
    w: usize,
    h: usize,
    s: Vec<i64>,
}

const FIXED: f64 = (1u64 << 20) as f64;

impl Sums {
    fn new(tiles: &EditTiles, (x0, x1): (i64, i64), (y0, y1): (i64, i64)) -> Sums {
        let t = TILE_SIZE as i64;
        let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
        let mut s = vec![0i64; (w + 1) * (h + 1)];
        for y in y0..y1 {
            let r = (y - y0) as usize + 1;
            let mut acc = 0i64;
            let mut x = x0;
            while x < x1 {
                let tx = x / t;
                let end = x1.min((tx + 1) * t);
                let vals = &tiles[&(tx as u32, (y / t) as u32)].1;
                let row = ((y % t) * t) as usize;
                for xx in x..end {
                    acc += (vals[row + (xx % t) as usize] as f64 * FIXED).round() as i64;
                    let c = (xx - x0) as usize + 1;
                    s[r * (w + 1) + c] = s[(r - 1) * (w + 1) + c] + acc;
                }
                x = end;
            }
        }
        Sums { x0, y0, w, h, s }
    }

    /// Mean of the pixels within `k` of (`x`, `y`) inside the box.
    fn mean(&self, x: i64, y: i64, k: i64) -> f32 {
        let a = |v: i64, lo: i64, n: usize| (v - lo).clamp(0, n as i64) as usize;
        let (xa, xb) = (a(x - k, self.x0, self.w), a(x + k + 1, self.x0, self.w));
        let (ya, yb) = (a(y - k, self.y0, self.h), a(y + k + 1, self.y0, self.h));
        let at = |c: usize, r: usize| self.s[r * (self.w + 1) + c];
        let sum = at(xb, yb) - at(xa, yb) - at(xb, ya) + at(xa, ya);
        (sum as f64 / FIXED / ((xb - xa) * (yb - ya)) as f64) as f32
    }
}

impl Project {
    /// Adds the stroke to `tiles`, loading the ones it reaches first; gives the tiles it changed.
    /// With `cached`, a tile in the tile cache is taken from it and its record is read later.
    /// `level` is the stroke's flatten level; `None` samples it under the first stamp.
    pub(crate) fn brush_into(
        &self,
        map: &Map,
        s: &BrushStroke,
        tiles: &mut EditTiles,
        cached: bool,
        level: &mut Option<f32>,
    ) -> Result<BTreeSet<(u32, u32)>> {
        let mut touched = BTreeSet::new();
        let mpp = map.settings.metres_per_pixel;
        let noisy = s.detail.amount > 0.0 || s.mode == BrushMode::Roughen;
        let seed = if noisy { self.seed()? } else { 0 };
        let (gx, gy) = ((map.origin.x / mpp).round(), (map.origin.y / mpp).round());
        let root = |x: i64, y: i64| RootPoint {
            x: (gx + x as f64 + 0.5) * mpp,
            y: (gy + y as f64 + 0.5) * mpp,
        };
        let (w, h) = (map.settings.width as i64, map.settings.height as i64);
        let t = TILE_SIZE as i64;
        let tiles_of = |a: i64, b: i64| a / t..(b - 1) / t + 1;
        let core = brush_core(s.falloff);
        let raise = s.strength > 0.0;
        for p in &s.points {
            let r = p.radius / mpp;
            let (cx, cy) = map.to_pixels(p.at);
            let lattice = noisy.then(|| {
                let (lo, hi) = (
                    RootPoint {
                        x: p.at.x - p.radius,
                        y: p.at.y - p.radius,
                    },
                    RootPoint {
                        x: p.at.x + p.radius,
                        y: p.at.y + p.radius,
                    },
                );
                Lattice::new(seed, s.detail.scale, mpp, lo, hi)
            });
            // Tiles loaded here only to read; those the stamp does not change are dropped after.
            let mut read = BTreeSet::new();
            if s.mode == BrushMode::Flatten && level.is_none() && w > 0 && h > 0 {
                let (x, y) = (
                    (cx.floor() as i64).clamp(0, w - 1),
                    (cy.floor() as i64).clamp(0, h - 1),
                );
                read = self.load_tiles(
                    map,
                    s.layer,
                    (x / t..x / t + 1, y / t..y / t + 1),
                    (tiles, cached),
                )?;
                *level = Some(
                    tiles[&((x / t) as u32, (y / t) as u32)].1[((y % t) * t + x % t) as usize],
                );
            }
            let k = (r / 4.0).ceil().max(1.0) as i64;
            let sums = if s.mode == BrushMode::Smooth {
                let bx = (
                    ((cx - r).floor() as i64 - k).max(0),
                    ((cx + r).ceil() as i64 + k).min(w),
                );
                let by = (
                    ((cy - r).floor() as i64 - k).max(0),
                    ((cy + r).ceil() as i64 + k).min(h),
                );
                if bx.0 >= bx.1 || by.0 >= by.1 {
                    continue;
                }
                read.extend(self.load_tiles(
                    map,
                    s.layer,
                    (tiles_of(bx.0, bx.1), tiles_of(by.0, by.1)),
                    (tiles, cached),
                )?);
                Some(Sums::new(tiles, bx, by))
            } else {
                None
            };
            let lvl = level.unwrap_or(0.0);
            let amount = s.strength * p.pressure;
            self.disc(
                map,
                s.layer,
                (cx, cy),
                r,
                (tiles, cached),
                &mut touched,
                |v, _, x, y| {
                    let at = root(x, y);
                    let d2 = dist2(at, p.at, p.radius);
                    if d2 >= 1.0 {
                        return false;
                    }
                    let fall = weight_in(d2, core);
                    let n = lattice.as_ref().map_or(0.0, |l| l.noise(at));
                    let wt = modulate(fall, s.detail.amount, n);
                    let old = *v;
                    match s.mode {
                        BrushMode::Add => match s.target {
                            None => *v += amount * wt,
                            Some(t) if (*v < t) == raise && *v != t => {
                                *v += (t - *v) * (p.pressure * wt).min(1.0)
                            }
                            Some(_) => return false,
                        },
                        BrushMode::Smooth => {
                            let m = sums.as_ref().expect("smooth sums").mean(x, y, k);
                            *v += (m - *v) * (amount * wt).min(1.0)
                        }
                        BrushMode::Flatten => *v += (lvl - *v) * (amount * wt).min(1.0),
                        BrushMode::Roughen => *v += amount * fall * n as f32,
                    }
                    *v != old
                },
            )?;
            for key in read {
                if !touched.contains(&key) {
                    tiles.remove(&key);
                }
            }
        }
        Ok(touched)
    }
}
