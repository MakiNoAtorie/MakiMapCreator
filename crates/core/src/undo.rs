//! Per-map undo history (ADR-0016), in memory and bounded.

use std::collections::VecDeque;

use crate::persist::{TileKey, TileRecord, Write};

use crate::entities::EntityChanges;
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

impl TileChange {
    /// The row write for the after state, or the before state when `forward` is false.
    pub(crate) fn write(&self, forward: bool) -> Write {
        let rec = if forward { &self.after } else { &self.before };
        Write::Tile(self.key.clone(), rec.clone())
    }
}

/// The records one command changed.
#[derive(Debug, Clone)]
pub(crate) enum Change {
    Tiles(Vec<TileChange>),
    Entities(EntityChanges),
    Page(crate::entities::PageChange),
    Anchor(crate::entities::AnchorChange),
    Generation(crate::generation::GenerationChange),
    Deletion(crate::delete::DeletionChange),
    Resync(crate::resync::ResyncChange),
    Objects(crate::object::ObjectChange),
    /// A child map was made; undo removes it.
    NewMap(crate::Map),
}

#[derive(Default)]
pub(crate) struct History {
    done: VecDeque<Change>,
    undone: Vec<Change>,
    /// The last `done` entry is a stroke that `apply_part` still extends.
    open_stroke: bool,
    /// Flatten level of the open stroke.
    level: Option<f32>,
}

impl Project {
    /// Applies `cmd` to `map` in the working copy and records it in the map's history.
    pub fn apply(&mut self, map: MapId, cmd: Command) -> Result<()> {
        self.finish_strokes()?;
        let changes = self.changes(self.map(map)?, &cmd, &mut None)?;
        let changes = self.clip_to_outline(map, changes)?;
        self.refuse_derived(map, &changes)?;
        self.record(map, changes)
    }

    /// Writes `changes` applied on `map` and records them in the history `history_key` picks.
    pub(crate) fn record(&mut self, map: MapId, changes: Change) -> Result<()> {
        self.finish_strokes()?;
        let key = crate::delete::history_key(map, &changes);
        self.write(&changes, true)?;
        self.after_write(map, &changes);
        self.push(key, changes);
        Ok(())
    }

    pub(crate) fn push(&mut self, key: MapId, changes: Change) {
        let h = self.history.entry(key).or_default();
        h.undone.clear();
        h.open_stroke = false;
        if h.done.len() == UNDO_LIMIT {
            h.done.pop_front();
        }
        h.done.push_back(changes);
    }

    /// The flatten level of the stroke `apply_part` extends on `map`, once its first stamp set it.
    pub fn stroke_level(&self, map: MapId) -> Option<f32> {
        match self.session_level(map) {
            Some(level) => level,
            None => self
                .history
                .get(&map)
                .filter(|h| h.open_stroke)
                .and_then(|h| h.level),
        }
    }

    /// Applies one part of a stroke drawn while the pen moves; parts until `end_stroke` are one undo step.
    /// A brush or paint part on a map without an outline stays in memory until `end_stroke` (see `stroke.rs`).
    pub fn apply_part(&mut self, map: MapId, cmd: Command) -> Result<()> {
        if self.map(map)?.outline.is_none() && matches!(cmd, Command::Brush(_) | Command::Paint(_))
        {
            return self.stroke_part(map, &cmd);
        }
        self.finish_strokes()?;
        let mut level = self
            .history
            .get(&map)
            .filter(|h| h.open_stroke)
            .and_then(|h| h.level);
        let changes = self.changes(self.map(map)?, &cmd, &mut level)?;
        let changes = self.clip_to_outline(map, changes)?;
        self.refuse_derived(map, &changes)?;
        let key = crate::delete::history_key(map, &changes);
        self.write(&changes, true)?;
        let h = self.history.entry(key).or_default();
        match (h.open_stroke, h.done.back_mut(), changes) {
            (true, Some(Change::Tiles(done)), Change::Tiles(part)) => merge_tiles(done, part),
            (_, _, changes) => self.push(key, changes),
        }
        let h = self.history.entry(key).or_default();
        h.open_stroke = true;
        h.level = level;
        Ok(())
    }

    /// Ends the stroke `apply_part` began on `map`: writes it and starts the rebuild of its derived layers.
    pub fn end_stroke(&mut self, map: MapId) -> Result<()> {
        if self.strokes_open(map) {
            return self.finish_stroke(map);
        }
        let Some(h) = self.history.get_mut(&map) else {
            return Ok(());
        };
        if !std::mem::take(&mut h.open_stroke) {
            return Ok(());
        }
        let layers: Vec<_> = match h.done.back() {
            Some(Change::Tiles(t)) => t.iter().map(|c| c.key.layer).collect(),
            _ => vec![],
        };
        self.layers_written(map, &layers);
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

    /// Tiles that the next redo (`forward`) or undo on `map` writes, so after an undo `step_tiles(map, true)`
    /// gives the tiles it wrote; 0 when the change has none.
    pub fn step_tiles(&self, map: MapId, forward: bool) -> usize {
        let h = self.history.get(&map);
        let next = h.and_then(|h| {
            if forward {
                h.undone.last()
            } else {
                h.done.back()
            }
        });
        match next {
            Some(Change::Tiles(t)) => t.len(),
            _ => 0,
        }
    }

    /// Redoes (`forward`) or undoes one change of the history `key`.
    pub(crate) fn step(&mut self, key: MapId, forward: bool) -> Result<bool> {
        self.finish_strokes()?;
        let h = self.history.entry(key).or_default();
        h.open_stroke = false;
        let Some(changes) = (if forward {
            h.undone.pop()
        } else {
            h.done.pop_back()
        }) else {
            return Ok(false);
        };
        let res = self.write(&changes, forward);
        if res.is_ok() {
            // Before the rebuild of derived layers takes the CPUs: the next render then shows the step at every level.
            if matches!(changes, Change::Tiles(_)) {
                self.wait_pyramid_shown();
            }
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
                let rows: Vec<_> = tiles.iter().map(|c| c.write(forward)).collect();
                self.write_rows(&rows)
            }
            Change::Entities(e) => self.write_entity_changes(e, forward),
            Change::Page(p) => self.write_page_change(p, forward),
            Change::Anchor(a) => self.write_anchor_change(a, forward),
            Change::Generation(g) => self.write_generation_change(g, forward),
            Change::Deletion(d) => self.write_deletion(d, forward),
            Change::Resync(r) => self.write_resync_change(r, forward),
            Change::Objects(o) => self.store.write(&o.rows(forward)),
            Change::NewMap(m) => self.write_new_map(m, forward),
        };
        res?;
        self.mark_out_of_date(changes)
    }
}

/// Adds a stroke part's tile changes to the stroke's: the first before and the last after of each tile.
fn merge_tiles(done: &mut Vec<TileChange>, part: Vec<TileChange>) {
    for c in part {
        match done.iter_mut().find(|d| d.key == c.key) {
            Some(d) => d.after = c.after,
            None => done.push(c),
        }
    }
}
