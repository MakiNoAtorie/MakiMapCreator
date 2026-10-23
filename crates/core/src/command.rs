//! Commands: every edit is one. Raster commands produce tile changes.

use std::collections::{BTreeMap, BTreeSet};

use crate::persist::TileRecord;

use crate::layer::{Layer, RootPoint, TILE_SIZE, decode, encode};
use crate::undo::{Change, TileChange};
use crate::{EntityEdit, Map, Project, Result};

/// Decoded level-0 tiles of one layer being edited: the stored record before (`None` while not
/// read yet, see `stroke.rs`), and the values now.
pub(crate) type EditTiles = BTreeMap<(u32, u32), (Option<Option<TileRecord>>, Vec<f32>)>;

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Brush(BrushStroke),
    /// Entities are project-level; the edit goes to the history of the map it is applied on.
    Entity(EntityEdit),
    /// Pages are project-level; the edit goes to the history of the map it is applied on.
    Page(crate::PageEdit),
    /// Anchors are project-level; the edit goes to the history of the map it is applied on.
    Anchor(crate::AnchorEdit),
    Paint(crate::PaintStroke),
    /// A generator's output (ADR-0003).
    Generate(crate::Generation),
    /// A map object edit on the map it is applied on.
    Object(crate::ObjectEdit),
}

/// Adds `strength * pressure * weight` to `layer` around each point, or with `target` moves each
/// pixel that part of the way to it; `mode` gives the other effects (`BrushMode`), `weight` is
/// `stamp_weight`. One stroke is one command.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushStroke {
    pub layer: Layer,
    pub points: Vec<StrokePoint>,
    /// Layer units at the centre at full pressure; negative lowers. Smooth and flatten: the
    /// fraction of the way a pixel moves per stamp at the centre at full pressure.
    pub strength: f32,
    /// 0 is a hard disc; 1 is the smooth `(1 - d²)²` profile; between, a hard core then that profile.
    pub falloff: f32,
    /// Raise to (`strength` > 0) or lower to (`strength` < 0) this value; each stamp moves a pixel
    /// `pressure * weight` of the way and never the other way. `None` adds.
    pub target: Option<f32>,
    pub mode: crate::BrushMode,
    /// Root-space noise on the stamp weight; the noise roughen adds.
    pub detail: crate::Detail,
}

impl Default for BrushStroke {
    /// An empty smooth height brush with no detail.
    fn default() -> BrushStroke {
        BrushStroke {
            layer: Layer::HEIGHT,
            points: vec![],
            strength: 0.0,
            falloff: 1.0,
            target: None,
            mode: crate::BrushMode::Add,
            detail: crate::Detail::NONE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokePoint {
    pub at: RootPoint,
    /// 0..=1.
    pub pressure: f32,
    /// Metres.
    pub radius: f64,
}

/// Brush weight at `d2`, the squared distance as a fraction of the radius (`d2 < 1`).
pub fn brush_weight(d2: f64, falloff: f32) -> f32 {
    weight_in(d2, brush_core(falloff))
}

/// Squared radius fraction of the hard core of a brush with `falloff`.
pub fn brush_core(falloff: f32) -> f64 {
    (1.0 - falloff.clamp(0.0, 1.0) as f64).powi(2)
}

pub(crate) fn weight_in(d2: f64, core: f64) -> f32 {
    if d2 <= core {
        return 1.0;
    }
    let t = (1.0 - d2) / (1.0 - core);
    (t * t) as f32
}

impl Project {
    /// `level` is the flatten level of the stroke `cmd` belongs to; `None` samples it.
    pub(crate) fn changes(
        &self,
        map: &Map,
        cmd: &Command,
        level: &mut Option<f32>,
    ) -> Result<Change> {
        match cmd {
            Command::Brush(s) => {
                let mut tiles = EditTiles::new();
                self.brush_into(map, s, &mut tiles, false, level)?;
                Ok(Change::Tiles(tile_changes(map, s.layer, tiles)))
            }
            Command::Entity(e @ EntityEdit::DeleteWithMap { .. }) => {
                Ok(Change::Deletion(self.entity_deletion(e)?))
            }
            Command::Entity(e) => {
                if let EntityEdit::Delete(id) = e
                    && !self.own_maps(*id)?.is_empty()
                {
                    return Err(crate::Error::OwnMapChoice(*id));
                }
                Ok(Change::Entities(self.entity_changes(e)?))
            }
            Command::Page(e) => Ok(Change::Page(self.page_change(e)?)),
            Command::Anchor(e) => Ok(Change::Anchor(self.anchor_change(e)?)),
            Command::Paint(s) => {
                let mut tiles = EditTiles::new();
                self.paint_into(map, s, &mut tiles, false)?;
                Ok(Change::Tiles(tile_changes(map, s.layer, tiles)))
            }
            Command::Generate(g) => Ok(Change::Generation(self.generation_change(
                map,
                g,
                (0, 0),
            )?)),
            Command::Object(e) => Ok(Change::Objects(self.object_change(map.id, e)?)),
        }
    }

    /// Calls `f(value, squared px distance, x, y)` for each pixel (`x`, `y`) of `layer` in the box
    /// of the disc at `c` with radius `r` px, tile by tile; `f` gives whether it changed the pixel.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn disc(
        &self,
        map: &Map,
        layer: Layer,
        (cx, cy): (f64, f64),
        r: f64,
        (tiles, cached): (&mut EditTiles, bool),
        touched: &mut BTreeSet<(u32, u32)>,
        f: impl Fn(&mut f32, f64, i64, i64) -> bool + Sync,
    ) -> Result<()> {
        let (w, h) = (map.settings.width as i64, map.settings.height as i64);
        let (x0, x1) = (
            ((cx - r).floor() as i64).max(0),
            ((cx + r).ceil() as i64).min(w),
        );
        let (y0, y1) = (
            ((cy - r).floor() as i64).max(0),
            ((cy + r).ceil() as i64).min(h),
        );
        if x0 >= x1 || y0 >= y1 {
            return Ok(());
        }
        let t = TILE_SIZE as i64;
        let (xs, ys) = (x0 / t..(x1 - 1) / t + 1, y0 / t..(y1 - 1) / t + 1);
        let loaded = self.load_tiles(map, layer, (xs.clone(), ys.clone()), (tiles, cached))?;
        let in_box = |k: &(u32, u32)| xs.contains(&(k.0 as i64)) && ys.contains(&(k.1 as i64));
        let mut work: Vec<((u32, u32), &mut Vec<f32>, bool)> = tiles
            .iter_mut()
            .filter(|(k, _)| in_box(k))
            .map(|(k, v)| (*k, &mut v.1, false))
            .collect();
        let one = |((tx, ty), values, hit): &mut ((u32, u32), &mut Vec<f32>, bool)| {
            let (tx, ty) = (*tx as i64, *ty as i64);
            for y in y0.max(ty * t)..y1.min((ty + 1) * t) {
                let dy = (y as f64 + 0.5 - cy).powi(2);
                if dy >= r * r {
                    continue;
                }
                let row = ((y - ty * t) * t) as usize;
                // Only the row's chord can be inside; `f` still tests each pixel.
                let half = (r * r - dy).sqrt();
                let xa = ((cx - 0.5 - half).floor() as i64).max(x0.max(tx * t));
                let xb = ((cx - 0.5 + half).ceil() as i64 + 1).min(x1.min((tx + 1) * t));
                if xa >= xb {
                    continue;
                }
                let (a, b) = (row + (xa - tx * t) as usize, row + (xb - tx * t) as usize);
                for (x, v) in (xa..xb).zip(&mut values[a..b]) {
                    let dist2 = (x as f64 + 0.5 - cx).powi(2) + dy;
                    *hit |= f(v, dist2, x, y);
                }
            }
        };
        // Large stamps (detail noise costs tens of ns a pixel) run tile by tile on scoped threads.
        let threads = if (x1 - x0) * (y1 - y0) >= 1 << 16 {
            std::thread::available_parallelism().map_or(1, |n| n.get())
        } else {
            1
        };
        if threads <= 1 || work.len() <= 1 {
            work.iter_mut().for_each(one);
        } else {
            let n = work.len().div_ceil(threads);
            std::thread::scope(|sc| {
                for chunk in work.chunks_mut(n) {
                    sc.spawn(|| chunk.iter_mut().for_each(one));
                }
            });
        }
        let hits: Vec<((u32, u32), bool)> = work.into_iter().map(|(k, _, h)| (k, h)).collect();
        for (key, hit) in hits {
            if hit {
                touched.insert(key);
            } else if loaded.contains(&key) {
                // Only the box reaches this tile; it stays unedited.
                tiles.remove(&key);
            }
        }
        Ok(())
    }

    /// Loads the tiles in `xs` x `ys` (tile indices) not in `tiles` yet: from the cache with
    /// `cached`, else from storage in one query. Gives the tiles it loaded.
    pub(crate) fn load_tiles(
        &self,
        map: &Map,
        layer: Layer,
        (xs, ys): (std::ops::Range<i64>, std::ops::Range<i64>),
        (tiles, cached): (&mut EditTiles, bool),
    ) -> Result<BTreeSet<(u32, u32)>> {
        // Tiles not loaded yet: from the cache, else from storage in one query.
        let mut missing = vec![];
        let mut loaded: BTreeSet<(u32, u32)> = BTreeSet::new();
        for ty in ys.clone() {
            for tx in xs.clone() {
                let key = (tx as u32, ty as u32);
                if tiles.contains_key(&key) {
                    continue;
                }
                let k = map.tile_key(layer, key.0, key.1);
                // Cached level-0 tiles equal the stored ones; a record costs more to read than a copy.
                match cached.then(|| self.tiles().get(&k)).flatten() {
                    Some(v) => {
                        tiles.insert(key, (None, v.to_vec()));
                        loaded.insert(key);
                    }
                    None => missing.push(key),
                }
            }
        }
        if !missing.is_empty() {
            let range = |r: std::ops::Range<i64>| r.start as u32..r.end as u32;
            let mut stored: BTreeMap<(u32, u32), TileRecord> = self
                .store
                .tile_range(map.id.0, layer, range(xs.clone()), range(ys.clone()))?
                .into_iter()
                .collect();
            for key in missing {
                let k = map.tile_key(layer, key.0, key.1);
                let before = match self.writer.get().and_then(|w| w.overlay().get(&k)) {
                    Some(rec) => rec,
                    None => stored.remove(&key),
                };
                let values = decode(before.as_ref())?;
                tiles.insert(key, (Some(before), values));
                loaded.insert(key);
            }
        }
        Ok(loaded)
    }
}

/// One change per edited tile: the record before and the encoded values after.
pub(crate) fn tile_changes(map: &Map, layer: Layer, tiles: EditTiles) -> Vec<TileChange> {
    tiles
        .into_iter()
        .map(|((tx, ty), (before, values))| TileChange {
            key: map.tile_key(layer, tx, ty),
            before: before.expect("read without the cache"),
            after: Some(TileRecord {
                data: encode(&values),
                edited: true,
            }),
        })
        .collect()
}
