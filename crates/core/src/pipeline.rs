//! Stage pipeline (ADR-0018): derived layers rebuild on workers after each edit (ADR-0014).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::persist::{TileKey, TileOpener, TileRecord, TileStore};

use crate::beyond::Neighbour;
use crate::jobs::{Cancelled, Jobs};
use crate::layer::{Layer, TILE_SIZE, decode, encode};
use crate::pyramid::{Level, levels_above};
use crate::undo::Change;
use crate::{Error, MapId, Project, Result};

// ponytail: entity and page changes feed no stage yet; map them to layers when a stage reads entities.
fn changed_layers(change: &Change) -> Vec<Layer> {
    match change {
        Change::Tiles(t) => t.iter().map(|c| c.key.layer).collect(),
        Change::Generation(g) => g.tiles.iter().map(|c| c.key.layer).collect(),
        Change::Resync(r) => r.tiles().iter().map(|c| c.key.layer).collect(),
        Change::Entities(_)
        | Change::Page(_)
        | Change::Anchor(_)
        | Change::Deletion(_)
        | Change::Objects(_)
        | Change::NewMap(_) => vec![],
    }
}

/// Decoded level-0 tiles of a layer by (tx, ty); a missing tile is all zeros.
pub type Raster = BTreeMap<(u32, u32), Vec<f32>>;

/// What a stage reads: the map's size and the rasters of the layers it declared.
pub struct StageInput<'a> {
    pub width: u32,
    pub height: u32,
    pub metres_per_pixel: f64,
    /// Global pixel of map pixel (0, 0): the root-space origin over `metres_per_pixel`.
    pub origin: (i64, i64),
    /// The project seed (ADR-0021).
    pub seed: u64,
    /// The map kind's option values.
    pub options: crate::OptionValues,
    /// Threads for `par_map`.
    pub threads: usize,
    /// True once a newer rebuild replaced this one; the stage then returns `None`.
    pub cancelled: &'a (dyn Fn() -> bool + Sync),
    pub layers: HashMap<Layer, Raster>,
    /// The ground the map owns (ADR-0019); `None` for a root map.
    pub(crate) outline: Option<Vec<crate::RootPoint>>,
    /// Maps of the same kind and scale within reach; `StageInput::padded` reads them.
    pub(crate) neighbours: Vec<Neighbour>,
}

/// A pipeline stage a map kind declares (ADR-0018).
pub struct Stage {
    pub reads: &'static [Layer],
    pub writes: &'static [Layer],
    /// One raster per `writes` entry; `None` when cancelled.
    pub run: fn(&StageInput) -> Option<Vec<Raster>>,
    /// Pixels it reads past the map edge, at a metres per pixel; edits of maps that near rerun it.
    pub reach: fn(f64) -> usize,
}

/// The largest reach of `stages` at `mpp`.
fn reach(stages: &[Stage], mpp: f64) -> i64 {
    stages.iter().map(|s| (s.reach)(mpp)).max().unwrap_or(0) as i64
}

pub(crate) fn writes(stages: &[Stage], layer: Layer) -> bool {
    stages.iter().any(|s| s.writes.contains(&layer))
}

fn decls(stages: &[Stage]) -> Vec<(&'static [Layer], &'static [Layer])> {
    stages.iter().map(|s| (s.reads, s.writes)).collect()
}

/// Stage indices so that each stage runs after the stages that write what it reads.
fn order<L: PartialEq>(decls: &[(&[L], &[L])]) -> Vec<usize> {
    let feeds = |i: usize, j: usize| i != j && decls[j].0.iter().any(|l| decls[i].1.contains(l));
    let mut out: Vec<usize> = vec![];
    while out.len() < decls.len() {
        let next = (0..decls.len())
            .find(|&j| {
                !out.contains(&j) && (0..decls.len()).all(|i| out.contains(&i) || !feeds(i, j))
            })
            .expect("stage declarations form a cycle");
        out.push(next);
    }
    out
}

/// Stages that must rerun when `changed` layers change, transitively.
fn dirty<L: PartialEq + Clone>(decls: &[(&[L], &[L])], changed: &[L]) -> Vec<bool> {
    let mut changed = changed.to_vec();
    let mut out = vec![false; decls.len()];
    for i in order(decls) {
        if decls[i].0.iter().any(|l| changed.contains(l)) {
            out[i] = true;
            changed.extend(decls[i].1.iter().cloned());
        }
    }
    out
}

/// `f` over `items` on scoped threads, in item order; `None` when `cancelled` became true.
pub fn par_map<T: Sync, R: Send>(
    threads: usize,
    items: &[T],
    cancelled: Cancelled,
    f: impl Fn(&T) -> R + Sync,
) -> Option<Vec<R>> {
    let next = AtomicUsize::new(0);
    let mut out: Vec<(usize, R)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads.max(1))
            .map(|_| {
                s.spawn(|| {
                    crate::jobs::background_priority();
                    let mut v = vec![];
                    loop {
                        let i = next.fetch_add(1, Relaxed);
                        if i >= items.len() || cancelled() {
                            break v;
                        }
                        v.push((i, f(&items[i])));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|w| w.join().unwrap())
            .collect()
    });
    if cancelled() {
        return None;
    }
    out.sort_by_key(|p| p.0);
    Some(out.into_iter().map(|p| p.1).collect())
}

struct MapJob {
    id: i64,
    stages: &'static [Stage],
    width: u32,
    height: u32,
    metres_per_pixel: f64,
    origin: (i64, i64),
    seed: u64,
    options: crate::OptionValues,
    open: TileOpener,
    outline: Option<Vec<crate::RootPoint>>,
    /// Each map in reach by id, its layers still empty.
    neighbours: Vec<(i64, Neighbour)>,
    dirty: Vec<bool>,
    threads: usize,
    /// Top pyramid level; derived layers are written with all levels.
    top: u32,
}

/// Per computed tile: digest and zstd bytes, `None` when all zeros.
type Encoded = Vec<((u32, u32), Option<(u64, Vec<u8>)>)>;
/// Per written layer: level-0 tiles, and levels 1..=top decoded for the pyramid.
type Output = (
    Box<dyn TileStore>,
    i64,
    usize,
    Vec<Layer>,
    Vec<(Layer, Encoded, Vec<Level>)>,
);

/// Digests of the derived tiles in the working copy; `None` until loaded.
type Committed = Mutex<Option<HashMap<(Layer, (u32, u32)), u64>>>;

// ponytail: 64-bit SipHash; a collision skips one tile write. Use BLAKE3 if that ever matters.
fn digest(data: &[u8]) -> u64 {
    let mut h = DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

fn tile_key(map: i64, layer: Layer, (tx, ty): (u32, u32)) -> TileKey {
    level_key(map, layer, 0, (tx, ty))
}

fn level_key(map: i64, layer: Layer, level: u32, (tx, ty): (u32, u32)) -> TileKey {
    TileKey {
        map_id: map,
        layer,
        level,
        tx,
        ty,
    }
}

/// Runs the dirty stages in order on its own connection to the working copy.
fn rebuild(job: MapJob, cancelled: Cancelled) -> Option<Result<Output>> {
    let reader = match (job.open)() {
        Ok(r) => r,
        Err(e) => return Some(Err(e)),
    };
    let mut cx = StageInput {
        width: job.width,
        height: job.height,
        metres_per_pixel: job.metres_per_pixel,
        origin: job.origin,
        seed: job.seed,
        options: job.options,
        threads: job.threads,
        cancelled,
        layers: HashMap::new(),
        outline: job.outline,
        neighbours: vec![],
    };
    let far: BTreeSet<Layer> = job
        .stages
        .iter()
        .filter(|s| (s.reach)(job.metres_per_pixel) > 0)
        .flat_map(|s| s.reads.iter().copied())
        .collect();
    let ts = TILE_SIZE as i64;
    let tiles = |a: i64, b: i64, o: i64| (a - o).div_euclid(ts)..=(b - 1 - o).div_euclid(ts);
    for (id, mut n) in job.neighbours {
        let (g, o) = (n.given, n.origin);
        for &l in &far {
            let mut raster = Raster::new();
            for ty in tiles(g[1], g[3], o.1) {
                for tx in tiles(g[0], g[2], o.0) {
                    if cancelled() {
                        return None;
                    }
                    let t = (tx as u32, ty as u32);
                    match reader.tile(&tile_key(id, l, t)) {
                        Ok(None) => {}
                        Ok(Some(r)) => match decode(Some(&r)) {
                            Ok(v) => drop(raster.insert(t, v)),
                            Err(e) => return Some(Err(e)),
                        },
                        Err(e) => return Some(Err(e)),
                    }
                }
            }
            n.layers.insert(l, raster);
        }
        cx.neighbours.push(n);
    }
    let mut written = vec![];
    for i in order(&decls(job.stages))
        .into_iter()
        .filter(|&i| job.dirty[i])
    {
        let stage = &job.stages[i];
        for &l in stage.reads {
            if cx.layers.contains_key(&l) {
                continue;
            }
            let tiles = match reader.layer_tiles_until(job.id, l, cancelled) {
                Ok(t) => t?,
                Err(e) => return Some(Err(e)),
            };
            let values = par_map(job.threads, &tiles, cancelled, |(_, r)| decode(Some(r)))?;
            let raster = tiles.iter().map(|(t, _)| *t).zip(values);
            match raster.map(|(t, v)| v.map(|v| (t, v))).collect() {
                Ok(r) => cx.layers.insert(l, r),
                Err(e) => return Some(Err(e)),
            };
        }
        for (&l, r) in stage.writes.iter().zip((stage.run)(&cx)?) {
            cx.layers.insert(l, r);
            written.push(l);
        }
    }
    let mut out = vec![];
    for l in written {
        let tiles: Vec<_> = cx.layers[&l].iter().collect();
        let encoded = par_map(job.threads, &tiles, cancelled, |(_, v)| {
            v.iter().any(|x| *x != 0.0).then(|| {
                let data = encode(v);
                (digest(&data), data)
            })
        })?;
        let above = levels_above(&cx.layers[&l], job.top);
        out.push((
            l,
            tiles.iter().map(|(t, _)| **t).zip(encoded).collect(),
            above,
        ));
    }
    let derived = job.stages.iter().flat_map(|s| s.writes).copied().collect();
    Some(Ok((reader, job.id, job.threads, derived, out)))
}

/// Writes only changed tiles; runs under the jobs lock, so the digests match the working copy.
fn commit(committed: &Committed, out: Result<Output>) -> Result<()> {
    let (conn, map, threads, derived, layers) = out?;
    let mut committed = committed.lock().unwrap();
    let mut known = match committed.take() {
        Some(k) => k,
        None => {
            let mut k = HashMap::new();
            for l in derived {
                let tiles = conn
                    .layer_tiles_until(map, l, &|| false)?
                    .unwrap_or_default();
                for (t, r) in tiles {
                    k.insert((l, t), digest(&r.data));
                }
            }
            k
        }
    };
    let mut writes = vec![];
    for (l, tiles, above) in layers {
        let first = writes.len();
        let mut stale: BTreeSet<(u32, u32)> =
            known.keys().filter(|k| k.0 == l).map(|k| k.1).collect();
        for (t, enc) in tiles {
            let Some((d, data)) = enc else { continue };
            stale.remove(&t);
            if known.insert((l, t), d) != Some(d) {
                let rec = TileRecord {
                    data,
                    edited: false,
                };
                writes.push((tile_key(map, l, t), Some(rec)));
            }
        }
        for t in stale {
            known.remove(&(l, t));
            writes.push((tile_key(map, l, t), None));
        }
        // Pyramid tiles above the changed level-0 tiles only.
        let changed: Vec<(u32, u32)> = writes[first..].iter().map(|(k, _)| (k.tx, k.ty)).collect();
        let mut upper = BTreeSet::new();
        for (i, _) in above.iter().enumerate() {
            let shift = i as u32 + 1;
            upper.extend(
                changed
                    .iter()
                    .map(|&(x, y)| (shift, (x >> shift, y >> shift))),
            );
        }
        let upper: Vec<_> = upper.into_iter().collect();
        let encoded = par_map(threads, &upper, &|| false, |&(level, t)| {
            let v = above[level as usize - 1].get(&t)?;
            v.iter().any(|x| *x != 0.0).then(|| encode(v))
        })
        .expect("never cancelled");
        for ((level, t), data) in upper.into_iter().zip(encoded) {
            let rec = data.map(|data| TileRecord {
                data,
                edited: false,
            });
            writes.push((level_key(map, l, level, t), rec));
        }
    }
    crate::jobs::holding_write_lock(|| conn.write_tiles(&writes))?;
    *committed = Some(known);
    Ok(())
}

#[derive(Default)]
struct MapState {
    /// Stages to rerun; cleared when the latest job has committed.
    dirty: Vec<bool>,
    jobs: Jobs<Result<()>>,
    committed: Arc<Committed>,
}

pub(crate) struct Pipeline {
    threads: usize,
    maps: HashMap<MapId, MapState>,
}

impl Default for Pipeline {
    fn default() -> Self {
        Pipeline {
            threads: crate::cpus(),
            maps: HashMap::new(),
        }
    }
}

impl Project {
    /// Worker threads per rebuild; at least 1. The default is the CPU count.
    pub fn set_worker_threads(&mut self, n: usize) {
        self.pipeline.threads = n.max(1);
    }

    /// Starts a full rebuild of all derived layers of `map` in the background.
    pub fn rebuild_derived(&mut self, map: MapId) -> Result<()> {
        let n = self.kind_of(map)?.stages().len();
        self.request(map, vec![true; n]);
        Ok(())
    }

    /// Collects finished rebuilds without blocking; `true` if one finished. Errors surface here.
    pub fn poll_jobs(&mut self) -> Result<bool> {
        let mut any = false;
        for st in self.pipeline.maps.values_mut() {
            if let Some(res) = st.jobs.take() {
                res?;
                st.dirty.fill(false);
                any = true;
            }
        }
        if any {
            self.forget_derived_tiles();
            self.match_anchors()?;
        }
        Ok(any)
    }

    /// Blocks until every requested rebuild has finished.
    pub fn wait_for_jobs(&mut self) -> Result<()> {
        self.flush_writes()?;
        let mut any = false;
        for st in self.pipeline.maps.values_mut() {
            if let Some(res) = st.jobs.wait() {
                res?;
                st.dirty.fill(false);
                any = true;
            }
        }
        if any {
            self.forget_derived_tiles();
            self.match_anchors()?;
        }
        Ok(())
    }

    /// The option values of map kind `kind` in this project.
    pub fn kind_options(&self, kind: crate::MapKind) -> Result<crate::OptionValues> {
        Ok(crate::OptionValues(self.store.kind_options(kind.as_str())?))
    }

    /// Rebuilds the kind's derived layers; a project setting, not an edit, so no undo step.
    pub fn set_kind_option(&mut self, kind: crate::MapKind, id: &str, value: f64) -> Result<()> {
        self.flush_writes()?;
        self.store.write(&[crate::persist::Write::KindOption {
            kind: kind.as_str().into(),
            id: id.into(),
            value,
        }])?;
        let maps: Vec<MapId> = self
            .maps()
            .iter()
            .filter(|m| m.settings.kind == kind)
            .map(|m| m.id)
            .collect();
        for m in maps {
            self.rebuild_derived(m)?;
        }
        Ok(())
    }

    /// True while a stroke is unwritten or a rebuild is not collected by `poll_jobs`.
    pub fn derived_pending(&self, map: MapId) -> bool {
        self.strokes.unwritten(map)
            || self
                .pipeline
                .maps
                .get(&map)
                .is_some_and(|st| st.dirty.contains(&true))
    }

    pub(crate) fn refuse_derived(&self, map: MapId, change: &Change) -> Result<()> {
        let stages = self.kind_of(map)?.stages();
        match changed_layers(change)
            .into_iter()
            .find(|l| writes(stages, *l))
        {
            Some(l) => Err(Error::DerivedLayer(l)),
            None => Ok(()),
        }
    }

    /// Marks stages downstream of the written layers dirty and starts a rebuild.
    pub(crate) fn after_write(&mut self, map: MapId, change: &Change) {
        self.layers_written(map, &changed_layers(change));
    }

    pub(crate) fn layers_written(&mut self, map: MapId, layers: &[Layer]) {
        let (Ok(kind), Ok(m)) = (self.kind_of(map), self.map(map)) else {
            return;
        };
        let stages = kind.stages();
        let mpp = m.settings.metres_per_pixel;
        let dirty = dirty(&decls(stages), layers);
        // Neighbours rerun only the stages that read past their edge, and the stages fed by them.
        let far: Vec<bool> = dirty
            .iter()
            .zip(stages)
            .map(|(&d, s)| d && (s.reach)(mpp) > 0)
            .collect();
        let fed: Vec<Layer> = (stages.iter().zip(&far))
            .filter(|p| *p.1)
            .flat_map(|p| p.0.writes.iter().copied())
            .collect();
        let down = self::dirty(&decls(stages), &fed);
        let near: Vec<bool> = far.iter().zip(down).map(|(&a, b)| a || b).collect();
        let around: Vec<MapId> = self.neighbours(map).iter().map(|(n, _)| n.id).collect();
        self.request(map, dirty);
        // ponytail: reruns the whole neighbour; pass the tiles in reach if edits near a seam lag.
        for n in around {
            self.request(n, near.clone());
        }
    }

    /// Maps of `map`'s kind and scale in its tree, with their global pixels in reach.
    fn neighbours(&self, map: MapId) -> Vec<(&crate::Map, [i64; 4])> {
        let (Ok(m), Ok(kind)) = (self.map(map), self.kind_of(map)) else {
            return vec![];
        };
        let s = m.settings;
        let r = reach(kind.stages(), s.metres_per_pixel);
        if r == 0 {
            return vec![];
        }
        let rect = |m: &crate::Map| {
            let o = global_origin(m);
            let s = m.settings;
            [o.0, o.1, o.0 + s.width as i64, o.1 + s.height as i64]
        };
        let own = rect(m);
        let root = |m: &crate::Map| {
            let mut id = m.id;
            while let Some(p) = self.map(id).ok().and_then(|m| m.parent) {
                id = p;
            }
            id
        };
        // Root maps are separate worlds; child maps neighbour only in the same tree.
        if m.parent.is_none() {
            return vec![];
        }
        self.maps()
            .iter()
            .filter(|n| {
                n.id != map
                    && n.parent.is_some()
                    && root(n) == root(m)
                    && n.settings.kind == s.kind
                    && n.settings.metres_per_pixel == s.metres_per_pixel
            })
            .filter_map(|n| {
                let b = rect(n);
                let g = [
                    b[0].max(own[0] - r),
                    b[1].max(own[1] - r),
                    b[2].min(own[2] + r),
                    b[3].min(own[3] + r),
                ];
                (g[0] < g[2] && g[1] < g[3]).then_some((n, g))
            })
            .collect()
    }

    fn request(&mut self, map: MapId, dirty: Vec<bool>) {
        if !dirty.contains(&true) {
            return;
        }
        let (Ok(m), Ok(kind)) = (self.map(map), self.kind_of(map)) else {
            return;
        };
        let s = m.settings;
        let origin = global_origin(m);
        let outline = m.outline.clone();
        let neighbours = self
            .neighbours(map)
            .into_iter()
            .map(|(n, given)| {
                let origin = global_origin(n);
                let outline = n.outline.clone();
                let layers = HashMap::new();
                (
                    n.id.0,
                    Neighbour {
                        origin,
                        given,
                        outline,
                        layers,
                    },
                )
            })
            .collect();
        let seed = self.store.seed().unwrap_or_default();
        let options = self.kind_options(s.kind).unwrap_or_default();
        let stages = kind.stages();
        let open = self.tile_opener();
        let st = self.pipeline.maps.entry(map).or_default();
        st.dirty.resize(stages.len(), false);
        for (d, n) in st.dirty.iter_mut().zip(dirty) {
            *d |= n;
        }
        let job = MapJob {
            id: map.0,
            stages,
            width: s.width,
            height: s.height,
            metres_per_pixel: s.metres_per_pixel,
            origin,
            seed,
            options,
            open,
            outline,
            neighbours,
            dirty: st.dirty.clone(),
            threads: self.pipeline.threads,
            top: crate::pyramid::top_level(&s),
        };
        let committed = Arc::clone(&st.committed);
        st.jobs.submit(
            move |cancelled| rebuild(job, cancelled),
            move |out| commit(&committed, out),
        );
    }
}

/// Global pixel of `m`'s pixel (0, 0).
fn global_origin(m: &crate::Map) -> (i64, i64) {
    let mpp = m.settings.metres_per_pixel;
    (
        (m.origin.x / mpp).round() as i64,
        (m.origin.y / mpp).round() as i64,
    )
}

impl Pipeline {
    /// Cancels and joins the jobs of `map` and drops what it knows of its derived tiles.
    pub(crate) fn forget(&mut self, map: MapId) {
        self.maps.remove(&map);
    }

    /// Cancels and joins all jobs.
    pub(crate) fn shutdown(&mut self) {
        self.maps.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_run_after_the_stages_that_feed_them() {
        let d: Vec<(&[&str], &[&str])> = vec![
            (&["b"], &["c"]),
            (&["a"], &["b"]),
            (&["height"], &["a"]),
            (&["height"], &["x"]),
        ];
        assert_eq!(order(&d), [2, 1, 0, 3]);
        assert_eq!(dirty(&d, &["a"]), [true, true, false, false]);
        assert_eq!(dirty(&d, &["height"]), [true; 4]);
        assert_eq!(dirty(&d, &["c"]), [false; 4]);
    }

    #[test]
    #[should_panic(expected = "cycle")]
    fn a_cycle_is_refused() {
        let d: Vec<(&[&str], &[&str])> = vec![(&["a"], &["b"]), (&["b"], &["a"])];
        order(&d);
    }

    #[test]
    fn par_map_keeps_item_order() {
        let items: Vec<u32> = (0..100).collect();
        let out = par_map(4, &items, &|| false, |x| x * 2).unwrap();
        assert_eq!(out, items.iter().map(|x| x * 2).collect::<Vec<_>>());
    }
}
