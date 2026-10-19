//! Background save and crash restore of the working copy (ADR-0011, ADR-0014).

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::{Result, Store, check_header, path_str, remove_db_files};

/// Writes a snapshot of the working copy over `file` from its own connection, so the
/// working copy stays writable: `VACUUM INTO` reads one WAL snapshot into `<file>.tmp`,
/// then an atomic rename. Callers run one save at a time per `file`.
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
        let conn = Connection::open_with_flags(working_copy, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        check_header(&conn)?;
        Store::init(conn, working_copy)
    }

    /// Deletes a leftover working copy and its WAL files.
    pub fn discard(working_copy: &Path) -> Result<()> {
        remove_db_files(working_copy)
    }
}
