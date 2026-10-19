//! One-shot generation (ADR-0003): a generator's output is one command; the file records
//! the generator, seed and app version of each run (ADR-0021).

use mmc_store::{EntityRecord, GenerationRecord, OutlineRecord};

use crate::{EntityId, Error, Map, MapId, Project, Result, RootPoint};

/// The version of the app that runs generators.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The output of one generator run.
#[derive(Debug, Clone, PartialEq)]
pub struct Generation {
    pub generator: String,
    pub seed: u64,
    /// Created in order, with consecutive new ids.
    pub entities: Vec<NewEntity>,
}

/// An entity a generator creates, with its outline on the generated map.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEntity {
    pub kind: String,
    pub name: String,
    /// Polygon in root space.
    pub outline: Vec<RootPoint>,
}

/// A recorded generator run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationInfo {
    pub generator: String,
    pub seed: u64,
    pub app_version: String,
}

/// The rows one generation adds.
#[derive(Debug, Clone)]
pub(crate) struct GenerationChange {
    record: GenerationRecord,
    entities: Vec<EntityRecord>,
    outlines: Vec<OutlineRecord>,
}

fn encode_outline(points: &[RootPoint]) -> Vec<u8> {
    points
        .iter()
        .flat_map(|p| [p.x.to_le_bytes(), p.y.to_le_bytes()])
        .flatten()
        .collect()
}

fn decode_outline(bytes: &[u8]) -> Result<Vec<RootPoint>> {
    let (pairs, rest) = bytes.as_chunks::<16>();
    if !rest.is_empty() {
        return Err(Error::CorruptEntity);
    }
    Ok(pairs
        .iter()
        .map(|b| {
            let (x, y) = b.split_at(8);
            RootPoint {
                x: f64::from_le_bytes(x.try_into().unwrap()),
                y: f64::from_le_bytes(y.try_into().unwrap()),
            }
        })
        .collect())
}

impl Project {
    pub(crate) fn generation_change(&self, map: &Map, g: &Generation) -> Result<GenerationChange> {
        let kinds = self.entity_kinds()?;
        let first = self.next_entity_id()?.0;
        let mut c = GenerationChange {
            record: GenerationRecord {
                id: self.store.next_generation_id()?,
                map_id: map.id.0,
                generator: g.generator.clone(),
                seed: g.seed as i64,
                app_version: APP_VERSION.into(),
            },
            entities: vec![],
            outlines: vec![],
        };
        for (id, e) in (first..).zip(&g.entities) {
            if !kinds.iter().any(|k| k.id == e.kind) {
                return Err(Error::UnknownEntityKind(e.kind.clone()));
            }
            c.entities.push(EntityRecord {
                id,
                kind: e.kind.clone(),
                name: e.name.clone(),
                fields: "{}".into(),
            });
            c.outlines.push(OutlineRecord {
                map_id: map.id.0,
                entity_id: id,
                points: encode_outline(&e.outline),
            });
        }
        Ok(c)
    }

    pub(crate) fn write_generation_change(
        &self,
        c: &GenerationChange,
        forward: bool,
    ) -> Result<()> {
        Ok(self
            .store
            .write_generation(&c.record, &c.entities, &c.outlines, forward)?)
    }

    /// Outlines of entities on `map`, ordered by entity id.
    pub fn outlines(&self, map: MapId) -> Result<Vec<(EntityId, Vec<RootPoint>)>> {
        self.map(map)?;
        self.store
            .outlines(map.0)?
            .into_iter()
            .map(|o| Ok((EntityId(o.entity_id), decode_outline(&o.points)?)))
            .collect()
    }

    /// Generator runs on `map`, oldest first.
    pub fn generations(&self, map: MapId) -> Result<Vec<GenerationInfo>> {
        self.map(map)?;
        Ok(self
            .store
            .generations(map.0)?
            .into_iter()
            .map(|r| GenerationInfo {
                generator: r.generator,
                seed: r.seed as u64,
                app_version: r.app_version,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn outline_bytes_round_trip() {
        let pts = vec![
            crate::RootPoint { x: -1.5, y: 2e9 },
            crate::RootPoint { x: 0.25, y: 3.0 },
        ];
        assert_eq!(
            super::decode_outline(&super::encode_outline(&pts)).unwrap(),
            pts
        );
        assert!(super::decode_outline(&[0; 15]).is_err());
    }
}
