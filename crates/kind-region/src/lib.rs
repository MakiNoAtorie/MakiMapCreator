//! Crude region map kind (Phase 0): height from an upsample of the parent's land plus
//! root-space noise (ADR-0002, ADR-0008).

use mmc_core::{
    Command, Error, Generation, Layer, MapId, MapKind, MapKindModule, Project, Result, RootPoint,
    Summary, TILE_SIZE, Tool, rng,
};

/// Height step across the parent coast, metres: land 1 maps to +RELIEF/2, sea to -RELIEF/2.
const RELIEF: f32 = 400.0;
/// Noise amplitude, metres. Below RELIEF/2, so a coast moves at most about one parent cell.
const AMPLITUDE: f32 = 200.0;
/// Noise lattice cells, root-space metres, coarse first.
const OCTAVES: [(f64, f32); 2] = [(4000.0, 0.65), (1000.0, 0.35)];

pub struct RegionKind;

impl MapKindModule for RegionKind {
    fn kind(&self) -> MapKind {
        MapKind::Region
    }

    fn layers(&self) -> &'static [Layer] {
        &[Layer::Height, Layer::Slope, Layer::Lock]
    }

    fn render_layers(&self) -> &'static [Layer] {
        &[Layer::Height]
    }

    fn tools(&self) -> &'static [Tool] {
        &[
            Tool {
                id: "brush-height",
                layer: Layer::Height,
            },
            Tool {
                id: "paint-lock",
                layer: Layer::Lock,
            },
        ]
    }

    fn generators(&self) -> &'static [&'static str] {
        &["from-parent"]
    }

    fn generate(
        &self,
        project: &Project,
        map: MapId,
        generator: &str,
        seed: u64,
    ) -> Result<Command> {
        if generator != "from-parent" {
            return Err(Error::UnknownGenerator(generator.into()));
        }
        let m = project
            .maps()
            .iter()
            .find(|m| m.id == map)
            .ok_or(Error::UnknownMap(map))?;
        let land = project
            .parent_context(map, Layer::Land)?
            .ok_or(Error::InvalidChild("the map has no parent land to refine"))?;
        let s = m.settings;
        let mpp = s.metres_per_pixel;
        // Global pixel indices: neighbours compute the same root-space centres bit for bit.
        let (gx, gy) = (
            (m.origin.x / mpp).round() as i64,
            (m.origin.y / mpp).round() as i64,
        );
        // ponytail: one thread; use the pipeline's par_map if 8192² generation is too slow.
        let values = (0..s.height as i64)
            .flat_map(|j| {
                let land = &land;
                (0..s.width as i64).map(move |i| {
                    let at = RootPoint {
                        x: ((gx + i) as f64 + 0.5) * mpp,
                        y: ((gy + j) as f64 + 0.5) * mpp,
                    };
                    let n: f32 = OCTAVES
                        .iter()
                        .map(|&(cell, w)| w * rng::noise(seed, "region.height", at, cell) as f32)
                        .sum();
                    (land.sample(at) - 0.5) * RELIEF + (n - 0.5) * AMPLITUDE
                })
            })
            .collect();
        Ok(Command::Generate(Generation {
            generator: generator.into(),
            seed,
            entities: vec![],
            rasters: vec![(Layer::Height, values)],
        }))
    }

    fn entity_kinds(&self) -> &'static [&'static str] {
        &[]
    }

    /// A tint from sea blue to land green by the share of owned ground above sea level.
    fn summary(&self, project: &Project, map: MapId) -> Result<Option<Summary>> {
        let m = project
            .maps()
            .iter()
            .find(|m| m.id == map)
            .ok_or(Error::UnknownMap(map))?;
        let (s, mpp) = (m.settings, m.settings.metres_per_pixel);
        let (mut land, mut n) = (0u64, 0u64);
        // ponytail: reads every tile on each call; cache summaries when large parents redraw slowly.
        for ty in 0..s.height.div_ceil(TILE_SIZE) {
            for tx in 0..s.width.div_ceil(TILE_SIZE) {
                let tile = project.layer_tile(map, Layer::Height, tx, ty)?;
                for y in (0..TILE_SIZE).step_by(SUMMARY_STEP) {
                    for x in (0..TILE_SIZE).step_by(SUMMARY_STEP) {
                        let (px, py) = (tx * TILE_SIZE + x, ty * TILE_SIZE + y);
                        let at = RootPoint {
                            x: m.origin.x + (px as f64 + 0.5) * mpp,
                            y: m.origin.y + (py as f64 + 0.5) * mpp,
                        };
                        if px >= s.width || py >= s.height || !project.owns(map, at)? {
                            continue;
                        }
                        n += 1;
                        land += (tile[(y * TILE_SIZE + x) as usize] > 0.0) as u64;
                    }
                }
            }
        }
        if n == 0 {
            return Ok(None);
        }
        let f = land as f32 / n as f32;
        let mix = |sea: f32, land: f32| (sea + (land - sea) * f).round() as u8;
        Ok(Some(Summary {
            tint: [
                mix(60.0, 90.0),
                mix(110.0, 170.0),
                mix(190.0, 70.0),
                SUMMARY_ALPHA,
            ],
        }))
    }
}

/// Summary sample spacing in pixels.
const SUMMARY_STEP: usize = 8;
/// Summary tint opacity.
pub const SUMMARY_ALPHA: u8 = 160;
