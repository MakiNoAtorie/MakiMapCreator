//! Edit write-behind (ADR-0014): an edit's tile rows go to one writer thread and an in-memory
//! overlay, so an edit never waits for SQLite, the disk or a background writer. Every tile read
//! in core and on worker connections looks at the overlay first.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::persist::{LayerTiles, TileKey, TileOpener, TileRecord, TileStore};
use crate::{Error, Result};

/// Written records kept in the overlay as a read cache, so a re-edited tile is not read back.
const KEEP_WRITTEN: usize = 64;
/// Pause after each write transaction.
const GATHER: std::time::Duration = std::time::Duration::from_millis(5);

type Rows = Vec<(TileKey, Option<TileRecord>)>;

struct Entry {
    seq: u64,
    rec: Option<TileRecord>,
    written: bool,
}

#[derive(Default)]
struct State {
    queue: VecDeque<(u64, Rows)>,
    overlay: HashMap<TileKey, Entry>,
    submitted: u64,
    written: u64,
    error: Option<Error>,
    stop: bool,
}

#[derive(Default)]
pub(crate) struct Overlay {
    state: Mutex<State>,
    work: Condvar,
    done: Condvar,
}

impl Overlay {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The record an edit wrote at `key`, if one is pending or recent; `Some(None)` means deleted.
    pub(crate) fn get(&self, key: &TileKey) -> Option<Option<TileRecord>> {
        self.lock().overlay.get(key).map(|e| e.rec.clone())
    }

    /// The pending and recent records of one layer's level-0 tiles.
    fn layer(&self, map_id: i64, layer: crate::Layer) -> Vec<((u32, u32), Option<TileRecord>)> {
        self.lock()
            .overlay
            .iter()
            .filter(|(k, _)| k.map_id == map_id && k.layer == layer && k.level == 0)
            .map(|(k, e)| ((k.tx, k.ty), e.rec.clone()))
            .collect()
    }
}

pub(crate) struct Writer {
    overlay: Arc<Overlay>,
    open: TileOpener,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    pub(crate) fn new(open: TileOpener) -> Writer {
        let overlay = Arc::new(Overlay::default());
        let (o, op) = (overlay.clone(), open.clone());
        let thread = Some(std::thread::spawn(move || run(&o, op)));
        Writer {
            overlay,
            open,
            thread,
        }
    }

    /// Queues `rows`; reads see them at once. Gives the error of an earlier write, if any.
    pub(crate) fn submit(&self, rows: Rows) -> Result<()> {
        let mut s = self.overlay.lock();
        if let Some(e) = s.error.take() {
            return Err(e);
        }
        s.submitted += 1;
        let seq = s.submitted;
        for (k, rec) in &rows {
            let rec = rec.clone();
            s.overlay.insert(
                k.clone(),
                Entry {
                    seq,
                    rec,
                    written: false,
                },
            );
        }
        s.queue.push_back((seq, rows));
        self.overlay.work.notify_all();
        Ok(())
    }

    /// Blocks until every queued row is in the working copy.
    pub(crate) fn flush(&self) -> Result<()> {
        let mut s = self.overlay.lock();
        loop {
            if let Some(e) = s.error.take() {
                // The writer retries the queued rows.
                self.overlay.work.notify_all();
                return Err(e);
            }
            if s.written == s.submitted {
                return Ok(());
            }
            s = self.overlay.done.wait(s).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// After a write that did not go through `submit`: the overlay must not hide it.
    pub(crate) fn wrote(&self, keys: impl Iterator<Item = TileKey>) {
        let mut s = self.overlay.lock();
        for k in keys {
            s.overlay.remove(&k);
        }
    }

    /// Forgets every written record kept for reading, after a write that may have moved tiles.
    pub(crate) fn forget_written(&self) {
        self.overlay.lock().overlay.retain(|_, e| !e.written);
    }

    pub(crate) fn overlay(&self) -> &Arc<Overlay> {
        &self.overlay
    }

    /// A worker connection opener whose reads see the overlay.
    pub(crate) fn opener(&self) -> TileOpener {
        let (open, overlay) = (self.open.clone(), self.overlay.clone());
        Arc::new(move || {
            Ok(Box::new(Overlaid {
                inner: open()?,
                overlay: overlay.clone(),
            }) as Box<dyn TileStore>)
        })
    }
}

impl Drop for Writer {
    /// Writes what is queued, then stops.
    fn drop(&mut self) {
        self.overlay.lock().stop = true;
        self.overlay.work.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(o: &Overlay, open: TileOpener) {
    let mut conn: Option<Box<dyn TileStore>> = None;
    loop {
        let batch: Vec<(u64, Rows)> = {
            let mut s = o.lock();
            while s.queue.is_empty() || s.error.is_some() {
                if s.stop && (s.queue.is_empty() || s.error.is_some()) {
                    return;
                }
                s = o.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
            s.queue.drain(..).collect()
        };
        let last = batch.last().map_or(0, |b| b.0);
        let rows: Rows = batch.iter().flat_map(|b| b.1.iter().cloned()).collect();
        let r = match &conn {
            Some(c) => c.write_edit(&rows),
            None => open().and_then(|c| {
                let r = c.write_edit(&rows);
                conn = Some(c);
                r
            }),
        };
        let mut s = o.lock();
        match r {
            Ok(()) => {
                s.written = last;
                for e in s.overlay.values_mut().filter(|e| e.seq <= last) {
                    e.written = true;
                }
                let mut kept: Vec<(u64, TileKey)> = s
                    .overlay
                    .iter()
                    .filter(|(_, e)| e.written)
                    .map(|(k, e)| (e.seq, k.clone()))
                    .collect();
                if kept.len() > KEEP_WRITTEN {
                    kept.sort_by_key(|k| k.0);
                    for (_, k) in &kept[..kept.len() - KEEP_WRITTEN] {
                        s.overlay.remove(k);
                    }
                }
            }
            Err(e) => {
                // ponytail: the batch goes back to the front and waits for the next submit or flush to retry.
                for b in batch.into_iter().rev() {
                    s.queue.push_front(b);
                }
                s.error = Some(e);
            }
        }
        o.done.notify_all();
        drop(s);
        // Gathers the next edits into one transaction and leaves background writers the lock meanwhile.
        std::thread::sleep(GATHER);
    }
}

/// A worker connection whose reads see the overlay.
struct Overlaid {
    inner: Box<dyn TileStore>,
    overlay: Arc<Overlay>,
}

impl TileStore for Overlaid {
    fn tile(&self, key: &TileKey) -> Result<Option<TileRecord>> {
        match self.overlay.get(key) {
            Some(rec) => Ok(rec),
            None => self.inner.tile(key),
        }
    }

    fn layer_tiles_until(
        &self,
        map_id: i64,
        layer: crate::Layer,
        stop: &dyn Fn() -> bool,
    ) -> Result<Option<LayerTiles>> {
        // The overlay first: a row written meanwhile is then in it or in the read.
        let pending = self.overlay.layer(map_id, layer);
        let Some(tiles) = self.inner.layer_tiles_until(map_id, layer, stop)? else {
            return Ok(None);
        };
        if pending.is_empty() {
            return Ok(Some(tiles));
        }
        let mut all: std::collections::BTreeMap<(u32, u32), TileRecord> =
            tiles.into_iter().collect();
        for (t, rec) in pending {
            match rec {
                Some(r) => all.insert(t, r),
                None => all.remove(&t),
            };
        }
        Ok(Some(all.into_iter().collect()))
    }

    fn write_tiles(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        self.inner.write_tiles(writes)
    }

    fn write_edit(&self, writes: &[(TileKey, Option<TileRecord>)]) -> Result<()> {
        self.inner.write_edit(writes)
    }
}
