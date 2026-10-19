//! Child maps: the outline a child owns (ADR-0019) and its parent context snapshot (ADR-0002).

use mmc_core::persist::{ChildMapRecord, ContextRecord, NewChildMap};
use rusqlite::{Connection, params};

use crate::{Result, Store};

// Root maps keep origin 0 (ADR-0020); the outline is a copy, so a later edit on the parent leaves it.
pub(crate) const MIGRATION: &str = "
    ALTER TABLE maps ADD COLUMN origin_x REAL NOT NULL DEFAULT 0;
    ALTER TABLE maps ADD COLUMN origin_y REAL NOT NULL DEFAULT 0;
    CREATE TABLE child_maps (
        map_id INTEGER PRIMARY KEY REFERENCES maps(id),
        entity_id INTEGER NOT NULL,
        outline BLOB NOT NULL
    );
    CREATE TABLE parent_context (
        map_id INTEGER NOT NULL REFERENCES maps(id),
        layer TEXT NOT NULL,
        origin_x REAL NOT NULL,
        origin_y REAL NOT NULL,
        metres_per_pixel REAL NOT NULL,
        x0 INTEGER NOT NULL,
        y0 INTEGER NOT NULL,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        data BLOB NOT NULL,
        PRIMARY KEY (map_id, layer)
    ) WITHOUT ROWID;
";

impl Store {
    /// Inserts a child map with its outline and parent context in one transaction.
    pub fn insert_child_map(&self, m: &NewChildMap) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO maps (parent_id, kind, width, height, metres_per_pixel, origin_x, origin_y)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                m.parent_id,
                m.kind,
                m.width,
                m.height,
                m.metres_per_pixel,
                m.origin_x,
                m.origin_y
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO child_maps (map_id, entity_id, outline) VALUES (?1, ?2, ?3)",
            params![id, m.entity_id, m.outline],
        )?;
        insert_context(&tx, id, &m.context)?;
        tx.commit()?;
        Ok(id)
    }

    /// All child map rows, ordered by map id.
    pub fn child_maps(&self) -> Result<Vec<ChildMapRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT map_id, entity_id, outline FROM child_maps ORDER BY map_id")?;
        let rows = stmt.query_map([], |r| {
            Ok(ChildMapRecord {
                map_id: r.get(0)?,
                entity_id: r.get(1)?,
                outline: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn parent_context(&self, map_id: i64, layer: &str) -> Result<Option<ContextRecord>> {
        Ok(context(&self.conn, map_id)?
            .into_iter()
            .find(|c| c.layer == layer))
    }
}

/// Parent context rows of a map, ordered by layer.
pub(crate) fn context(conn: &Connection, map_id: i64) -> Result<Vec<ContextRecord>> {
    let mut stmt = conn.prepare(
        "SELECT layer, origin_x, origin_y, metres_per_pixel, x0, y0, width, height, data
         FROM parent_context WHERE map_id = ?1 ORDER BY layer",
    )?;
    let rows = stmt.query_map([map_id], |r| {
        Ok(ContextRecord {
            layer: r.get(0)?,
            origin_x: r.get(1)?,
            origin_y: r.get(2)?,
            metres_per_pixel: r.get(3)?,
            x0: r.get(4)?,
            y0: r.get(5)?,
            width: r.get(6)?,
            height: r.get(7)?,
            data: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Inserts parent context rows of a map, inside the caller's transaction.
pub(crate) fn insert_context(tx: &Connection, map_id: i64, rows: &[ContextRecord]) -> Result<()> {
    for c in rows {
        tx.execute(
            "INSERT INTO parent_context
             (map_id, layer, origin_x, origin_y, metres_per_pixel, x0, y0, width, height, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                map_id,
                c.layer,
                c.origin_x,
                c.origin_y,
                c.metres_per_pixel,
                c.x0,
                c.y0,
                c.width,
                c.height,
                c.data
            ],
        )?;
    }
    Ok(())
}
