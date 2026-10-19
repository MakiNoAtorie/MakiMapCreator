//! SQLite persistence. The only crate that uses SQL.

use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

mod assets;
mod tiles;
pub use tiles::{LayerTiles, TileConn};
pub use tiles::{TileKey, TileRecord};
mod reader;
pub use reader::FileReader;
mod entities;
pub use entities::{EntityKindRecord, EntityRecord, FieldDefRecord, RelationRecord};
mod pages;
pub use pages::PageRecord;
mod anchors;
pub use anchors::AnchorRecord;
mod search;
pub use search::{Reader, TextHit};
mod generations;
pub use generations::{GenerationRecord, OutlineRecord};
mod children;
pub use children::{ChildMapRecord, ContextRecord, NewChildMap};
mod save;
pub use save::save_snapshot;
mod delete;
pub use delete::ChildLink;
mod resync;
pub use resync::ChildState;

/// `PRAGMA application_id` of a project file ("MMC1").
pub const APPLICATION_ID: i32 = 0x4D4D_4331;

/// Ordered migrations; `user_version` is the count applied.
const MIGRATIONS: &[&str] = &[
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
];

pub const SCHEMA_VERSION: i32 = MIGRATIONS.len() as i32;

#[derive(Debug)]
pub enum Error {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    NotAProject,
    NewerVersion { found: i32, supported: i32 },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Sqlite(e) => write!(f, "database error: {e}"),
            Error::Io(e) => write!(f, "file error: {e}"),
            Error::NotAProject => write!(f, "the file is not a MakiMapCreator project"),
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

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq)]
pub struct MapRecord {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel: f64,
    /// Root-space position of pixel (0, 0)'s top-left corner, in metres.
    pub origin_x: f64,
    pub origin_y: f64,
}

/// An open project: a working copy in WAL mode that `save` snapshots to the project file.
pub struct Store {
    conn: Connection,
    working_copy: PathBuf,
}

impl Store {
    /// Creates an empty project at `working_copy`.
    pub fn create(working_copy: &Path) -> Result<Store> {
        refuse_existing(working_copy)?;
        let conn = Connection::open(working_copy)?;
        conn.pragma_update(None, "application_id", APPLICATION_ID)?;
        Store::init(conn, working_copy)
    }

    /// Copies the project `file` to `working_copy` and migrates the copy.
    /// The project file is only read.
    pub fn open(file: &Path, working_copy: &Path) -> Result<Store> {
        let src = open_read_only(file)?;
        refuse_existing(working_copy)?;
        // A plain copy under a read lock: VACUUM INTO rebuilds every page and fsyncs the
        // copy, 5-10x slower on CI disks. The project file is never in WAL mode (save
        // writes it with VACUUM INTO), so the file alone is the whole database.
        src.execute_batch("BEGIN; SELECT count(*) FROM sqlite_master;")?;
        // Not fs::copy: it keeps a read-only attribute on the working copy.
        let mut from = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(file)?);
        std::io::copy(&mut from, &mut std::fs::File::create_new(working_copy)?)?;
        drop(src);
        Store::init(Connection::open(working_copy)?, working_copy)
    }

    fn init(mut conn: Connection, working_copy: &Path) -> Result<Store> {
        // No fsync for the switch to WAL: it would flush the whole fresh copy of an opened
        // file. The project file still holds the data until the first edit.
        conn.pragma_update(None, "synchronous", "OFF")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // No fsync per edit: a running save floods the disk and makes each fsync wait.
        // WAL stays consistent after an app crash; power loss may drop the last edits.
        conn.pragma_update(None, "synchronous", "NORMAL")?;
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
        })
    }

    /// Writes a snapshot of the working copy over `file`: `VACUUM INTO` a
    /// temporary file in the same folder, then an atomic rename.
    pub fn save(&self, file: &Path) -> Result<()> {
        save_snapshot(&self.working_copy, file)
    }

    /// Closes the project and deletes the working copy.
    pub fn close(self) -> Result<()> {
        self.conn.close().map_err(|(_, e)| e)?;
        remove_db_files(&self.working_copy)
    }

    pub fn insert_map(
        &self,
        parent_id: Option<i64>,
        kind: &str,
        width: u32,
        height: u32,
        metres_per_pixel: f64,
    ) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO maps (parent_id, kind, width, height, metres_per_pixel)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![parent_id, kind, width, height, metres_per_pixel],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// All maps, ordered by id.
    pub fn maps(&self) -> Result<Vec<MapRecord>> {
        maps(&self.conn)
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
    for suffix in ["", "-wal", "-shm", "-journal"] {
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
