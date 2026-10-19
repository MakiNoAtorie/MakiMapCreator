//! Entities: project-level places with kinds, fields and relations (ADR-0009, ADR-0010).

use crate::persist::{EntityRecord, FieldDefRecord, RelationRecord, Write};
use serde_json::{Map, Value};

use crate::{Error, FieldProblem, Project, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId(pub(crate) i64);

/// Entity reference fields hold the id as a JSON integer.
impl From<EntityId> for Value {
    fn from(id: EntityId) -> Value {
        Value::from(id.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityKind {
    pub id: String,
    /// A UI string id (ADR-0017).
    pub label: String,
    /// "political" or "natural".
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldType {
    Text,
    Markdown,
    Number,
    YesNo,
    Choice(Vec<String>),
    EntityRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDef {
    pub kind: String,
    pub key: String,
    /// A UI string id for built-in fields; the user's text for user fields.
    pub label: String,
    pub field_type: FieldType,
    /// Built-in fields are read and written by the app; the others are user fields.
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entity {
    pub id: EntityId,
    pub kind: String,
    pub name: String,
    pub fields: Map<String, Value>,
}

/// A typed link, for example `part_of` or `capital_of`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub from: EntityId,
    pub to: EntityId,
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntityEdit {
    /// Takes its id from `Project::next_entity_id`.
    Create(Entity),
    Rename {
        id: EntityId,
        name: String,
    },
    /// `Value::Null` clears the field.
    SetField {
        id: EntityId,
        key: String,
        value: Value,
    },
    /// Also removes the entity's relations; refused with `OwnMapChoice` if it has its own map.
    Delete(EntityId),
    /// Deletes an entity and says what happens to the maps made from it.
    DeleteWithMap {
        id: EntityId,
        own_map: crate::OwnMap,
    },
    Relate(Relation),
    Unrelate(Relation),
    /// Adds a user field to a kind (ADR-0010); refuses a key the kind already has.
    AddField(FieldDef),
}

/// Rows before and after one entity edit.
#[derive(Debug, Clone, Default)]
pub(crate) struct EntityChanges {
    entities: Vec<EntityRow>,
    relations: Vec<(RelationRecord, bool, bool)>,
    /// A user field added (`true`) or removed.
    fields: Vec<(FieldDefRecord, bool)>,
}

impl EntityChanges {
    /// The `after` rows, or the `before` rows when `forward` is false.
    pub(crate) fn rows(&self, forward: bool) -> Vec<Write> {
        let entities = self
            .entities
            .iter()
            .map(|(id, b, a)| Write::Entity(*id, if forward { a } else { b }.clone()));
        let relations = self
            .relations
            .iter()
            .map(|(r, b, a)| Write::Relation(r.clone(), if forward { *a } else { *b }));
        let fields = self
            .fields
            .iter()
            .map(|(d, added)| Write::FieldDef(d.clone(), *added == forward));
        entities.chain(relations).chain(fields).collect()
    }
}

impl FieldType {
    fn to_record(&self) -> (&'static str, Option<String>) {
        match self {
            FieldType::Text => ("text", None),
            FieldType::Markdown => ("markdown", None),
            FieldType::Number => ("number", None),
            FieldType::YesNo => ("yes_no", None),
            FieldType::Choice(c) => ("choice", Some(Value::from(c.clone()).to_string())),
            FieldType::EntityRef => ("entity", None),
        }
    }

    fn from_record(ty: &str, choices: Option<&str>) -> Result<FieldType> {
        Ok(match ty {
            "text" => FieldType::Text,
            "markdown" => FieldType::Markdown,
            "number" => FieldType::Number,
            "yes_no" => FieldType::YesNo,
            "entity" => FieldType::EntityRef,
            "choice" => FieldType::Choice(
                serde_json::from_str(choices.unwrap_or("[]")).map_err(|_| Error::CorruptEntity)?,
            ),
            _ => return Err(Error::CorruptEntity),
        })
    }
}

fn entity_from(r: EntityRecord) -> Result<Entity> {
    let Ok(Value::Object(fields)) = serde_json::from_str(&r.fields) else {
        return Err(Error::CorruptEntity);
    };
    Ok(Entity {
        id: EntityId(r.id),
        kind: r.kind,
        name: r.name,
        fields,
    })
}

fn record_of(e: &Entity) -> EntityRecord {
    EntityRecord {
        id: e.id.0,
        kind: e.kind.clone(),
        name: e.name.clone(),
        fields: Value::Object(e.fields.clone()).to_string(),
    }
}

type EntityRow = (i64, Option<EntityRecord>, Option<EntityRecord>);

fn update(before: &Entity, after: &Entity) -> EntityRow {
    (before.id.0, Some(record_of(before)), Some(record_of(after)))
}

fn relation_record(r: &Relation) -> RelationRecord {
    RelationRecord {
        from_id: r.from.0,
        to_id: r.to.0,
        rel_type: r.kind.clone(),
    }
}

impl Project {
    /// Entity kinds in seed order.
    pub fn entity_kinds(&self) -> Result<Vec<EntityKind>> {
        Ok(self
            .store
            .entity_kinds()?
            .into_iter()
            .map(|k| EntityKind {
                id: k.id,
                label: k.label,
                category: k.category,
            })
            .collect())
    }

    /// Field definitions of `kind`, ordered by key.
    pub fn field_defs(&self, kind: &str) -> Result<Vec<FieldDef>> {
        self.store
            .field_defs(kind)?
            .into_iter()
            .map(|d| {
                Ok(FieldDef {
                    field_type: FieldType::from_record(&d.field_type, d.choices.as_deref())?,
                    kind: d.kind,
                    key: d.key,
                    label: d.label,
                    builtin: d.builtin,
                })
            })
            .collect()
    }

    pub fn next_entity_id(&self) -> Result<EntityId> {
        Ok(EntityId(self.store.next_entity_id()?))
    }

    pub fn entity(&self, id: EntityId) -> Result<Option<Entity>> {
        self.store.entity(id.0)?.map(entity_from).transpose()
    }

    /// All entities, ordered by id.
    pub fn entities(&self) -> Result<Vec<Entity>> {
        self.store
            .entities()?
            .into_iter()
            .map(entity_from)
            .collect()
    }

    /// All relations, ordered by (from, to, kind).
    pub fn relations(&self) -> Result<Vec<Relation>> {
        Ok(self
            .store
            .relations()?
            .into_iter()
            .map(|r| Relation {
                from: EntityId(r.from_id),
                to: EntityId(r.to_id),
                kind: r.rel_type,
            })
            .collect())
    }

    fn check_kind(&self, kind: &str) -> Result<()> {
        if self.store.entity_kinds()?.iter().any(|k| k.id == kind) {
            Ok(())
        } else {
            Err(Error::UnknownEntityKind(kind.into()))
        }
    }

    fn existing(&self, id: EntityId) -> Result<Entity> {
        self.entity(id)?.ok_or(Error::UnknownEntity(id))
    }

    fn check_field(&self, defs: &[FieldDef], key: &str, v: &Value) -> Result<()> {
        let invalid = |problem| {
            Err(Error::InvalidField {
                key: key.into(),
                problem,
            })
        };
        let Some(def) = defs.iter().find(|d| d.key == key) else {
            return invalid(FieldProblem::NoSuchField);
        };
        let ok = match &def.field_type {
            FieldType::Text | FieldType::Markdown => v.is_string(),
            FieldType::Number => v.is_number(),
            FieldType::YesNo => v.is_boolean(),
            FieldType::Choice(c) => v.as_str().is_some_and(|s| c.iter().any(|o| o == s)),
            FieldType::EntityRef => match v.as_i64() {
                Some(id) => {
                    if self.store.entity(id)?.is_none() {
                        return invalid(FieldProblem::NoSuchEntity);
                    }
                    true
                }
                None => false,
            },
        };
        if ok {
            Ok(())
        } else {
            invalid(FieldProblem::WrongType)
        }
    }

    pub(crate) fn entity_changes(&self, edit: &EntityEdit) -> Result<EntityChanges> {
        let mut c = EntityChanges::default();
        match edit {
            EntityEdit::Create(e) => {
                self.check_kind(&e.kind)?;
                if self.entity(e.id)?.is_some() {
                    return Err(Error::EntityExists(e.id));
                }
                let defs = self.field_defs(&e.kind)?;
                for (k, v) in &e.fields {
                    self.check_field(&defs, k, v)?;
                }
                c.entities.push((e.id.0, None, Some(record_of(e))));
            }
            EntityEdit::Rename { id, name } => {
                let before = self.existing(*id)?;
                c.entities.push(update(
                    &before,
                    &Entity {
                        name: name.clone(),
                        ..before.clone()
                    },
                ));
            }
            EntityEdit::SetField { id, key, value } => {
                let before = self.existing(*id)?;
                let mut after = before.clone();
                if value.is_null() {
                    after.fields.remove(key);
                } else {
                    self.check_field(&self.field_defs(&before.kind)?, key, value)?;
                    after.fields.insert(key.clone(), value.clone());
                }
                c.entities.push(update(&before, &after));
            }
            EntityEdit::Delete(id) | EntityEdit::DeleteWithMap { id, .. } => {
                let before = self.existing(*id)?;
                for r in self.store.relations()? {
                    if r.from_id == id.0 || r.to_id == id.0 {
                        c.relations.push((r, true, false));
                    }
                }
                c.entities.push((id.0, Some(record_of(&before)), None));
            }
            EntityEdit::AddField(d) => {
                self.check_kind(&d.kind)?;
                if self.field_defs(&d.kind)?.iter().any(|x| x.key == d.key) {
                    return Err(Error::InvalidField {
                        key: d.key.clone(),
                        problem: FieldProblem::Duplicate,
                    });
                }
                let (ty, choices) = d.field_type.to_record();
                let def = FieldDefRecord {
                    kind: d.kind.clone(),
                    key: d.key.clone(),
                    label: d.label.clone(),
                    field_type: ty.into(),
                    choices,
                    builtin: false,
                };
                c.fields.push((def, true));
            }
            EntityEdit::Relate(r) | EntityEdit::Unrelate(r) => {
                self.existing(r.from)?;
                self.existing(r.to)?;
                let rec = relation_record(r);
                let present = self.store.relations()?.contains(&rec);
                c.relations
                    .push((rec, present, matches!(edit, EntityEdit::Relate(_))));
            }
        }
        Ok(c)
    }

    /// Writes the `after` rows, or the `before` rows when `forward` is false.
    pub(crate) fn write_entity_changes(&mut self, c: &EntityChanges, forward: bool) -> Result<()> {
        self.store.write(&c.rows(forward))?;
        self.note_names(c, forward);
        Ok(())
    }

    /// Keeps the name index equal to the entity rows `c` wrote.
    pub(crate) fn note_names(&mut self, c: &EntityChanges, forward: bool) {
        for (id, b, a) in &c.entities {
            match if forward { a } else { b } {
                Some(r) => self.names.set(EntityId(*id), &r.name),
                None => self.names.remove(EntityId(*id)),
            }
        }
    }
}
