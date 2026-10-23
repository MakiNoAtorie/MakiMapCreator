//! Brush stamps with root-space detail; `stamp_weight` and `detail_noise` are the reference (ADR-0025).

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
    /// `v += (mean - v) * min(1, strength * p * w)`, `mean` the box mean around (ADR-0025).
    Smooth,
    /// `v += (level - v) * min(1, strength * p * w)`, `level` read under the first stamp.
    Flatten,
    /// `v += strength * p * falloff weight * detail_noise`: no net raise, the noise has mean 0.
    Roughen,
}

/// Root-space noise on a brush's stamp weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detail {
    /// 0 is the smooth falloff; 1 scales the weight by 0 to 2. Used clamped (`amount()`).
    pub amount: f32,
    /// Cell of the coarsest octave, metres.
    pub scale: f64,
}

impl Detail {
    pub const NONE: Detail = Detail {
        amount: 0.0,
        scale: 1000.0,
    };

    pub fn amount(self) -> f32 {
        self.amount.clamp(0.0, 1.0)
    }
}

impl BrushStroke {
    /// The stroke reads detail noise: detail on its weight, or roughen.
    pub fn noisy(&self) -> bool {
        self.detail.amount > 0.0 || self.mode == BrushMode::Roughen
    }
}

/// `rng` stage of the brush detail noise; the seed is the project seed.
pub const DETAIL_STAGE: &str = "brush-detail";
const OCTAVES: u32 = 4;

/// Detail noise is evaluated every `NODE` global pixels and interpolated between.
const NODE: i64 = 4;
/// Octaves with a cell under this many pixels are skipped.
const MIN_CELL: f64 = (2 * NODE) as f64;
/// Sum of all octave amplitudes, skipped ones included.
const TOTAL: f64 = 1.875;

/// Octave amplitudes and cells, coarse first; `false` marks a skipped octave.
fn octaves(scale: f64, mpp: f64) -> impl Iterator<Item = (f64, f64, bool)> {
    (0..OCTAVES as i32).map(move |o| {
        let f = 0.5f64.powi(o);
        (f, scale * f, scale * f >= MIN_CELL * mpp)
    })
}

/// Detail noise in -1..1 at the pixel whose centre is `at`; the maths is in ADR-0025.
pub fn detail_noise(seed: u64, at: RootPoint, scale: f64, mpp: f64) -> f64 {
    let g = (
        (at.x / mpp - 0.5).round() as i64,
        (at.y / mpp - 0.5).round() as i64,
    );
    interpolate(g, |nx, ny| {
        let node = node_at(nx, ny, mpp);
        fractal(scale, mpp, |_, cell| {
            rng::noise(seed, DETAIL_STAGE, node, cell)
        })
    })
}

fn node_at(nx: i64, ny: i64, mpp: f64) -> RootPoint {
    RootPoint {
        x: ((nx * NODE) as f64 + 0.5) * mpp,
        y: ((ny * NODE) as f64 + 0.5) * mpp,
    }
}

/// Bilinear mix at global pixel `g` of the node values `node(kx, ky)`.
fn interpolate((gx, gy): (i64, i64), node: impl Fn(i64, i64) -> f64) -> f64 {
    let (nx, ny) = (gx.div_euclid(NODE), gy.div_euclid(NODE));
    let f = NODE as f64;
    let (fx, fy) = (
        gx.rem_euclid(NODE) as f64 / f,
        gy.rem_euclid(NODE) as f64 / f,
    );
    let left = lerp(node(nx, ny), node(nx, ny + 1), fy);
    let right = lerp(node(nx + 1, ny), node(nx + 1, ny + 1), fy);
    lerp(left, right, fx)
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn fractal(scale: f64, mpp: f64, noise: impl Fn(usize, f64) -> f64) -> f64 {
    let mut sum = 0.0;
    for (o, (amp, cell, kept)) in octaves(scale, mpp).enumerate() {
        if kept {
            sum += amp * (2.0 * noise(o, cell) - 1.0);
        }
    }
    sum / TOTAL
}

/// `detail_noise` over a range of global pixels as tables, for a port without u64 or f64 (WGSL).
pub struct DetailTables {
    /// Node index of column and row 0 of the node tables.
    pub nx0: i64,
    pub ny0: i64,
    /// Sum of all octave amplitudes, skipped ones included.
    pub total: f64,
    pub octaves: Vec<OctaveTable>,
}

/// One octave that is not skipped.
pub struct OctaveTable {
    pub amp: f64,
    /// Per node column (row): lattice column (row) below the node and smoothstep weight to the next.
    pub node_x: Vec<(u32, f64)>,
    pub node_y: Vec<(u32, f64)>,
    /// Per lattice column the hash state after its x; per lattice row the word its y mixes in.
    /// A corner's value is `to_unit(splitmix64_finaliser(cols[i] ^ rows[j]))`.
    pub cols: Vec<u64>,
    pub rows: Vec<u64>,
}

/// Tables for global pixels `xs` x `ys` (inclusive).
pub fn detail_tables(
    seed: u64,
    scale: f64,
    mpp: f64,
    xs: (i64, i64),
    ys: (i64, i64),
) -> DetailTables {
    let prefix = rng::prefix(seed, DETAIL_STAGE);
    let (nx0, nx1) = (xs.0.div_euclid(NODE), xs.1.div_euclid(NODE) + 1);
    let (ny0, ny1) = (ys.0.div_euclid(NODE), ys.1.div_euclid(NODE) + 1);
    let mut tables = vec![];
    for (amp, cell, kept) in octaves(scale, mpp) {
        if kept {
            let axis = |n0: i64, n1: i64, at: &dyn Fn(i64) -> f64| {
                let v: Vec<(f64, f64)> = (n0..=n1).map(|n| rng::axis(at(n), cell)).collect();
                let i0 = v[0].0;
                let nodes = v.iter().map(|&(g, f)| ((g - i0) as u32, f)).collect();
                (nodes, i0 as i64, v[v.len() - 1].0 as i64 + 1)
            };
            let (node_x, ix0, ix1) = axis(nx0, nx1, &|n| node_at(n, 0, mpp).x);
            let (node_y, iy0, iy1) = axis(ny0, ny1, &|n| node_at(0, n, mpp).y);
            tables.push(OctaveTable {
                amp,
                node_x,
                node_y,
                cols: (ix0..=ix1)
                    .map(|i| rng::at_x(prefix, i as f64 * cell))
                    .collect(),
                rows: (iy0..=iy1).map(|j| rng::at_y(j as f64 * cell)).collect(),
            });
        }
    }
    DetailTables {
        nx0,
        ny0,
        total: TOTAL,
        octaves: tables,
    }
}

impl DetailTables {
    /// Detail noise at global pixel `g`, from the tables: the reference for the WGSL port.
    pub fn noise_at(&self, (gx, gy): (i64, i64)) -> f64 {
        interpolate((gx, gy), |nx, ny| {
            let (kx, ky) = ((nx - self.nx0) as usize, (ny - self.ny0) as usize);
            let mut sum = 0.0;
            for o in &self.octaves {
                let ((ix, fx), (iy, fy)) = (o.node_x[kx], o.node_y[ky]);
                let (ix, iy) = (ix as usize, iy as usize);
                let c = |i: usize, j: usize| rng::to_unit(rng::at_finish(o.cols[i], o.rows[j]));
                let n = rng::mix4(
                    [c(ix, iy), c(ix + 1, iy), c(ix, iy + 1), c(ix + 1, iy + 1)],
                    fx,
                    fy,
                );
                sum += o.amp * (2.0 * n - 1.0);
            }
            sum / self.total
        })
    }
}

/// Squared distance from `at` to `centre` as a fraction of `radius`, all root-space metres.
fn dist2(at: RootPoint, centre: RootPoint, radius: f64) -> f64 {
    let (dx, dy) = (at.x - centre.x, at.y - centre.y);
    (dx * dx + dy * dy) / (radius * radius)
}

/// `w = falloff(d²) * max(0, 1 + amount * n)` in f64, then to f32.
fn modulate(w: f32, d: Detail, n: f64) -> f32 {
    (w as f64 * (1.0 + d.amount() as f64 * n).max(0.0)) as f32
}

/// Weight of a stamp at the pixel centre `at`, 0 outside the disc; the maths is in ADR-0025.
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
    modulate(weight_in(d2, brush_core(falloff)), detail, n)
}

/// One octave's hashed lattice corners over a stamp, rows of `w` from (`ix0`, `iy0`).
struct LatticeOctave {
    cell: f64,
    ix0: i64,
    iy0: i64,
    w: usize,
    corners: Vec<f64>,
}

/// The detail noise's lattice corners over one stamp; same bits as `detail_noise`.
struct Lattice {
    octaves: Vec<LatticeOctave>,
}

impl Lattice {
    fn new(seed: u64, scale: f64, mpp: f64, lo: RootPoint, hi: RootPoint) -> Lattice {
        let prefix = rng::prefix(seed, DETAIL_STAGE);
        let mut list = vec![];
        for (_, cell, kept) in octaves(scale, mpp) {
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
            let mut corners = vec![];
            if kept && w > 0 && h > 0 {
                corners = vec![0.0; w * h];
                par_rows(&mut corners, w, |r, out| {
                    let iy = iy0 + r as i64;
                    for (ix, o) in (ix0..).zip(out) {
                        let p = RootPoint {
                            x: ix as f64 * cell,
                            y: iy as f64 * cell,
                        };
                        *o = rng::to_unit(rng::hash_from(prefix, Key::At(p)));
                    }
                });
            }
            list.push(LatticeOctave {
                cell,
                ix0,
                iy0,
                w,
                corners,
            });
        }
        Lattice { octaves: list }
    }

    /// Per coordinate in `vs`, octave `o`'s corner offset and smoothstep weight along x or y.
    fn axis(&self, o: usize, y: bool, vs: impl Iterator<Item = f64>) -> Vec<(usize, f64)> {
        let oct = &self.octaves[o];
        let i0 = if y { oct.iy0 } else { oct.ix0 };
        vs.map(|v| {
            let (g0, f) = rng::axis(v, oct.cell);
            ((g0 as i64 - i0) as usize, f)
        })
        .collect()
    }
}

/// Detail noise under one stamp, node values mixed in y per pixel row; same bits as `detail_noise`.
struct Nodes {
    nx0: i64,
    gy0: i64,
    w: usize,
    /// Row `gy - gy0`, node column `nx - nx0`.
    rows: Vec<f64>,
}

impl Nodes {
    /// Nodes for global pixels `gx0..=gx1` x `gy0..=gy1`.
    fn new(
        seed: u64,
        d: Detail,
        mpp: f64,
        (gx0, gx1): (i64, i64),
        (gy0, gy1): (i64, i64),
    ) -> Nodes {
        let (nx0, ny0) = (gx0.div_euclid(NODE), gy0.div_euclid(NODE));
        let (nx1, ny1) = (gx1.div_euclid(NODE) + 1, gy1.div_euclid(NODE) + 1);
        let lattice = Lattice::new(
            seed,
            d.scale,
            mpp,
            node_at(nx0, ny0, mpp),
            node_at(nx1, ny1, mpp),
        );
        let w = (nx1 - nx0 + 1) as usize;
        let axes: Vec<_> = (0..OCTAVES as usize)
            .map(|o| {
                let xs = (nx0..=nx1).map(|n| node_at(n, 0, mpp).x);
                let ys = (ny0..=ny1).map(|n| node_at(0, n, mpp).y);
                (lattice.axis(o, false, xs), lattice.axis(o, true, ys))
            })
            .collect();
        let mut v = vec![0.0; w * (ny1 - ny0 + 1) as usize];
        par_rows(&mut v, w, |r, out| {
            for (col, o) in out.iter_mut().enumerate() {
                // Same operations as `rng::noise` at the node, with corners from the lattice.
                *o = fractal(d.scale, mpp, |oct, _| {
                    let (lw, lv) = (lattice.octaves[oct].w, &lattice.octaves[oct].corners);
                    let ((ix, fx), (iy, fy)) = (axes[oct].0[col], axes[oct].1[r]);
                    let at = iy * lw + ix;
                    rng::mix4([lv[at], lv[at + 1], lv[at + lw], lv[at + lw + 1]], fx, fy)
                });
            }
        });
        let mut rows = vec![0.0; w * (gy1 - gy0 + 1) as usize];
        par_rows(&mut rows, w, |r, out| {
            let gy = gy0 + r as i64;
            let fy = gy.rem_euclid(NODE) as f64 / NODE as f64;
            let at = (gy.div_euclid(NODE) - ny0) as usize * w;
            let (a, c) = (&v[at..at + w], &v[at + w..at + 2 * w]);
            for ((o, a), c) in out.iter_mut().zip(a).zip(c) {
                *o = lerp(*a, *c, fy);
            }
        });
        Nodes { nx0, gy0, w, rows }
    }

    /// Detail noise at global pixel `g`.
    fn at(&self, (gx, gy): (i64, i64)) -> f64 {
        let i = (gy - self.gy0) as usize * self.w + (gx.div_euclid(NODE) - self.nx0) as usize;
        let fx = gx.rem_euclid(NODE) as f64 / NODE as f64;
        lerp(self.rows[i], self.rows[i + 1], fx)
    }
}

/// Fills `v`, rows of `w`, with `f(row, values)`; on scoped threads when it is large.
fn par_rows(v: &mut [f64], w: usize, f: impl Fn(usize, &mut [f64]) + Sync) {
    let rows = v.len() / w;
    let threads = if v.len() >= 1 << 13 {
        std::thread::available_parallelism().map_or(1, |n| n.get())
    } else {
        1
    };
    // A thread costs tens of µs; small stamps run here.
    if threads <= 1 || rows <= 1 {
        v.chunks_mut(w).enumerate().for_each(|(r, out)| f(r, out));
        return;
    }
    let per = rows.div_ceil(threads).max(1);
    let f = &f;
    std::thread::scope(|sc| {
        for (c, chunk) in v.chunks_mut(per * w).enumerate() {
            sc.spawn(move || {
                for (r, out) in chunk.chunks_mut(w).enumerate() {
                    f(c * per + r, out);
                }
            });
        }
    });
}

/// Exact fixed-point prefix sums over a pixel box, so a mean does not depend on the box start.
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
    /// Adds the stroke to `tiles` and gives the tiles it changed; `level` `None` samples flatten's.
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
        let noisy = s.noisy();
        let seed = if noisy { self.seed()? } else { 0 };
        let (gx, gy) = ((map.origin.x / mpp).round(), (map.origin.y / mpp).round());
        let (gxi, gyi) = (gx as i64, gy as i64);
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
            let nodes = noisy.then(|| {
                let xs = (gxi + (cx - r).floor() as i64, gxi + (cx + r).ceil() as i64);
                let ys = (gyi + (cy - r).floor() as i64, gyi + (cy + r).ceil() as i64);
                Nodes::new(seed, s.detail, mpp, xs, ys)
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
                    let n = nodes.as_ref().map_or(0.0, |l| l.at((gxi + x, gyi + y)));
                    let wt = modulate(fall, s.detail, n);
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
