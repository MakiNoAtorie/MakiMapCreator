//! Summaries, out of date, re-sync and masked regeneration (ADR-0002, ADR-0003, ADR-0022).

use crate::persist::{ChildState, ContextRecord, TileRecord, Write};

use crate::child::inside;
use crate::generation::{GenerationChange, decode_outline, encode_outline};
use crate::layer::{TILE_LEN, TILE_SIZE, decode, encode};
use crate::undo::{Change, TileChange};
use crate::{
    Command, Error, Kinds, Layer, Map, MapId, MapKindModule, Project, Result, RootPoint, Summary,
};

/// A level-0 tile: (layer, tx, ty).
pub type TileAt = (Layer, u32, u32);

/// What one child map shows on its parent, inside its outline (root space).
#[derive(Debug, Clone, PartialEq)]
pub struct ChildSummary {
    pub map: MapId,
    pub outline: Vec<RootPoint>,
    pub summary: Summary,
}

impl ChildSummary {
    /// Whether `at` is inside the outline, in the outline's space.
    pub fn contains(&self, at: RootPoint) -> bool {
        inside(&self.outline, at)
    }
}

/// The parent context a re-sync preview gives the generator in place of the stored one.
#[derive(Default)]
pub(crate) struct State {
    context: Option<(MapId, Vec<ContextRecord>)>,
}

/// What a re-sync or masked regeneration would write. The preview writes nothing;
/// `Project::apply_plan` writes it as one command.
#[derive(Debug, Clone)]
pub struct RegenPlan {
    pub map: MapId,
    /// Tiles the apply step rewrites.
    pub regenerate: Vec<TileAt>,
    /// Tiles the user edited by hand. They are kept, except ground the map gains.
    pub kept_edited: Vec<TileAt>,
    /// Tiles with locked pixels. Those pixels are kept.
    pub kept_locked: Vec<TileAt>,
    /// Edited tiles on ground the new outline loses. Overwritten only on confirm.
    pub lost_edited: Vec<TileAt>,
    change: ResyncChange,
    lost: Vec<TileChange>,
}

/// The records one re-sync or masked regeneration changes.
#[derive(Debug, Clone)]
pub(crate) struct ResyncChange {
    generation: GenerationChange,
    /// Child state before and after; `None` when only tiles change.
    child: Option<(ChildState, ChildState)>,
}

impl ResyncChange {
    pub(crate) fn tiles(&self) -> &[TileChange] {
        &self.generation.tiles
    }
}

/// Whether `p` is inside the rectangle `m` covers.
fn in_rect(m: &Map, p: RootPoint) -> bool {
    let s = m.settings;
    let (w, h) = (
        s.width as f64 * s.metres_per_pixel,
        s.height as f64 * s.metres_per_pixel,
    );
    p.x >= m.origin.x && p.y >= m.origin.y && p.x < m.origin.x + w && p.y < m.origin.y + h
}

/// Whether tile (`tx`, `ty`) of `parent` overlaps the rectangle `child` covers.
fn tile_overlaps(parent: &Map, tx: u32, ty: u32, child: &Map) -> bool {
    let t = TILE_SIZE as f64 * parent.settings.metres_per_pixel;
    let (x0, y0) = (
        parent.origin.x + tx as f64 * t,
        parent.origin.y + ty as f64 * t,
    );
    let s = child.settings;
    let (w, h) = (
        s.width as f64 * s.metres_per_pixel,
        s.height as f64 * s.metres_per_pixel,
    );
    x0 < child.origin.x + w
        && x0 + t > child.origin.x
        && y0 < child.origin.y + h
        && y0 + t > child.origin.y
}

/// Pixel (x, y) of index `i` in tile (`tx`, `ty`).
fn pixel(tx: u32, ty: u32, i: usize) -> (u32, u32) {
    (
        tx * TILE_SIZE + i as u32 % TILE_SIZE,
        ty * TILE_SIZE + i as u32 / TILE_SIZE,
    )
}

impl Project {
    /// Whether a parent edit changed ground under child `map` since its last re-sync.
    /// Always `false` for a root map.
    pub fn is_out_of_date(&self, map: MapId) -> Result<bool> {
        self.map(map)?;
        self.store.out_of_date(map.0)
    }

    /// Summaries of `map`'s child maps that their kinds define (ADR-0002).
    pub fn child_summaries(&self, kinds: &Kinds, map: MapId) -> Result<Vec<ChildSummary>> {
        self.map(map)?;
        let mut out = vec![];
        for m in self.maps.iter().filter(|m| m.parent == Some(map)) {
            let (Some(k), Some(outline)) = (kinds.get(m.settings.kind), &m.outline) else {
                continue;
            };
            if let Some(summary) = k.summary(self, m.id)? {
                out.push(ChildSummary {
                    map: m.id,
                    outline: outline.clone(),
                    summary,
                });
            }
        }
        Ok(out)
    }

    pub(crate) fn context_override(&self, map: MapId, layer: Layer) -> Option<ContextRecord> {
        let (m, records) = self.resync.context.as_ref()?;
        (*m == map)
            .then(|| records.iter().find(|r| r.layer == layer.as_str()).cloned())
            .flatten()
    }

    /// Flags child maps whose area plus margin has a pixel that `change` changed.
    pub(crate) fn mark_out_of_date(&self, change: &Change) -> Result<()> {
        let tiles = match change {
            Change::Tiles(t) => t.as_slice(),
            Change::Generation(g) => &g.tiles,
            Change::Resync(r) => r.tiles(),
            Change::Entities(_) | Change::Page(_) | Change::Anchor(_) | Change::Deletion(_) => {
                return Ok(());
            }
        };
        for c in tiles.iter().filter(|c| c.key.level == 0) {
            let Some(parent) = self.maps.iter().find(|m| m.id.0 == c.key.map_id) else {
                continue;
            };
            let (tx, ty) = (c.key.tx, c.key.ty);
            let children: Vec<&Map> = self
                .maps
                .iter()
                .filter(|m| m.parent == Some(parent.id) && tile_overlaps(parent, tx, ty, m))
                .collect();
            if children.is_empty() {
                continue;
            }
            let (before, after) = (decode(c.before.as_ref())?, decode(c.after.as_ref())?);
            for child in children {
                if self.store.out_of_date(child.id.0)? {
                    continue;
                }
                let hit = before.iter().zip(&after).enumerate().any(|(i, (b, a))| {
                    let (x, y) = pixel(tx, ty, i);
                    a != b && in_rect(child, parent.pixel_centre(x, y))
                });
                if hit {
                    self.store.write(&[Write::OutOfDate(child.id.0, true)])?;
                }
            }
        }
        Ok(())
    }

    fn kind_of<'k>(&self, kinds: &'k Kinds, map: MapId) -> Result<&'k dyn MapKindModule> {
        kinds
            .get(self.map(map)?.settings.kind)
            .ok_or(Error::InvalidChild("this map kind is not available"))
    }

    /// Previews running `generator` with `seed` on `map` again, only inside `selection`
    /// (root space; `None` is the whole map). Edited tiles and locked pixels are kept
    /// (ADR-0003, ADR-0022). Writes nothing.
    pub fn preview_regeneration(
        &self,
        kinds: &Kinds,
        map: MapId,
        generator: &str,
        seed: u64,
        selection: Option<&[RootPoint]>,
    ) -> Result<RegenPlan> {
        let cmd = self
            .kind_of(kinds, map)?
            .generate(self, map, generator, seed)?;
        self.plan(map, cmd, selection, None, None)
    }

    /// Previews re-syncing child `map` with its parent: a new parent snapshot, then the map's
    /// last generator and seed on unedited, unlocked tiles. `outline` moves the outline
    /// (root space); ground gained is generated, ground lost becomes context. Writes nothing.
    pub fn preview_resync(
        &mut self,
        kinds: &Kinds,
        map: MapId,
        outline: Option<Vec<RootPoint>>,
    ) -> Result<RegenPlan> {
        let m = self.map(map)?.clone();
        let parent = self
            .map(
                m.parent
                    .ok_or(Error::InvalidChild("a root map has no parent"))?,
            )?
            .clone();
        let before = self
            .store
            .child_state(map.0)?
            .ok_or(Error::InvalidChild("the map has no outline"))?;
        let old = m.outline.clone().unwrap_or_default();
        let new = outline.unwrap_or_else(|| old.clone());
        // ponytail: the child's rectangle stays; resize the map when outlines move further.
        if new.len() < 3 || !new.iter().all(|&q| in_rect(&m, q)) {
            return Err(Error::InvalidChild(
                "the outline must stay inside the child map",
            ));
        }
        let s = m.settings;
        let context = self.snapshot(
            self.kind_of(kinds, parent.id)?.layers(),
            &parent,
            m.origin,
            s.width as f64 * s.metres_per_pixel,
            s.height as f64 * s.metres_per_pixel,
        )?;
        let g = self
            .generations(map)?
            .pop()
            .ok_or(Error::InvalidChild("the map was never generated"))?;
        let kind = self.kind_of(kinds, map)?;
        self.resync.context = Some((map, context.clone()));
        let cmd = kind.generate(self, map, &g.generator, g.seed);
        self.resync.context = None;
        let after = ChildState {
            outline: encode_outline(&new),
            context,
            out_of_date: false,
        };
        self.plan(map, cmd?, None, Some((&old, &new)), Some((before, after)))
    }

    fn plan(
        &self,
        map: MapId,
        cmd: Command,
        selection: Option<&[RootPoint]>,
        outlines: Option<(&[RootPoint], &[RootPoint])>,
        child: Option<(ChildState, ChildState)>,
    ) -> Result<RegenPlan> {
        let Command::Generate(mut g) = cmd else {
            return Err(Error::UnknownGenerator("not a generator".into()));
        };
        // ponytail: rasters only; entities stay as they are until a kind needs them regenerated.
        g.entities.clear();
        let m = self.map(map)?;
        let mut gc = self.generation_change(m, &g)?;
        let (old, new) = (outlines.map(|o| o.0), outlines.map(|o| o.1));
        let all = std::mem::take(&mut gc.tiles);
        let mut plan = RegenPlan {
            map,
            regenerate: vec![],
            kept_edited: vec![],
            kept_locked: vec![],
            lost_edited: vec![],
            change: ResyncChange {
                generation: gc,
                child,
            },
            lost: vec![],
        };
        let mut tiles = vec![];
        for c in all {
            let layer = c.key.layer;
            let at = (layer, c.key.tx, c.key.ty);
            let lock = self.store.tile(&m.tile_key(Layer::Lock, at.1, at.2))?;
            let lock = decode(lock.as_ref())?;
            let before = decode(c.before.as_ref())?;
            let generated = decode(c.after.as_ref())?;
            let edited = c.before.as_ref().is_some_and(|r| r.edited);
            // Values without and with the confirm to delete lost ground.
            let (mut keep, mut lose) = (before.clone(), before.clone());
            let (mut locked, mut lost_any, mut owned) = (false, false, false);
            for i in 0..TILE_LEN {
                let (x, y) = pixel(at.1, at.2, i);
                let p = m.pixel_centre(x, y);
                let (was, now) = (
                    old.is_none_or(|o| inside(o, p)),
                    new.is_none_or(|o| inside(o, p)),
                );
                owned |= now;
                if lock[i] > 0.5 {
                    locked = true;
                    continue;
                }
                if selection.is_some_and(|s| !inside(s, p)) {
                    continue;
                }
                if !edited || (!was && now) {
                    keep[i] = generated[i];
                    lose[i] = generated[i];
                } else if was && !now {
                    lose[i] = generated[i];
                    lost_any = true;
                }
            }
            if locked {
                plan.kept_locked.push(at);
            }
            if edited {
                plan.kept_edited.push(at);
            }
            let change = |values: &[f32], edited| TileChange {
                key: c.key.clone(),
                before: c.before.clone(),
                after: Some(TileRecord {
                    data: encode(values),
                    edited,
                }),
            };
            if keep != before {
                plan.regenerate.push(at);
                tiles.push(change(&keep, edited));
            }
            if edited && lost_any {
                plan.lost_edited.push(at);
                plan.lost.push(change(&lose, owned));
            }
        }
        plan.change.generation.tiles = tiles;
        Ok(plan)
    }

    /// Writes a previewed plan as one undoable command on its map. Edited tiles on lost ground
    /// are overwritten only when `confirm_lost` is true. Refuses a plan the map has changed under.
    pub fn apply_plan(&mut self, plan: RegenPlan, confirm_lost: bool) -> Result<()> {
        let RegenPlan {
            map,
            mut change,
            lost,
            ..
        } = plan;
        self.map(map)?;
        if confirm_lost {
            let tiles = &mut change.generation.tiles;
            tiles.retain(|c| !lost.iter().any(|l| l.key == c.key));
            tiles.extend(lost);
        }
        for c in &change.generation.tiles {
            if self.store.tile(&c.key)? != c.before {
                return Err(Error::StalePlan);
            }
        }
        if change.generation.record.id != self.store.next_generation_id()? {
            return Err(Error::StalePlan);
        }
        if let Some((before, _)) = &change.child
            && self.store.child_state(map.0)?.as_ref() != Some(before)
        {
            return Err(Error::StalePlan);
        }
        self.record(map, Change::Resync(change))
    }

    pub(crate) fn write_resync_change(&mut self, c: &ResyncChange, forward: bool) -> Result<()> {
        let mut rows = c.generation.rows(forward);
        let id = c.generation.record.map_id;
        if let Some((before, after)) = &c.child {
            let s = if forward { after } else { before };
            rows.push(Write::ChildState(id, s.clone()));
        }
        self.write_rows(&rows)?;
        self.note_generation(&c.generation, forward);
        if let Some((before, after)) = &c.child {
            let s = if forward { after } else { before };
            let outline = decode_outline(&s.outline)?;
            if let Some(m) = self.maps.iter_mut().find(|m| m.id.0 == id) {
                m.outline = Some(outline);
            }
        }
        Ok(())
    }
}
