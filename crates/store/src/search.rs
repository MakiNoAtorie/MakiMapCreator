//! Full-text index (FTS5) over entity names, text fields and pages (ADR-0014).

use rusqlite::{Connection, OpenFlags, params};

use crate::{Result, Store};

// Rowid = entity id. Entity rows are written with the entity, page rows with the page.
// A deleted entity keeps its page row (undo restores it); queries join `entities` to hide it.
pub(crate) const MIGRATION: &str = "
    CREATE VIRTUAL TABLE fts_entities USING fts5(name, text);
    CREATE VIRTUAL TABLE fts_pages USING fts5(body);
    INSERT INTO fts_entities (rowid, name, text)
        SELECT id, name, COALESCE((SELECT group_concat(value, ' ') FROM json_each(fields)
                                   WHERE type = 'text'), '') FROM entities;
    INSERT INTO fts_pages (rowid, body) SELECT entity_id, body FROM pages;
";

/// A full-text hit. `snippet` comes from the page text and marks matches with `[` `]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextHit {
    pub entity_id: i64,
    pub snippet: Option<String>,
}

pub(crate) fn index_entity(
    tx: &rusqlite::Transaction,
    id: i64,
    rec: Option<(&str, &str)>,
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM fts_entities WHERE rowid = ?1", [id])?;
    if let Some((name, fields)) = rec {
        // Only string values of the fields object are indexed.
        tx.execute(
            "INSERT INTO fts_entities (rowid, name, text) VALUES (?1, ?2,
             (SELECT COALESCE(group_concat(value, ' '), '') FROM json_each(?3) WHERE type = 'text'))",
            params![id, name, fields],
        )?;
    }
    Ok(())
}

pub(crate) fn index_page(
    tx: &rusqlite::Transaction,
    id: i64,
    body: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute("DELETE FROM fts_pages WHERE rowid = ?1", [id])?;
    if let Some(b) = body {
        tx.execute(
            "INSERT INTO fts_pages (rowid, body) VALUES (?1, ?2)",
            params![id, b],
        )?;
    }
    Ok(())
}

/// Every word becomes a quoted prefix term, so user text never reaches the FTS5 query syntax.
fn match_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// A read-only connection to the working copy, for a worker thread. Sees committed writes only.
pub struct Reader {
    conn: Connection,
}

impl Store {
    pub fn reader(&self) -> Result<Reader> {
        let conn =
            Connection::open_with_flags(&self.working_copy, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Reader { conn })
    }
}

impl Reader {
    /// Existing entities whose name, text fields or page match every word of `text` (last as prefix).
    /// Name and field hits come first, then page hits; best rank first in each group.
    pub fn search_text(&self, text: &str, limit: usize) -> Result<Vec<TextHit>> {
        let limit = limit as i64;
        let Some(q) = match_query(text) else {
            return Ok(vec![]);
        };
        let mut hits: Vec<TextHit> = vec![];
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.rowid FROM fts_entities f JOIN entities e ON e.id = f.rowid
             WHERE fts_entities MATCH ?1 ORDER BY f.rank LIMIT ?2",
        )?;
        for id in stmt.query_map(params![q, limit], |r| r.get::<_, i64>(0))? {
            hits.push(TextHit {
                entity_id: id?,
                snippet: None,
            });
        }
        if (hits.len() as i64) < limit {
            let mut stmt = self.conn.prepare_cached(
                "SELECT f.rowid, snippet(fts_pages, 0, '[', ']', '...', 12)
                 FROM fts_pages f JOIN entities e ON e.id = f.rowid
                 WHERE fts_pages MATCH ?1 ORDER BY f.rank LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![q, limit], |r| Ok((r.get(0)?, r.get(1)?)))?;
            for row in rows {
                let (entity_id, snippet): (i64, String) = row?;
                if let Some(h) = hits.iter_mut().find(|h| h.entity_id == entity_id) {
                    h.snippet = Some(snippet);
                } else if (hits.len() as i64) < limit {
                    hits.push(TextHit {
                        entity_id,
                        snippet: Some(snippet),
                    });
                }
            }
        }
        Ok(hits)
    }
}
