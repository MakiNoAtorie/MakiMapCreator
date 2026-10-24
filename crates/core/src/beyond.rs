//! Stage reads past a map's edge come from its neighbours, so maps agree at a seam (ADR-0026).

use std::collections::HashMap;

use crate::layer::{Layer, RootPoint, TILE_SIZE};
use crate::pipeline::{Raster, StageInput};

/// A map of the same kind and scale within a stage's reach of the map that rebuilds.
pub(crate) struct Neighbour {
    /// Global pixel of its pixel (0, 0); its tiles are keyed from there.
    pub(crate) origin: (i64, i64),
    /// Global pixels it gives, `[x0, y0, x1, y1)`: its extent within the reach.
    pub(crate) given: [i64; 4],
    pub(crate) outline: Option<Vec<RootPoint>>,
    /// Only the tiles that hold `given`.
    pub(crate) layers: HashMap<Layer, Raster>,
}

struct Source<'a> {
    origin: (i64, i64),
    given: [i64; 4],
    outline: Option<&'a [RootPoint]>,
    raster: Option<&'a Raster>,
}

impl Source<'_> {
    fn has(&self, gx: i64, gy: i64) -> bool {
        (self.given[0]..self.given[2]).contains(&gx) && (self.given[1]..self.given[3]).contains(&gy)
    }
}

impl StageInput<'_> {
    /// `layer` from map pixel (`x0`, `y0`), past the edge too: each pixel from its owner first.
    pub fn padded(&self, layer: Layer, x0: i64, y0: i64, w: usize, h: usize) -> Vec<f32> {
        let own = [
            self.origin.0,
            self.origin.1,
            self.origin.0 + self.width as i64,
            self.origin.1 + self.height as i64,
        ];
        let mut sources = vec![Source {
            origin: self.origin,
            given: own,
            outline: self.outline.as_deref(),
            raster: self.layers.get(&layer),
        }];
        sources.extend(self.neighbours.iter().map(|n| Source {
            origin: n.origin,
            given: n.given,
            outline: n.outline.as_deref(),
            raster: n.layers.get(&layer),
        }));
        let (gx0, gy0) = (self.origin.0 + x0, self.origin.1 + y0);
        let gx1 = gx0 + w as i64;
        let covered = |gy: i64| {
            sources.iter().any(|s| {
                (s.given[1]..s.given[3]).contains(&gy) && s.given[0] < gx1 && gx0 < s.given[2]
            })
        };
        let mut out = Vec::with_capacity(w * h);
        let mut last: Option<(i64, usize)> = None;
        for gy in gy0..gy0 + h as i64 {
            // A row no map has repeats the nearest row one has; own rows end the search.
            let row = (0..)
                .flat_map(|d| [gy - d, gy + d])
                .find(|&y| covered(y))
                .unwrap();
            match last {
                Some((y, start)) if y == row => out.extend_from_within(start..start + w),
                _ => {
                    last = Some((row, out.len()));
                    self.row(&sources, row, gx0, w, &mut out);
                }
            }
        }
        out
    }

    fn row(&self, sources: &[Source], gy: i64, gx0: i64, w: usize, out: &mut Vec<f32>) {
        let mpp = self.metres_per_pixel;
        let yc = (gy as f64 + 0.5) * mpp;
        let crossings: Vec<Option<Vec<f64>>> = sources
            .iter()
            .map(|s| s.outline.map(|o| crossings(o, yc)))
            .collect();
        let mut passed = vec![0usize; sources.len()];
        let mut pick: Vec<Option<usize>> = Vec::with_capacity(w);
        for gx in gx0..gx0 + w as i64 {
            let xc = (gx as f64 + 0.5) * mpp;
            let mut owner = None;
            for (k, c) in crossings.iter().enumerate() {
                let owns = match c {
                    None => true,
                    Some(c) => {
                        while passed[k] < c.len() && c[passed[k]] <= xc {
                            passed[k] += 1;
                        }
                        (c.len() - passed[k]) % 2 == 1
                    }
                };
                if owner.is_none() && owns && sources[k].has(gx, gy) {
                    owner = Some(k);
                }
            }
            pick.push(owner.or_else(|| sources.iter().position(|s| s.has(gx, gy))));
        }
        // Else a map that has the pixel, this one first; else the pixel before it in the row.
        let start = out.len();
        let mut i = 0;
        while i < w {
            let j = (i..w).find(|&j| pick[j] != pick[i]).unwrap_or(w);
            match pick[i] {
                Some(k) => {
                    let s = &sources[k];
                    let x = gx0 + i as i64 - s.origin.0;
                    copy_row(s.raster, gy - s.origin.1, x, x + (j - i) as i64, out);
                }
                None => out.resize(out.len() + j - i, f32::NAN),
            }
            i = j;
        }
        let row = &mut out[start..];
        let first = pick
            .iter()
            .position(Option::is_some)
            .map_or(0.0, |i| row[i]);
        let mut prev = first;
        for (v, p) in row.iter_mut().zip(&pick) {
            match p {
                Some(_) => prev = *v,
                None => *v = prev,
            }
        }
    }
}

/// Root-space x of each outline edge that crosses `y`, sorted; as `child::inside` counts them.
fn crossings(poly: &[RootPoint], y: f64) -> Vec<f64> {
    let mut c: Vec<f64> = poly
        .iter()
        .zip(poly.iter().cycle().skip(1))
        .filter(|(a, b)| (a.y > y) != (b.y > y))
        .map(|(a, b)| a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x))
        .collect();
    c.sort_by(f64::total_cmp);
    c
}

/// Pixels `x0..x1` of row `y` of `r`, by tile slices; a missing tile is zeros.
fn copy_row(r: Option<&Raster>, y: i64, x0: i64, x1: i64, out: &mut Vec<f32>) {
    let ts = TILE_SIZE as i64;
    let row = (y % ts * ts) as usize;
    let mut x = x0;
    while x < x1 {
        let t = x / ts;
        let end = ((t + 1) * ts).min(x1);
        let (a, b) = (row + (x % ts) as usize, row + (end - t * ts) as usize);
        match r.and_then(|r| r.get(&(t as u32, (y / ts) as u32))) {
            Some(v) => out.extend_from_slice(&v[a..b]),
            None => out.resize(out.len() + b - a, 0.0),
        }
        x = end;
    }
}
