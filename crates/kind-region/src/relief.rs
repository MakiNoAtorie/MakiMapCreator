//! The derived relief layer (ADR-0026): erosion gullies the height shading adds.

use std::collections::BTreeSet;

use mmc_core::{
    Layer, OptionValues, Param, Raster, ReliefOptions, Stage, StageInput, TILE_SIZE, par_map,
    relief, relief_margin,
};

/// Derived: metres the height shading adds, 0 on low and flat ground; never stored height.
pub const RELIEF: Layer = Layer::new("relief");

/// Gully depth at full mask, metres.
pub const RELIEF_STRENGTH: Param = Param {
    id: "relief-strength",
    default: 200.0,
    min: 0.0,
    max: 400.0,
};
/// Slope, metres per metre, from which gullies are full; gentler ground gets fainter ones.
pub const RELIEF_SLOPE: Param = Param {
    id: "relief-slope",
    default: 0.4,
    min: 0.05,
    max: 2.0,
};
/// Height above the water level, metres, from which gullies are full.
pub const RELIEF_HEIGHT: Param = Param {
    id: "relief-height",
    default: 1200.0,
    min: 50.0,
    max: 5000.0,
};

pub(crate) const OPTIONS: &[Param] = &[RELIEF_STRENGTH, RELIEF_SLOPE, RELIEF_HEIGHT];

pub(crate) fn options(v: &OptionValues) -> ReliefOptions {
    ReliefOptions {
        strength: v.get(&RELIEF_STRENGTH),
        slope: v.get(&RELIEF_SLOPE),
        height: v.get(&RELIEF_HEIGHT),
    }
}

pub(crate) const STAGE: Stage = Stage {
    reads: &[Layer::HEIGHT],
    writes: &[RELIEF],
    run,
};

const TILE_LEN: usize = (TILE_SIZE * TILE_SIZE) as usize;

/// The relief of every tile with height, from the tile and `relief_margin` pixels around it,
/// read past the map edge as the edge pixel.
// ponytail: recomputes the whole map after each edit; pass the changed tiles if large maps lag.
fn run(cx: &StageInput) -> Option<Vec<Raster>> {
    let h = &cx.layers[&Layer::HEIGHT];
    let o = options(&cx.options);
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
        let (x, y) = (x.clamp(0, w - 1) as u32, y.clamp(0, hh - 1) as u32);
        let i = ((y % TILE_SIZE) * TILE_SIZE + x % TILE_SIZE) as usize;
        h.get(&(x / TILE_SIZE, y / TILE_SIZE)).map_or(0.0, |v| v[i])
    };
    let (m, ts) = (relief_margin(cx.metres_per_pixel) as i64, TILE_SIZE as i64);
    let n = (ts + 2 * m) as usize;
    let tiles = par_map(cx.threads, &want, cx.cancelled, |&(tx, ty)| {
        let (x0, y0) = (tx as i64 * ts - m, ty as i64 * ts - m);
        let mut pad = Vec::with_capacity(n * n);
        for y in y0..y0 + n as i64 {
            let gy = y.clamp(0, hh - 1);
            let mut x = x0;
            while x < x0 + n as i64 {
                if x < 0 || x >= w {
                    pad.push(at(x, y));
                    x += 1;
                    continue;
                }
                // The rest of this row inside one tile, as one slice.
                let t = x / ts;
                let end = ((t + 1) * ts).min(w).min(x0 + n as i64);
                let row = (gy % ts * ts) as usize;
                let (a, b) = (row + (x % ts) as usize, row + (end - t * ts) as usize);
                match h.get(&(t as u32, (gy / ts) as u32)) {
                    Some(v) => pad.extend_from_slice(&v[a..b]),
                    None => pad.resize(pad.len() + b - a, 0.0),
                }
                x = end;
            }
        }
        let g0 = (cx.origin.0 + x0, cx.origin.1 + y0);
        let r = relief(cx.seed, &o, cx.metres_per_pixel, g0, n, n, &pad);
        let mut out = vec![0.0f32; TILE_LEN];
        for y in 0..ts.min(hh - ty as i64 * ts) {
            for x in 0..ts.min(w - tx as i64 * ts) {
                out[(y * ts + x) as usize] = r[((y + m) * n as i64 + x + m) as usize];
            }
        }
        out
    })?;
    Some(vec![want.into_iter().zip(tiles).collect()])
}
