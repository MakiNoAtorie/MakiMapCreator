//! Tile cache: workers load tiles from the working copy into an LRU bounded by memory (ADR-0014).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use mmc_store::{FileReader, TileKey};

use crate::layer::{TILE_LEN, decode};

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
    stop: bool,
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
    pub(crate) fn new(working_copy: &Path) -> TileCache {
        let shared = Arc::new(Shared::default());
        shared.lock().limit = TILE_CACHE_BYTES;
        let n = std::thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 4));
        let workers = (0..n)
            .map(|_| {
                let (shared, path) = (shared.clone(), working_copy.to_owned());
                std::thread::spawn(move || work(&shared, path))
            })
            .collect();
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
        self.shared.work.notify_one();
    }

    /// Puts tiles that were just written, so edits show without a reload.
    pub(crate) fn put(&self, tiles: Vec<(TileKey, Tile)>) {
        let mut s = self.shared.lock();
        s.epoch += 1;
        for (k, t) in tiles {
            insert(&mut s, k, t);
        }
    }

    /// Drops loaded tiles that match, so the next request reloads them.
    pub(crate) fn forget(&self, matches: impl Fn(&TileKey) -> bool) {
        let mut s = self.shared.lock();
        s.epoch += 1;
        s.tiles.retain(|k, _| !matches(k));
    }

    pub(crate) fn wait_idle(&self) {
        let mut s = self.shared.lock();
        while !s.queue.is_empty() || s.loading > 0 {
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

fn work(shared: &Shared, path: PathBuf) {
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
            reader = FileReader::open(&path).ok();
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
                shared.work.notify_one();
            }
            _ => {}
        }
        if s.queue.is_empty() && s.loading == 0 {
            shared.idle.notify_all();
        }
    }
}
