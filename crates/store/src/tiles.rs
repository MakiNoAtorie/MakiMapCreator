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

/// Moves level-0 tiles of a map by (`dx`, `dy`) tiles and drops its upper levels, inside the
/// caller's transaction. Keys go through negative values so no update hits another row.
pub(crate) fn shift_tiles(tx: &Connection, map_id: i64, dx: i64, dy: i64) -> Result<()> {
    tx.execute(
        "DELETE FROM tiles WHERE map_id = ?1 AND level > 0",
        [map_id],
    )?;
    tx.execute(
        "UPDATE tiles SET tx = -(tx + ?2) - 1, ty = -(ty + ?3) - 1 WHERE map_id = ?1",
        params![map_id, dx, dy],
    )?;
    tx.execute(
        "UPDATE tiles SET tx = -tx - 1, ty = -ty - 1 WHERE map_id = ?1",
        [map_id],
    )?;
    Ok(())
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

/// Bytes of tile data per background write transaction: an edit waits at most this long.
const CHUNK_BYTES: usize = 1 << 20;

/// A second connection to the working copy for worker threads. WAL lets it read while the
/// main connection writes. Its writes go in small transactions that give way to edits.
pub struct TileConn {
    conn: Connection,
    gate: crate::Gate,
}

impl TileConn {
    pub fn open(working_copy: &Path) -> Result<TileConn> {
        TileConn::open_gated(working_copy, Default::default())
    }

    pub(crate) fn open_gated(working_copy: &Path, gate: crate::Gate) -> Result<TileConn> {
        let conn = Connection::open(working_copy)?;
        crate::tune(&conn)?;
        Ok(TileConn { conn, gate })
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

    /// Writes tiles in transactions of about `CHUNK_BYTES`; `None` deletes the tile. Before
    /// each one it waits while the store has an edit pending, so edits never queue behind it.
    pub fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        let mut rest = writes;
        while !rest.is_empty() {
            while self.gate.load(std::sync::atomic::Ordering::SeqCst) > 0 {
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
            let (mut n, mut bytes) = (0, 0);
            while n < rest.len() && (n == 0 || bytes < CHUNK_BYTES) {
                bytes += rest[n].1.as_ref().map_or(64, |r| r.data.len());
                n += 1;
            }
            let tx = rusqlite::Transaction::new_unchecked(
                &self.conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            for (k, rec) in &rest[..n] {
                write_tile(&tx, k, rec.as_ref())?;
            }
            tx.commit()?;
            rest = &rest[n..];
        }
        Ok(())
    }
}
