//! Tile pyramid: level `l` pixels are 2x2 means of level `l - 1`; the top tile is the overview.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::persist::{ProjectReader, TileKey, TileRecord, Write};

use crate::cache::{Tile, TileCache};
use crate::layer::{Layer, TILE_LEN, TILE_SIZE, decode};
use crate::{Map, MapId, MapSettings, Project, Result};

/// Longest wait of an undo or redo for the cached upper levels; about 1000 tiles take it.
const PYRAMID_SHOWN_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// One tile of one pyramid level of a layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileId {
    pub map: MapId,
    pub layer: Layer,
    /// 0 = full resolution; level `l` has 2^l map pixels per tile pixel.
    pub level: u32,
    pub tx: u32,
    pub ty: u32,
}

/// The top pyramid level of one layer of a map: a coarse image of the whole map.
#[derive(Debug, Clone, PartialEq)]
pub struct Overview {
    pub map: Map,
    pub level: u32,
    /// `TILE_SIZE`² values, row by row.
    pub values: Arc<[f32]>,
}

/// Writes the 2x2 means of `child` (tile `tx`, `ty`) into its quadrant of `parent`.
fn downsample(parent: &mut [f32], child: &[f32], tx: u32, ty: u32) {
    place(parent, &halve(child), tx, ty);
}

/// The 2x2 means of `child`, `TILE_SIZE / 2` pixels a side.
fn halve(child: &[f32]) -> Vec<f32> {
    let (t, half) = (TILE_SIZE as usize, TILE_SIZE as usize / 2);
    let mut out = Vec::with_capacity(half * half);
    for y in 0..half {
        for x in 0..half {
            let c = |dx: usize, dy: usize| child[(2 * y + dy) * t + 2 * x + dx];
            out.push((c(0, 0) + c(1, 0) + c(0, 1) + c(1, 1)) * 0.25);
        }
    }
    out
}

/// Copies `half`, from `halve`, into the quadrant of `parent` that child tile `tx`, `ty` covers.
fn place(parent: &mut [f32], half: &[f32], tx: u32, ty: u32) {
    let (t, h) = (TILE_SIZE as usize, TILE_SIZE as usize / 2);
    let (ox, oy) = ((tx % 2) as usize * h, (ty % 2) as usize * h);
    for (y, row) in half.chunks_exact(h).enumerate() {
        parent[(oy + y) * t + ox..][..h].copy_from_slice(row);
    }
}

/// Decoded tiles of one level of a layer by (tx, ty); a missing tile is all zeros.
pub(crate) type Level = BTreeMap<(u32, u32), Vec<f32>>;

/// Levels 1..=`top` of a whole layer from its level 0, for derived layers rebuilt at once.
pub(crate) fn levels_above(level0: &Level, top: u32) -> Vec<Level> {
    let mut out: Vec<Level> = Vec::new();
    for _ in 0..top {
        let below = out.last().unwrap_or(level0);
        let mut next = Level::new();
        for (&(tx, ty), child) in below {
            let parent = next
                .entry((tx / 2, ty / 2))
                .or_insert_with(|| vec![0.0; TILE_LEN]);
            downsample(parent, child, tx, ty);
        }
        out.push(next);
    }
    out
}

/// The level at which one tile covers the whole map.
pub(crate) fn top_level(s: &MapSettings) -> u32 {
    let side = s.width.max(s.height) as u64;
    (0..32)
        .find(|l| side <= (TILE_SIZE as u64) << l)
        .unwrap_or(31)
}

fn key(id: TileId) -> TileKey {
    TileKey {
        map_id: id.map.0,
        layer: id.layer,
        level: id.level,
        tx: id.tx,
        ty: id.ty,
    }
}

/// Reads the overviews of `layer` from a project file without opening it, one per map.
pub fn read_overviews(
    reader: &dyn ProjectReader,
    kinds: &crate::Kinds,
    layer: Layer,
) -> Result<Vec<Overview>> {
    reader
        .maps()?
        .into_iter()
        .map(|r| {
            let map = Map::from_record(r, kinds)?;
            let level = top_level(&map.settings);
            let id = TileId {
                map: map.id,
                layer,
                level,
                tx: 0,
                ty: 0,
            };
            let values = decode(reader.tile(&key(id))?.as_ref())?.into();
            Ok(Overview { map, level, values })
        })
        .collect()
}

impl Project {
    pub(crate) fn tiles(&self) -> &TileCache {
        self.cache
            .get_or_init(|| TileCache::new(self.writer().opener()))
    }

    pub(crate) fn writer(&self) -> &crate::writer::Writer {
        self.writer
            .get_or_init(|| crate::writer::Writer::new(self.store.tile_opener()))
    }

    /// A worker connection opener whose reads see edits still being written.
    pub(crate) fn tile_opener(&self) -> crate::persist::TileOpener {
        self.writer().opener()
    }

    /// The stored record at `key`, or the one an edit is still writing.
    pub(crate) fn tile_record(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        match self.writer.get().and_then(|w| w.overlay().get(key)) {
            Some(rec) => Ok(rec),
            None => self.store.tile(key),
        }
    }

    /// Blocks until every edit's tiles are in the working copy.
    pub(crate) fn flush_writes(&self) -> Result<()> {
        self.writer.get().map_or(Ok(()), |w| w.flush())
    }

    /// The coarsest pyramid level of `map`; its single tile covers the whole map.
    pub fn top_level(&self, map: MapId) -> Result<u32> {
        Ok(top_level(&self.map(map)?.settings))
    }

    /// Never blocks: queues `id` and returns the finest loaded tile at or above its level.
    pub fn cached_tile(&self, id: TileId) -> Result<Option<(u32, Tile)>> {
        let top = self.top_level(id.map)?;
        let cache = self.tiles();
        let at = |level: u32| {
            let d = level - id.level;
            key(TileId {
                level,
                tx: id.tx >> d,
                ty: id.ty >> d,
                ..id
            })
        };
        cache.request(at(id.level));
        for level in id.level..=top.max(id.level) {
            if let Some(t) = cache.get(&at(level)) {
                return Ok(Some((level, t)));
            }
        }
        // The top tile is the stand-in of last resort.
        cache.request(at(top.max(id.level)));
        Ok(None)
    }

    /// True while levels above level 0 of `map` do not yet show the last edit.
    pub fn pyramid_pending(&self, map: MapId) -> bool {
        self.strokes.unwritten(map) || self.cache.get().is_some_and(|c| c.pyramid_pending(map.0))
    }

    /// Blocks until all queued tile loads and pyramid updates after edits are done.
    pub fn wait_tiles(&self) {
        self.tiles().wait_idle();
    }

    /// Sets the memory limit of the tile cache; the default is `TILE_CACHE_BYTES`.
    pub fn set_tile_cache_limit(&self, bytes: usize) {
        self.tiles().set_limit(bytes);
    }

    /// Memory that loaded tiles use now.
    pub fn tile_cache_bytes(&self) -> usize {
        self.tiles().bytes()
    }

    /// Drops cached tiles of derived layers after a rebuild replaced them.
    pub(crate) fn forget_derived_tiles(&self) {
        if let Some(cache) = self.cache.get() {
            cache.forget(|k| self.is_derived(MapId(k.map_id), k.layer).unwrap_or(false));
        }
    }

    /// Writes `rows`: tile-only rows on the edit writer, others in one transaction after it;
    /// a worker then updates pyramid levels above level-0 tiles.
    pub(crate) fn write_rows(&self, rows: &[Write]) -> Result<()> {
        let tile_rows: Vec<_> = rows
            .iter()
            .filter_map(|w| match w {
                Write::Tile(k, rec) => Some((k, rec)),
                _ => None,
            })
            .collect();
        // Decoding runs on several threads: undo of a large stroke decodes hundreds of tiles under the project lock.
        let threads = crate::cpus().saturating_sub(1).max(1);
        let put = crate::par_map(threads, &tile_rows, &|| false, |(k, rec)| {
            Ok(((*k).clone(), decode(rec.as_ref())?.into()))
        })
        .unwrap_or_default()
        .into_iter()
        .collect::<Result<Vec<(TileKey, Tile)>>>()?;
        if !put.is_empty() && put.len() == rows.len() {
            let tiles = rows
                .iter()
                .filter_map(|w| match w {
                    Write::Tile(k, rec) => Some((k.clone(), rec.clone())),
                    _ => None,
                })
                .collect();
            self.writer().submit(tiles)?;
        } else {
            self.flush_writes()?;
            self.store.write(rows)?;
            if let Some(w) = self.writer.get() {
                w.wrote(put.iter().map(|(k, _)| k.clone()));
                if rows.iter().any(|r| matches!(r, Write::ShiftTiles { .. })) {
                    w.forget_written();
                }
            }
        }
        if put.is_empty() {
            return Ok(());
        }
        let cache = self.tiles();
        let dirty = put
            .iter()
            .map(|(k, _)| Ok((k.clone(), self.top_level(MapId(k.map_id))?)))
            .collect::<Result<Vec<_>>>()?;
        // The pyramid worker reads level 0 from the cache, so the cache gets the tiles first.
        cache.put(put);
        // One call, so the pyramid worker takes the whole write as one batch.
        cache.mark_dirty(dirty);
        Ok(())
    }

    /// Drops cached tiles and pending pyramid updates of `map`.
    pub(crate) fn forget_map_tiles(&self, map: MapId) {
        if let Some(cache) = self.cache.get() {
            cache.forget(|k| k.map_id == map.0);
            cache.drop_dirty(|k| k.map_id == map.0);
        }
    }

    /// After a map's tiles moved: drops its cached tiles, rebuilds its pyramid and derived layers.
    pub(crate) fn after_resize(&mut self, map: MapId) -> Result<()> {
        let s = self.map(map)?.settings;
        let top = top_level(&s);
        if let Some(cache) = self.cache.get() {
            cache.forget(|k| k.map_id == map.0);
        }
        let layers = self.kind_of(map)?.layers();
        let cache = self.tiles();
        for &layer in layers {
            let keys = (0..s.height.div_ceil(TILE_SIZE)).flat_map(|ty| {
                (0..s.width.div_ceil(TILE_SIZE)).map(move |tx| TileKey {
                    map_id: map.0,
                    layer,
                    level: 0,
                    tx,
                    ty,
                })
            });
            cache.mark_dirty(keys.map(|k| (k, top)));
        }
        self.pipeline.forget(map);
        self.rebuild_derived(map)
    }

    /// Blocks until cached pyramid levels show past edits, at most `PYRAMID_SHOWN_WAIT`; their writes go on after.
    pub(crate) fn wait_pyramid_shown(&self) {
        if let Some(cache) = self.cache.get() {
            cache.wait_shown(PYRAMID_SHOWN_WAIT);
        }
    }

    /// Blocks until pyramid levels above past edits are written; no-op before any edit.
    pub(crate) fn wait_pyramid(&self) {
        if let Some(cache) = self.cache.get() {
            cache.wait_idle();
        }
    }
}

/// Decoded pyramid tiles above `changed` level-0 tiles; `read` gives a parent tile before the change.
pub(crate) fn upper_levels(
    changed: Vec<(TileKey, Tile)>,
    top: u32,
    read: impl Fn(&TileKey) -> Result<Vec<f32>>,
) -> Result<Vec<(TileKey, Tile)>> {
    // Halving runs on several threads: the screen waits for these levels after an undo.
    let threads = crate::cpus().saturating_sub(1).max(1);
    let mut put = Vec::new();
    let mut cur = changed;
    for level in 1..=top {
        let halves =
            crate::par_map(threads, &cur, &|| false, |(_, t)| halve(t)).unwrap_or_default();
        let mut next: HashMap<TileKey, Vec<f32>> = HashMap::new();
        for ((k, _), half) in cur.iter().zip(halves) {
            let pk = TileKey {
                level,
                tx: k.tx / 2,
                ty: k.ty / 2,
                ..k.clone()
            };
            let parent = match next.entry(pk) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let v = read(e.key())?;
                    e.insert(v)
                }
            };
            place(parent, &half, k.tx, k.ty);
        }
        cur = next.into_iter().map(|(k, v)| (k, Tile::from(v))).collect();
        put.extend(cur.iter().cloned());
    }
    Ok(put)
}
