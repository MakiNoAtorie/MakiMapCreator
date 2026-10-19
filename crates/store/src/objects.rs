//! Map objects: points, paths and polygons on object layers. Entity outlines move here.

use mmc_core::persist::ObjectRecord;
use rusqlite::{Connection, params};

use crate::{Result, Store};

// AUTOINCREMENT: ids are never reused, so redo stays safe. No key on entities, as for anchors.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE map_objects (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        map_id INTEGER NOT NULL REFERENCES maps(id),
        layer TEXT NOT NULL,
        shape TEXT NOT NULL,
        points BLOB NOT NULL,
        entity_id INTEGER,
        label_dx REAL,
        label_dy REAL
    );
    CREATE INDEX map_objects_map ON map_objects (map_id);
    INSERT INTO map_objects (map_id, layer, shape, points, entity_id)
        SELECT map_id, 'outlines', 'polygon', points, entity_id FROM outlines
        ORDER BY map_id, entity_id;
    DROP TABLE outlines;
";

impl Store {
    /// Map objects of a map, ordered by id.
    pub fn objects(&self, map_id: i64) -> Result<Vec<ObjectRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, map_id, layer, shape, points, entity_id, label_dx, label_dy
             FROM map_objects WHERE map_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([map_id], |r| {
            Ok(ObjectRecord {
                id: r.get(0)?,
                map_id: r.get(1)?,
                layer: r.get(2)?,
                shape: r.get(3)?,
                points: r.get(4)?,
                entity_id: r.get(5)?,
                label_dx: r.get(6)?,
                label_dy: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn next_object_id(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'map_objects'), 0) + 1",
            [],
            |r| r.get(0),
        )?)
    }
}

/// Writes or deletes (`None`) one object, inside the caller's transaction.
pub(crate) fn write_object(tx: &Connection, id: i64, rec: Option<&ObjectRecord>) -> Result<()> {
    match rec {
        Some(o) => tx.execute(
            "INSERT OR REPLACE INTO map_objects
             (id, map_id, layer, shape, points, entity_id, label_dx, label_dy)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                o.id,
                o.map_id,
                o.layer,
                o.shape,
                o.points,
                o.entity_id,
                o.label_dx,
                o.label_dy
            ],
        )?,
        None => tx.execute("DELETE FROM map_objects WHERE id = ?1", [id])?,
    };
    Ok(())
}
