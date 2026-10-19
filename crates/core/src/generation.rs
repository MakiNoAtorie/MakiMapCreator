//! One-shot generation (ADR-0003): a generator's output is one command; the file records
//! the generator, seed and app version of each run (ADR-0021).

use crate::persist::{EntityRecord, GenerationRecord, OutlineRecord, TileRecord, Write};

use crate::layer::{TILE_LEN, TILE_SIZE, encode};
use crate::undo::TileChange;
use crate::{EntityId, Error, GenRun, Layer, Map, MapId, Project, Result, RootPoint};

/// The version of the app that runs generators.
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The output of one generator run.
#[derive(Debug, Clone, PartialEq)]
pub struct Generation {
    pub run: GenRun,
    /// Created in order, with consecutive new ids.
    pub entities: Vec<NewEntity>,
    /// Whole-map values per layer, row by row (width × height). Written as unedited tiles.
    // ponytail: holds a full raster in memory (256 MB at 8192²); stream tiles if that hurts.
    pub rasters: Vec<(Layer, Vec<f32>)>,
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
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationInfo {
    pub run: GenRun,
    pub app_version: String,
}

/// Parameter values as a JSON object, for the generation record.
fn encode_params(params: &[(String, f64)]) -> String {
    let m: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
        .collect();
    serde_json::Value::Object(m).to_string()
}

fn decode_params(s: &str) -> Result<Vec<(String, f64)>> {
    let Ok(serde_json::Value::Object(m)) = serde_json::from_str(s) else {
        return Err(Error::CorruptEntity);
    };
    m.into_iter()
        .map(|(k, v)| Ok((k, v.as_f64().ok_or(Error::CorruptEntity)?)))
        .collect()
}

/// The rows one generation adds.
#[derive(Debug, Clone)]
pub(crate) struct GenerationChange {
    pub(crate) record: GenerationRecord,
    entities: Vec<EntityRecord>,
    outlines: Vec<OutlineRecord>,
    pub(crate) tiles: Vec<TileChange>,
}

pub(crate) fn encode_outline(points: &[RootPoint]) -> Vec<u8> {
    points
        .iter()
        .flat_map(|p| [p.x.to_le_bytes(), p.y.to_le_bytes()])
        .flatten()
        .collect()
}

pub(crate) fn decode_outline(bytes: &[u8]) -> Result<Vec<RootPoint>> {
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

impl GenerationChange {
    /// Tiles, entities, outlines and the record, for one transaction (#14, #15).
    pub(crate) fn rows(&self, forward: bool) -> Vec<Write> {
        let mut rows: Vec<Write> = self.tiles.iter().map(|t| t.write(forward)).collect();
        for e in &self.entities {
            rows.push(Write::Entity(e.id, forward.then(|| e.clone())));
        }
        for o in &self.outlines {
            rows.push(Write::Outline(o.clone(), forward));
        }
        rows.push(Write::Generation(self.record.clone(), forward));
        rows
    }
}

impl Project {
    pub(crate) fn generation_change(&self, map: &Map, g: &Generation) -> Result<GenerationChange> {
        let kinds = self.entity_kinds()?;
        let first = self.next_entity_id()?.0;
        let mut c = GenerationChange {
            record: GenerationRecord {
                id: self.store.next_generation_id()?,
                map_id: map.id.0,
                generator: g.run.generator.clone(),
                seed: g.run.seed as i64,
                params: encode_params(&g.run.params),
                app_version: APP_VERSION.into(),
            },
            entities: vec![],
            outlines: vec![],
            tiles: vec![],
        };
        let s = map.settings;
        for (layer, values) in &g.rasters {
            assert_eq!(values.len(), (s.width * s.height) as usize, "raster size");
            for ty in 0..s.height.div_ceil(TILE_SIZE) {
                for tx in 0..s.width.div_ceil(TILE_SIZE) {
                    let mut tile = vec![0.0; TILE_LEN];
                    for y in 0..TILE_SIZE.min(s.height - ty * TILE_SIZE) {
                        let row = ((ty * TILE_SIZE + y) * s.width + tx * TILE_SIZE) as usize;
                        let n = TILE_SIZE.min(s.width - tx * TILE_SIZE) as usize;
                        let at = (y * TILE_SIZE) as usize;
                        tile[at..at + n].copy_from_slice(&values[row..row + n]);
                    }
                    let key = map.tile_key(*layer, tx, ty);
                    c.tiles.push(TileChange {
                        before: self.store.tile(&key)?,
                        key,
                        after: Some(TileRecord {
                            data: encode(&tile),
                            edited: false,
                        }),
                    });
                }
            }
        }
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
        &mut self,
        c: &GenerationChange,
        forward: bool,
    ) -> Result<()> {
        self.write_rows(&c.rows(forward))?;
        self.note_generation(c, forward);
        Ok(())
    }

    /// Keeps the name index equal to the entities `c` wrote.
    pub(crate) fn note_generation(&mut self, c: &GenerationChange, forward: bool) {
        for e in &c.entities {
            if forward {
                self.names.set(EntityId(e.id), &e.name);
            } else {
                self.names.remove(EntityId(e.id));
            }
        }
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
        self.store
            .generations(map.0)?
            .into_iter()
            .map(|r| {
                Ok(GenerationInfo {
                    run: GenRun {
                        generator: r.generator,
                        seed: r.seed as u64,
                        params: decode_params(&r.params)?,
                    },
                    app_version: r.app_version,
                })
            })
            .collect()
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
