//! One-time move of the old authored land layer into height, on open (ADR-0024).

use crate::layer::{COAST_RELIEF, TILE_SIZE, WATER_LEVEL, decode, decode_values, encode};
use crate::persist::{ContextRecord, TileRecord, Write};
use crate::{Layer, MapId, Project, Result};

/// The land layer of older files: above 0.5 is land.
const LAND: Layer = Layer::new("land");

/// The height of a pixel with old land value `land` and height `h`.
fn migrated(land: f32, h: f32) -> f32 {
    let is_land = land > 0.5;
    if is_land && h <= WATER_LEVEL {
        WATER_LEVEL + COAST_RELIEF
    } else if !is_land && h >= WATER_LEVEL {
        // Old worlds had no height, so all their sea was at 0.
        WATER_LEVEL - COAST_RELIEF
    } else {
        h
    }
}

impl Project {
    /// Writes old land into height and deletes the land layer and land parent context,
    /// one transaction per map. A file without them is left alone, so this runs once.
    pub(crate) fn migrate_land(&mut self) -> Result<()> {
        let maps: Vec<MapId> = self.maps.iter().map(|m| m.id).collect();
        for id in maps {
            let mut rows = self.migrated_tiles(id)?;
            rows.extend(self.migrated_context(id)?);
            if !rows.is_empty() {
                self.write_rows(&rows)?;
            }
        }
        Ok(())
    }

    fn migrated_tiles(&self, id: MapId) -> Result<Vec<Write>> {
        let map = self.map(id)?.clone();
        let s = map.settings;
        let (nx, ny) = (s.width.div_ceil(TILE_SIZE), s.height.div_ceil(TILE_SIZE));
        let land = self.store.tile_range(id.0, LAND, 0..nx, 0..ny)?;
        if land.is_empty() {
            return Ok(vec![]);
        }
        // A missing land tile is sea, so every tile of the map is migrated.
        let land: std::collections::BTreeMap<_, _> = land.into_iter().collect();
        let mut rows = vec![Write::DropLayer {
            map_id: id.0,
            layer: LAND,
        }];
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
            rows.push(Write::Tile(key, Some(TileRecord { data, edited })));
        }
        Ok(rows)
    }

    fn migrated_context(&self, id: MapId) -> Result<Option<Write>> {
        if self.store.parent_context(id.0, LAND.as_str())?.is_none() {
            return Ok(None);
        }
        let Some(mut state) = self.store.child_state(id.0)? else {
            return Ok(None);
        };
        let i = state
            .context
            .iter()
            .position(|r| r.layer == LAND.as_str())
            .expect("land context read above");
        let land = state.context.remove(i);
        let len = (land.width * land.height) as usize;
        let h = match state
            .context
            .iter()
            .position(|r| r.layer == Layer::HEIGHT.as_str())
        {
            Some(j) => decode_values(&state.context.remove(j).data, len)?,
            None => vec![0.0; len],
        };
        let out: Vec<f32> = decode_values(&land.data, len)?
            .into_iter()
            .zip(h)
            .map(|(l, h)| migrated(l, h))
            .collect();
        state.context.push(ContextRecord {
            layer: Layer::HEIGHT.as_str().into(),
            data: encode(&out),
            ..land
        });
        Ok(Some(Write::ChildState(id.0, state)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn land_rises_above_and_sea_sinks_below_water_and_other_heights_stay() {
        assert_eq!(migrated(1.0, 0.0), COAST_RELIEF);
        assert_eq!(migrated(1.0, -50.0), COAST_RELIEF);
        assert_eq!(migrated(1.0, 700.0), 700.0);
        assert_eq!(migrated(0.0, 0.0), -COAST_RELIEF);
        assert_eq!(migrated(0.0, 50.0), -COAST_RELIEF);
        assert_eq!(migrated(0.0, -5.0), -5.0);
    }
}
