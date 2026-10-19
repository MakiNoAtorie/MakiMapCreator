//! Generation records (ADR-0021) and the outlines generators give entities on a map.

use mmc_core::persist::GenerationRecord;
use rusqlite::{Connection, params};

use crate::{Result, Store};

// Outlines have no key on entities, so a generation and its entities are written in any order.
/// Generator parameter values of each run, a JSON object (ADR-0021).
pub(crate) const PARAMS_MIGRATION: &str =
    "ALTER TABLE generations ADD COLUMN params TEXT NOT NULL DEFAULT '{}';";

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

impl Store {
    pub fn next_generation_id(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(MAX(id), 0) + 1 FROM generations",
            [],
            |r| r.get(0),
        )?)
    }

    /// Generations of a map, oldest first.
    pub fn generations(&self, map_id: i64) -> Result<Vec<GenerationRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, map_id, generator, seed, params, app_version FROM generations
             WHERE map_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([map_id], |r| {
            Ok(GenerationRecord {
                id: r.get(0)?,
                map_id: r.get(1)?,
                generator: r.get(2)?,
                seed: r.get(3)?,
                params: r.get(4)?,
                app_version: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

pub(crate) fn write_generation(tx: &Connection, g: &GenerationRecord, present: bool) -> Result<()> {
    if present {
        tx.execute(
            "INSERT OR REPLACE INTO generations (id, map_id, generator, seed, params, app_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![g.id, g.map_id, g.generator, g.seed, g.params, g.app_version],
        )?;
    } else {
        tx.execute("DELETE FROM generations WHERE id = ?1", [g.id])?;
    }
    Ok(())
}
