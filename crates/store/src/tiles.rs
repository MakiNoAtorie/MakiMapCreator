//! Raster tiles: one opaque blob per (map, layer, level, tile) plus the edited flag.

use std::path::Path;

use mmc_core::persist::{LayerTiles, TileKey, TileRecord};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, Store};

pub(crate) fn tile(conn: &rusqlite::Connection, key: &TileKey) -> Result<Option<TileRecord>> {
    Ok(conn
        .query_row(
            "SELECT data, edited FROM tiles
             WHERE map_id = ?1 AND layer = ?2 AND level = ?3 AND tx = ?4 AND ty = ?5",
            params![key.map_id, key.layer.as_str(), key.level, key.tx, key.ty],
            |r| {
                Ok(TileRecord {
                    data: r.get(0)?,
                    edited: r.get(1)?,
                })
            },
        )
        .optional()?)
}

pub(crate) const MIGRATION: &str = "CREATE TABLE tiles (
        map_id INTEGER NOT NULL REFERENCES maps(id),
        layer TEXT NOT NULL,
        level INTEGER NOT NULL,
        tx INTEGER NOT NULL,
        ty INTEGER NOT NULL,
        data BLOB NOT NULL,
        edited INTEGER NOT NULL,
        PRIMARY KEY (map_id, layer, level, tx, ty)
    ) WITHOUT ROWID;";

impl Store {
    pub fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        tile(&self.conn, key)
    }

    /// Level-0 tiles of a layer that the user edited by hand, ordered by (tx, ty).
    pub fn edited_tiles(&self, map_id: i64, layer: &str) -> Result<Vec<(u32, u32)>> {
        let mut stmt = self.conn.prepare(
            "SELECT tx, ty FROM tiles
             WHERE map_id = ?1 AND layer = ?2 AND level = 0 AND edited
             ORDER BY tx, ty",
        )?;
        let rows = stmt.query_map(params![map_id, layer], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Writes one tile inside the caller's transaction; `None` deletes it.
pub(crate) fn write_tile(tx: &Connection, k: &TileKey, rec: Option<&TileRecord>) -> Result<()> {
    let layer = k.layer.as_str();
    match rec {
        Some(r) => tx.execute(
            "INSERT OR REPLACE INTO tiles (map_id, layer, level, tx, ty, data, edited)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![k.map_id, layer, k.level, k.tx, k.ty, r.data, r.edited],
        )?,
        None => tx.execute(
            "DELETE FROM tiles
             WHERE map_id = ?1 AND layer = ?2 AND level = ?3 AND tx = ?4 AND ty = ?5",
            params![k.map_id, layer, k.level, k.tx, k.ty],
        )?,
    };
    Ok(())
}

/// A second connection to the working copy for worker threads. WAL lets it read while the
/// main connection writes; writes wait for each other up to the busy timeout.
pub struct TileConn {
    conn: Connection,
}

impl TileConn {
    pub fn open(working_copy: &Path) -> Result<TileConn> {
        Ok(TileConn {
            conn: Connection::open(working_copy)?,
        })
    }

    /// Level-0 tiles of a layer, read in one snapshot.
    pub fn layer_tiles(&self, map_id: i64, layer: &str) -> Result<LayerTiles> {
        Ok(self
            .layer_tiles_until(map_id, layer, &|| false)?
            .unwrap_or_default())
    }

    /// As `layer_tiles`, but gives `None` as soon as `stop` returns true, so a superseded
    /// rebuild frees the CPU and disk at once.
    pub fn layer_tiles_until(
        &self,
        map_id: i64,
        layer: &str,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<LayerTiles>> {
        let mut stmt = self.conn.prepare(
            "SELECT tx, ty, data, edited FROM tiles
             WHERE map_id = ?1 AND layer = ?2 AND level = 0
             ORDER BY tx, ty",
        )?;
        let rows = stmt.query_map(params![map_id, layer], |r| {
            Ok((
                (r.get(0)?, r.get(1)?),
                TileRecord {
                    data: r.get(2)?,
                    edited: r.get(3)?,
                },
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            if stop() {
                return Ok(None);
            }
            out.push(row?);
        }
        Ok(Some(out))
    }

    pub fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        tile(&self.conn, key)
    }

    /// Writes all tiles in one transaction; `None` deletes the tile.
    pub fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        for (k, rec) in writes {
            write_tile(&tx, k, rec.as_ref())?;
        }
        Ok(tx.commit()?)
    }
}
