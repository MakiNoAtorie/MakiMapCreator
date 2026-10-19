//! Background save and crash restore of the working copy (ADR-0011, ADR-0014).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::{Result, Store, check_header, path_str, remove_db_files};

/// Writes a WAL snapshot over `file` on its own connection (`VACUUM INTO`, then rename).
pub fn save_snapshot(working_copy: &Path, file: &Path) -> Result<()> {
    let mut tmp = file.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    remove_db_files(&tmp)?;
    // No CREATE flag: a closed project must not come back as an empty file.
    let conn = Connection::open_with_flags(working_copy, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.execute("VACUUM INTO ?1", [path_str(&tmp)?])?;
    drop(conn);
    std::fs::rename(&tmp, file)?;
    Ok(())
}

impl Store {
    /// Opens a working copy that a crash left behind, in place, with its unsaved edits.
    pub fn restore(working_copy: &Path) -> Result<Store> {
        let lock = crate::lock(working_copy)?;
        let conn = Connection::open_with_flags(working_copy, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        check_header(&conn)?;
        Store::init(conn, working_copy, lock)
    }

    /// Deletes a leftover working copy and its WAL files; refuses one a running app holds.
    pub fn discard(working_copy: &Path) -> Result<()> {
        drop(crate::lock(working_copy)?);
        remove_db_files(working_copy)
    }
}
