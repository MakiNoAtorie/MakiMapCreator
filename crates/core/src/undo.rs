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
    Deletion(crate::delete::DeletionChange),
    Resync(crate::resync::ResyncChange),
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
        self.record(map, changes)
    }

    /// Writes `changes` applied on `map` and records them in the history `history_key` picks.
    pub(crate) fn record(&mut self, map: MapId, changes: Change) -> Result<()> {
        let key = crate::delete::history_key(map, &changes);
        self.write(&changes, true)?;
        self.after_write(map, &changes);
        let h = self.history.entry(key).or_default();
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
        self.step(map, false)
    }

    /// Redoes the last undone command on `map`; `false` if there is none.
    pub fn redo(&mut self, map: MapId) -> Result<bool> {
        self.map(map)?;
        self.step(map, true)
    }

    /// Redoes (`forward`) or undoes one change of the history `key`.
    pub(crate) fn step(&mut self, key: MapId, forward: bool) -> Result<bool> {
        let h = self.history.entry(key).or_default();
        let Some(changes) = (if forward {
            h.undone.pop()
        } else {
            h.done.pop_back()
        }) else {
            return Ok(false);
        };
        let res = self.write(&changes, forward);
        if res.is_ok() {
            self.after_write(key, &changes);
        }
        let h = self.history.get_mut(&key).unwrap();
        if res.is_ok() == forward {
            h.done.push_back(changes);
        } else {
            h.undone.push(changes);
        }
        res.map(|()| true)
    }
    /// Writes the after records, or the before records when `forward` is false.
    fn write(&mut self, changes: &Change, forward: bool) -> Result<()> {
        let res = match changes {
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
            Change::Deletion(d) => self.write_deletion(d, forward),
            Change::Resync(r) => self.write_resync_change(r, forward),
        };
        res?;
        self.mark_out_of_date(changes)
    }
}
