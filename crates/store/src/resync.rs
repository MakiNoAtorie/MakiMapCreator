//! Re-sync state of child maps (ADR-0022): the out-of-date flag, and the outline and parent
//! context that a re-sync replaces.

use mmc_core::persist::ChildState;
use rusqlite::{Connection, params};

use crate::{Result, Store};

pub(crate) const MIGRATION: &str =
    "ALTER TABLE child_maps ADD COLUMN out_of_date INTEGER NOT NULL DEFAULT 0;";

impl Store {
    /// `false` for a map that is not a child map.
    pub fn out_of_date(&self, map_id: i64) -> Result<bool> {
        use rusqlite::OptionalExtension;
        Ok(self
            .conn
            .query_row(
                "SELECT out_of_date FROM child_maps WHERE map_id = ?1",
                [map_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false))
    }

    /// The re-sync state of a child map; `None` for a map that is not a child map.
    pub fn child_state(&self, map_id: i64) -> Result<Option<ChildState>> {
        use rusqlite::OptionalExtension;
        let Some((outline, out_of_date)) = self
            .conn
            .query_row(
                "SELECT outline, out_of_date FROM child_maps WHERE map_id = ?1",
                [map_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
        else {
            return Ok(None);
        };
        Ok(Some(ChildState {
            outline,
            context: crate::children::context(&self.conn, map_id)?,
            out_of_date,
        }))
    }
}

pub(crate) fn write_out_of_date(tx: &Connection, map_id: i64, out_of_date: bool) -> Result<()> {
    tx.execute(
        "UPDATE child_maps SET out_of_date = ?2 WHERE map_id = ?1",
        params![map_id, out_of_date],
    )?;
    Ok(())
}

/// Replaces the outline, parent context and flag of a child map, inside the caller's transaction.
pub(crate) fn write_child_state(tx: &Connection, map_id: i64, s: &ChildState) -> Result<()> {
    tx.execute(
        "UPDATE child_maps SET outline = ?2, out_of_date = ?3 WHERE map_id = ?1",
        params![map_id, s.outline, s.out_of_date],
    )?;
    tx.execute("DELETE FROM parent_context WHERE map_id = ?1", [map_id])?;
    crate::children::insert_context(tx, map_id, &s.context)
}
