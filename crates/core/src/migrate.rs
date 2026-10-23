//! One-time move of the old authored land layer into height, on open (ADR-0024).

use std::collections::BTreeSet;

use crate::layer::{TILE_SIZE, WATER_LEVEL, decode, encode};
use crate::persist::{ContextRecord, TileKey, TileRecord, Write};
use crate::{Error, Layer, MapId, Project, Result};

/// The land layer of older files: above 0.5 is land.
const LAND: Layer = Layer::new("land");
/// Height for old land at or below water level, and its negative for old sea at or above it;
/// equal to the world kind's paint targets, so old children regenerate as before.
const MIGRATED_RELIEF: f32 = 200.0;

/// The height of a pixel with old land value `land` and height `h`.
fn migrated(land: f32, h: f32) -> f32 {
    let is_land = land > 0.5;
    if is_land && h <= WATER_LEVEL {
        WATER_LEVEL + MIGRATED_RELIEF
    } else if !is_land && h >= WATER_LEVEL {
        // Old sea at water level too: worlds had no height, so all their sea was at 0.
        WATER_LEVEL - MIGRATED_RELIEF
    } else {
        h
    }
}

fn values(r: &ContextRecord) -> Result<Vec<f32>> {
    let bytes = zstd::decode_all(r.data.as_slice()).map_err(|_| Error::CorruptTile)?;
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

impl Project {
    /// Writes old land into height and deletes the land layer and land parent context.
    /// A file without them is left alone, so this runs once.
    pub(crate) fn migrate_land(&mut self) -> Result<()> {
        let maps: Vec<MapId> = self.maps.iter().map(|m| m.id).collect();
        for id in maps {
            self.migrate_tiles(id)?;
            self.migrate_context(id)?;
        }
        Ok(())
    }

    fn migrate_tiles(&self, id: MapId) -> Result<()> {
        let map = self.map(id)?.clone();
        let s = map.settings;
        let (nx, ny) = (s.width.div_ceil(TILE_SIZE), s.height.div_ceil(TILE_SIZE));
        let land = self.store.tile_range(id.0, LAND, 0..nx, 0..ny)?;
        if land.is_empty() {
            return Ok(());
        }
        // A missing land tile is sea, so every tile of the map is migrated.
        let land: std::collections::BTreeMap<_, _> = land.into_iter().collect();
        let top = self.top_level(id)?;
        let mut rows = vec![];
        let mut upper = BTreeSet::new();
        let mut put = vec![];
        for (tx, ty) in (0..ny).flat_map(|ty| (0..nx).map(move |tx| (tx, ty))) {
            let key = map.tile_key(Layer::HEIGHT, tx, ty);
            let h = self.tile_record(&key)?;
            let l = land.get(&(tx, ty));
            let mut values = decode(h.as_ref())?;
            for (v, l) in values.iter_mut().zip(decode(l)?) {
                *v = migrated(l, *v);
            }
            let edited = l.is_some_and(|r| r.edited) || h.is_some_and(|h| h.edited);
            let data = encode(&values);
            rows.push(Write::Tile(key.clone(), Some(TileRecord { data, edited })));
            put.push((key, values.into()));
            if l.is_some() {
                rows.push(Write::Tile(map.tile_key(LAND, tx, ty), None));
                for level in 1..=top {
                    upper.insert((level, tx >> level, ty >> level));
                }
            }
        }
        rows.extend(upper.into_iter().map(|(level, tx, ty)| {
            let key = TileKey {
                level,
                ..map.tile_key(LAND, tx, ty)
            };
            Write::Tile(key, None)
        }));
        // One transaction, so a crash keeps either the land layer or its height.
        self.store.write(&rows)?;
        let keys: Vec<TileKey> = put.iter().map(|(k, _)| k.clone()).collect();
        let cache = self.tiles();
        cache.put(put);
        cache.mark_dirty(keys, top);
        Ok(())
    }

    fn migrate_context(&self, id: MapId) -> Result<()> {
        if self.store.parent_context(id.0, LAND.as_str())?.is_none() {
            return Ok(());
        }
        let Some(mut state) = self.store.child_state(id.0)? else {
            return Ok(());
        };
        let i = state
            .context
            .iter()
            .position(|r| r.layer == LAND.as_str())
            .expect("land context read above");
        let land = state.context.remove(i);
        let h = match state
            .context
            .iter()
            .position(|r| r.layer == Layer::HEIGHT.as_str())
        {
            Some(j) => values(&state.context.remove(j))?,
            None => vec![0.0; (land.width * land.height) as usize],
        };
        let out: Vec<f32> = values(&land)?
            .into_iter()
            .zip(h)
            .map(|(l, h)| migrated(l, h))
            .collect();
        state.context.push(ContextRecord {
            layer: Layer::HEIGHT.as_str().into(),
            data: encode(&out),
            ..land
        });
        self.store.write(&[Write::ChildState(id.0, state)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn land_rises_above_and_sea_sinks_below_water_and_other_heights_stay() {
        assert_eq!(migrated(1.0, 0.0), MIGRATED_RELIEF);
        assert_eq!(migrated(1.0, -50.0), MIGRATED_RELIEF);
        assert_eq!(migrated(1.0, 700.0), 700.0);
        assert_eq!(migrated(0.0, 0.0), -MIGRATED_RELIEF);
        assert_eq!(migrated(0.0, 50.0), -MIGRATED_RELIEF);
        assert_eq!(migrated(0.0, -5.0), -5.0);
    }
}
