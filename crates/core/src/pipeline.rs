//! Stage pipeline (ADR-0018): derived layers rebuild on workers after each edit (ADR-0014).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};

use crate::persist::{TileKey, TileOpener, TileRecord, TileStore};

use crate::jobs::{Cancelled, Jobs};
use crate::layer::{Layer, decode, encode};
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
    /// Threads for `par_map`.
    pub threads: usize,
    /// True once a newer rebuild replaced this one; the stage then returns `None`.
    pub cancelled: &'a (dyn Fn() -> bool + Sync),
    pub layers: HashMap<Layer, Raster>,
}

/// A pipeline stage a map kind declares (ADR-0018).
pub struct Stage {
    pub reads: &'static [Layer],
    pub writes: &'static [Layer],
    /// One raster per `writes` entry; `None` when cancelled.
    pub run: fn(&StageInput) -> Option<Vec<Raster>>,
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
    open: TileOpener,
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
        threads: job.threads,
        cancelled,
        layers: HashMap::new(),
    };
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
        let Ok(kind) = self.kind_of(map) else {
            return;
        };
        let dirty = dirty(&decls(kind.stages()), layers);
        self.request(map, dirty);
    }

    fn request(&mut self, map: MapId, dirty: Vec<bool>) {
        if !dirty.contains(&true) {
            return;
        }
        let (Ok(s), Ok(kind)) = (self.map(map).map(|m| m.settings), self.kind_of(map)) else {
            return;
        };
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
            open,
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
