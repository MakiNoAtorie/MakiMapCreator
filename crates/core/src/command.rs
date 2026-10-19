//! Commands: every edit is one. Raster commands produce tile changes.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use mmc_store::TileRecord;

use crate::layer::{Layer, RootPoint, decode, encode, tile_of};
use crate::undo::{Change, TileChange};
use crate::{EntityEdit, Map, Project, Result};

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
}

/// Adds `strength * pressure * falloff` to `layer` around each point.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushStroke {
    pub layer: Layer,
    pub points: Vec<StrokePoint>,
    /// Metres.
    pub radius: f64,
    /// Layer units at the centre at full pressure; negative lowers.
    pub strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokePoint {
    pub at: RootPoint,
    /// 0..=1.
    pub pressure: f32,
}

impl Project {
    pub(crate) fn changes(&self, map: &Map, cmd: &Command) -> Result<Change> {
        match cmd {
            Command::Brush(s) => Ok(Change::Tiles(self.brush(map, s)?)),
            Command::Entity(e @ EntityEdit::DeleteWithMap { .. }) => {
                Ok(Change::Deletion(self.entity_deletion(e)?))
            }
            Command::Entity(e) => Ok(Change::Entities(self.entity_changes(e)?)),
            Command::Page(e) => Ok(Change::Page(self.page_change(e)?)),
            Command::Anchor(e) => Ok(Change::Anchor(self.anchor_change(e)?)),
            Command::Paint(s) => Ok(Change::Tiles(self.paint(map, s)?)),
            Command::Generate(g) => Ok(Change::Generation(self.generation_change(map, g)?)),
        }
    }

    fn brush(&self, map: &Map, s: &BrushStroke) -> Result<Vec<TileChange>> {
        let mut tiles: BTreeMap<(u32, u32), (Option<TileRecord>, Vec<f32>)> = BTreeMap::new();
        let (w, h) = (map.settings.width as i64, map.settings.height as i64);
        let r = s.radius / map.settings.metres_per_pixel;
        for p in &s.points {
            let (cx, cy) = map.to_pixels(p.at);
            let amount = s.strength * p.pressure;
            for y in ((cy - r).floor() as i64).max(0)..((cy + r).ceil() as i64).min(h) {
                for x in ((cx - r).floor() as i64).max(0)..((cx + r).ceil() as i64).min(w) {
                    let d2 =
                        ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)) / (r * r);
                    if d2 >= 1.0 {
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
                    tile.1[i] += amount * ((1.0 - d2) * (1.0 - d2)) as f32;
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
