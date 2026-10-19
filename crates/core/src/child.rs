//! Child maps: made from an entity outline on the parent, own only it, keep parent context (ADR-0019).

use crate::persist::{ContextRecord, NewChildMap, TileRecord, Write};

use crate::generation::{decode_outline, encode_outline};
use crate::layer::{TILE_SIZE, decode, encode, tile_of};
use crate::undo::Change;
use crate::{EntityId, Error, Layer, Map, MapId, MapSettings, Project, Result, RootPoint};

/// Margin around a child's outline in parent pixels: refinement moves a coast about one cell (ADR-0008).
pub const CHILD_MARGIN: f64 = 2.0;

/// Largest child side in pixels, the default region size (256 MB of f32 a layer while generating).
pub const MAX_CHILD_SIZE: f64 = 8192.0;

/// A snapshot of one parent layer around a child map, taken when the child was made.
#[derive(Debug, Clone, PartialEq)]
pub struct ParentContext {
    /// The parent's transform.
    origin: RootPoint,
    metres_per_pixel: f64,
    /// Parent pixel of `values[0]`.
    x0: u32,
    y0: u32,
    width: u32,
    height: u32,
    values: Vec<f32>,
}

impl ParentContext {
    /// Bilinear sample at a root-space point; global parent pixel indices make snapshots agree.
    pub fn sample(&self, at: RootPoint) -> f32 {
        let m = self.metres_per_pixel;
        let gx = (at.x - self.origin.x) / m - 0.5;
        let gy = (at.y - self.origin.y) / m - 0.5;
        let (fx, fy) = (gx.floor(), gy.floor());
        let (tx, ty) = ((gx - fx) as f32, (gy - fy) as f32);
        let ix = |g: f64| (g as i64 - self.x0 as i64).clamp(0, self.width as i64 - 1) as usize;
        let iy = |g: f64| (g as i64 - self.y0 as i64).clamp(0, self.height as i64 - 1) as usize;
        let v = |x: f64, y: f64| self.values[iy(y) * self.width as usize + ix(x)];
        let top = v(fx, fy) + (v(fx + 1.0, fy) - v(fx, fy)) * tx;
        let bottom = v(fx, fy + 1.0) + (v(fx + 1.0, fy + 1.0) - v(fx, fy + 1.0)) * tx;
        top + (bottom - top) * ty
    }
}

/// Even-odd point in polygon.
pub(crate) fn inside(poly: &[RootPoint], p: RootPoint) -> bool {
    let mut odd = false;
    for (a, b) in poly.iter().zip(poly.iter().cycle().skip(1)) {
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            odd = !odd;
        }
    }
    odd
}

impl Map {
    /// Root-space centre of pixel (`x`, `y`).
    pub(crate) fn pixel_centre(&self, x: u32, y: u32) -> RootPoint {
        let m = self.settings.metres_per_pixel;
        RootPoint {
            x: self.origin.x + (x as f64 + 0.5) * m,
            y: self.origin.y + (y as f64 + 0.5) * m,
        }
    }
}

impl Project {
    /// Makes a child map of `entity`'s outline on `parent` (bounding box plus `CHILD_MARGIN`).
    pub fn create_child_map(
        &mut self,
        parent: MapId,
        entity: EntityId,
        metres_per_pixel: Option<f64>,
    ) -> Result<MapId> {
        let pm = self.map(parent)?.clone();
        let decl = self
            .kind_of(parent)?
            .child()
            .ok_or(Error::NoChildMaps(pm.settings.kind))?;
        let kind = decl.kind;
        let outline = self
            .outlines(parent)?
            .into_iter()
            .find(|o| o.0 == entity)
            .ok_or(Error::NoOutline(entity))?
            .1;
        let mut mpp = metres_per_pixel.unwrap_or(pm.settings.metres_per_pixel / decl.scale_factor);
        if !(mpp.is_finite() && mpp > 0.0) {
            return Err(Error::InvalidScale);
        }
        let margin = CHILD_MARGIN * pm.settings.metres_per_pixel;
        let (mut lo, mut hi) = (
            RootPoint {
                x: f64::MAX,
                y: f64::MAX,
            },
            RootPoint {
                x: f64::MIN,
                y: f64::MIN,
            },
        );
        for q in &outline {
            (lo.x, lo.y) = (lo.x.min(q.x - margin), lo.y.min(q.y - margin));
            (hi.x, hi.y) = (hi.x.max(q.x + margin), hi.y.max(q.y + margin));
        }
        // The default scale doubles until the map fits, so equal areas still share a scale.
        let side = (hi.x - lo.x).max(hi.y - lo.y);
        while metres_per_pixel.is_none() && side / mpp + 2.0 > MAX_CHILD_SIZE {
            mpp *= 2.0;
        }
        // On the child's pixel grid in root space, so neighbours share pixel centres.
        let origin = RootPoint {
            x: (lo.x / mpp).floor() * mpp,
            y: (lo.y / mpp).floor() * mpp,
        };
        let (w, h) = (
            ((hi.x - origin.x) / mpp).ceil(),
            ((hi.y - origin.y) / mpp).ceil(),
        );
        if !(w >= 1.0 && h >= 1.0 && w <= MAX_CHILD_SIZE && h <= MAX_CHILD_SIZE) {
            return Err(Error::ChildTooLarge);
        }
        let settings = MapSettings {
            kind,
            width: w as u32,
            height: h as u32,
            metres_per_pixel: mpp,
        };

        let context = self.snapshot(decl.context, &pm, origin, w * mpp, h * mpp)?;

        let id = MapId(self.store.insert_child_map(&NewChildMap {
            parent_id: parent.0,
            kind: kind.as_str().into(),
            width: settings.width,
            height: settings.height,
            metres_per_pixel: mpp,
            origin_x: origin.x,
            origin_y: origin.y,
            entity_id: entity.0,
            outline: encode_outline(&outline),
            context,
        })?);
        let map = Map {
            id,
            parent: Some(parent),
            settings,
            origin,
            outline: Some(outline),
        };
        self.maps.push(map.clone());
        // Undo of the parent's history removes the child again (#1 story 35).
        self.record(parent, Change::NewMap(map))?;
        Ok(id)
    }

    /// Writes a child map's creation (`forward`) or its removal.
    pub(crate) fn write_new_map(&mut self, m: &Map, forward: bool) -> Result<()> {
        if !forward && self.maps.iter().any(|x| x.parent == Some(m.id)) {
            return Err(Error::MapHasChildren(m.id));
        }
        self.store.write(&[Write::MapDeleted(m.id.0, !forward)])?;
        self.maps.retain(|x| x.id != m.id);
        if forward {
            self.maps.push(m.clone());
            self.maps.sort_by_key(|x| x.id);
            self.rebuild_derived(m.id)?;
        } else {
            self.pipeline.forget(m.id);
            self.forget_map_tiles(m.id);
        }
        Ok(())
    }

    /// A snapshot of `pm`'s `layers` under a child area, plus one pixel for interpolation.
    pub(crate) fn snapshot(
        &self,
        layers: &[Layer],
        pm: &Map,
        origin: RootPoint,
        width_m: f64,
        height_m: f64,
    ) -> Result<Vec<ContextRecord>> {
        // Parent pixels under the child plus one for interpolation, clipped to the parent.
        let (px0, py0) = pm.to_pixels(origin);
        let (px1, py1) = pm.to_pixels(RootPoint {
            x: origin.x + width_m,
            y: origin.y + height_m,
        });
        let clip = |v: f64, n: u32| v.clamp(0.0, n as f64) as u32;
        let (x0, x1) = (
            clip(px0.floor() - 1.0, pm.settings.width),
            clip(px1.ceil() + 1.0, pm.settings.width),
        );
        let (y0, y1) = (
            clip(py0.floor() - 1.0, pm.settings.height),
            clip(py1.ceil() + 1.0, pm.settings.height),
        );
        let mut context = vec![];
        for &layer in layers {
            if x0 == x1 || y0 == y1 {
                break;
            }
            let mut values = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
            let mut tile = None;
            for y in y0..y1 {
                for x in x0..x1 {
                    let (t, i) = tile_of(x, y);
                    if tile.as_ref().is_none_or(|(k, _)| *k != t) {
                        tile = Some((t, self.layer_tile(pm.id, layer, t.0, t.1)?));
                    }
                    values.push(tile.as_ref().unwrap().1[i]);
                }
            }
            context.push(ContextRecord {
                layer: layer.as_str().into(),
                origin_x: pm.origin.x,
                origin_y: pm.origin.y,
                metres_per_pixel: pm.settings.metres_per_pixel,
                x0,
                y0,
                width: x1 - x0,
                height: y1 - y0,
                data: encode(&values),
            });
        }
        Ok(context)
    }

    /// The snapshot of the parent's `layer` kept with `map`; `None` for a root map.
    pub fn parent_context(&self, map: MapId, layer: Layer) -> Result<Option<ParentContext>> {
        self.map(map)?;
        let r = match self.context_override(map, layer) {
            Some(r) => r,
            None => match self.store.parent_context(map.0, layer.as_str())? {
                Some(r) => r,
                None => return Ok(None),
            },
        };
        let bytes = zstd::decode_all(r.data.as_slice()).map_err(|_| Error::CorruptTile)?;
        let values: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        if values.len() != (r.width * r.height) as usize || values.is_empty() {
            return Err(Error::CorruptTile);
        }
        Ok(Some(ParentContext {
            origin: RootPoint {
                x: r.origin_x,
                y: r.origin_y,
            },
            metres_per_pixel: r.metres_per_pixel,
            x0: r.x0,
            y0: r.y0,
            width: r.width,
            height: r.height,
            values,
        }))
    }

    /// Whether `map` owns the ground at `at` (ADR-0019). A root map owns all of it.
    pub fn owns(&self, map: MapId, at: RootPoint) -> Result<bool> {
        Ok(self
            .map(map)?
            .outline
            .as_ref()
            .is_none_or(|o| inside(o, at)))
    }

    /// Keeps only the pixels a child map owns; refuses a command that changes none of them.
    pub(crate) fn clip_to_outline(&self, map: MapId, change: Change) -> Result<Change> {
        let map = self.map(map)?;
        let (Some(outline), Change::Tiles(tiles)) = (&map.outline, &change) else {
            return Ok(change);
        };
        let mut kept = vec![];
        for c in tiles {
            let before = decode(c.before.as_ref())?;
            let mut after = decode(c.after.as_ref())?;
            let mut owned = false;
            for (i, (a, b)) in after.iter_mut().zip(&before).enumerate() {
                if a == b {
                    continue;
                }
                let x = c.key.tx * TILE_SIZE + i as u32 % TILE_SIZE;
                let y = c.key.ty * TILE_SIZE + i as u32 / TILE_SIZE;
                if inside(outline, map.pixel_centre(x, y)) {
                    owned = true;
                } else {
                    *a = *b;
                }
            }
            if owned {
                let mut c = c.clone();
                c.after = Some(TileRecord {
                    data: encode(&after),
                    edited: true,
                });
                kept.push(c);
            }
        }
        if kept.is_empty() && !tiles.is_empty() {
            return Err(Error::OutsideOutline);
        }
        Ok(Change::Tiles(kept))
    }

    /// Outlines of child maps, read on open.
    pub(crate) fn load_child_outlines(&mut self) -> Result<()> {
        for r in self.store.child_maps()? {
            let outline = decode_outline(&r.outline)?;
            if let Some(m) = self.maps.iter_mut().find(|m| m.id.0 == r.map_id) {
                m.outline = Some(outline);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_interpolates_between_pixel_centres_and_clamps_at_edges() {
        let c = ParentContext {
            origin: RootPoint { x: 0.0, y: 0.0 },
            metres_per_pixel: 10.0,
            x0: 3,
            y0: 0,
            width: 2,
            height: 1,
            values: vec![0.0, 1.0],
        };
        let s = |x| c.sample(RootPoint { x, y: 5.0 });
        assert_eq!(s(35.0), 0.0);
        assert_eq!(s(40.0), 0.5);
        assert_eq!(s(45.0), 1.0);
        assert_eq!(s(0.0), 0.0);
        assert_eq!(s(99.0), 1.0);
    }
}
