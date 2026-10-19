//! Generation records (ADR-0021) and the outlines generators give entities on a map.

use rusqlite::params;

use crate::entities::write_entity_rows;
use crate::{EntityRecord, Result, Store};

// Outlines have no key on entities, so a generation and its entities are written in any order.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE generations (
        id INTEGER PRIMARY KEY,
        map_id INTEGER NOT NULL REFERENCES maps(id),
        generator TEXT NOT NULL,
        seed INTEGER NOT NULL,
        app_version TEXT NOT NULL
    );
    CREATE TABLE outlines (
        map_id INTEGER NOT NULL REFERENCES maps(id),
        entity_id INTEGER NOT NULL,
        points BLOB NOT NULL,
        PRIMARY KEY (map_id, entity_id)
    ) WITHOUT ROWID;
";

/// One run of a generator on a map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRecord {
    pub id: i64,
    pub map_id: i64,
    pub generator: String,
    /// The u64 seed, bit for bit.
    pub seed: i64,
    pub app_version: String,
}

/// The outline polygon of an entity on a map; `points` is opaque to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineRecord {
    pub map_id: i64,
    pub entity_id: i64,
    pub points: Vec<u8>,
}

impl Store {
    pub fn next_generation_id(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(id), 0) + 1 FROM generations",
            [],
            |r| r.get(0),
        )?)
    }

    /// Writes (`present`) or removes one generation with its new entities and outlines,
    /// in one transaction.
    pub fn write_generation(
        &self,
        generation: &GenerationRecord,
        entities: &[EntityRecord],
        outlines: &[OutlineRecord],
        present: bool,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let rows: Vec<_> = entities
            .iter()
            .map(|e| (e.id, present.then(|| e.clone())))
            .collect();
        write_entity_rows(&tx, &rows, &[])?;
        for o in outlines {
            if present {
                tx.execute(
                    "INSERT OR REPLACE INTO outlines (map_id, entity_id, points)
                     VALUES (?1, ?2, ?3)",
                    params![o.map_id, o.entity_id, o.points],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM outlines WHERE map_id = ?1 AND entity_id = ?2",
                    params![o.map_id, o.entity_id],
                )?;
            }
        }
        let g = generation;
        if present {
            tx.execute(
                "INSERT OR REPLACE INTO generations (id, map_id, generator, seed, app_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![g.id, g.map_id, g.generator, g.seed, g.app_version],
            )?;
        } else {
            tx.execute("DELETE FROM generations WHERE id = ?1", [g.id])?;
        }
        Ok(tx.commit()?)
    }

    /// Generations of a map, oldest first.
    pub fn generations(&self, map_id: i64) -> Result<Vec<GenerationRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, map_id, generator, seed, app_version FROM generations
             WHERE map_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([map_id], |r| {
            Ok(GenerationRecord {
                id: r.get(0)?,
                map_id: r.get(1)?,
                generator: r.get(2)?,
                seed: r.get(3)?,
                app_version: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Outlines of existing entities on a map, ordered by entity id.
    pub fn outlines(&self, map_id: i64) -> Result<Vec<OutlineRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT o.map_id, o.entity_id, o.points FROM outlines o
             JOIN entities e ON e.id = o.entity_id WHERE o.map_id = ?1 ORDER BY o.entity_id",
        )?;
        let rows = stmt.query_map([map_id], |r| {
            Ok(OutlineRecord {
                map_id: r.get(0)?,
                entity_id: r.get(1)?,
                points: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}
