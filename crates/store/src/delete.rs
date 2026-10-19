//! Map deletion: a deleted map keeps its rows, flagged, while undo can bring it back;
//! opening a working copy purges them and the rows of deleted entities.

use mmc_core::persist::ChildLink;
use rusqlite::{Connection, params};

use crate::{Result, Store};

pub(crate) const MIGRATION: &str =
    "ALTER TABLE maps ADD COLUMN deleted INTEGER NOT NULL DEFAULT 0;";

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
        Ok(Some(ChildLink {
            parent_id,
            entity_id,
            outline,
            context: crate::children::context(&self.conn, map_id)?,
        }))
    }
}

pub(crate) fn write_map_deleted(tx: &Connection, id: i64, deleted: bool) -> Result<()> {
    tx.execute(
        "UPDATE maps SET deleted = ?2 WHERE id = ?1",
        params![id, deleted],
    )?;
    Ok(())
}

/// Writes or drops (`None`, the map becomes a root map) a map's parent link.
pub(crate) fn write_child_link(tx: &Connection, id: i64, link: Option<&ChildLink>) -> Result<()> {
    tx.execute("DELETE FROM parent_context WHERE map_id = ?1", [id])?;
    tx.execute("DELETE FROM child_maps WHERE map_id = ?1", [id])?;
    let Some(l) = link else {
        tx.execute("UPDATE maps SET parent_id = NULL WHERE id = ?1", [id])?;
        return Ok(());
    };
    tx.execute(
        "UPDATE maps SET parent_id = ?2 WHERE id = ?1",
        params![id, l.parent_id],
    )?;
    tx.execute(
        "INSERT INTO child_maps (map_id, entity_id, outline) VALUES (?1, ?2, ?3)",
        params![id, l.entity_id, l.outline],
    )?;
    crate::children::insert_context(tx, id, &l.context)
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
