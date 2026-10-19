//! Paint: sets a layer to one value inside a disc, for example land and sea.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::persist::TileRecord;

use crate::layer::{Layer, RootPoint, decode, encode, tile_of};
use crate::undo::TileChange;
use crate::{Map, Project, Result};

/// Sets `layer` to `value` within `radius` of each point.
#[derive(Debug, Clone, PartialEq)]
pub struct PaintStroke {
    pub layer: Layer,
    pub points: Vec<RootPoint>,
    /// Metres.
    pub radius: f64,
    pub value: f32,
}

impl Project {
    pub(crate) fn paint(&self, map: &Map, s: &PaintStroke) -> Result<Vec<TileChange>> {
        let mut tiles: BTreeMap<(u32, u32), (Option<TileRecord>, Vec<f32>)> = BTreeMap::new();
        let (w, h) = (map.settings.width as i64, map.settings.height as i64);
        let r = s.radius / map.settings.metres_per_pixel;
        for p in &s.points {
            let (cx, cy) = map.to_pixels(*p);
            for y in ((cy - r).floor() as i64).max(0)..((cy + r).ceil() as i64).min(h) {
                for x in ((cx - r).floor() as i64).max(0)..((cx + r).ceil() as i64).min(w) {
                    if (x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2) >= r * r {
                        continue;
                    }
                    let (t, i) = tile_of(x as u32, y as u32);
                    let tile = match tiles.entry(t) {
                        Entry::Occupied(e) => e.into_mut(),
                        Entry::Vacant(e) => {
                            let before = self.store.tile(&map.tile_key(s.layer, t.0, t.1))?;
                            let values = decode(before.as_ref())?;
                            e.insert((before, values))
                        }
                    };
                    tile.1[i] = s.value;
                }
            }
        }
        Ok(tiles
            .into_iter()
            .map(|((tx, ty), (before, values))| TileChange {
                key: map.tile_key(s.layer, tx, ty),
                before,
                after: Some(TileRecord {
                    data: encode(&values),
                    edited: true,
                }),
            })
            .collect())
    }
}
