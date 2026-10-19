//! Scenes from pyramid levels: the cached scene never blocks and draws stand-ins (ADR-0014).

use std::sync::Arc;

use mmc_core::{Layer, Map, Overview, Project, TILE_SIZE, TileId};

use crate::{Error, Raster, Renderer, Result, Scene, View};

/// A tile of the asked level, or a coarser one with its level.
type Fetched = Option<(u32, Arc<[f32]>)>;

impl Renderer {
    /// Never blocks: reads the pyramid level that fits the view's zoom from the tile cache.
    /// Tiles not loaded yet are drawn from the finest loaded level above; the scene is then
    /// not complete, and a later call after the loads finish replaces it.
    pub fn scene_cached(&self, project: &Project, view: View) -> Result<Scene> {
        let map = find_map(project, view)?;
        let level = fit_level(map, view, project.top_level(map.id)?);
        self.assemble(map, view, level, |layer, tx, ty| {
            Ok(project.cached_tile(TileId {
                map: map.id,
                layer,
                level,
                tx,
                ty,
            })?)
        })
    }

    /// The first frame of a map, drawn from its overview before the project is open.
    pub fn overview_scene(&self, overview: &Overview, view: View) -> Result<Scene> {
        let map = &overview.map;
        if map.id != view.map {
            return Err(mmc_core::Error::UnknownMap(view.map).into());
        }
        let level = fit_level(map, view, overview.level);
        let values = overview.values.clone();
        self.assemble(map, view, level, |_, _, _| {
            Ok(Some((overview.level, values.clone())))
        })
    }

    /// Reads `level` of the layers that registered passes draw, for the area of `view`.
    pub(crate) fn assemble(
        &self,
        map: &Map,
        view: View,
        level: u32,
        mut fetch: impl FnMut(Layer, u32, u32) -> Result<Fetched>,
    ) -> Result<Scene> {
        let s = map.settings;
        let m = s.metres_per_pixel * (1u64 << level) as f64;
        // One pixel of margin for neighbour reads (hillshade).
        let range = |min: f64, max: f64, size: u32| {
            let size = size.div_ceil(1 << level) as f64;
            let lo = ((min / m).floor() - 1.0).clamp(0.0, size) as u32;
            let hi = ((max / m).ceil() + 1.0).clamp(0.0, size) as u32;
            (lo, hi)
        };
        let (x0, x1) = range(view.min.x, view.max.x, s.width);
        let (y0, y1) = range(view.min.y, view.max.y, s.height);
        if x0 >= x1 || y0 >= y1 || view.width == 0 || view.height == 0 {
            return Err(Error::OutsideMap);
        }
        let mut layers: Vec<Layer> = self.passes.iter().map(|p| p.0).collect();
        layers.sort();
        layers.dedup();
        let mut complete = true;
        let mut rasters = Vec::new();
        let t = TILE_SIZE;
        for layer in layers {
            let (w, h) = (x1 - x0, y1 - y0);
            let mut values = vec![0.0; (w * h) as usize];
            for ty in y0 / t..=(y1 - 1) / t {
                for tx in x0 / t..=(x1 - 1) / t {
                    let Some((l, tile)) = fetch(layer, tx, ty)? else {
                        complete = false;
                        continue;
                    };
                    complete &= l == level;
                    let d = l - level;
                    let (ax, bx) = ((tx * t).max(x0), ((tx + 1) * t).min(x1));
                    for y in (ty * t).max(y0)..((ty + 1) * t).min(y1) {
                        let dst = ((y - y0) * w + ax - x0) as usize;
                        let row = ((y >> d) % t * t) as usize;
                        for (i, x) in (ax..bx).enumerate() {
                            values[dst + i] = tile[row + ((x >> d) % t) as usize];
                        }
                    }
                }
            }
            rasters.push((
                layer,
                Raster {
                    x0,
                    y0,
                    w,
                    h,
                    values,
                },
            ));
        }
        Ok(Scene {
            view,
            metres_per_pixel: m,
            rasters,
            complete,
            summaries: Vec::new(),
        })
    }
}

pub(crate) fn find_map(project: &Project, view: View) -> Result<&Map> {
    Ok(project
        .maps()
        .iter()
        .find(|m| m.id == view.map)
        .ok_or(mmc_core::Error::UnknownMap(view.map))?)
}

/// The coarsest level that still has at least one level pixel per image pixel.
fn fit_level(map: &Map, view: View, top: u32) -> u32 {
    let m = map.settings.metres_per_pixel;
    let scale = ((view.max.x - view.min.x) / m / view.width as f64)
        .min((view.max.y - view.min.y) / m / view.height as f64);
    let mut level = 0;
    while level < top && (2u64 << level) as f64 <= scale {
        level += 1;
    }
    level
}
