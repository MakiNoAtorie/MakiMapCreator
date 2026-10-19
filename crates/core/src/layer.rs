//! Raster layers stored as square tiles of f32 (ADR-0020), one zstd blob per tile.

use crate::persist::{TileKey, TileRecord};

use crate::{Error, Map, MapId, Project, Result};

/// Width and height of a tile in pixels.
pub const TILE_SIZE: u32 = 256;

pub(crate) const TILE_LEN: usize = (TILE_SIZE * TILE_SIZE) as usize;

/// A layer of a map, named by its stored id. Kinds define their own layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Layer(&'static str);

impl Layer {
    pub const fn new(id: &'static str) -> Layer {
        Layer(id)
    }

    /// Metres, sea level 0.
    pub const HEIGHT: Layer = Layer("height");
    /// Authored: above 0.5 locks the pixel against re-sync and regeneration (ADR-0022).
    pub const LOCK: Layer = Layer("lock");

    pub fn as_str(self) -> &'static str {
        self.0
    }
}

/// A point in root space, in metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RootPoint {
    pub x: f64,
    pub y: f64,
}

impl Map {
    /// Root space to pixel space, in pixels (pixel `i` spans `i..i+1`).
    pub(crate) fn to_pixels(&self, p: RootPoint) -> (f64, f64) {
        let m = self.settings.metres_per_pixel;
        ((p.x - self.origin.x) / m, (p.y - self.origin.y) / m)
    }

    pub(crate) fn tile_key(&self, layer: Layer, tx: u32, ty: u32) -> TileKey {
        TileKey {
            map_id: self.id.0,
            layer,
            level: 0,
            tx,
            ty,
        }
    }
}

/// Index of pixel (`x`, `y`) inside its tile.
pub(crate) fn tile_of(x: u32, y: u32) -> ((u32, u32), usize) {
    let (tx, ty) = (x / TILE_SIZE, y / TILE_SIZE);
    let i = (y % TILE_SIZE) * TILE_SIZE + x % TILE_SIZE;
    ((tx, ty), i as usize)
}

/// A missing tile is all zeros (sea level) and was never edited.
pub(crate) fn decode(rec: Option<&TileRecord>) -> Result<Vec<f32>> {
    let Some(rec) = rec else {
        return Ok(vec![0.0; TILE_LEN]);
    };
    let bytes = zstd::decode_all(rec.data.as_slice()).map_err(|_| Error::CorruptTile)?;
    if bytes.len() != TILE_LEN * 4 {
        return Err(Error::CorruptTile);
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

pub(crate) fn encode(values: &[f32]) -> Vec<u8> {
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    zstd::bulk::compress(&bytes, 3).expect("zstd compresses an in-memory buffer")
}

impl Project {
    pub(crate) fn map(&self, id: MapId) -> Result<&Map> {
        self.maps
            .iter()
            .find(|m| m.id == id)
            .ok_or(Error::UnknownMap(id))
    }

    /// Value of `layer` at a root-space point; `None` outside the map.
    pub fn layer_value(&self, map: MapId, layer: Layer, at: RootPoint) -> Result<Option<f32>> {
        let map = self.map(map)?;
        let (x, y) = map.to_pixels(at);
        let s = &map.settings;
        if x < 0.0 || y < 0.0 || x >= s.width as f64 || y >= s.height as f64 {
            return Ok(None);
        }
        let ((tx, ty), i) = tile_of(x as u32, y as u32);
        // ponytail: decodes a whole tile per read; the tile cache comes with the pyramid (#9).
        let rec = self.store.tile(&map.tile_key(layer, tx, ty))?;
        Ok(Some(decode(rec.as_ref())?[i]))
    }

    /// All `TILE_SIZE`² values of tile (`tx`, `ty`) of `layer`, row by row.
    pub fn layer_tile(&self, map: MapId, layer: Layer, tx: u32, ty: u32) -> Result<Vec<f32>> {
        let map = self.map(map)?;
        decode(self.store.tile(&map.tile_key(layer, tx, ty))?.as_ref())
    }

    /// Tiles (tx, ty) of `layer` that the user edited by hand, ordered.
    pub fn edited_tiles(&self, map: MapId, layer: Layer) -> Result<Vec<(u32, u32)>> {
        self.map(map)?;
        self.store.edited_tiles(map.0, layer)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tile_blob_is_zstd_and_round_trips() {
        let values: Vec<f32> = (0..super::TILE_LEN)
            .map(|i| i as f32 * 0.25 - 7.0)
            .collect();
        let data = super::encode(&values);
        assert_eq!(&data[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
        let rec = crate::persist::TileRecord { data, edited: true };
        assert_eq!(super::decode(Some(&rec)).unwrap(), values);
    }
}
