//! Anchors of named natural features and their match state (ADR-0023).

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store};

// No foreign key to entities: undo of a delete restores the entity, and the row must survive.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE anchors (
        entity_id INTEGER PRIMARY KEY,
        points TEXT NOT NULL,
        feature_id INTEGER,
        unplaced INTEGER NOT NULL
    );
";

/// `points` is `x,y;x,y` in root-space metres; core owns the meaning.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorRecord {
    pub entity_id: i64,
    pub points: String,
    pub feature_id: Option<i64>,
    pub unplaced: bool,
}

fn record(r: &Row) -> rusqlite::Result<AnchorRecord> {
    Ok(AnchorRecord {
        entity_id: r.get(0)?,
        points: r.get(1)?,
        feature_id: r.get(2)?,
        unplaced: r.get(3)?,
    })
}

const COLS: &str = "entity_id, points, feature_id, unplaced";

impl Store {
    pub fn anchor(&self, entity_id: i64) -> Result<Option<AnchorRecord>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {COLS} FROM anchors WHERE entity_id = ?1"),
                [entity_id],
                record,
            )
            .optional()?)
    }

    /// All anchor rows of existing entities, ordered by entity id.
    pub fn anchors(&self) -> Result<Vec<AnchorRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.entity_id, a.points, a.feature_id, a.unplaced FROM anchors a
             JOIN entities e ON e.id = a.entity_id ORDER BY a.entity_id",
        )?;
        let rows = stmt.query_map([], record)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Writes the row, or deletes it when `rec` is `None`.
    pub fn write_anchor(&self, entity_id: i64, rec: Option<&AnchorRecord>) -> Result<()> {
        match rec {
            Some(a) => self.conn.execute(
                "INSERT INTO anchors (entity_id, points, feature_id, unplaced)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (entity_id) DO UPDATE SET points = excluded.points,
                 feature_id = excluded.feature_id, unplaced = excluded.unplaced",
                params![a.entity_id, a.points, a.feature_id, a.unplaced],
            )?,
            None => self
                .conn
                .execute("DELETE FROM anchors WHERE entity_id = ?1", [entity_id])?,
        };
        Ok(())
    }
}
