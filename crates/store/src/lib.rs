//! SQLite persistence. The only crate that uses SQL.

use std::fmt;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

mod assets;
mod tiles;
pub use tiles::{LayerTiles, TileConn};
pub use tiles::{TileKey, TileRecord};
mod entities;
pub use entities::{EntityKindRecord, EntityRecord, FieldDefRecord, RelationRecord};
mod pages;
pub use pages::PageRecord;

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
        let src = Connection::open_with_flags(file, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let app_id: i32 = src
            .query_row("PRAGMA application_id", [], |r| r.get(0))
            .map_err(|_| Error::NotAProject)?;
        if app_id != APPLICATION_ID {
            return Err(Error::NotAProject);
        }
        let found = user_version(&src)?;
        if found > SCHEMA_VERSION {
            return Err(Error::NewerVersion {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        refuse_existing(working_copy)?;
        src.execute("VACUUM INTO ?1", [path_str(working_copy)?])?;
        drop(src);
        Store::init(Connection::open(working_copy)?, working_copy)
    }

    fn init(mut conn: Connection, working_copy: &Path) -> Result<Store> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", true)?;
        let tx = conn.transaction()?;
        for sql in &MIGRATIONS[user_version(&tx)? as usize..] {
            tx.execute_batch(sql)?;
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(Store {
            conn,
            working_copy: working_copy.to_owned(),
        })
    }

    /// Writes a snapshot of the working copy over `file`: `VACUUM INTO` a
    /// temporary file in the same folder, then an atomic rename.
    pub fn save(&self, file: &Path) -> Result<()> {
        let mut tmp = file.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        remove_db_files(&tmp)?;
        self.conn.execute("VACUUM INTO ?1", [path_str(&tmp)?])?;
        std::fs::rename(&tmp, file)?;
        Ok(())
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
        let mut stmt = self.conn.prepare(
            "SELECT id, parent_id, kind, width, height, metres_per_pixel FROM maps ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(MapRecord {
                id: r.get(0)?,
                parent_id: r.get(1)?,
                kind: r.get(2)?,
                width: r.get(3)?,
                height: r.get(4)?,
                metres_per_pixel: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn user_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
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
