//! Anchors: named rivers and lakes keep their identity across derived rebuilds (ADR-0023).

use crate::persist::{AnchorRecord, Write};

use crate::{AnchorProblem, EntityId, Error, Project, Result, RootPoint};

/// Metres between an anchor and a derived feature for a match.
pub const DEFAULT_ANCHOR_TOLERANCE: f64 = 500.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FeatureId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureKind {
    River,
    Lake,
}

/// A river or lake a derived stage found. Root-space metres.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedFeature {
    pub id: FeatureId,
    pub kind: FeatureKind,
    /// River: a polyline ordered from the source to the mouth. Lake: a polygon ring.
    pub geometry: Vec<RootPoint>,
}

/// Where a named feature is, in root space.
#[derive(Debug, Clone, PartialEq)]
pub enum Anchors {
    River { mouth: RootPoint, picked: RootPoint },
    Lake { inside: RootPoint },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AnchorEdit {
    /// Sets the anchors and matches them at once against the known derived features.
    Set { entity: EntityId, anchors: Anchors },
    /// Attaches an entity to a derived feature, moving its anchors onto that feature.
    Reattach {
        entity: EntityId,
        feature: FeatureId,
    },
}

pub(crate) struct State {
    /// `None` until a derived stage has output features; matching waits for it.
    features: Option<Vec<DerivedFeature>>,
    tolerance: f64,
}

impl Default for State {
    fn default() -> Self {
        State {
            features: None,
            tolerance: DEFAULT_ANCHOR_TOLERANCE,
        }
    }
}

/// Anchor rows before and after one anchor edit.
#[derive(Debug, Clone)]
pub(crate) struct AnchorChange {
    entity: i64,
    before: Option<AnchorRecord>,
    after: Option<AnchorRecord>,
}

impl Anchors {
    fn kind(&self) -> FeatureKind {
        match self {
            Anchors::River { .. } => FeatureKind::River,
            Anchors::Lake { .. } => FeatureKind::Lake,
        }
    }

    fn encode(&self) -> String {
        let pts = match self {
            Anchors::River { mouth, picked } => vec![mouth, picked],
            Anchors::Lake { inside } => vec![inside],
        };
        let pts: Vec<_> = pts.iter().map(|p| format!("{},{}", p.x, p.y)).collect();
        pts.join(";")
    }

    fn decode(s: &str) -> Result<Anchors> {
        let pts: Vec<RootPoint> = s
            .split(';')
            .map(|p| {
                let (x, y) = p.split_once(',').ok_or(Error::CorruptEntity)?;
                let num = |v: &str| v.parse().map_err(|_| Error::CorruptEntity);
                Ok(RootPoint {
                    x: num(x)?,
                    y: num(y)?,
                })
            })
            .collect::<Result<_>>()?;
        match pts[..] {
            [mouth, picked] => Ok(Anchors::River { mouth, picked }),
            [inside] => Ok(Anchors::Lake { inside }),
            _ => Err(Error::CorruptEntity),
        }
    }
}

fn dist(a: RootPoint, b: RootPoint) -> f64 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn segment_dist(p: RootPoint, a: RootPoint, b: RootPoint) -> f64 {
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

/// Distance to the polyline, or to the closed ring when `ring`.
fn outline_dist(p: RootPoint, pts: &[RootPoint], ring: bool) -> f64 {
    let mut d = pts.first().map_or(f64::INFINITY, |a| dist(p, *a));
    for w in pts.windows(2) {
        d = d.min(segment_dist(p, w[0], w[1]));
    }
    if ring && pts.len() > 2 {
        d = d.min(segment_dist(p, pts[pts.len() - 1], pts[0]));
    }
    d
}

fn inside(p: RootPoint, ring: &[RootPoint]) -> bool {
    let mut c = false;
    for (i, a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            c = !c;
        }
    }
    c
}

/// Distance from the anchors to the feature; a river's score sums mouth and picked point.
fn score(anchors: &Anchors, f: &DerivedFeature, tolerance: f64) -> Option<f64> {
    let g = &f.geometry;
    let (d, ok) = match anchors {
        Anchors::River { mouth, picked } => {
            let dm = g.last().map_or(f64::INFINITY, |e| dist(*mouth, *e));
            let dp = outline_dist(*picked, g, false);
            (dm + dp, dm <= tolerance && dp <= tolerance)
        }
        Anchors::Lake { inside: p } => {
            let d = if inside(*p, g) {
                0.0
            } else {
                outline_dist(*p, g, true)
            };
            (d, d <= tolerance)
        }
    };
    ok.then_some(d)
}

fn anchors_of(f: &DerivedFeature) -> Result<Anchors> {
    let empty = Error::InvalidAnchor(AnchorProblem::NoGeometry);
    let g = &f.geometry;
    match f.kind {
        FeatureKind::River => Ok(Anchors::River {
            mouth: *g.last().ok_or(empty)?,
            picked: g[g.len() / 2],
        }),
        FeatureKind::Lake if g.is_empty() => Err(empty),
        FeatureKind::Lake => {
            let n = g.len() as f64;
            Ok(Anchors::Lake {
                inside: RootPoint {
                    x: g.iter().map(|p| p.x).sum::<f64>() / n,
                    y: g.iter().map(|p| p.y).sum::<f64>() / n,
                },
            })
        }
    }
}

impl Project {
    /// Replaces the derived features. A derived stage calls this before its rebuild commits;
    /// tests call it with a synthetic set. Matching runs after the next rebuild, or on `match_anchors`.
    pub fn set_derived_features(&mut self, features: Vec<DerivedFeature>) {
        self.anchors.features = Some(features);
    }

    /// Metres; default `DEFAULT_ANCHOR_TOLERANCE`.
    pub fn set_anchor_tolerance(&mut self, metres: f64) {
        self.anchors.tolerance = metres;
    }

    /// The anchors of `entity`, if it has any.
    pub fn anchors(&self, entity: EntityId) -> Result<Option<Anchors>> {
        let rec = self.store.anchor(entity.0)?;
        rec.map(|r| Anchors::decode(&r.points)).transpose()
    }

    /// The derived feature `entity` is attached to.
    pub fn attached_feature(&self, entity: EntityId) -> Result<Option<FeatureId>> {
        let rec = self.store.anchor(entity.0)?;
        Ok(rec.and_then(|r| r.feature_id).map(|f| FeatureId(f as u64)))
    }

    /// Entities whose anchors matched no derived feature, ordered by id.
    pub fn unplaced_entities(&self) -> Result<Vec<EntityId>> {
        let rows = self.store.anchors()?;
        Ok(rows
            .into_iter()
            .filter(|r| r.unplaced)
            .map(|r| EntityId(r.entity_id))
            .collect())
    }

    /// Attaches each anchored entity to the nearest derived feature within the tolerance, or
    /// flags it unplaced. Not an undoable edit: it follows the derived data. Does nothing until
    /// `set_derived_features` was called.
    pub fn match_anchors(&mut self) -> Result<()> {
        for mut rec in self.store.anchors()? {
            let new = self.matched(&rec)?;
            if (rec.feature_id, rec.unplaced) != new {
                (rec.feature_id, rec.unplaced) = new;
                self.store
                    .write(&[Write::Anchor(rec.entity_id, Some(rec))])?;
            }
        }
        Ok(())
    }

    /// (feature, unplaced) for `rec`; unchanged while no features are known.
    fn matched(&self, rec: &AnchorRecord) -> Result<(Option<i64>, bool)> {
        let Some(features) = &self.anchors.features else {
            return Ok((rec.feature_id, rec.unplaced));
        };
        let a = Anchors::decode(&rec.points)?;
        let best = features
            .iter()
            .filter(|f| f.kind == a.kind())
            .filter_map(|f| Some((score(&a, f, self.anchors.tolerance)?, f.id)))
            .min_by(|x, y| x.0.total_cmp(&y.0).then(x.1.cmp(&y.1)));
        Ok(match best {
            Some((_, id)) => (Some(id.0 as i64), false),
            None => (None, true),
        })
    }

    pub(crate) fn anchor_change(&self, edit: &AnchorEdit) -> Result<AnchorChange> {
        let (entity, anchors, feature) = match edit {
            AnchorEdit::Set { entity, anchors } => (*entity, anchors.clone(), None),
            AnchorEdit::Reattach { entity, feature } => {
                let f = self
                    .anchors
                    .features
                    .iter()
                    .flatten()
                    .find(|f| f.id == *feature)
                    .ok_or(Error::UnknownFeature(*feature))?;
                (*entity, anchors_of(f)?, Some(*feature))
            }
        };
        let kind = self
            .entity(entity)?
            .ok_or(Error::UnknownEntity(entity))?
            .kind;
        let want = match kind.as_str() {
            "river" => FeatureKind::River,
            "lake" => FeatureKind::Lake,
            _ => return Err(Error::InvalidAnchor(AnchorProblem::NotAnchored)),
        };
        if anchors.kind() != want {
            return Err(Error::InvalidAnchor(AnchorProblem::WrongShape));
        }
        let mut after = AnchorRecord {
            entity_id: entity.0,
            points: anchors.encode(),
            feature_id: feature.map(|f| f.0 as i64),
            unplaced: false,
        };
        if feature.is_none() {
            (after.feature_id, after.unplaced) = self.matched(&after)?;
        }
        Ok(AnchorChange {
            entity: entity.0,
            before: self.store.anchor(entity.0)?,
            after: Some(after),
        })
    }

    pub(crate) fn write_anchor_change(&self, c: &AnchorChange, forward: bool) -> Result<()> {
        let rec = if forward { &c.after } else { &c.before };
        self.store.write(&[Write::Anchor(c.entity, rec.clone())])
    }
}
