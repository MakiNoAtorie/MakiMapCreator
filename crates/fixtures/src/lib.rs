//! Seeded, parametric fixture generators shared by tests and the benchmark gate.

mod search;
pub use search::{LongPages, ManyEntities, word};

use std::path::Path;
use std::sync::Arc;

use mmc_core::{
    BrushStroke, Command, Kinds, Layer, MapId, MapSettings, Overview, Project, Result, RootPoint,
    StrokePoint,
};
use mmc_kind_region::{REGION, RegionKind};
use mmc_kind_world::WorldKind;
use mmc_store::{FileReader, Store};

/// The kinds the app ships.
pub fn kinds() -> Arc<Kinds> {
    Arc::new(Kinds::new(vec![Box::new(WorldKind), Box::new(RegionKind)]))
}

/// A new project with its working copy at `working_copy`, on the SQLite store.
pub fn create(working_copy: &Path, root: MapSettings) -> Result<Project> {
    create_with(working_copy, kinds(), root)
}

/// As `create`, with other map kinds.
pub fn create_with(working_copy: &Path, kinds: Arc<Kinds>, root: MapSettings) -> Result<Project> {
    Project::create(Box::new(Store::create(working_copy)?), kinds, root)
}

/// Opens the project `file` into a new working copy.
pub fn open(file: &Path, working_copy: &Path) -> Result<Project> {
    open_with(file, working_copy, kinds())
}

/// As `open`, with other map kinds.
pub fn open_with(file: &Path, working_copy: &Path, kinds: Arc<Kinds>) -> Result<Project> {
    Project::load(Box::new(Store::open(file, working_copy)?), kinds)
}

/// Opens a working copy a crash left behind, in place.
pub fn restore(working_copy: &Path) -> Result<Project> {
    Project::load(Box::new(Store::restore(working_copy)?), kinds())
}

pub fn discard_working_copy(working_copy: &Path) -> Result<()> {
    Ok(Store::discard(working_copy)?)
}

/// The overviews of `layer` in the project `file`, read without opening it.
pub fn read_overviews(file: &Path, layer: Layer) -> Result<Vec<Overview>> {
    mmc_core::read_overviews(&FileReader::open(file)?, &kinds(), layer)
}

/// Splitmix64: small, seeded and stable across platforms.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in 0..1.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Parameters of a large-map fixture. Entity and page counts join with #10 and #11.
#[derive(Debug, Clone, Copy)]
pub struct LargeMap {
    pub seed: u64,
    /// Side length in pixels.
    pub size: u32,
    pub metres_per_pixel: f64,
    pub strokes: usize,
    pub points_per_stroke: usize,
    /// Brush radius in metres.
    pub radius: f64,
}

impl LargeMap {
    /// Default region map (ADR-0014): 8192² with a spread of edited tiles.
    pub fn region(seed: u64) -> LargeMap {
        LargeMap {
            seed,
            size: 8192,
            metres_per_pixel: 10.0,
            strokes: 200,
            points_per_stroke: 20,
            radius: 400.0,
        }
    }

    pub fn settings(&self) -> MapSettings {
        MapSettings {
            kind: REGION,
            width: self.size,
            height: self.size,
            metres_per_pixel: self.metres_per_pixel,
        }
    }

    /// The next stroke from `rng`; the same seed gives the same strokes.
    pub fn stroke(&self, rng: &mut Rng) -> Command {
        let extent = self.size as f64 * self.metres_per_pixel;
        let (mut x, mut y) = (rng.unit() * extent, rng.unit() * extent);
        let step = self.radius * 0.5;
        let points = (0..self.points_per_stroke)
            .map(|_| {
                x = (x + (rng.unit() - 0.5) * step).clamp(0.0, extent - 1.0);
                y = (y + (rng.unit() - 0.5) * step).clamp(0.0, extent - 1.0);
                StrokePoint {
                    at: RootPoint { x, y },
                    pressure: (0.3 + 0.7 * rng.unit()) as f32,
                }
            })
            .collect();
        Command::Brush(BrushStroke {
            layer: Layer::HEIGHT,
            points,
            radius: self.radius,
            strength: (rng.unit() * 40.0 - 20.0) as f32,
        })
    }

    /// Creates the project with its working copy at `working_copy`, edited by `strokes` strokes.
    pub fn build(&self, working_copy: &Path) -> (Project, MapId) {
        let mut p = create(working_copy, self.settings()).unwrap();
        let map = p.maps()[0].id;
        let mut rng = Rng::new(self.seed);
        for _ in 0..self.strokes {
            p.apply(map, self.stroke(&mut rng)).unwrap();
        }
        (p, map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_gives_same_map_and_other_seed_differs() {
        let dir = tempfile::tempdir().unwrap();
        let small = |seed| LargeMap {
            size: 1024,
            strokes: 5,
            ..LargeMap::region(seed)
        };
        let tiles = |name: &str, seed| {
            let (p, m) = small(seed).build(&dir.path().join(name));
            p.edited_tiles(m, Layer::HEIGHT).unwrap()
        };
        let a = tiles("a", 1);
        assert!(!a.is_empty());
        assert_eq!(a, tiles("b", 1));
        assert_ne!(a, tiles("c", 2));
    }
}
