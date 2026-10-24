//! Brush stamps with root-space detail; `stamp_weight` and `detail_noise` are the reference (ADR-0025).

use std::collections::BTreeSet;

use crate::command::{BrushStroke, EditTiles, brush_core, weight_in};
use crate::layer::{RootPoint, TILE_SIZE, WATER_LEVEL};
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
    /// `roughen(v, strength * p * falloff * noise)`; the noise has mean 0 and land stays land.
    Roughen,
}

/// Root-space noise on a brush's stamp weight.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detail {
    /// 0 to 1, clamped in use; 0 keeps the smooth falloff.
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
const OCTAVES: i32 = 6;
/// Each octave's cell is the one before over this; not 2, so octaves never share lattice lines.
pub const LACUNARITY: f64 = 2.01;
/// Turn of each octave's lattice from the one before (36.87 degrees), so no lattice axis shows.
const TURN: (f64, f64) = (0.8, 0.6);

/// Detail noise is evaluated every `NODE` global pixels and interpolated between.
const NODE: i64 = 4;
/// Octaves with a cell under this many pixels are skipped.
const MIN_CELL: f64 = 7.0;
/// The value noise is its octave sum over this; no clamp.
pub const VALUE_SPREAD: f64 = 0.45;
/// The ridged noise is (octave sum - `RIDGED_MID`) / `RIDGED_SPREAD`; no clamp.
pub const RIDGED_MID: f64 = 1.0;
pub const RIDGED_SPREAD: f64 = 0.6;
/// Raising adds blend from value noise to ridged noise between these heights, metres.
pub const RIDGE_HEIGHTS: (f32, f32) = (150.0, 1200.0);
/// Ridged noise counts this many times, so that half detail carves full ridges.
pub const RIDGE_GAIN: f64 = 2.0;
/// Detail fades to 0 within this many metres of `WATER_LEVEL`, so it never speckles the coast.
pub const COAST_BAND: f32 = 20.0;

const S: f64 = std::f64::consts::FRAC_1_SQRT_2;
/// Picked by the top 3 bits of the corner hash; `live.wgsl` has the same table.
const GRADIENTS: [(f64, f64); 8] = [
    (1.0, 0.0),
    (-1.0, 0.0),
    (0.0, 1.0),
    (0.0, -1.0),
    (S, S),
    (-S, S),
    (S, -S),
    (-S, -S),
];

/// Detail noise at a pixel, both about -1..1 with mean 0, not clamped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailNoise {
    /// Gradient noise: lumps.
    pub value: f64,
    /// Ridged multifractal: crests, with smooth valleys.
    pub ridged: f64,
}

fn smoothstep(lo: f32, hi: f32, v: f32) -> f64 {
    let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) as f64;
    t * t * (3.0 - 2.0 * t)
}

impl DetailNoise {
    /// Ridged noise on high ground when the stamp raises; 0 at the water level, so coasts stay.
    pub fn at(self, height: f32, raise: bool) -> f64 {
        let n = if raise {
            let s = smoothstep(RIDGE_HEIGHTS.0, RIDGE_HEIGHTS.1, height);
            self.value * (1.0 - s) + RIDGE_GAIN * self.ridged * s
        } else {
            self.value
        };
        n * smoothstep(0.0, COAST_BAND, (height - WATER_LEVEL).abs())
    }
}

/// An octave that is not skipped; root point `p` is at lattice point `a * p + off`.
struct Octave {
    amp: f64,
    a: [f64; 4],
    off: (f64, f64),
    prefix: u64,
}

impl Octave {
    /// The parts from root x and root y, kept apart so that a port adds them exactly.
    fn parts(&self, p: RootPoint) -> ((f64, f64), (f64, f64)) {
        let a = self.a;
        (
            (a[0] * p.x + self.off.0, a[2] * p.x + self.off.1),
            (a[1] * p.y, a[3] * p.y),
        )
    }

    fn lattice(&self, p: RootPoint) -> (f64, f64) {
        let ((x0, y0), (x1, y1)) = self.parts(p);
        (x0 + x1, y0 + y1)
    }
}

/// The octaves of `scale` that are not skipped at `mpp`, coarse first.
fn octaves(seed: u64, scale: f64, mpp: f64) -> Vec<Octave> {
    let prefix = rng::prefix(seed, DETAIL_STAGE);
    let (mut c, mut s) = (1.0, 0.0);
    let mut list = vec![];
    for o in 0..OCTAVES {
        let cell = scale / LACUNARITY.powi(o);
        if cell < MIN_CELL * mpp {
            break;
        }
        let k = 1.0 / cell;
        let f = (o + 1) as f64;
        list.push(Octave {
            amp: 0.5f64.powi(o),
            a: [c * k, -s * k, s * k, c * k],
            // Off the origin, where every octave would have a corner.
            off: (0.37 * f, 0.71 * f),
            prefix: rng::hash_from(prefix, Key::Entity(o as i64)),
        });
        (c, s) = (c * TURN.0 - s * TURN.1, s * TURN.0 + c * TURN.1);
    }
    list
}

fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// Gradient noise at lattice corner `c` plus fraction `f` (0..1): about -0.71..0.71.
fn gradient((cx, cy): (i64, i64), (fx, fy): (f64, f64), prefix: u64) -> f64 {
    let dot = |i: i64, j: i64| {
        let g = GRADIENTS[(rng::lattice(prefix, cx + i, cy + j) >> 61) as usize];
        g.0 * (fx - i as f64) + g.1 * (fy - j as f64)
    };
    rng::mix4(
        [dot(0, 0), dot(1, 0), dot(0, 1), dot(1, 1)],
        fade(fx),
        fade(fy),
    )
}

/// Value and ridged sums of (amplitude, noise), coarse first; ridged is Musgrave's multifractal.
fn combine(octaves: impl Iterator<Item = (f64, f64)>) -> (f64, f64) {
    let (mut value, mut ridged, mut weight) = (0.0, 0.0, 1.0);
    for (amp, n) in octaves {
        value += amp * n;
        let s = 1.0 - (n * std::f64::consts::SQRT_2).abs();
        let signal = s * s * weight;
        ridged += amp * signal;
        // Musgrave's min(1, 2 * signal), without the crease.
        weight = signal * (2.0 - signal);
    }
    (value / VALUE_SPREAD, (ridged - RIDGED_MID) / RIDGED_SPREAD)
}

/// Value and ridged noise of `octaves` at root point `p`.
fn node_noise(octaves: &[Octave], p: RootPoint) -> (f64, f64) {
    combine(octaves.iter().map(|o| {
        let (x, y) = o.lattice(p);
        let (x0, y0) = (x.floor(), y.floor());
        let n = gradient((x0 as i64, y0 as i64), (x - x0, y - y0), o.prefix);
        (o.amp, n)
    }))
}

/// Detail noise at the pixel whose centre is `at`; the maths is in ADR-0025.
pub fn detail_noise(seed: u64, at: RootPoint, scale: f64, mpp: f64) -> DetailNoise {
    let g = (
        (at.x / mpp - 0.5).round() as i64,
        (at.y / mpp - 0.5).round() as i64,
    );
    let octaves = octaves(seed, scale, mpp);
    interpolate(g, |nx, ny| node_noise(&octaves, node_at(nx, ny, mpp)))
}

fn node_at(nx: i64, ny: i64, mpp: f64) -> RootPoint {
    RootPoint {
        x: ((nx * NODE) as f64 + 0.5) * mpp,
        y: ((ny * NODE) as f64 + 0.5) * mpp,
    }
}

/// Middle node and spline parameter at global pixel `g`; the spline reads nodes k - 1..=k + 1.
fn spline_at(g: i64) -> (i64, f64) {
    let h = g + NODE / 2;
    (h.div_euclid(NODE), h.rem_euclid(NODE) as f64 / NODE as f64)
}

/// Uniform quadratic B-spline of node values `a`, `b`, `c` at `t`: smooth across nodes,
/// so hillshade shows no node grid.
fn spline(a: f64, b: f64, c: f64, t: f64) -> f64 {
    let s = 1.0 - t;
    0.5 * (s * s * a + (1.0 + 2.0 * t * s) * b + t * t * c)
}

/// Spline at global pixel `g` of the node values `node(kx, ky)`, first in y, then in x.
fn interpolate((gx, gy): (i64, i64), node: impl Fn(i64, i64) -> (f64, f64)) -> DetailNoise {
    let ((kx, tx), (ky, ty)) = (spline_at(gx), spline_at(gy));
    let col = |x: i64| {
        let (a, b, c) = (node(x, ky - 1), node(x, ky), node(x, ky + 1));
        (spline(a.0, b.0, c.0, ty), spline(a.1, b.1, c.1, ty))
    };
    let (a, b, c) = (col(kx - 1), col(kx), col(kx + 1));
    DetailNoise {
        value: spline(a.0, b.0, c.0, tx),
        ridged: spline(a.1, b.1, c.1, tx),
    }
}

/// `detail_noise` over a range of global pixels as tables, for a port without u64 or f64 (WGSL).
pub struct DetailTables {
    /// Node index of column and row 0 of the node tables.
    pub nx0: i64,
    pub ny0: i64,
    pub octaves: Vec<OctaveTable>,
}

/// One octave that is not skipped.
pub struct OctaveTable {
    pub amp: f64,
    /// The corner hash is `rng::lattice(prefix, ix, iy)`.
    pub prefix: u64,
    /// Corner and 2^-32 fraction per node column and row; a port adds them without rounding.
    pub cols: Vec<((i64, i64), (u32, u32))>,
    pub rows: Vec<((i64, i64), (u32, u32))>,
}

const FRACTION: f64 = 4294967296.0;

fn split((x, y): (f64, f64)) -> ((i64, i64), (u32, u32)) {
    let part = |v: f64| {
        let w = v.floor();
        (w as i64, ((v - w) * FRACTION) as u32)
    };
    let ((x0, fx), (y0, fy)) = (part(x), part(y));
    ((x0, y0), (fx, fy))
}

/// Tables for global pixels `xs` x `ys` (inclusive).
pub fn detail_tables(
    seed: u64,
    scale: f64,
    mpp: f64,
    xs: (i64, i64),
    ys: (i64, i64),
) -> DetailTables {
    // Node column 0 is the one before the node of `xs.0` rounded down, wherever `xs.0` lies.
    let (nx0, nx1) = (xs.0.div_euclid(NODE) - 1, spline_at(xs.1).0 + 1);
    let (ny0, ny1) = (ys.0.div_euclid(NODE) - 1, spline_at(ys.1).0 + 1);
    let octaves = octaves(seed, scale, mpp)
        .into_iter()
        .map(|o| OctaveTable {
            amp: o.amp,
            prefix: o.prefix,
            cols: (nx0..=nx1)
                .map(|n| split(o.parts(node_at(n, 0, mpp)).0))
                .collect(),
            rows: (ny0..=ny1)
                .map(|n| split(o.parts(node_at(0, n, mpp)).1))
                .collect(),
        })
        .collect();
    DetailTables { nx0, ny0, octaves }
}

impl DetailTables {
    /// Detail noise at global pixel `g`, from the tables: the reference for the WGSL port.
    pub fn noise_at(&self, (gx, gy): (i64, i64)) -> DetailNoise {
        interpolate((gx, gy), |nx, ny| {
            let (kx, ky) = ((nx - self.nx0) as usize, (ny - self.ny0) as usize);
            combine(self.octaves.iter().map(|o| {
                let ((cx, cy), (fx, fy)) = o.cols[kx];
                let ((rx, ry), (gx, gy)) = o.rows[ky];
                let (fx, fy) = (fx as u64 + gx as u64, fy as u64 + gy as u64);
                let corner = (cx + rx + (fx >> 32) as i64, cy + ry + (fy >> 32) as i64);
                let f = |v: u64| (v as u32) as f64 / FRACTION;
                (o.amp, gradient(corner, (f(fx), f(fy)), o.prefix))
            }))
        })
    }
}

/// Squared distance from `at` to `centre` as a fraction of `radius`, all root-space metres.
fn dist2(at: RootPoint, centre: RootPoint, radius: f64) -> f64 {
    let (dx, dy) = (at.x - centre.x, at.y - centre.y);
    (dx * dx + dy * dy) * (1.0 / (radius * radius))
}

/// Not tanh: WARP's exp and tanh are too coarse for the GPU port's 1e-3 m.
fn soft(x: f64) -> f64 {
    x / (1.0 + x * x).sqrt()
}

fn modulate(w: f32, d: Detail, n: f64) -> f32 {
    (w as f64 * (1.0 + soft(d.amount() as f64 * n))) as f32
}

/// At most the distance to the water level, so land stays land and sea stays sea.
fn roughen(v: f32, d: f64) -> f32 {
    let a = (v - WATER_LEVEL).abs() as f64;
    if a == 0.0 {
        return v;
    }
    let next = v + (a * soft(d / a)) as f32;
    if (next > WATER_LEVEL) == (v > WATER_LEVEL) {
        next
    } else {
        v
    }
}

/// Weight of a stamp at the pixel centre `at` with detail noise `n` there (`DetailNoise::at`),
/// 0 outside the disc; the maths is in ADR-0025.
pub fn stamp_weight(
    at: RootPoint,
    centre: RootPoint,
    radius: f64,
    falloff: f32,
    detail: Detail,
    n: f64,
) -> f32 {
    let d2 = dist2(at, centre, radius);
    if d2 >= 1.0 {
        return 0.0;
    }
    modulate(weight_in(d2, brush_core(falloff)), detail, n)
}

/// Detail noise under one stamp, node values mixed in y per pixel row; same bits as `detail_noise`.
struct Nodes {
    nx0: i64,
    gy0: i64,
    /// Values per row: (value, ridged) per node column.
    w: usize,
    /// Row `gy - gy0`, node column `nx - nx0`, value then ridged.
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
        let (nx0, ny0) = (spline_at(gx0).0 - 1, spline_at(gy0).0 - 1);
        let (nx1, ny1) = (spline_at(gx1).0 + 1, spline_at(gy1).0 + 1);
        let octaves = octaves(seed, d.scale, mpp);
        let w = 2 * (nx1 - nx0 + 1) as usize;
        let mut v = vec![0.0; w * (ny1 - ny0 + 1) as usize];
        par_rows(&mut v, w, |r, out| {
            for (col, o) in out.chunks_mut(2).enumerate() {
                let p = node_at(nx0 + col as i64, ny0 + r as i64, mpp);
                (o[0], o[1]) = node_noise(&octaves, p);
            }
        });
        let mut rows = vec![0.0; w * (gy1 - gy0 + 1) as usize];
        par_rows(&mut rows, w, |r, out| {
            let (ky, ty) = spline_at(gy0 + r as i64);
            let at = (ky - 1 - ny0) as usize * w;
            let (a, b, c) = (&v[at..], &v[at + w..], &v[at + 2 * w..]);
            for (i, o) in out.iter_mut().enumerate() {
                *o = spline(a[i], b[i], c[i], ty);
            }
        });
        Nodes { nx0, gy0, w, rows }
    }

    /// Per global pixel column in `gx0..gx1`: its first node column and spline parameter, for `at`.
    fn columns(&self, gx0: i64, gx1: i64) -> Vec<(usize, f64)> {
        (gx0..gx1)
            .map(|gx| {
                let (k, t) = spline_at(gx);
                ((k - 1 - self.nx0) as usize, t)
            })
            .collect()
    }

    /// Detail noise at global pixel row `gy` and a column from `columns`.
    fn at(&self, gy: i64, (c, t): (usize, f64)) -> DetailNoise {
        let r = &self.rows[(gy - self.gy0) as usize * self.w + 2 * c..];
        DetailNoise {
            value: spline(r[0], r[2], r[4], t),
            ridged: spline(r[1], r[3], r[5], t),
        }
    }
}

/// Fills `v`, rows of `w`, with `f(row, values)`; on scoped threads when it is large.
fn par_rows(v: &mut [f64], w: usize, f: impl Fn(usize, &mut [f64]) + Sync) {
    let rows = v.len() / w;
    let threads = if v.len() >= 1 << 13 {
        crate::jobs::edit_threads()
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
        let ridged = raise && s.mode == BrushMode::Add && s.target.is_none();
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
            // Per column and row of the disc's box, the parts of `dist2` and the noise.
            let (bx0, bx1) = (
                ((cx - r).floor() as i64).max(0),
                ((cx + r).ceil() as i64).min(w),
            );
            let (by0, by1) = (
                ((cy - r).floor() as i64).max(0),
                ((cy + r).ceil() as i64).min(h),
            );
            let sq = |d: f64| d * d;
            let dxx: Vec<f64> = (bx0..bx1).map(|x| sq(root(x, 0).x - p.at.x)).collect();
            let dyy: Vec<f64> = (by0..by1).map(|y| sq(root(0, y).y - p.at.y)).collect();
            let inv = 1.0 / (p.radius * p.radius);
            let cols = nodes.as_ref().map(|l| l.columns(gxi + bx0, gxi + bx1));
            self.disc(
                map,
                s.layer,
                (cx, cy),
                r,
                (tiles, cached),
                &mut touched,
                |v, _, x, y| {
                    // `dist2` of the pixel centre, term by term.
                    let d2 = (dxx[(x - bx0) as usize] + dyy[(y - by0) as usize]) * inv;
                    if d2 >= 1.0 {
                        return false;
                    }
                    let fall = weight_in(d2, core);
                    let (n, wt) = match (&nodes, &cols) {
                        (Some(l), Some(c)) => {
                            let n = l.at(gyi + y, c[(x - bx0) as usize]).at(*v, ridged);
                            (n, modulate(fall, s.detail, n))
                        }
                        _ => (0.0, fall),
                    };
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
                        BrushMode::Roughen => *v = roughen(*v, amount as f64 * fall as f64 * n),
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
