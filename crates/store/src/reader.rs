//! Extra read-only connections: tile loading workers, and reading a project file without opening it.

use std::path::Path;

use mmc_core::persist::{MapRecord, TileKey, TileRecord};
use rusqlite::Connection;

use crate::Result;

/// A read-only connection to a project file or a working copy. Sees committed data only.
pub struct FileReader {
    conn: Connection,
}

impl FileReader {
    pub fn open(file: &Path) -> Result<FileReader> {
        Ok(FileReader {
            conn: crate::open_read_only(file)?,
        })
    }

    /// All maps, ordered by id.
    pub fn maps(&self) -> Result<Vec<MapRecord>> {
        crate::maps(&self.conn)
    }

    pub fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        crate::tiles::tile(&self.conn, key)
    }
}
