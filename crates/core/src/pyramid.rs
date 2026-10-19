//! Tile pyramid: level `l` halves level `l - 1`; each pixel is the mean of 2x2 pixels below.
//! The top level is one tile over the whole map and serves as the map's overview.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::persist::{ProjectReader, TileKey, TileRecord, Write};

use crate::cache::{Tile, TileCache};
use crate::layer::{Layer, TILE_LEN, TILE_SIZE, decode, encode};
use crate::{Map, MapId, MapSettings, Project, Result};

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
    let (t, half) = (TILE_SIZE as usize, TILE_SIZE as usize / 2);
    let (ox, oy) = ((tx % 2) as usize * half, (ty % 2) as usize * half);
    for y in 0..half {
        for x in 0..half {
            let c = |dx: usize, dy: usize| child[(2 * y + dy) * t + 2 * x + dx];
            parent[(oy + y) * t + ox + x] = (c(0, 0) + c(1, 0) + c(0, 1) + c(1, 1)) * 0.25;
        }
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
    fn tiles(&self) -> &TileCache {
        self.cache
            .get_or_init(|| TileCache::new(self.store.tile_opener()))
    }

    /// The coarsest pyramid level of `map`; its single tile covers the whole map.
    pub fn top_level(&self, map: MapId) -> Result<u32> {
        Ok(top_level(&self.map(map)?.settings))
    }

    /// Never blocks. Queues `id` for loading on workers and returns the finest loaded tile
    /// at `id.level` or above that covers it, with its level; `None` if none is loaded yet.
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

    /// Writes `rows` in one transaction. For level-0 tiles among them, a worker then updates
    /// the pyramid levels above.
    pub(crate) fn write_rows(&self, rows: &[Write]) -> Result<()> {
        let put = rows
            .iter()
            .filter_map(|w| match w {
                Write::Tile(k, rec) => Some((k, rec)),
                _ => None,
            })
            .map(|(k, rec)| Ok((k.clone(), decode(rec.as_ref())?.into())))
            .collect::<Result<Vec<(TileKey, Tile)>>>()?;
        self.store.write(rows)?;
        if put.is_empty() {
            return Ok(());
        }
        let cache = self.tiles();
        for (k, _) in &put {
            let top = self.top_level(MapId(k.map_id))?;
            cache.mark_dirty([k.clone()], top);
        }
        cache.put(put);
        Ok(())
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
            cache.mark_dirty(keys, top);
        }
        self.pipeline.forget(map);
        self.rebuild_derived(map)
    }

    /// Blocks until pyramid levels above past edits are written; no-op before any edit.
    pub(crate) fn wait_pyramid(&self) {
        if let Some(cache) = self.cache.get() {
            cache.wait_idle();
        }
    }
}

/// Pyramid tiles above `changed` level-0 tiles (with their new values): the records to
/// write and the decoded tiles. `read` gives the stored parents, for their other quadrants.
pub(crate) type Upper = (Vec<(TileKey, Option<TileRecord>)>, Vec<(TileKey, Tile)>);

pub(crate) fn upper_levels(
    changed: Vec<(TileKey, Vec<f32>)>,
    top: u32,
    read: impl Fn(&TileKey) -> Result<Option<TileRecord>>,
) -> Result<Upper> {
    let (mut writes, mut put) = (Vec::new(), Vec::new());
    let mut cur: HashMap<TileKey, Vec<f32>> = changed.into_iter().collect();
    for level in 1..=top {
        let mut next: HashMap<TileKey, Vec<f32>> = HashMap::new();
        for (k, child) in &cur {
            let pk = TileKey {
                level,
                tx: k.tx / 2,
                ty: k.ty / 2,
                ..k.clone()
            };
            let parent = match next.entry(pk) {
                Entry::Occupied(e) => e.into_mut(),
                Entry::Vacant(e) => {
                    let rec = read(e.key())?;
                    e.insert(decode(rec.as_ref())?)
                }
            };
            downsample(parent, child, k.tx, k.ty);
        }
        for (k, values) in &next {
            let rec = TileRecord {
                data: encode(values),
                edited: false,
            };
            writes.push((k.clone(), Some(rec)));
        }
        cur = next;
        put.extend(
            cur.iter()
                .map(|(k, v)| (k.clone(), Tile::from(v.as_slice()))),
        );
    }
    Ok((writes, put))
}
