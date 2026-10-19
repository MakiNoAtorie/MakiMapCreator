//! Commands: every edit is one. Raster commands produce tile changes.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use crate::persist::TileRecord;

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
    /// A map object edit on the map it is applied on.
    Object(crate::ObjectEdit),
}

/// Adds `strength * pressure * weight` to `layer` around each point; one stroke is one command.
#[derive(Debug, Clone, PartialEq)]
pub struct BrushStroke {
    pub layer: Layer,
    pub points: Vec<StrokePoint>,
    /// Layer units at the centre at full pressure; negative lowers.
    pub strength: f32,
    /// 0 is a hard disc; 1 is the smooth `(1 - d²)²` profile; between, a hard core then that profile.
    pub falloff: f32,
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
    let core = (1.0 - falloff.clamp(0.0, 1.0) as f64).powi(2);
    if d2 <= core {
        return 1.0;
    }
    let t = (1.0 - d2) / (1.0 - core);
    (t * t) as f32
}

impl Project {
    pub(crate) fn changes(&self, map: &Map, cmd: &Command) -> Result<Change> {
        match cmd {
            Command::Brush(s) => Ok(Change::Tiles(self.brush(map, s)?)),
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
            Command::Paint(s) => Ok(Change::Tiles(self.paint(map, s)?)),
            Command::Generate(g) => Ok(Change::Generation(self.generation_change(
                map,
                g,
                (0, 0),
            )?)),
            Command::Object(e) => Ok(Change::Objects(self.object_change(map.id, e)?)),
        }
    }

    fn brush(&self, map: &Map, s: &BrushStroke) -> Result<Vec<TileChange>> {
        let mut tiles: BTreeMap<(u32, u32), (Option<TileRecord>, Vec<f32>)> = BTreeMap::new();
        let (w, h) = (map.settings.width as i64, map.settings.height as i64);
        for p in &s.points {
            let r = p.radius / map.settings.metres_per_pixel;
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
                    tile.1[i] += amount * brush_weight(d2, s.falloff);
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
