//! Per-map undo history (ADR-0016), in memory and bounded.

use std::collections::VecDeque;

use mmc_store::{TileKey, TileRecord};

use crate::entity::EntityChanges;
use crate::{Command, MapId, Project, Result};

/// Commands kept per map; the oldest drops first.
pub const UNDO_LIMIT: usize = 100;

/// One tile before and after a command; `None` means the tile row is absent.
#[derive(Debug, Clone)]
pub(crate) struct TileChange {
    pub key: TileKey,
    pub before: Option<TileRecord>,
    pub after: Option<TileRecord>,
}

/// The records one command changed.
#[derive(Debug, Clone)]
pub(crate) enum Change {
    Tiles(Vec<TileChange>),
    Entities(EntityChanges),
    Page(crate::page::PageChange),
    Anchor(crate::anchor::AnchorChange),
    Generation(crate::generation::GenerationChange),
}

#[derive(Default)]
pub(crate) struct History {
    done: VecDeque<Change>,
    undone: Vec<Change>,
}

impl Project {
    /// Applies `cmd` to `map` in the working copy and records it in the map's history.
    pub fn apply(&mut self, map: MapId, cmd: Command) -> Result<()> {
        let changes = self.changes(self.map(map)?, &cmd)?;
        let changes = self.clip_to_outline(map, changes)?;
        Self::refuse_derived(&changes)?;
        self.write(&changes, true)?;
        self.after_write(map, &changes);
        let h = self.history.entry(map).or_default();
        h.undone.clear();
        if h.done.len() == UNDO_LIMIT {
            h.done.pop_front();
        }
        h.done.push_back(changes);
        Ok(())
    }

    /// Undoes the last command on `map`; `false` if there is none.
    pub fn undo(&mut self, map: MapId) -> Result<bool> {
        self.map(map)?;
        let Some(changes) = self.history.entry(map).or_default().done.pop_back() else {
            return Ok(false);
        };
        let res = self.write(&changes, false);
        if res.is_ok() {
            self.after_write(map, &changes);
        }
        let h = self.history.get_mut(&map).unwrap();
        match res {
            Ok(()) => h.undone.push(changes),
            Err(_) => h.done.push_back(changes),
        }
        res.map(|()| true)
    }

    /// Redoes the last undone command on `map`; `false` if there is none.
    pub fn redo(&mut self, map: MapId) -> Result<bool> {
        self.map(map)?;
        let Some(changes) = self.history.entry(map).or_default().undone.pop() else {
            return Ok(false);
        };
        let res = self.write(&changes, true);
        if res.is_ok() {
            self.after_write(map, &changes);
        }
        let h = self.history.get_mut(&map).unwrap();
        match res {
            Ok(()) => h.done.push_back(changes),
            Err(_) => h.undone.push(changes),
        }
        res.map(|()| true)
    }

    /// Writes the after records, or the before records when `forward` is false.
    fn write(&mut self, changes: &Change, forward: bool) -> Result<()> {
        match changes {
            Change::Tiles(tiles) => {
                let writes: Vec<_> = tiles
                    .iter()
                    .map(|c| {
                        (
                            c.key.clone(),
                            if forward { &c.after } else { &c.before }.clone(),
                        )
                    })
                    .collect();
                Ok(self.store.write_tiles(&writes)?)
            }
            Change::Entities(e) => self.write_entity_changes(e, forward),
            Change::Page(p) => self.write_page_change(p, forward),
            Change::Anchor(a) => self.write_anchor_change(a, forward),
            Change::Generation(g) => self.write_generation_change(g, forward),
        }
    }
}
