//! Summaries, out of date, re-sync and masked regeneration (ADR-0002, ADR-0003, ADR-0022).

use crate::persist::{ChildState, ContextRecord, TileKey, TileRecord, Write};

use crate::child::inside;
use crate::generation::{GenerationChange, decode_outline, encode_outline};
use crate::layer::{TILE_LEN, TILE_SIZE, decode, encode};
use crate::undo::{Change, TileChange};
use crate::{
    CHILD_MARGIN, Command, Error, GenRun, Layer, MAX_CHILD_SIZE, Map, MapId, Project, Result,
    RootPoint, Summary,
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
    /// Tiles with ground the new outline loses. That ground stays as read-only context; its
    /// data is cleared only on confirm.
    pub lost_ground: Vec<TileAt>,
    /// The edited tiles among `lost_ground`.
    pub lost_edited: Vec<TileAt>,
    /// The map's new rectangle when the outline grows past it: (origin, width, height).
    pub resize: Option<(RootPoint, u32, u32)>,
    change: ResyncChange,
    lost: Vec<TileChange>,
}

/// A map's rectangle: root-space origin and size in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    origin: RootPoint,
    width: u32,
    height: u32,
}

impl Rect {
    fn of(m: &Map) -> Rect {
        Rect {
            origin: m.origin,
            width: m.settings.width,
            height: m.settings.height,
        }
    }

    fn write(&self, id: MapId) -> Write {
        Write::MapRect {
            id: id.0,
            origin_x: self.origin.x,
            origin_y: self.origin.y,
            width: self.width,
            height: self.height,
        }
    }
}

/// A re-sync that grows the map: its rectangle before and after, and the tile shift.
#[derive(Debug, Clone)]
struct Resize {
    before: Rect,
    after: Rect,
    shift: (u32, u32),
}

/// The records one re-sync or masked regeneration changes.
#[derive(Debug, Clone)]
pub(crate) struct ResyncChange {
    generation: GenerationChange,
    /// Child state before and after; `None` when only tiles change.
    child: Option<(ChildState, ChildState)>,
    resize: Option<Resize>,
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
    pub fn child_summaries(&self, map: MapId) -> Result<Vec<ChildSummary>> {
        self.map(map)?;
        let mut out = vec![];
        for m in self.maps.iter().filter(|m| m.parent == Some(map)) {
            let (Some(k), Some(outline)) = (self.kinds.get(m.settings.kind), &m.outline) else {
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
            Change::Entities(_)
            | Change::Page(_)
            | Change::Anchor(_)
            | Change::Deletion(_)
            | Change::Objects(_)
            | Change::NewMap(_) => {
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

    /// Previews `run` on `map` again, only inside `selection` (root space; `None` is the
    /// whole map). Edited tiles and locked pixels are kept (ADR-0003, ADR-0022). Writes nothing.
    pub fn preview_regeneration(
        &self,
        map: MapId,
        run: &GenRun,
        selection: Option<&[RootPoint]>,
    ) -> Result<RegenPlan> {
        let cmd = self.kind_of(map)?.generate(self, map, run)?;
        let m = self.map(map)?.clone();
        self.plan(&m, cmd, selection, None, None)
    }

    /// A stored level-0 tile at `key` of a map whose tiles move by `shift` tiles first.
    pub(crate) fn tile_at(&self, key: &TileKey, shift: (u32, u32)) -> Result<Option<TileRecord>> {
        if key.tx < shift.0 || key.ty < shift.1 {
            return Ok(None);
        }
        let old = TileKey {
            tx: key.tx - shift.0,
            ty: key.ty - shift.1,
            ..key.clone()
        };
        self.store.tile(&old)
    }

    /// Previews re-syncing child `map` with its parent: a new parent snapshot, then the map's
    /// last generator run on unedited, unlocked tiles. `outline` moves the outline (root
    /// space): gained ground is generated; lost ground stays as read-only context and is
    /// cleared only on confirm. An outline past the map's rectangle grows the map. Writes nothing.
    pub fn preview_resync(
        &mut self,
        map: MapId,
        outline: Option<Vec<RootPoint>>,
    ) -> Result<RegenPlan> {
        let mut m = self.map(map)?.clone();
        let parent = self.map(m.parent.ok_or(Error::RootMap(map))?)?.clone();
        let before = self.store.child_state(map.0)?.ok_or(Error::RootMap(map))?;
        let old = m.outline.clone().unwrap_or_default();
        let new = outline.unwrap_or_else(|| old.clone());
        if new.len() < 3 {
            return Err(Error::InvalidOutline);
        }
        let resize = grow(&m, &new, CHILD_MARGIN * parent.settings.metres_per_pixel)?;
        let old_map = m.clone();
        if let Some(r) = &resize {
            (m.origin, m.settings.width, m.settings.height) =
                (r.after.origin, r.after.width, r.after.height);
        }
        let s = m.settings;
        let decl = self
            .kind_of(parent.id)?
            .child()
            .ok_or(Error::NoChildMaps(parent.settings.kind))?;
        let context = self.snapshot(
            decl.context,
            &parent,
            m.origin,
            s.width as f64 * s.metres_per_pixel,
            s.height as f64 * s.metres_per_pixel,
        )?;
        let g = self
            .generations(map)?
            .pop()
            .ok_or(Error::NeverGenerated(map))?;
        let kinds = self.kinds.clone();
        let kind = kinds
            .get(m.settings.kind)
            .ok_or_else(|| Error::UnknownMapKind(m.settings.kind.as_str().into()))?;
        // The generator sees the grown map and the new snapshot; both are put back after.
        self.resync.context = Some((map, context.clone()));
        self.set_map(m.clone());
        let cmd = kind.generate(self, map, &g.run);
        self.set_map(old_map.clone());
        self.resync.context = None;
        let after = ChildState {
            outline: encode_outline(&new),
            context,
            out_of_date: false,
        };
        let outlines = (old.as_slice(), new.as_slice(), &old_map);
        let mut plan = self.plan(&m, cmd?, None, Some(outlines), resize)?;
        plan.change.child = Some((before, after));
        Ok(plan)
    }

    /// Replaces the in-memory map with the same id.
    fn set_map(&mut self, m: Map) {
        if let Some(x) = self.maps.iter_mut().find(|x| x.id == m.id) {
            *x = m;
        }
    }

    /// The tiles `cmd` would rewrite on `m`. `outlines` is (old, new, map before a resize)
    /// for a re-sync that moves the outline.
    fn plan(
        &self,
        m: &Map,
        cmd: Command,
        selection: Option<&[RootPoint]>,
        outlines: Option<(&[RootPoint], &[RootPoint], &Map)>,
        resize: Option<Resize>,
    ) -> Result<RegenPlan> {
        let Command::Generate(mut g) = cmd else {
            return Err(Error::UnknownGenerator("not a generator".into()));
        };
        // ponytail: rasters only; entities stay as they are until a kind needs them regenerated.
        g.entities.clear();
        let shift = resize.as_ref().map_or((0, 0), |r| r.shift);
        let mut gc = self.generation_change(m, &g, shift)?;
        let all = std::mem::take(&mut gc.tiles);
        let mut plan = RegenPlan {
            map: m.id,
            regenerate: vec![],
            kept_edited: vec![],
            kept_locked: vec![],
            lost_ground: vec![],
            lost_edited: vec![],
            resize: resize
                .as_ref()
                .map(|r| (r.after.origin, r.after.width, r.after.height)),
            change: ResyncChange {
                generation: gc,
                child: None,
                resize,
            },
            lost: vec![],
        };
        let mut tiles = vec![];
        for c in all {
            let layer = c.key.layer;
            let at = (layer, c.key.tx, c.key.ty);
            let lock = self.tile_at(&m.tile_key(Layer::LOCK, at.1, at.2), shift)?;
            let lock = decode(lock.as_ref())?;
            let before = decode(c.before.as_ref())?;
            let generated = decode(c.after.as_ref())?;
            let edited = c.before.as_ref().is_some_and(|r| r.edited);
            // Values without and with the confirm to clear lost ground.
            let (mut keep, mut lose) = (before.clone(), before.clone());
            let (mut locked, mut lost_any, mut owned) = (false, false, false);
            for i in 0..TILE_LEN {
                let (x, y) = pixel(at.1, at.2, i);
                let p = m.pixel_centre(x, y);
                let (was, now) = match outlines {
                    Some((old, new, before)) => {
                        (in_rect(before, p) && inside(old, p), inside(new, p))
                    }
                    None => (true, true),
                };
                owned |= now;
                if lock[i] > 0.5 {
                    locked = true;
                    continue;
                }
                if selection.is_some_and(|s| !inside(s, p)) {
                    continue;
                }
                if was && !now {
                    lose[i] = 0.0;
                    lost_any = true;
                } else if !edited || (now && !was) {
                    keep[i] = generated[i];
                    lose[i] = generated[i];
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
            if lost_any {
                plan.lost_ground.push(at);
                if edited {
                    plan.lost_edited.push(at);
                }
                plan.lost.push(change(&lose, edited && owned));
            }
        }
        plan.change.generation.tiles = tiles;
        Ok(plan)
    }

    /// Writes a previewed plan as one undoable command on its map. Lost ground is cleared only
    /// when `confirm_lost` is true. Refuses a plan the map has changed under.
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
        let shift = change.resize.as_ref().map_or((0, 0), |r| r.shift);
        for c in &change.generation.tiles {
            if self.tile_at(&c.key, shift)? != c.before {
                return Err(Error::StalePlan);
            }
        }
        if let Some(r) = &change.resize
            && Rect::of(self.map(map)?) != r.before
        {
            return Err(Error::StalePlan);
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
        let id = c.generation.record.map_id;
        // Forward: move the old tiles, then write; back: write the old values, then move back.
        let mut rows = vec![];
        let shift_rows = |r: &Resize, forward: bool| {
            let (dx, dy) = (r.shift.0 as i64, r.shift.1 as i64);
            let (dx, dy, rect) = if forward {
                (dx, dy, r.after)
            } else {
                (-dx, -dy, r.before)
            };
            let mut v = vec![rect.write(MapId(id))];
            if (dx, dy) != (0, 0) {
                v.push(Write::ShiftTiles { map_id: id, dx, dy });
            }
            v
        };
        if forward && let Some(r) = &c.resize {
            rows.extend(shift_rows(r, true));
        }
        rows.extend(c.generation.rows(forward));
        if !forward && let Some(r) = &c.resize {
            rows.extend(shift_rows(r, false));
        }
        if let Some((before, after)) = &c.child {
            let s = if forward { after } else { before };
            rows.push(Write::ChildState(id, s.clone()));
        }
        self.write_rows(&rows)?;
        self.note_generation(&c.generation, forward);
        if let Some(r) = &c.resize {
            let rect = if forward { r.after } else { r.before };
            if let Some(m) = self.maps.iter_mut().find(|m| m.id.0 == id) {
                (m.origin, m.settings.width, m.settings.height) =
                    (rect.origin, rect.width, rect.height);
            }
            self.after_resize(MapId(id))?;
        }
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

/// The rectangle a child map needs for `outline` plus `margin` metres: its own rectangle grown
/// by whole tiles to the left and top, so stored tiles only move by whole tiles. `None` when
/// the outline fits.
fn grow(m: &Map, outline: &[RootPoint], margin: f64) -> Result<Option<Resize>> {
    let mpp = m.settings.metres_per_pixel;
    let tile = TILE_SIZE as f64 * mpp;
    let before = Rect::of(m);
    let (mut lo, mut hi) = (
        m.origin,
        RootPoint {
            x: m.origin.x + before.width as f64 * mpp,
            y: m.origin.y + before.height as f64 * mpp,
        },
    );
    if outline.iter().all(|&q| in_rect(m, q)) {
        return Ok(None);
    }
    for q in outline {
        (lo.x, lo.y) = (lo.x.min(q.x - margin), lo.y.min(q.y - margin));
        (hi.x, hi.y) = (hi.x.max(q.x + margin), hi.y.max(q.y + margin));
    }
    let kx = ((m.origin.x - lo.x) / tile).ceil().max(0.0);
    let ky = ((m.origin.y - lo.y) / tile).ceil().max(0.0);
    let origin = RootPoint {
        x: m.origin.x - kx * tile,
        y: m.origin.y - ky * tile,
    };
    let (w, h) = (
        ((hi.x - origin.x) / mpp).ceil(),
        ((hi.y - origin.y) / mpp).ceil(),
    );
    if w > MAX_CHILD_SIZE || h > MAX_CHILD_SIZE {
        return Err(Error::ChildTooLarge);
    }
    Ok(Some(Resize {
        before,
        after: Rect {
            origin,
            width: w as u32,
            height: h as u32,
        },
        shift: (kx as u32, ky as u32),
    }))
}
