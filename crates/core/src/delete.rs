//! Deletion rules: a deleted map keeps its entities and pages, and its child maps become root
//! maps. An entity with its own map is deleted only with an explicit `OwnMap` choice.
//!
//! Undo rule: a change that deletes map M goes to the history of M's parent, or to the project
//! history (`undo_project`) when M is a root map. Any other change goes to the history of the
//! map it was applied on. A deleted map keeps its rows and history until the project reopens.

use crate::persist::{ChildLink, Write};

use crate::entities::EntityChanges;
use crate::undo::Change;
use crate::{EntityEdit, EntityId, Error, Map, MapId, Project, Result};

/// What happens to the map made from an entity when the entity is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnMap {
    Delete,
    KeepAsRoot,
}

/// History key of the project history; map ids start at 1.
const PROJECT: MapId = MapId(0);

#[derive(Debug, Clone)]
pub(crate) struct DeletionChange {
    entities: EntityChanges,
    /// Maps as they were before the deletion.
    deleted: Vec<Map>,
    unlinked: Vec<(Map, ChildLink)>,
}

impl Project {
    /// Deletes `map` (one command). Entities and pages stay; child maps become root maps.
    pub fn delete_map(&mut self, map: MapId) -> Result<()> {
        self.map(map)?;
        let c = self.deletion(EntityChanges::default(), &[map], &[])?;
        self.record(map, Change::Deletion(c))
    }

    /// Undoes the last deletion of a root map; `false` if there is none.
    pub fn undo_project(&mut self) -> Result<bool> {
        self.step(PROJECT, false)
    }

    /// Redoes the last undone deletion of a root map; `false` if there is none.
    pub fn redo_project(&mut self) -> Result<bool> {
        self.step(PROJECT, true)
    }

    /// Live child maps made from `entity`.
    pub(crate) fn own_maps(&self, entity: EntityId) -> Result<Vec<MapId>> {
        Ok(self
            .store
            .child_maps()?
            .into_iter()
            .filter(|c| c.entity_id == entity.0)
            .map(|c| MapId(c.map_id))
            .filter(|m| self.map(*m).is_ok())
            .collect())
    }

    pub(crate) fn entity_deletion(&self, edit: &EntityEdit) -> Result<DeletionChange> {
        let EntityEdit::DeleteWithMap { id, own_map } = edit else {
            unreachable!("only DeleteWithMap is routed here");
        };
        let entities = self.entity_changes(edit)?;
        let own = self.own_maps(*id)?;
        match own_map {
            OwnMap::Delete => self.deletion(entities, &own, &[]),
            OwnMap::KeepAsRoot => self.deletion(entities, &[], &own),
        }
    }

    /// Deletes `maps`, and turns `roots` and the children of `maps` into root maps.
    fn deletion(
        &self,
        entities: EntityChanges,
        maps: &[MapId],
        roots: &[MapId],
    ) -> Result<DeletionChange> {
        let deleted: Vec<Map> = self
            .maps
            .iter()
            .filter(|m| maps.contains(&m.id))
            .cloned()
            .collect();
        let mut unlinked = vec![];
        for m in &self.maps {
            let child_of_deleted = m.parent.is_some_and(|p| maps.contains(&p));
            if (roots.contains(&m.id) || child_of_deleted) && !maps.contains(&m.id) {
                let link = self.store.child_link(m.id.0)?.ok_or(Error::RootMap(m.id))?;
                unlinked.push((m.clone(), link));
            }
        }
        Ok(DeletionChange {
            entities,
            deleted,
            unlinked,
        })
    }

    pub(crate) fn write_deletion(&mut self, c: &DeletionChange, forward: bool) -> Result<()> {
        let mut rows: Vec<Write> = c
            .deleted
            .iter()
            .map(|m| Write::MapDeleted(m.id.0, forward))
            .collect();
        rows.extend(
            c.unlinked
                .iter()
                .map(|(m, l)| Write::ChildLink(m.id.0, (!forward).then(|| l.clone()))),
        );
        rows.extend(c.entities.rows(forward));
        self.store.write(&rows)?;
        self.note_names(&c.entities, forward);
        for m in &c.deleted {
            if forward {
                // No job may write into a deleted map.
                self.pipeline.forget(m.id);
                self.forget_map_tiles(m.id);
            }
        }
        if forward {
            self.maps
                .retain(|m| !c.deleted.iter().any(|d| d.id == m.id));
        } else {
            self.maps.extend(c.deleted.iter().cloned());
            self.maps.sort_by_key(|m| m.id);
            // Its rebuild stopped at the deletion.
            for m in &c.deleted {
                self.rebuild_derived(m.id)?;
            }
        }
        for (before, _) in &c.unlinked {
            if let Some(m) = self.maps.iter_mut().find(|m| m.id == before.id) {
                *m = before.clone();
                if forward {
                    (m.parent, m.outline) = (None, None);
                }
            }
        }
        Ok(())
    }
}

/// The history a change goes to when it is applied on `map`.
pub(crate) fn history_key(map: MapId, change: &Change) -> MapId {
    let Change::Deletion(d) = change else {
        return map;
    };
    match d.deleted.iter().find(|m| m.id == map) {
        Some(m) => m
            .parent
            .filter(|p| !d.deleted.iter().any(|x| x.id == *p))
            .unwrap_or(PROJECT),
        None => map,
    }
}
