//! Raster tiles: one opaque blob per (map, layer, level, tile) plus the edited flag.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, Store};

pub(crate) fn tile(conn: &rusqlite::Connection, key: &TileKey) -> Result<Option<TileRecord>> {
    Ok(conn
        .query_row(
            "SELECT data, edited FROM tiles
             WHERE map_id = ?1 AND layer = ?2 AND level = ?3 AND tx = ?4 AND ty = ?5",
            params![key.map_id, key.layer, key.level, key.tx, key.ty],
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub map_id: i64,
    pub layer: String,
    pub level: u32,
    pub tx: u32,
    pub ty: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileRecord {
    pub data: Vec<u8>,
    pub edited: bool,
}

impl Store {
    pub fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        tile(&self.conn, key)
    }

    /// Writes all tiles in one transaction; `None` deletes the tile.
    pub fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        write_tiles(&self.conn, writes)
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

/// Writes all tiles in one transaction; `None` deletes the tile.
fn write_tiles(conn: &Connection, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for (k, rec) in writes {
        match rec {
            Some(r) => tx.execute(
                "INSERT OR REPLACE INTO tiles (map_id, layer, level, tx, ty, data, edited)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![k.map_id, k.layer, k.level, k.tx, k.ty, r.data, r.edited],
            )?,
            None => tx.execute(
                "DELETE FROM tiles
                 WHERE map_id = ?1 AND layer = ?2 AND level = ?3 AND tx = ?4 AND ty = ?5",
                params![k.map_id, k.layer, k.level, k.tx, k.ty],
            )?,
        };
    }
    Ok(tx.commit()?)
}

/// Level-0 tiles of one layer as ((tx, ty), record), ordered.
pub type LayerTiles = Vec<((u32, u32), TileRecord)>;

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
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        tile(&self.conn, key)
    }

    /// As `Store::write_tiles`, on this connection.
    pub fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        write_tiles(&self.conn, writes)
    }
}
