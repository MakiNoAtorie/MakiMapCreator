//! Project data the panels show, reloaded on change, so drawing never queries (ADR-0014).

use std::collections::HashMap;

use mmc_core::{Entity, EntityId, FieldDef, Map, MapId, Project, RootPoint};

use crate::i18n::tr_args;
use crate::workspace::{self, Tab};
use crate::{App, errors};

#[derive(Default)]
pub struct Snapshot {
    pub maps: Vec<Map>,
    pub entities: Vec<Entity>,
    pub defs: HashMap<String, Vec<FieldDef>>,
    /// Outlines of the entities on the shown map.
    pub outlines: Vec<(EntityId, Vec<RootPoint>)>,
    pub rows: Vec<workspace::Row>,
    pub last_run: Option<String>,
}

impl Snapshot {
    pub fn load(p: &Project, map: MapId) -> mmc_core::Result<Snapshot> {
        let entities = p.entities()?;
        let relations = p.relations()?;
        let kinds = p.entity_kinds()?;
        let mut defs = HashMap::new();
        for k in &kinds {
            defs.insert(k.id.clone(), p.field_defs(&k.id)?);
        }
        let last_run = p.generations(map)?.last().map(|g| {
            tr_args(
                "generate-last",
                &[
                    ("generator", &g.run.generator),
                    ("seed", &g.run.seed.to_string()),
                    ("version", &g.app_version),
                ],
            )
        });
        Ok(Snapshot {
            maps: p.maps().to_vec(),
            rows: workspace::outliner_rows(&entities, &relations, &kinds),
            outlines: p.outlines(map)?,
            entities,
            defs,
            last_run,
        })
    }
}

impl App {
    /// Writes the snapshot and save state while the project is free.
    pub(crate) fn sync(&mut self) {
        let Some(map) = self.doc.as_ref().map(|d| d.map) else {
            return;
        };
        let seen = self.painters.get(&map).map(|p| p.version);
        if seen != self.painter_seen {
            self.painter_seen = seen;
            self.dirty = true;
        }
        let Ok(mut guard) = self.project.try_lock() else {
            return;
        };
        let Some(p) = guard.as_mut() else {
            return;
        };
        let report =
            |id: &str, e: &mmc_core::Error| Some(tr_args(id, &[("error", &errors::text(e))]));
        if let Some(file) = self.pending_save.take() {
            p.save(&file);
            self.saving = true;
        }
        if self.saving
            && let Some(r) = p.poll_save()
        {
            self.saving = false;
            if let Err(e) = r {
                self.message = report("error-save", &e);
            }
        }
        if self.dirty {
            self.dirty = false;
            match Snapshot::load(p, map) {
                Ok(s) => self.snap = s,
                Err(e) => self.message = report("error-read", &e),
            }
            if self
                .selection
                .is_some_and(|s| !self.snap.entities.iter().any(|e| e.id == s))
            {
                self.selection = None;
            }
            let es = &self.snap.entities;
            self.tabs.retain(|t| match t {
                Tab::Page(id) => es.iter().any(|e| e.id == *id),
                Tab::Map(_) => true,
            });
        }
        if let Some(Tab::Page(id)) = self.tabs.active()
            && self.page.as_ref().map(|x| x.0) != Some(id)
        {
            match p.page(id) {
                Ok(text) => self.page = Some((id, text)),
                Err(e) => self.message = report("error-read", &e),
            }
        }
    }
}
