//! Embedded assets: blobs deduplicated by content hash, mapped from (pack id, asset id).

use rusqlite::OptionalExtension;

use crate::{Result, Store};

pub(crate) const MIGRATION: &str = "CREATE TABLE asset_blobs (
        hash BLOB PRIMARY KEY,
        data BLOB NOT NULL
    );
    CREATE TABLE assets (
        pack_id TEXT NOT NULL,
        asset_id TEXT NOT NULL,
        hash BLOB NOT NULL REFERENCES asset_blobs(hash),
        PRIMARY KEY (pack_id, asset_id)
    );";

impl Store {
    /// Stores `data` under (`pack_id`, `asset_id`); identical content is stored once.
    pub fn add_asset(&self, pack_id: &str, asset_id: &str, data: &[u8]) -> Result<()> {
        let hash = blake3::hash(data);
        self.conn.execute(
            "INSERT OR IGNORE INTO asset_blobs (hash, data) VALUES (?1, ?2)",
            rusqlite::params![hash.as_bytes().as_slice(), data],
        )?;
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (pack_id, asset_id, hash) VALUES (?1, ?2, ?3)",
            rusqlite::params![pack_id, asset_id, hash.as_bytes().as_slice()],
        )?;
        Ok(())
    }

    pub fn asset(&self, pack_id: &str, asset_id: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT b.data FROM assets a JOIN asset_blobs b ON b.hash = a.hash
                 WHERE a.pack_id = ?1 AND a.asset_id = ?2",
                [pack_id, asset_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn asset_blob_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM asset_blobs", [], |r| r.get(0))?)
    }
}
