//! SQLite persistence. The only crate that uses SQL.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::time::Duration;

use mmc_core::persist::{MapRecord, Write};
use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};

mod assets;
mod tiles;
pub use tiles::TileConn;
mod reader;
pub use reader::FileReader;
mod anchors;
mod entities;
mod pages;
mod search;
pub use search::Reader;
mod children;
mod generations;
mod save;
pub use save::save_snapshot;
mod delete;
mod objects;
mod port;
mod resync;

/// `PRAGMA application_id` of a project file ("MMC1").
pub const APPLICATION_ID: i32 = 0x4D4D_4331;

/// Ordered migrations; `user_version` is the count applied. Public for migration tests.
pub const MIGRATIONS: &[&str] = &[
    "CREATE TABLE maps (
        id INTEGER PRIMARY KEY,
        parent_id INTEGER REFERENCES maps(id),
        kind TEXT NOT NULL,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        metres_per_pixel REAL NOT NULL
    );",
    assets::MIGRATION,
    tiles::MIGRATION,
    entities::MIGRATION,
    pages::MIGRATION,
    anchors::MIGRATION,
    search::MIGRATION,
    generations::MIGRATION,
    children::MIGRATION,
    delete::MIGRATION,
    resync::MIGRATION,
    generations::PARAMS_MIGRATION,
    objects::MIGRATION,
    // One project seed (ADR-0021); a new or older file gets a random one.
    "CREATE TABLE project (seed INTEGER NOT NULL);
     INSERT INTO project (seed) VALUES (random());",
    entities::LABEL_KEYS,
];

pub const SCHEMA_VERSION: i32 = MIGRATIONS.len() as i32;

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    NotAProject,
    NewerVersion {
        found: i32,
        supported: i32,
    },
    /// Another running app holds this working copy.
    InUse,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "database error: {e}"),
            Error::Io(e) => write!(f, "file error: {e}"),
            Error::NotAProject => write!(f, "the file is not a MakiMapCreator project"),
            Error::InUse => write!(f, "another window has this work open"),
            Error::NewerVersion { found, supported } => write!(
                f,
                "the file was saved by a newer version of the app \
                 (file version {found}, this app supports up to {supported})"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Sqlite(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<Error> for mmc_core::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::NotAProject => mmc_core::Error::NotAProject,
            Error::InUse => mmc_core::Error::WorkingCopyInUse,
            Error::NewerVersion { found, supported } => {
                mmc_core::Error::NewerVersion { found, supported }
            }
            e => mmc_core::Error::Storage(e.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Foreground writes pending; background writers wait between chunks while it is above 0.
pub(crate) type Gate = Arc<AtomicUsize>;

/// Retries a locked database every 100 µs for about 5 s; the default waits up to 100 ms a retry.
fn busy_wait(attempt: i32) -> bool {
    std::thread::sleep(Duration::from_micros(100));
    attempt < 50_000
}

/// Settings of every connection to a working copy.
pub(crate) fn tune(conn: &Connection) -> Result<()> {
    conn.busy_handler(Some(busy_wait))?;
    // No fsync per commit; see `Store::init`.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

/// An open project: a working copy in WAL mode that `save` snapshots to the project file.
pub struct Store {
    conn: Connection,
    working_copy: PathBuf,
    gate: Gate,
    /// Held while the store is open, so another app instance never restores or discards it.
    _lock: std::fs::File,
}

/// The lock file of a working copy, next to it in local app data.
fn lock_path(working_copy: &Path) -> PathBuf {
    let mut p = working_copy.as_os_str().to_owned();
    p.push("-lock");
    PathBuf::from(p)
}

/// Takes the exclusive lock of `working_copy`; the OS drops it when the process ends.
fn lock(working_copy: &Path) -> Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(working_copy))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(std::fs::TryLockError::WouldBlock) => Err(Error::InUse),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

impl Store {
    /// Creates an empty project at `working_copy`.
    pub fn create(working_copy: &Path) -> Result<Store> {
        refuse_existing(working_copy)?;
        let lock = lock(working_copy)?;
        let conn = Connection::open(working_copy)?;
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        Store::init(conn, working_copy, lock)
    }

    /// Whether a running app holds `working_copy`. A crash leaves it unheld.
    pub fn in_use(working_copy: &Path) -> bool {
        matches!(lock(working_copy), Err(Error::InUse))
    }

    /// Copies the project `file` to `working_copy` and migrates the copy; the file is only read.
    pub fn open(file: &Path, working_copy: &Path) -> Result<Store> {
        let src = open_read_only(file)?;
        refuse_existing(working_copy)?;
        let lock = lock(working_copy)?;
        // A plain copy under a read lock: the saved file is never in WAL mode, and VACUUM INTO is slow.
        src.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")?;
        // Not fs::copy: it keeps a read-only attribute on the working copy.
        let mut from = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(file)?);
        std::io::copy(&mut from, &mut std::fs::File::create_new(working_copy)?)?;
        drop(src);
        Store::init(Connection::open(working_copy)?, working_copy, lock)
    }

    fn init(mut conn: Connection, working_copy: &Path, lock: std::fs::File) -> Result<Store> {
        // No fsync for the switch to WAL; the project file holds the data until the first edit.
        conn.pragma_update(None, "synchronous", "OFF")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // No fsync per edit; WAL survives an app crash, a power loss may drop the last edits.
        tune(&conn)?;
        // Worker connections checkpoint the WAL; a checkpoint on an edit's commit stalls it.
        conn.pragma_update(None, "wal_autocheckpoint", 0)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let tx = conn.transaction()?;
        for sql in &MIGRATIONS[user_version(&tx)? as usize..] {
            tx.execute_batch(sql)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.execute_batch(delete::PURGE)?;
        tx.commit()?;
        Ok(Store {
            conn,
            working_copy: working_copy.to_owned(),
            gate: Gate::default(),
            _lock: lock,
        })
    }

    /// Runs `f` in an IMMEDIATE transaction, ahead of background writers.
    pub(crate) fn edit<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        self.gate.fetch_add(1, SeqCst);
        let r = (|| {
            let tx = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;
            let v = f(&tx)?;
            tx.commit()?;
            Ok(v)
        })();
        self.gate.fetch_sub(1, SeqCst);
        r
    }

    /// A worker connection whose writes give way to this store's edits.
    pub fn tile_conn(&self) -> Result<TileConn> {
        TileConn::open_gated(&self.working_copy, self.gate.clone())
    }

    /// Writes a snapshot over `file`: `VACUUM INTO` a temporary file, then an atomic rename.
    pub fn save(&self, file: &Path) -> Result<()> {
        save_snapshot(&self.working_copy, file)
    }

    /// Closes the project and deletes the working copy.
    pub fn close(self) -> Result<()> {
        let Store {
            conn,
            working_copy,
            _lock,
            ..
        } = self;
        conn.close().map_err(|(_, e)| e)?;
        drop(_lock);
        remove_db_files(&working_copy)
    }

    pub fn insert_map(
        &self,
        parent_id: Option<i64>,
        kind: &str,
        width: u32,
        height: u32,
        metres_per_pixel: f64,
    ) -> Result<i64> {
        self.edit(|tx| {
            tx.execute(
                "INSERT INTO maps (parent_id, kind, width, height, metres_per_pixel)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![parent_id, kind, width, height, metres_per_pixel],
            )?;
            Ok(tx.last_insert_rowid())
        })
    }

    /// All maps, ordered by id.
    pub fn maps(&self) -> Result<Vec<MapRecord>> {
        maps(&self.conn)
    }

    /// The u64 project seed, bit for bit.
    pub fn seed(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT seed FROM project", [], |r| r.get(0))?)
    }

    /// Writes all rows in one transaction.
    pub fn write(&self, writes: &[Write]) -> Result<()> {
        self.edit(|tx| {
            for w in writes {
                match w {
                    Write::Tile(k, rec) => tiles::write_tile(tx, k, rec.as_ref())?,
                    Write::Entity(id, rec) => entities::write_entity(tx, *id, rec.as_ref())?,
                    Write::Relation(r, present) => entities::write_relation(tx, r, *present)?,
                    Write::Page(p) => pages::write_page(tx, p)?,
                    Write::Anchor(id, rec) => anchors::write_anchor(tx, *id, rec.as_ref())?,
                    Write::Generation(g, present) => {
                        generations::write_generation(tx, g, *present)?
                    }
                    Write::Object(id, rec) => objects::write_object(tx, *id, rec.as_ref())?,
                    Write::FieldDef(d, present) => entities::write_field_def(tx, d, *present)?,
                    Write::MapDeleted(id, d) => delete::write_map_deleted(tx, *id, *d)?,
                    Write::ChildLink(id, link) => delete::write_child_link(tx, *id, link.as_ref())?,
                    Write::ChildState(id, s) => resync::write_child_state(tx, *id, s)?,
                    Write::OutOfDate(id, o) => resync::write_out_of_date(tx, *id, *o)?,
                    Write::MapRect {
                        id,
                        origin_x,
                        origin_y,
                        width,
                        height,
                    } => {
                        tx.execute(
                            "UPDATE maps SET origin_x = ?2, origin_y = ?3, width = ?4, height = ?5
                         WHERE id = ?1",
                            rusqlite::params![id, origin_x, origin_y, width, height],
                        )?;
                    }
                    Write::ShiftTiles { map_id, dx, dy } => {
                        tiles::shift_tiles(tx, *map_id, *dx, *dy)?
                    }
                }
            }
            Ok(())
        })
    }

    /// Path of the working copy, for extra read connections (see `FileReader`).
    pub fn working_copy(&self) -> &Path {
        &self.working_copy
    }
}

pub(crate) fn maps(conn: &Connection) -> Result<Vec<MapRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, parent_id, kind, width, height, metres_per_pixel, origin_x, origin_y
         FROM maps ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(MapRecord {
            id: r.get(0)?,
            parent_id: r.get(1)?,
            kind: r.get(2)?,
            width: r.get(3)?,
            height: r.get(4)?,
            metres_per_pixel: r.get(5)?,
            origin_x: r.get(6)?,
            origin_y: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Opens `file` read-only after checking that it is a project this app can read.
pub(crate) fn open_read_only(file: &Path) -> Result<Connection> {
    let src = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    check_header(&src)?;
    Ok(src)
}

fn user_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Refuses a file that is not a project or comes from a newer app.
fn check_header(conn: &Connection) -> Result<()> {
    let app_id: i32 = conn
        .query_row("PRAGMA application_id", [], |r| r.get(0))
        .map_err(|_| Error::NotAProject)?;
    if app_id != APPLICATION_ID {
        return Err(Error::NotAProject);
    }
    let found = user_version(conn)?;
    if found > SCHEMA_VERSION {
        return Err(Error::NewerVersion {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not valid UTF-8",
        ))
    })
}

/// A leftover working copy may hold unsaved work, so never overwrite it.
fn refuse_existing(working_copy: &Path) -> Result<()> {
    if working_copy.exists() {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("working copy already exists: {}", working_copy.display()),
        )));
    }
    Ok(())
}

/// Removes a database file and its WAL and shared-memory files, if present.
fn remove_db_files(db: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm", "-journal", "-lock"] {
        let mut p = db.as_os_str().to_owned();
        p.push(suffix);
        match std::fs::remove_file(PathBuf::from(p)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn vacuum_into_output_is_not_wal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = super::Store::create(&tmp.path().join("wc.sqlite")).unwrap();
        let out = tmp.path().join("out.mmc");
        store.save(&out).unwrap();
        let header = std::fs::read(&out).unwrap();
        // Bytes 18 and 19 are 1 for rollback journal, 2 for WAL.
        assert_eq!((header[18], header[19]), (1, 1));
    }
}
