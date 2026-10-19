//! Child maps: the outline a child owns (ADR-0019) and its parent context snapshot (ADR-0002).

use rusqlite::params;

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

/// A child map to insert; `outline` and context `data` are opaque to the store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewChildMap {
    pub parent_id: i64,
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    pub entity_id: i64,
    pub outline: Vec<u8>,
    pub context: Vec<ContextRecord>,
}

/// A rectangle of one parent layer: parent pixels `x0..x0+width`, `y0..y0+height`.
/// `origin_x`, `origin_y` and `metres_per_pixel` are the parent's transform.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextRecord {
    pub layer: String,
    pub origin_x: f64,
    pub origin_y: f64,
    pub metres_per_pixel: f64,
    pub x0: u32,
    pub y0: u32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildMapRecord {
    pub map_id: i64,
    pub entity_id: i64,
    pub outline: Vec<u8>,
}

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
        for c in &m.context {
            tx.execute(
                "INSERT INTO parent_context
                 (map_id, layer, origin_x, origin_y, metres_per_pixel, x0, y0, width, height, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    id,
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
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT layer, origin_x, origin_y, metres_per_pixel, x0, y0, width, height, data
                 FROM parent_context WHERE map_id = ?1 AND layer = ?2",
                params![map_id, layer],
                |r| {
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
                },
            )
            .optional()?)
    }
}
