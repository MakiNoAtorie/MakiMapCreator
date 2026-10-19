//! Stage pipeline (ADR-0018): stages declare the layers they read and write; derived
//! layers rebuild on worker threads after each edit (ADR-0014).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use mmc_store::{LayerTiles, TileConn, TileRecord};

use crate::jobs::{Cancelled, Jobs};
use crate::layer::{Layer, TILE_LEN, TILE_SIZE, decode, encode, tile_of};
use crate::undo::Change;
use crate::{Error, MapId, Project, Result};

// ponytail: entity and page changes feed no stage yet; map them to layers when a stage reads entities.
fn changed_layers(change: &Change) -> Vec<Layer> {
    match change {
        Change::Tiles(t) => t
            .iter()
            .filter_map(|c| Layer::parse(&c.key.layer))
            .collect(),
        Change::Entities(_) | Change::Page(_) => vec![],
    }
}

/// Decoded level-0 tiles of a layer; a missing tile is all zeros.
type Raster = BTreeMap<(u32, u32), Vec<f32>>;

struct Ctx<'a> {
    width: u32,
    height: u32,
    metres_per_pixel: f64,
    threads: usize,
    cancelled: Cancelled<'a>,
    layers: HashMap<Layer, Raster>,
}

struct Stage {
    reads: &'static [Layer],
    writes: &'static [Layer],
    /// One raster per `writes` entry; `None` when cancelled.
    run: fn(&Ctx) -> Option<Vec<Raster>>,
}

const STAGES: &[Stage] = &[Stage {
    reads: &[Layer::Height],
    writes: &[Layer::Slope],
    run: slope,
}];

pub(crate) fn writes(layer: Layer) -> bool {
    STAGES.iter().any(|s| s.writes.contains(&layer))
}

fn decls() -> Vec<(&'static [Layer], &'static [Layer])> {
    STAGES.iter().map(|s| (s.reads, s.writes)).collect()
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

/// `f` over `items` on `threads` scoped threads; output in item order, so it never depends on scheduling.
fn par_map<T: Sync, R: Send>(
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

/// Gradient magnitude of height by central differences, clamped at the map edge.
fn slope(cx: &Ctx) -> Option<Vec<Raster>> {
    let h = &cx.layers[&Layer::Height];
    let (w, hh) = (cx.width as i64, cx.height as i64);
    let (ntx, nty) = (
        cx.width.div_ceil(TILE_SIZE) as i64,
        cx.height.div_ceil(TILE_SIZE) as i64,
    );
    let mut want = BTreeSet::new();
    for &(tx, ty) in h.keys() {
        for (dx, dy) in (-1..=1).flat_map(|dx| (-1..=1).map(move |dy| (dx, dy))) {
            let (x, y) = (tx as i64 + dx, ty as i64 + dy);
            if (0..ntx).contains(&x) && (0..nty).contains(&y) {
                want.insert((x as u32, y as u32));
            }
        }
    }
    let want: Vec<(u32, u32)> = want.into_iter().collect();
    let at = |x: i64, y: i64| {
        let (t, i) = tile_of(x.clamp(0, w - 1) as u32, y.clamp(0, hh - 1) as u32);
        h.get(&t).map_or(0.0, |v| v[i])
    };
    let k = (1.0 / (2.0 * cx.metres_per_pixel)) as f32;
    let tiles = par_map(cx.threads, &want, cx.cancelled, |&(tx, ty)| {
        // Height with a one-pixel border, so the inner loop needs no lookups.
        let (ts, n) = (TILE_SIZE as usize, TILE_SIZE as usize + 2);
        let (x0, y0) = ((tx * TILE_SIZE) as i64 - 1, (ty * TILE_SIZE) as i64 - 1);
        let centre = h.get(&(tx, ty));
        let mut pad = vec![0.0f32; n * n];
        for py in 0..n {
            for px in 0..n {
                let (gx, gy) = (x0 + px as i64, y0 + py as i64);
                let inner = (1..=ts).contains(&px) && (1..=ts).contains(&py) && gx < w && gy < hh;
                pad[py * n + px] = match (inner, centre) {
                    (true, Some(c)) => c[(py - 1) * ts + px - 1],
                    (true, None) => 0.0,
                    (false, _) => at(gx, gy),
                };
            }
        }
        let mut out = vec![0.0f32; TILE_LEN];
        for py in 0..ts {
            if y0 + 1 + py as i64 >= hh {
                break;
            }
            for px in 0..ts.min((w - x0 - 1) as usize) {
                let c = (py + 1) * n + px + 1;
                let dx = (pad[c + 1] - pad[c - 1]) * k;
                let dy = (pad[c + n] - pad[c - n]) * k;
                out[py * ts + px] = (dx * dx + dy * dy).sqrt();
            }
        }
        out
    })?;
    Some(vec![want.into_iter().zip(tiles).collect()])
}

struct MapJob {
    id: i64,
    width: u32,
    height: u32,
    metres_per_pixel: f64,
    working_copy: PathBuf,
    dirty: Vec<bool>,
    threads: usize,
}

type Output = (TileConn, i64, Vec<(Layer, LayerTiles)>);

/// Runs the dirty stages in order on its own connection to the working copy.
fn rebuild(job: MapJob, cancelled: Cancelled) -> Option<Result<Output>> {
    let reader = match TileConn::open(&job.working_copy) {
        Ok(r) => r,
        Err(e) => return Some(Err(e.into())),
    };
    let mut cx = Ctx {
        width: job.width,
        height: job.height,
        metres_per_pixel: job.metres_per_pixel,
        threads: job.threads,
        cancelled,
        layers: HashMap::new(),
    };
    let mut written = vec![];
    for i in order(&decls()).into_iter().filter(|&i| job.dirty[i]) {
        let stage = &STAGES[i];
        for &l in stage.reads {
            if cx.layers.contains_key(&l) {
                continue;
            }
            let tiles = match reader.layer_tiles(job.id, l.as_str()) {
                Ok(t) => t,
                Err(e) => return Some(Err(e.into())),
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
            v.iter().any(|x| *x != 0.0).then(|| encode(v))
        })?;
        let records = tiles.iter().zip(encoded).filter_map(|((t, _), data)| {
            Some((
                **t,
                TileRecord {
                    data: data?,
                    edited: false,
                },
            ))
        });
        out.push((l, records.collect()));
    }
    Some(Ok((reader, job.id, out)))
}

/// Swaps the new derived tiles in: one transaction, so readers see the old or the new state.
fn commit(out: Result<Output>) -> Result<()> {
    let (mut conn, map, layers) = out?;
    let layers: Vec<_> = layers.into_iter().map(|(l, t)| (l.as_str(), t)).collect();
    Ok(conn.replace_layers(map, &layers)?)
}

#[derive(Default)]
struct MapState {
    /// Stages to rerun; cleared when the latest job has committed.
    dirty: Vec<bool>,
    jobs: Jobs<Result<()>>,
}

pub(crate) struct Pipeline {
    threads: usize,
    maps: HashMap<MapId, MapState>,
}

impl Default for Pipeline {
    fn default() -> Self {
        Pipeline {
            threads: std::thread::available_parallelism().map_or(1, |n| n.get()),
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
        self.map(map)?;
        self.request(map, vec![true; STAGES.len()]);
        Ok(())
    }

    /// Collects finished rebuilds without blocking; `true` if one has finished since the last call.
    /// Rebuild errors surface here.
    pub fn poll_jobs(&mut self) -> Result<bool> {
        let mut any = false;
        for st in self.pipeline.maps.values_mut() {
            if let Some(res) = st.jobs.take() {
                res?;
                st.dirty.fill(false);
                any = true;
            }
        }
        Ok(any)
    }

    /// Blocks until every requested rebuild has finished.
    pub fn wait_for_jobs(&mut self) -> Result<()> {
        for st in self.pipeline.maps.values_mut() {
            if let Some(res) = st.jobs.wait() {
                res?;
                st.dirty.fill(false);
            }
        }
        Ok(())
    }

    pub(crate) fn refuse_derived(change: &Change) -> Result<()> {
        match changed_layers(change).into_iter().find(|l| l.is_derived()) {
            Some(l) => Err(Error::DerivedLayer(l)),
            None => Ok(()),
        }
    }

    /// Marks stages downstream of the written layers dirty and starts a rebuild.
    pub(crate) fn after_write(&mut self, map: MapId, change: &Change) {
        self.request(map, dirty(&decls(), &changed_layers(change)));
    }

    fn request(&mut self, map: MapId, dirty: Vec<bool>) {
        if !dirty.contains(&true) {
            return;
        }
        let Ok(s) = self.map(map).map(|m| m.settings) else {
            return;
        };
        let st = self.pipeline.maps.entry(map).or_default();
        st.dirty.resize(STAGES.len(), false);
        for (d, n) in st.dirty.iter_mut().zip(dirty) {
            *d |= n;
        }
        let job = MapJob {
            id: map.0,
            width: s.width,
            height: s.height,
            metres_per_pixel: s.metres_per_pixel,
            working_copy: self.store.working_copy().to_owned(),
            dirty: st.dirty.clone(),
            threads: self.pipeline.threads,
        };
        // ponytail: the commit holds the SQLite write lock (~0.5 s for 4096² of slope), so an edit
        // made meanwhile waits; write in chunks or shrink derived tiles if strokes stall.
        st.jobs
            .submit(move |cancelled| rebuild(job, cancelled), commit);
    }
}

impl Pipeline {
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
