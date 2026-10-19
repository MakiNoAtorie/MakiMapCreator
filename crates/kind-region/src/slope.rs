//! The derived slope layer: one pipeline stage that proves the pipeline (ADR-0018).

use std::collections::BTreeSet;

use mmc_core::{Layer, Raster, Stage, StageInput, TILE_SIZE, par_map};

/// Derived: gradient magnitude of height, metres per metre.
pub const SLOPE: Layer = Layer::new("slope");

pub(crate) const STAGES: &[Stage] = &[Stage {
    reads: &[Layer::HEIGHT],
    writes: &[SLOPE],
    run: slope,
}];

const TILE_LEN: usize = (TILE_SIZE * TILE_SIZE) as usize;

/// Tile of pixel (`x`, `y`) and the pixel's index inside it.
fn tile_of(x: u32, y: u32) -> ((u32, u32), usize) {
    let i = (y % TILE_SIZE) * TILE_SIZE + x % TILE_SIZE;
    ((x / TILE_SIZE, y / TILE_SIZE), i as usize)
}

/// Gradient magnitude of height by central differences, clamped at the map edge.
fn slope(cx: &StageInput) -> Option<Vec<Raster>> {
    let h = &cx.layers[&Layer::HEIGHT];
    let (w, hh) = (cx.width as i64, cx.height as i64);
    let (ntx, nty) = (
        cx.width.div_ceil(TILE_SIZE) as i64,
        cx.height.div_ceil(TILE_SIZE) as i64,
    );
    let mut want = BTreeSet::new();
    for &(tx, ty) in h.keys() {
        for (dx, dy) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))) {
            let (x, y) = (tx as i64 + dx, ty as i64 + dy);
            if (0..ntx).contains(&x) && (0..nty).contains(&y) {
                want.insert((x as u32, y as u32));
            }
        }
    }
    let want: Vec<(u32, u32)> = want.into_iter().collect();
    let at = |x: i64, y: i64| {
        let (t, i) = tile_of(x.clamp(0, w - 1) as u32, y.clamp(0, hh - 1) as u32);
        h.get(&t).map_or(0.0, |v| v[i])
    };
    let k = (1.0 / (2.0 * cx.metres_per_pixel)) as f32;
    let tiles = par_map(cx.threads, &want, cx.cancelled, |&(tx, ty)| {
        // Height with a one-pixel border, so the inner loop needs no lookups.
        let (ts, n) = (TILE_SIZE as usize, TILE_SIZE as usize + 2);
        let (x0, y0) = ((tx * TILE_SIZE) as i64 - 1, (ty * TILE_SIZE) as i64 - 1);
        let centre = h.get(&(tx, ty));
        let mut pad = vec![0.0f32; n * n];
        for py in 0..n {
            for px in 0..n {
                let (gx, gy) = (x0 + px as i64, y0 + py as i64);
                let inner = (1..=ts).contains(&px) && (1..=ts).contains(&py) && gx < w && gy < hh;
                pad[py * n + px] = match (inner, centre) {
                    (true, Some(c)) => c[(py - 1) * ts + px - 1],
                    (true, None) => 0.0,
                    (false, _) => at(gx, gy),
                };
            }
        }
        let mut out = vec![0.0f32; TILE_LEN];
        for py in 0..ts {
            if y0 + 1 + py as i64 >= hh {
                break;
            }
            for px in 0..ts.min((w - x0 - 1) as usize) {
                let c = (py + 1) * n + px + 1;
                let dx = (pad[c + 1] - pad[c - 1]) * k;
                let dy = (pad[c + n] - pad[c - n]) * k;
                out[py * ts + px] = (dx * dx + dy * dy).sqrt();
            }
        }
        out
    })?;
    Some(vec![want.into_iter().zip(tiles).collect()])
}
