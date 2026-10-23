//! Scenes from pyramid levels: the cached scene never blocks and draws stand-ins (ADR-0014).

use std::sync::Arc;

use mmc_core::{ChildSummary, Layer, Map, Overview, Project, TILE_SIZE, TileId};

use crate::{Error, Raster, Renderer, Result, Scene, View};

/// A tile of the asked level, or a coarser one with its level.
type Tile = Option<(u32, Arc<[f32]>)>;
/// One layer's tiles by (tx, ty).
type LayerTiles = (Layer, Vec<((u32, u32), Tile)>);

/// The tiles a scene reads. Taking them is quick, so a worker can do it under the project lock
/// and copy them into rasters (`scene`) after it lets go.
pub struct Fetched {
    view: View,
    level: u32,
    metres_per_pixel: f64,
    /// Global level-0 pixel of map pixel (0, 0).
    origin: (i64, i64),
    /// Level pixels `x0..x1`, `y0..y1`.
    range: (u32, u32, u32, u32),
    layers: Vec<LayerTiles>,
    summaries: Vec<ChildSummary>,
    /// False while upper levels lag an edit.
    settled: bool,
}

impl Renderer {
    /// Never blocks: tiles not loaded yet show a coarser level and the scene is not complete.
    pub fn scene_cached(&self, project: &Project, view: View) -> Result<Scene> {
        Ok(self.fetch_cached(project, view)?.scene())
    }

    /// The tiles `scene_cached` reads, without the copy into rasters.
    pub fn fetch_cached(&self, project: &Project, view: View) -> Result<Fetched> {
        let map = find_map(project, view)?;
        let level = fit_level(map, view, project.top_level(map.id)?);
        // Upper levels lag an edit until the pyramid worker has rebuilt them.
        let settled = level == 0 || !project.pyramid_pending(map.id);
        let mut f = self.fetch(map, view, level, |layer, tx, ty| {
            Ok(project.cached_tile(TileId {
                map: map.id,
                layer,
                level,
                tx,
                ty,
            })?)
        })?;
        f.settled = settled;
        Ok(f)
    }

    /// The first frame of a map, drawn from its overview before the project is open.
    pub fn overview_scene(&self, overview: &Overview, view: View) -> Result<Scene> {
        let map = &overview.map;
        if map.id != view.map {
            return Err(mmc_core::Error::UnknownMap(view.map).into());
        }
        let level = fit_level(map, view, overview.level);
        let values = overview.values.clone();
        let f = self.fetch(map, view, level, |_, _, _| {
            Ok(Some((overview.level, values.clone())))
        })?;
        Ok(f.scene())
    }

    /// The tiles of `level` of the layers that registered passes draw, for the area of `view`.
    pub(crate) fn fetch(
        &self,
        map: &Map,
        view: View,
        level: u32,
        mut fetch: impl FnMut(Layer, u32, u32) -> Result<Tile>,
    ) -> Result<Fetched> {
        let s = map.settings;
        let m = s.metres_per_pixel * (1u64 << level) as f64;
        // Two pixels of margin: bilinear reads one more texel, the hillshade its neighbours.
        let range = |min: f64, max: f64, size: u32| {
            let size = size.div_ceil(1 << level) as f64;
            let lo = ((min / m).floor() - 2.0).clamp(0.0, size) as u32;
            let hi = ((max / m).ceil() + 2.0).clamp(0.0, size) as u32;
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
        let t = TILE_SIZE;
        let mut out = Vec::new();
        for layer in layers {
            let mut tiles = Vec::new();
            for ty in y0 / t..=(y1 - 1) / t {
                for tx in x0 / t..=(x1 - 1) / t {
                    tiles.push(((tx, ty), fetch(layer, tx, ty)?));
                }
            }
            out.push((layer, tiles));
        }
        Ok(Fetched {
            view,
            level,
            metres_per_pixel: m,
            origin: (
                (map.origin.x / s.metres_per_pixel).round() as i64,
                (map.origin.y / s.metres_per_pixel).round() as i64,
            ),
            range: (x0, x1, y0, y1),
            layers: out,
            summaries: Vec::new(),
            settled: true,
        })
    }
}

impl Fetched {
    /// Adds the summaries of the view map's child maps, as `Scene::with_summaries`.
    pub fn with_summaries(mut self, project: &Project) -> Result<Fetched> {
        self.summaries = crate::summary::summaries(project, self.view.map)?;
        Ok(self)
    }

    /// Copies the tiles into the rasters the passes draw.
    pub fn scene(self) -> Scene {
        let (x0, x1, y0, y1) = self.range;
        let (level, t) = (self.level, TILE_SIZE);
        let (w, h) = (x1 - x0, y1 - y0);
        let mut complete = self.settled;
        let mut rasters = Vec::new();
        for (layer, tiles) in self.layers {
            let mut values = vec![0.0; (w * h) as usize];
            for ((tx, ty), tile) in tiles {
                let Some((l, tile)) = tile else {
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
        Scene {
            view: self.view,
            metres_per_pixel: self.metres_per_pixel,
            level,
            origin: self.origin,
            rasters,
            complete,
            summaries: self.summaries,
        }
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
