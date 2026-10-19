//! Entity pages, page templates and the page link index (ADR-0012).

use mmc_core::persist::PageRecord;
use rusqlite::{Connection, OptionalExtension, params};

use crate::{Result, Store};

// No foreign key to entities: a deleted entity keeps its page row so undo of the delete restores it.
// ponytail: orphan pages stay in the file; purge them on save if file size matters.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE page_templates (
        kind TEXT PRIMARY KEY REFERENCES entity_kinds(id),
        body TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE pages (
        entity_id INTEGER PRIMARY KEY,
        body TEXT NOT NULL
    );
    CREATE TABLE page_links (
        to_id INTEGER NOT NULL,
        from_id INTEGER NOT NULL,
        PRIMARY KEY (to_id, from_id)
    ) WITHOUT ROWID;
    CREATE INDEX page_links_from ON page_links (from_id);
    INSERT INTO page_templates (kind, body) VALUES
        ('nation', '## Overview\n\n## History\n\n## Government\n\n## People\n'),
        ('region', '## Overview\n\n## History\n\n## Places\n'),
        ('settlement', '## Overview\n\n## History\n\n## People\n\n## Places\n'),
        ('landmark', '## Overview\n\n## History\n'),
        ('dungeon', '## Overview\n\n## History\n\n## Dangers\n'),
        ('mountain_range', '## Overview\n\n## Peaks\n'),
        ('river', '## Overview\n\n## Course\n'),
        ('lake', '## Overview\n'),
        ('forest', '## Overview\n\n## Wildlife\n'),
        ('sea', '## Overview\n\n## Coasts\n'),
        ('island', '## Overview\n\n## Places\n'),
        ('desert', '## Overview\n\n## Places\n');
";

impl Store {
    pub fn page(&self, entity_id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT body FROM pages WHERE entity_id = ?1",
                [entity_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn page_template(&self, kind: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT body FROM page_templates WHERE kind = ?1",
                [kind],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Existing entities whose page links to `entity_id`, ordered by id.
    pub fn backlinks(&self, entity_id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT l.from_id FROM page_links l JOIN entities e ON e.id = l.from_id
             WHERE l.to_id = ?1 ORDER BY l.from_id",
        )?;
        let rows = stmt.query_map([entity_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// Writes the page and rebuilds its link index rows, inside the caller's transaction.
pub(crate) fn write_page(tx: &Connection, p: &PageRecord) -> Result<()> {
    match &p.body {
        Some(b) => tx.execute(
            "INSERT INTO pages (entity_id, body) VALUES (?1, ?2)
             ON CONFLICT (entity_id) DO UPDATE SET body = excluded.body",
            params![p.entity_id, b],
        )?,
        None => tx.execute("DELETE FROM pages WHERE entity_id = ?1", [p.entity_id])?,
    };
    crate::search::index_page(tx, p.entity_id, p.body.as_deref())?;
    tx.execute("DELETE FROM page_links WHERE from_id = ?1", [p.entity_id])?;
    for to in &p.links {
        tx.execute(
            "INSERT OR IGNORE INTO page_links (to_id, from_id) VALUES (?1, ?2)",
            [to, &p.entity_id],
        )?;
    }
    Ok(())
}
