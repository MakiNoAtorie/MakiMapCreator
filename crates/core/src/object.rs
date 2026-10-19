//! Map objects: points, paths and polygons on an object layer of one map. Each has a stable
//! id, may reference an entity and may carry a label offset.

use crate::generation::{decode_outline, encode_outline};
use crate::persist::{ObjectRecord, Write};
use crate::{EntityId, Error, Layer, MapId, Project, Result, RootPoint};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub(crate) i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Point,
    Path,
    Polygon,
}

impl Shape {
    fn as_str(self) -> &'static str {
        match self {
            Shape::Point => "point",
            Shape::Path => "path",
            Shape::Polygon => "polygon",
        }
    }

    fn parse(s: &str) -> Result<Shape> {
        [Shape::Point, Shape::Path, Shape::Polygon]
            .into_iter()
            .find(|x| x.as_str() == s)
            .ok_or(Error::CorruptEntity)
    }

    /// The fewest points a shape needs.
    fn min_points(self) -> usize {
        match self {
            Shape::Point => 1,
            Shape::Path => 2,
            Shape::Polygon => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapObject {
    /// For `ObjectEdit::Create`, the id from `Project::next_object_id`.
    pub id: ObjectId,
    pub layer: Layer,
    pub shape: Shape,
    /// Root space; a point has one.
    pub points: Vec<RootPoint>,
    pub entity: Option<EntityId>,
    /// Label position relative to the object's first point, metres.
    pub label_offset: Option<(f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ObjectEdit {
    Create(MapObject),
    /// Replaces the object with the same id.
    Update(MapObject),
    Delete(ObjectId),
}

/// Object rows before and after one edit.
#[derive(Debug, Clone)]
pub(crate) struct ObjectChange {
    rows: Vec<(i64, Option<ObjectRecord>, Option<ObjectRecord>)>,
}

impl ObjectChange {
    pub(crate) fn rows(&self, forward: bool) -> Vec<Write> {
        self.rows
            .iter()
            .map(|(id, b, a)| Write::Object(*id, if forward { a } else { b }.clone()))
            .collect()
    }
}

fn dist(a: RootPoint, b: RootPoint) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

pub(crate) fn segment_dist(p: RootPoint, a: RootPoint, b: RootPoint) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    };
    dist(
        p,
        RootPoint {
            x: a.x + t * dx,
            y: a.y + t * dy,
        },
    )
}

pub(crate) fn record_of(map: MapId, o: &MapObject) -> ObjectRecord {
    ObjectRecord {
        id: o.id.0,
        map_id: map.0,
        layer: o.layer.as_str().into(),
        shape: o.shape.as_str().into(),
        points: encode_outline(&o.points),
        entity_id: o.entity.map(|e| e.0),
        label_dx: o.label_offset.map(|l| l.0),
        label_dy: o.label_offset.map(|l| l.1),
    }
}

impl Project {
    /// The id the next new object gets; ids are never reused.
    pub fn next_object_id(&self) -> Result<ObjectId> {
        Ok(ObjectId(self.store.next_object_id()?))
    }

    /// Objects on `map`, ordered by id. Layers no kind of this project names are skipped.
    pub fn objects(&self, map: MapId) -> Result<Vec<MapObject>> {
        self.map(map)?;
        let layers = self.object_layers(map)?;
        let mut out = vec![];
        for r in self.store.objects(map.0)? {
            let Some(&layer) = layers.iter().find(|l| l.as_str() == r.layer) else {
                continue;
            };
            out.push(MapObject {
                id: ObjectId(r.id),
                layer,
                shape: Shape::parse(&r.shape)?,
                points: decode_outline(&r.points)?,
                entity: r.entity_id.map(EntityId),
                label_offset: r.label_dx.zip(r.label_dy),
            });
        }
        Ok(out)
    }

    /// The entity drawn at `at` on `map`: the newest map object of an existing entity that
    /// covers it (a polygon) or lies within `tolerance` metres (a point or path), else an
    /// anchored river or lake within `tolerance`.
    pub fn entity_at(&self, map: MapId, at: RootPoint, tolerance: f64) -> Result<Option<EntityId>> {
        for o in self.objects(map)?.into_iter().rev() {
            let Some(e) = o.entity else { continue };
            let hit = match o.shape {
                Shape::Point => dist(o.points[0], at) <= tolerance,
                Shape::Path => o
                    .points
                    .windows(2)
                    .any(|w| segment_dist(at, w[0], w[1]) <= tolerance),
                Shape::Polygon => crate::child::inside(&o.points, at),
            };
            if hit && self.store.entity(e.0)?.is_some() {
                return Ok(Some(e));
            }
        }
        self.anchored_at(at, tolerance)
    }

    /// The object layers of `map`: its kind's, plus `Layer::OUTLINES`.
    fn object_layers(&self, map: MapId) -> Result<Vec<Layer>> {
        let mut l = self.kind_of(map)?.object_layers().to_vec();
        l.push(Layer::OUTLINES);
        Ok(l)
    }

    pub(crate) fn object_change(&self, map: MapId, edit: &ObjectEdit) -> Result<ObjectChange> {
        let current = |id: ObjectId| -> Result<ObjectRecord> {
            self.store
                .objects(map.0)?
                .into_iter()
                .find(|r| r.id == id.0)
                .ok_or(Error::UnknownObject(id))
        };
        let check = |o: &MapObject| -> Result<()> {
            if !self.object_layers(map)?.contains(&o.layer) {
                return Err(Error::NotAnObjectLayer(o.layer));
            }
            if o.points.len() < o.shape.min_points()
                || (o.shape == Shape::Point && o.points.len() > 1)
            {
                return Err(Error::InvalidObject(o.id));
            }
            if let Some(e) = o.entity
                && self.entity(e)?.is_none()
            {
                return Err(Error::UnknownEntity(e));
            }
            Ok(())
        };
        let row = match edit {
            ObjectEdit::Create(o) => {
                check(o)?;
                if o.id.0 < self.store.next_object_id()? {
                    return Err(Error::InvalidObject(o.id));
                }
                (o.id.0, None, Some(record_of(map, o)))
            }
            ObjectEdit::Update(o) => {
                check(o)?;
                (o.id.0, Some(current(o.id)?), Some(record_of(map, o)))
            }
            ObjectEdit::Delete(id) => (id.0, Some(current(*id)?), None),
        };
        Ok(ObjectChange { rows: vec![row] })
    }
}
