//! Map deletion: a deleted map keeps its rows, flagged, while undo can bring it back;
//! opening a working copy purges them and the rows of deleted entities.

use rusqlite::params;

use crate::{ContextRecord, Result, Store};

pub(crate) const MIGRATION: &str =
    "ALTER TABLE maps ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;";

/// A child map's link to its parent: what it loses when it becomes a root map.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildLink {
    pub parent_id: i64,
    pub entity_id: i64,
    pub outline: Vec<u8>,
    pub context: Vec<ContextRecord>,
}

impl Store {
    /// The parent link of `map_id`; `None` for a root map.
    pub fn child_link(&self, map_id: i64) -> Result<Option<ChildLink>> {
        let Some(parent_id) =
            self.conn
                .query_row("SELECT parent_id FROM maps WHERE id = ?1", [map_id], |r| {
                    r.get::<_, Option<i64>>(0)
                })?
        else {
            return Ok(None);
        };
        let (entity_id, outline) = self.conn.query_row(
            "SELECT entity_id, outline FROM child_maps WHERE map_id = ?1",
            [map_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let mut stmt = self.conn.prepare(
            "SELECT layer, origin_x, origin_y, metres_per_pixel, x0, y0, width, height, data
             FROM parent_context WHERE map_id = ?1 ORDER BY layer",
        )?;
        let context = stmt
            .query_map([map_id], |r| {
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
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(ChildLink {
            parent_id,
            entity_id,
            outline,
            context,
        }))
    }

    /// In one transaction: flags maps deleted or not, and writes or drops child links
    /// (`None` makes the map a root map).
    pub fn write_map_deletion(
        &self,
        deleted: &[(i64, bool)],
        links: &[(i64, Option<ChildLink>)],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (id, d) in deleted {
            tx.execute("UPDATE maps SET deleted = ?2 WHERE id = ?1", params![id, d])?;
        }
        for (id, link) in links {
            tx.execute("DELETE FROM parent_context WHERE map_id = ?1", [id])?;
            tx.execute("DELETE FROM child_maps WHERE map_id = ?1", [id])?;
            let Some(l) = link else {
                tx.execute("UPDATE maps SET parent_id = NULL WHERE id = ?1", [id])?;
                continue;
            };
            tx.execute(
                "UPDATE maps SET parent_id = ?2 WHERE id = ?1",
                params![id, l.parent_id],
            )?;
            tx.execute(
                "INSERT INTO child_maps (map_id, entity_id, outline) VALUES (?1, ?2, ?3)",
                params![id, l.entity_id, l.outline],
            )?;
            for c in &l.context {
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
        }
        Ok(tx.commit()?)
    }
}

/// Removes deleted maps with their rows, and rows of deleted entities. Runs when a working
/// copy opens, because no undo record outlives the session.
// Add here every new table that has a map or entity key.
pub(crate) const PURGE: &str = "
    CREATE TEMP TABLE gone AS SELECT id FROM maps WHERE deleted;
    DELETE FROM tiles WHERE map_id IN gone;
    DELETE FROM outlines WHERE map_id IN gone OR entity_id NOT IN (SELECT id FROM entities);
    DELETE FROM generations WHERE map_id IN gone;
    DELETE FROM parent_context WHERE map_id IN gone;
    DELETE FROM child_maps WHERE map_id IN gone;
    DELETE FROM maps WHERE id IN gone;
    DROP TABLE gone;
    DELETE FROM pages WHERE entity_id NOT IN (SELECT id FROM entities);
    DELETE FROM fts_pages WHERE rowid NOT IN (SELECT id FROM entities);
    DELETE FROM page_links WHERE from_id NOT IN (SELECT id FROM entities);
    DELETE FROM anchors WHERE entity_id NOT IN (SELECT id FROM entities);
";
