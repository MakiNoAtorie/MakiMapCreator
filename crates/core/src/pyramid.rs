//! Tile pyramid: level `l` halves level `l - 1`; each pixel is the mean of 2x2 pixels below.
//! The top level is one tile over the whole map and serves as the map's overview.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::Path;
use std::sync::Arc;

use mmc_store::{Reader, TileKey, TileRecord};

use crate::cache::{Tile, TileCache};
use crate::layer::{Layer, TILE_SIZE, decode, encode};
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
        layer: id.layer.as_str().to_owned(),
        level: id.level,
        tx: id.tx,
        ty: id.ty,
    }
}

/// Reads the overviews of `layer` from a project file without opening it, one per map.
pub fn read_overviews(file: &Path, layer: Layer) -> Result<Vec<Overview>> {
    let reader = Reader::open(file)?;
    reader
        .maps()?
        .into_iter()
        .map(|r| {
            let map = Map::from_record(r)?;
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
            .get_or_init(|| TileCache::new(self.store.working_copy()))
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

    /// Blocks until all queued tile loads are done.
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

    /// Writes level-0 tiles and the pyramid tiles above them in one transaction.
    pub(crate) fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        let mut all = writes.to_vec();
        let mut put: Vec<(TileKey, Tile)> = Vec::new();
        let mut cur: HashMap<TileKey, Vec<f32>> = HashMap::new();
        for (k, rec) in writes {
            cur.insert(k.clone(), decode(rec.as_ref())?);
        }
        let top = match writes.first() {
            Some((k, _)) => self.top_level(MapId(k.map_id))?,
            None => 0,
        };
        let half = (TILE_SIZE / 2) as usize;
        let t = TILE_SIZE as usize;
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
                        let rec = self.store.tile(e.key())?;
                        e.insert(decode(rec.as_ref())?)
                    }
                };
                let (ox, oy) = ((k.tx % 2) as usize * half, (k.ty % 2) as usize * half);
                for y in 0..half {
                    for x in 0..half {
                        let c = |dx: usize, dy: usize| child[(2 * y + dy) * t + 2 * x + dx];
                        parent[(oy + y) * t + ox + x] =
                            (c(0, 0) + c(1, 0) + c(0, 1) + c(1, 1)) * 0.25;
                    }
                }
            }
            for (k, values) in &next {
                let rec = TileRecord {
                    data: encode(values),
                    edited: false,
                };
                all.push((k.clone(), Some(rec)));
            }
            put.extend(cur.drain().map(|(k, v)| (k, v.into())));
            cur = next;
        }
        put.extend(cur.drain().map(|(k, v)| (k, v.into())));
        self.store.write_tiles(&all)?;
        if let Some(cache) = self.cache.get() {
            cache.put(put);
        }
        Ok(())
    }
}
