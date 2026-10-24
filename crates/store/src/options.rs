//! Map kind option values, one set per project (ADR-0026).

use rusqlite::{Transaction, params};

use crate::{Result, Store};

pub(crate) const MIGRATION: &str = "CREATE TABLE kind_options (
    kind TEXT NOT NULL,
    id TEXT NOT NULL,
    value REAL NOT NULL,
    PRIMARY KEY (kind, id)
);";

impl Store {
    /// The stored option values of `kind` by id.
    pub fn kind_options(&self, kind: &str) -> Result<Vec<(String, f64)>> {
        let mut st = self
            .conn
            .prepare("SELECT id, value FROM kind_options WHERE kind = ?1 ORDER BY id")?;
        let rows = st.query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

pub(crate) fn write_kind_option(tx: &Transaction, kind: &str, id: &str, value: f64) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO kind_options (kind, id, value) VALUES (?1, ?2, ?3)",
        params![kind, id, value],
    )?;
    Ok(())
}
