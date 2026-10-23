//! Tile cache: workers load tiles into a memory-bounded LRU and update pyramid levels (ADR-0014).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::persist::{TileKey, TileOpener};

use crate::layer::{TILE_LEN, decode};
use crate::pyramid::upper_levels;

/// Default memory limit of the tile cache.
pub const TILE_CACHE_BYTES: usize = 256 << 20;

const TILE_BYTES: usize = TILE_LEN * 4;

pub(crate) type Tile = Arc<[f32]>;

#[derive(Default)]
struct State {
    tiles: HashMap<TileKey, (Tile, u64)>,
    tick: u64,
    limit: usize,
    /// Newest request first: a newer request goes before older ones.
    queue: VecDeque<TileKey>,
    queued: HashSet<TileKey>,
    loading: usize,
    /// Bumped by every write, so a load that read older data is dropped.
    epoch: u64,
    /// Level-0 tiles whose pyramid levels are out of date, with their map's top level.
    dirty: HashMap<TileKey, u32>,
    building: bool,
    stop: bool,
}

impl State {
    fn idle(&self) -> bool {
        self.queue.is_empty() && self.loading == 0 && self.dirty.is_empty() && !self.building
    }
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    work: Condvar,
    idle: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) struct TileCache {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl TileCache {
    pub(crate) fn new(open: TileOpener) -> TileCache {
        let shared = Arc::new(Shared::default());
        shared.lock().limit = TILE_CACHE_BYTES;
        let n = crate::cpus().clamp(2, 4);
        let mut workers: Vec<_> = (0..n)
            .map(|_| {
                let (shared, open) = (shared.clone(), open.clone());
                std::thread::spawn(move || work(&shared, open))
            })
            .collect();
        let sh = shared.clone();
        workers.push(std::thread::spawn(move || build(&sh, open)));
        TileCache { shared, workers }
    }

    /// The loaded tile at `key`, marked as recently used.
    pub(crate) fn get(&self, key: &TileKey) -> Option<Tile> {
        let mut s = self.shared.lock();
        s.tick += 1;
        let tick = s.tick;
        s.tiles.get_mut(key).map(|e| {
            e.1 = tick;
            e.0.clone()
        })
    }

    /// Queues `key` for loading unless it is loaded or queued.
    pub(crate) fn request(&self, key: TileKey) {
        let mut s = self.shared.lock();
        if s.tiles.contains_key(&key) || !s.queued.insert(key.clone()) {
            return;
        }
        s.queue.push_front(key);
        self.shared.work.notify_all();
    }

    /// Puts tiles that were just written, so edits show without a reload.
    pub(crate) fn put(&self, tiles: Vec<(TileKey, Tile)>) {
        let mut s = self.shared.lock();
        s.epoch += 1;
        for (k, t) in tiles {
            insert(&mut s, k, t);
        }
    }

    /// Queues the pyramid levels above level-0 `keys` for update; `top` is their map's top level.
    pub(crate) fn mark_dirty(&self, keys: impl IntoIterator<Item = TileKey>, top: u32) {
        let mut s = self.shared.lock();
        s.dirty.extend(keys.into_iter().map(|k| (k, top)));
        self.shared.work.notify_all();
    }

    /// Drops pending pyramid updates that match.
    pub(crate) fn drop_dirty(&self, matches: impl Fn(&TileKey) -> bool) {
        let mut s = self.shared.lock();
        s.dirty.retain(|k, _| !matches(k));
        if s.idle() {
            self.shared.idle.notify_all();
        }
    }

    /// Drops loaded tiles that match, so the next request reloads them.
    pub(crate) fn forget(&self, matches: impl Fn(&TileKey) -> bool) {
        let mut s = self.shared.lock();
        s.epoch += 1;
        s.tiles.retain(|k, _| !matches(k));
    }

    /// Pyramid levels of `map_id` wait for an update after an edit.
    pub(crate) fn pyramid_pending(&self, map_id: i64) -> bool {
        let s = self.shared.lock();
        s.building || s.dirty.keys().any(|k| k.map_id == map_id)
    }

    pub(crate) fn wait_idle(&self) {
        let mut s = self.shared.lock();
        while !s.idle() {
            s = self.shared.idle.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub(crate) fn set_limit(&self, bytes: usize) {
        let mut s = self.shared.lock();
        s.limit = bytes;
        evict(&mut s);
    }

    pub(crate) fn bytes(&self) -> usize {
        self.shared.lock().tiles.len() * TILE_BYTES
    }
}

impl Drop for TileCache {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.work.notify_all();
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
    }
}

fn insert(s: &mut State, key: TileKey, tile: Tile) {
    s.tick += 1;
    let tick = s.tick;
    s.tiles.insert(key, (tile, tick));
    evict(s);
}

// ponytail: O(n) scan per eviction; n is at most limit / 256 KiB, about 1000 tiles.
fn evict(s: &mut State) {
    while s.tiles.len() * TILE_BYTES > s.limit {
        let Some(oldest) = s.tiles.iter().min_by_key(|e| e.1.1).map(|e| e.0.clone()) else {
            return;
        };
        s.tiles.remove(&oldest);
    }
}

fn work(shared: &Shared, open: TileOpener) {
    crate::jobs::background_priority();
    let mut reader = None;
    loop {
        let (key, epoch) = {
            let mut s = shared.lock();
            loop {
                if s.stop {
                    return;
                }
                if let Some(k) = s.queue.pop_front() {
                    s.loading += 1;
                    break (k, s.epoch);
                }
                s = shared.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        if reader.is_none() {
            reader = open().ok();
        }
        // ponytail: a failed read is dropped; the next frame's request retries it.
        let tile = reader
            .as_ref()
            .and_then(|r| r.tile(&key).ok())
            .and_then(|rec| decode(rec.as_ref()).ok());
        let mut s = shared.lock();
        s.loading -= 1;
        s.queued.remove(&key);
        match tile {
            Some(t) if s.epoch == epoch => insert(&mut s, key, t.into()),
            Some(_) if !s.tiles.contains_key(&key) => {
                s.queued.insert(key.clone());
                s.queue.push_front(key);
                shared.work.notify_all();
            }
            _ => {}
        }
        if s.idle() {
            shared.idle.notify_all();
        }
    }
}

/// Rebuilds pyramid tiles above dirty level-0 tiles, one batch per map, on its own connection.
fn build(shared: &Shared, open: TileOpener) {
    crate::jobs::background_priority();
    let mut conn = None;
    loop {
        let (batch, top) = {
            let mut s = shared.lock();
            loop {
                if s.stop {
                    return;
                }
                if let Some(map) = s.dirty.keys().next().map(|k| k.map_id) {
                    let keys: Vec<TileKey> = s
                        .dirty
                        .keys()
                        .filter(|k| k.map_id == map)
                        .cloned()
                        .collect();
                    let top = keys.iter().map(|k| s.dirty[k]).max().unwrap_or(0);
                    s.dirty.retain(|k, _| k.map_id != map);
                    s.building = true;
                    break (keys, top);
                }
                s = shared.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        if conn.is_none() {
            conn = open().ok();
        }
        // ponytail: a failed rebuild is dropped; the next edit nearby rebuilds those levels.
        // Cached level-0 tiles equal the stored ones: every write puts its tiles.
        let cached: Vec<Option<Tile>> = {
            let s = shared.lock();
            batch
                .iter()
                .map(|k| s.tiles.get(k).map(|t| t.0.clone()))
                .collect()
        };
        let built = conn.as_ref().and_then(|c| {
            let changed = batch
                .iter()
                .zip(cached)
                .map(|(k, t)| match t {
                    Some(t) => Ok((k.clone(), t.to_vec())),
                    None => Ok((k.clone(), decode(c.tile(k)?.as_ref())?)),
                })
                .collect::<crate::Result<Vec<_>>>()
                .ok()?;
            let (writes, put) = upper_levels(changed, top, |k| c.tile(k)).ok()?;
            crate::jobs::holding_write_lock(|| c.write_tiles(&writes)).ok()?;
            Some(put)
        });
        let mut s = shared.lock();
        s.building = false;
        if let Some(put) = built {
            // Loads that started before this write may hold older upper tiles.
            s.epoch += 1;
            for (k, t) in put {
                insert(&mut s, k, t);
            }
        }
        if s.idle() {
            shared.idle.notify_all();
        }
    }
}
