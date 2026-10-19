//! Re-sync state of child maps (ADR-0022): the out-of-date flag, and the outline and parent
//! context that a re-sync replaces.

use rusqlite::params;

use crate::{ContextRecord, Result, Store};

pub(crate) const MIGRATION: &str =
    "ALTER TABLE child_maps ADD COLUMN out_of_date INTEGER NOT NULL DEFAULT 0;";

/// What a re-sync replaces on a child map; `outline` and context `data` are opaque.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildState {
    pub outline: Vec<u8>,
    pub context: Vec<ContextRecord>,
    pub out_of_date: bool,
}

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

    pub fn set_out_of_date(&self, map_id: i64, out_of_date: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE child_maps SET out_of_date = ?2 WHERE map_id = ?1",
            params![map_id, out_of_date],
        )?;
        Ok(())
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
        Ok(Some(ChildState {
            outline,
            context,
            out_of_date,
        }))
    }

    /// Replaces the outline, parent context and flag of a child map in one transaction.
    pub fn write_child_state(&self, map_id: i64, s: &ChildState) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE child_maps SET outline = ?2, out_of_date = ?3 WHERE map_id = ?1",
            params![map_id, s.outline, s.out_of_date],
        )?;
        tx.execute("DELETE FROM parent_context WHERE map_id = ?1", [map_id])?;
        for c in &s.context {
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
        Ok(tx.commit()?)
    }
}
