//! Per-map undo history (ADR-0016), in memory and bounded.

use std::collections::VecDeque;

use mmc_store::{TileKey, TileRecord};

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

#[derive(Default)]
pub(crate) struct History {
    done: VecDeque<Vec<TileChange>>,
    undone: Vec<Vec<TileChange>>,
}

impl Project {
    /// Applies `cmd` to `map` in the working copy and records it in the map's history.
    pub fn apply(&mut self, map: MapId, cmd: Command) -> Result<()> {
        let changes = self.changes(self.map(map)?, &cmd)?;
        self.write(&changes, |c| c.after.clone())?;
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
        let res = self.write(&changes, |c| c.before.clone());
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
        let res = self.write(&changes, |c| c.after.clone());
        let h = self.history.get_mut(&map).unwrap();
        match res {
            Ok(()) => h.done.push_back(changes),
            Err(_) => h.undone.push(changes),
        }
        res.map(|()| true)
    }

    fn write(
        &self,
        changes: &[TileChange],
        side: impl Fn(&TileChange) -> Option<TileRecord>,
    ) -> Result<()> {
        let writes: Vec<_> = changes.iter().map(|c| (c.key.clone(), side(c))).collect();
        Ok(self.store.write_tiles(&writes)?)
    }
}
