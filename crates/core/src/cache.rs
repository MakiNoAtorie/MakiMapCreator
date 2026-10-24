//! Tile cache: workers load tiles into a memory-bounded LRU and update pyramid levels (ADR-0014).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::persist::{TileKey, TileOpener};

use crate::layer::{TILE_LEN, decode, encode};
use crate::persist::TileRecord;
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
    /// The batch being built is not in `tiles` yet.
    computing: bool,
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

    /// Queues the pyramid levels above level-0 keys for update, each with its map's top level.
    pub(crate) fn mark_dirty(&self, keys: impl IntoIterator<Item = (TileKey, u32)>) {
        let mut s = self.shared.lock();
        s.dirty.extend(keys);
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

    /// Cached pyramid levels of `map_id` wait for an update after an edit.
    pub(crate) fn pyramid_pending(&self, map_id: i64) -> bool {
        let s = self.shared.lock();
        s.computing || s.dirty.keys().any(|k| k.map_id == map_id)
    }

    pub(crate) fn wait_idle(&self) {
        let mut s = self.shared.lock();
        while !s.idle() {
            s = self.shared.idle.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Blocks until cached pyramid levels show every queued edit, or `limit` passed; their writes may still run.
    pub(crate) fn wait_shown(&self, limit: std::time::Duration) {
        let until = std::time::Instant::now() + limit;
        let mut s = self.shared.lock();
        while s.computing || !s.dirty.is_empty() {
            let Some(left) = until.checked_duration_since(std::time::Instant::now()) else {
                return;
            };
            s = self
                .shared
                .idle
                .wait_timeout(s, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
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
            // A tile put meanwhile is newer than the read: upper tiles are put before they are written.
            Some(_) if s.tiles.contains_key(&key) => {}
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
                    (s.building, s.computing) = (true, true);
                    break (keys, top);
                }
                s = shared.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };
        if conn.is_none() {
            conn = open().ok();
        }
        // ponytail: a failed rebuild is dropped; the next edit nearby rebuilds those levels.
        // Cached tiles are at least as new as stored ones: every write puts its tiles, and this
        // worker puts the upper tiles of a batch before it writes them.
        let built = conn.as_ref().and_then(|c| {
            // ponytail: an upper tile evicted and reloaded between its put and its write stays stale until the next edit there.
            let read = |k: &TileKey| {
                let cached = shared.lock().tiles.get(k).map(|t| t.0.clone());
                match cached {
                    Some(t) => Ok(t),
                    None => Ok(Tile::from(decode(c.tile(k)?.as_ref())?)),
                }
            };
            let changed = batch
                .iter()
                .map(|k| Ok((k.clone(), read(k)?)))
                .collect::<crate::Result<Vec<_>>>()
                .ok()?;
            upper_levels(changed, top, |k| read(k).map(|t| t.to_vec())).ok()
        });
        {
            // The screen shows the new levels now; their write can take a while.
            let mut s = shared.lock();
            s.computing = false;
            if let Some(put) = &built {
                s.epoch += 1;
                for (k, t) in put {
                    insert(&mut s, k.clone(), t.clone());
                }
            }
            shared.idle.notify_all();
        }
        let written = built.zip(conn.as_ref()).and_then(|(put, c)| {
            let threads = crate::cpus().saturating_sub(1).max(1);
            let data = crate::par_map(threads, &put, &|| false, |(_, t)| encode(t))?;
            let writes: Vec<_> = put
                .into_iter()
                .zip(data)
                .map(|((k, _), data)| {
                    let rec = TileRecord {
                        data,
                        edited: false,
                    };
                    (k, Some(rec))
                })
                .collect();
            crate::jobs::holding_write_lock(|| c.write_tiles(&writes)).ok()
        });
        let mut s = shared.lock();
        s.building = false;
        if written.is_some() {
            // Loads that started before this write may hold older upper tiles.
            s.epoch += 1;
        }
        if s.idle() {
            shared.idle.notify_all();
        }
    }
}
