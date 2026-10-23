//! Stroke sessions (ADR-0014): while the pen is down a stroke's tiles stay decoded in memory and
//! feed the tile cache; pen up encodes and writes them as one command and one undo step.

use std::collections::{HashMap, HashSet};

use crate::cache::Tile;
use crate::command::EditTiles;
use crate::layer::{Layer, encode};
use crate::persist::{TileOpener, TileRecord, Write};
use crate::undo::{Change, TileChange};
use crate::{Command, Error, MapId, Project, Result};

struct Session {
    layer: Layer,
    tiles: EditTiles,
    /// Flatten level, sampled by the stroke's first stamp.
    level: Option<f32>,
}

// ponytail: a stroke over the whole map keeps every tile decoded (256 KiB each); flush early if that hurts.
#[derive(Default)]
pub(crate) struct Strokes {
    open: HashMap<MapId, Session>,
    /// Taken by `take_stroke`, not yet written by `commit_stroke`.
    ending: HashSet<MapId>,
}

impl Strokes {
    /// A stroke on `map` is not in the working copy yet, so neither are pyramid levels above it.
    pub(crate) fn unwritten(&self, map: MapId) -> bool {
        self.open.contains_key(&map) || self.ending.contains(&map)
    }
}

/// An ended stroke's tiles, decoded. `encode` needs no project, so it can run without the project lock.
pub struct EndedStroke {
    map: MapId,
    layer: Layer,
    tiles: EditTiles,
    /// Reads the records of tiles taken from the cache, on a worker connection.
    open: TileOpener,
}

/// An ended stroke ready to write with `Project::commit_stroke`.
pub struct EncodedStroke {
    map: MapId,
    layer: Layer,
    tiles: Vec<EncodedTile>,
}

/// A tile, its record before the stroke, and its bytes after.
type EncodedTile = ((u32, u32), Option<TileRecord>, Vec<u8>);

impl EndedStroke {
    /// Tiles the stroke changed.
    pub fn tiles(&self) -> usize {
        self.tiles.len()
    }

    /// Encodes the tiles and reads the records before the stroke that parts left for later.
    /// Nothing else writes these tiles while the stroke is open, so the records are as they were.
    pub fn encode(self) -> Result<EncodedStroke> {
        let tiles: Vec<_> = self.tiles.into_iter().collect();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let data = crate::par_map(threads, &tiles, &|| false, |(_, (_, v))| encode(v))
            .expect("never cancelled");
        let mut conn = None;
        let mut out = Vec::with_capacity(tiles.len());
        for ((t, (before, _)), d) in tiles.into_iter().zip(data) {
            let before = match before {
                Some(b) => b,
                None => {
                    if conn.is_none() {
                        conn = Some((self.open)()?);
                    }
                    let key = crate::persist::TileKey {
                        map_id: self.map.0,
                        layer: self.layer,
                        level: 0,
                        tx: t.0,
                        ty: t.1,
                    };
                    conn.as_ref().expect("opened").tile(&key)?
                }
            };
            out.push((t, before, d));
        }
        Ok(EncodedStroke {
            map: self.map,
            layer: self.layer,
            tiles: out,
        })
    }
}

impl Project {
    /// Adds a brush or paint part to the open stroke of `map` and shows it through the tile cache.
    pub(crate) fn stroke_part(&mut self, map: MapId, cmd: &Command) -> Result<()> {
        let layer = match cmd {
            Command::Brush(b) => b.layer,
            Command::Paint(p) => p.layer,
            _ => unreachable!("only brush and paint strokes have sessions"),
        };
        if self.is_derived(map, layer)? {
            return Err(Error::DerivedLayer(layer));
        }
        if self
            .strokes
            .open
            .get(&map)
            .is_some_and(|s| s.layer != layer)
        {
            self.finish_stroke(map)?;
        }
        // Other maps' strokes are written first, so one history holds one open stroke at most.
        let others: Vec<MapId> = self
            .strokes
            .open
            .keys()
            .filter(|&&m| m != map)
            .copied()
            .collect();
        for m in others {
            self.finish_stroke(m)?;
        }
        let mut s = self.strokes.open.remove(&map).unwrap_or(Session {
            layer,
            tiles: EditTiles::new(),
            level: None,
        });
        let m = self.map(map)?;
        let touched = match cmd {
            Command::Brush(b) => self.brush_into(m, b, &mut s.tiles, true, &mut s.level),
            Command::Paint(p) => self.paint_into(m, p, &mut s.tiles, true),
            _ => unreachable!(),
        };
        let put = touched.as_ref().map_or(vec![], |t| {
            t.iter()
                .map(|&(tx, ty)| {
                    (
                        m.tile_key(layer, tx, ty),
                        Tile::from(&s.tiles[&(tx, ty)].1[..]),
                    )
                })
                .collect()
        });
        let empty = s.tiles.is_empty();
        if !empty {
            self.strokes.open.insert(map, s);
        }
        touched?;
        if !put.is_empty() {
            self.tiles().put(put);
        }
        Ok(())
    }

    /// Queues the level-0 tiles of `layer` within `radius` metres of `at` for the tile cache, so a
    /// stroke there takes them from memory. Never blocks; call it while the pen hovers or moves.
    pub fn prefetch(
        &self,
        map: MapId,
        layer: Layer,
        at: crate::RootPoint,
        radius: f64,
    ) -> Result<()> {
        let m = self.map(map)?;
        let (cx, cy) = m.to_pixels(at);
        let r = radius / m.settings.metres_per_pixel;
        let t = crate::TILE_SIZE as f64;
        let (w, h) = (m.settings.width as f64, m.settings.height as f64);
        let span = |c: f64, size: f64| {
            let lo = ((c - r).max(0.0) / t).floor() as u32;
            let hi = ((c + r).min(size - 1.0).max(0.0) / t).floor() as u32;
            lo..=hi
        };
        if cx + r < 0.0 || cy + r < 0.0 || cx - r >= w || cy - r >= h {
            return Ok(());
        }
        let cache = self.tiles();
        for ty in span(cy, h) {
            for tx in span(cx, w) {
                cache.request(m.tile_key(layer, tx, ty));
            }
        }
        Ok(())
    }

    /// Takes the open stroke of `map` for `encode` and `commit_stroke`; `None` when there is none.
    pub fn take_stroke(&mut self, map: MapId) -> Option<EndedStroke> {
        let s = self.strokes.open.remove(&map)?;
        self.strokes.ending.insert(map);
        Some(EndedStroke {
            map,
            layer: s.layer,
            tiles: s.tiles,
            open: self.tile_opener(),
        })
    }

    /// Writes an ended stroke in one transaction and records it as one undo step.
    pub fn commit_stroke(&mut self, s: EncodedStroke) -> Result<()> {
        self.strokes.ending.remove(&s.map);
        let m = self.map(s.map)?;
        let changes: Vec<TileChange> = s
            .tiles
            .into_iter()
            .map(|((tx, ty), before, data)| TileChange {
                key: m.tile_key(s.layer, tx, ty),
                before,
                after: Some(TileRecord { data, edited: true }),
            })
            .collect();
        let rows: Vec<Write> = changes.iter().map(|c| c.write(true)).collect();
        let keys: Vec<_> = changes.iter().map(|c| c.key.clone()).collect();
        let top = self.top_level(s.map)?;
        let tiles = rows
            .iter()
            .filter_map(|w| match w {
                Write::Tile(k, rec) => Some((k.clone(), rec.clone())),
                _ => None,
            })
            .collect();
        if let Err(e) = self.writer().submit(tiles) {
            // The cache showed the lost stroke; reload those tiles.
            self.tiles().forget(|k| keys.contains(k));
            return Err(e);
        }
        // The cache holds these tiles from the parts already.
        self.tiles().mark_dirty(keys, top);
        let change = Change::Tiles(changes);
        self.mark_out_of_date(&change)?;
        self.after_write(s.map, &change);
        self.push(s.map, change);
        Ok(())
    }

    pub(crate) fn strokes_open(&self, map: MapId) -> bool {
        self.strokes.open.contains_key(&map)
    }

    /// `Some` while a stroke session is open on `map`: its flatten level, once its first stamp set it.
    pub(crate) fn session_level(&self, map: MapId) -> Option<Option<f32>> {
        self.strokes.open.get(&map).map(|s| s.level)
    }

    /// Ends the open stroke of `map` here and now, if there is one.
    pub(crate) fn finish_stroke(&mut self, map: MapId) -> Result<()> {
        match self.take_stroke(map) {
            Some(s) => {
                let encoded = s.encode();
                match encoded {
                    Ok(e) => self.commit_stroke(e),
                    Err(e) => {
                        self.strokes.ending.remove(&map);
                        self.tiles().forget(|k| k.map_id == map.0);
                        Err(e)
                    }
                }
            }
            None => Ok(()),
        }
    }

    /// Writes every open stroke, before an edit, undo or save reads or writes the working copy.
    pub(crate) fn finish_strokes(&mut self) -> Result<()> {
        let open: Vec<MapId> = self.strokes.open.keys().copied().collect();
        for m in open {
            self.finish_stroke(m)?;
        }
        Ok(())
    }
}
