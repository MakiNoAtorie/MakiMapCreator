//! Entities (project-level), their kinds, field definitions and relations (ADR-0009, ADR-0010).

use rusqlite::{OptionalExtension, Row, params};

use crate::{Result, Store};

// Relation keys are deferred so one transaction can delete or restore an entity with its relations.
pub(crate) const MIGRATION: &str = "
    CREATE TABLE entity_kinds (
        id TEXT PRIMARY KEY,
        label TEXT NOT NULL,
        category TEXT NOT NULL
    );
    CREATE TABLE field_defs (
        kind TEXT NOT NULL REFERENCES entity_kinds(id),
        key TEXT NOT NULL,
        label TEXT NOT NULL,
        type TEXT NOT NULL,
        choices TEXT CHECK (choices IS NULL OR json_valid(choices)),
        builtin INTEGER NOT NULL,
        PRIMARY KEY (kind, key)
    ) WITHOUT ROWID;
    CREATE TABLE entities (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        kind TEXT NOT NULL REFERENCES entity_kinds(id),
        name TEXT NOT NULL,
        fields TEXT NOT NULL CHECK (json_valid(fields))
    );
    CREATE TABLE relations (
        from_id INTEGER NOT NULL REFERENCES entities(id) DEFERRABLE INITIALLY DEFERRED,
        to_id INTEGER NOT NULL REFERENCES entities(id) DEFERRABLE INITIALLY DEFERRED,
        type TEXT NOT NULL,
        PRIMARY KEY (from_id, to_id, type)
    ) WITHOUT ROWID;
    INSERT INTO entity_kinds (id, label, category) VALUES
        ('nation', 'Nation', 'political'),
        ('region', 'Region', 'political'),
        ('settlement', 'Settlement', 'political'),
        ('landmark', 'Landmark', 'political'),
        ('dungeon', 'Dungeon', 'political'),
        ('mountain_range', 'Mountain range', 'natural'),
        ('river', 'River', 'natural'),
        ('lake', 'Lake', 'natural'),
        ('forest', 'Forest', 'natural'),
        ('sea', 'Sea', 'natural'),
        ('island', 'Island', 'natural'),
        ('desert', 'Desert', 'natural');
    INSERT INTO field_defs (kind, key, label, type, choices, builtin) VALUES
        ('nation', 'colour', 'Colour', 'text', NULL, 1),
        ('nation', 'capital', 'Capital', 'entity', NULL, 1),
        ('settlement', 'population', 'Population', 'number', NULL, 1);
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityKindRecord {
    pub id: String,
    pub label: String,
    /// "political" or "natural".
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDefRecord {
    pub kind: String,
    pub key: String,
    pub label: String,
    pub field_type: String,
    /// JSON array of options for choice fields.
    pub choices: Option<String>,
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRecord {
    pub id: i64,
    pub kind: String,
    pub name: String,
    /// JSON object of field values.
    pub fields: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationRecord {
    pub from_id: i64,
    pub to_id: i64,
    pub rel_type: String,
}

fn entity_row(r: &Row) -> rusqlite::Result<EntityRecord> {
    Ok(EntityRecord {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        fields: r.get(3)?,
    })
}

impl Store {
    pub fn entity_kinds(&self) -> Result<Vec<EntityKindRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, label, category FROM entity_kinds ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok(EntityKindRecord {
                id: r.get(0)?,
                label: r.get(1)?,
                category: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Field definitions of one kind, ordered by key.
    pub fn field_defs(&self, kind: &str) -> Result<Vec<FieldDefRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, key, label, type, choices, builtin FROM field_defs
             WHERE kind = ?1 ORDER BY key",
        )?;
        let rows = stmt.query_map([kind], |r| {
            Ok(FieldDefRecord {
                kind: r.get(0)?,
                key: r.get(1)?,
                label: r.get(2)?,
                field_type: r.get(3)?,
                choices: r.get(4)?,
                builtin: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn insert_field_def(&self, d: &FieldDefRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO field_defs (kind, key, label, type, choices, builtin)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![d.kind, d.key, d.label, d.field_type, d.choices, d.builtin],
        )?;
        Ok(())
    }

    /// The id the next new entity gets; ids of deleted entities are never reused.
    pub fn next_entity_id(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'entities'), 0) + 1",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn entity(&self, id: i64) -> Result<Option<EntityRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, kind, name, fields FROM entities WHERE id = ?1",
                [id],
                entity_row,
            )
            .optional()?)
    }

    /// All entities, ordered by id.
    pub fn entities(&self) -> Result<Vec<EntityRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, name, fields FROM entities ORDER BY id")?;
        let rows = stmt.query_map([], entity_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// All relations, ordered by (from, to, type).
    pub fn relations(&self) -> Result<Vec<RelationRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT from_id, to_id, type FROM relations ORDER BY from_id, to_id, type")?;
        let rows = stmt.query_map([], |r| {
            Ok(RelationRecord {
                from_id: r.get(0)?,
                to_id: r.get(1)?,
                rel_type: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Writes entities and relations in one transaction. `None` deletes an
    /// entity; `false` deletes a relation.
    pub fn write_entities(
        &self,
        entities: &[(i64, Option<EntityRecord>)],
        relations: &[(RelationRecord, bool)],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        write_entity_rows(&tx, entities, relations)?;
        Ok(tx.commit()?)
    }
}

/// Entity and relation writes inside the caller's transaction.
pub(crate) fn write_entity_rows(
    tx: &rusqlite::Transaction,
    entities: &[(i64, Option<EntityRecord>)],
    relations: &[(RelationRecord, bool)],
) -> Result<()> {
    for (id, rec) in entities {
        match rec {
            Some(e) => tx.execute(
                "INSERT INTO entities (id, kind, name, fields) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE
                 SET kind = excluded.kind, name = excluded.name, fields = excluded.fields",
                params![e.id, e.kind, e.name, e.fields],
            )?,
            None => tx.execute("DELETE FROM entities WHERE id = ?1", [id])?,
        };
        let rec = rec.as_ref().map(|e| (e.name.as_str(), e.fields.as_str()));
        crate::search::index_entity(tx, *id, rec)?;
    }
    for (r, present) in relations {
        let sql = if *present {
            "INSERT OR IGNORE INTO relations (from_id, to_id, type) VALUES (?1, ?2, ?3)"
        } else {
            "DELETE FROM relations WHERE from_id = ?1 AND to_id = ?2 AND type = ?3"
        };
        tx.execute(sql, params![r.from_id, r.to_id, r.rel_type])?;
    }
    Ok(())
}
