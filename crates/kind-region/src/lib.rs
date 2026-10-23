//! Crude region map kind: height from the parent's height plus root-space noise (ADR-0008, ADR-0024).

use mmc_core::{
    COAST_RELIEF, Command, Error, GenRun, Generation, GeneratorDecl, Layer, MapId, MapKind,
    MapKindModule, MapSettings, Param, Project, RenderPass, Result, RootPoint, Stage, Summary,
    TILE_SIZE, Tool, ToolAction, WATER_LEVEL, par_map, rng,
};

mod slope;
pub use slope::SLOPE;

pub const REGION: MapKind = MapKind::new("region");
/// Noise amplitude, metres. At most `COAST_RELIEF`, so a coast moves at most about one parent cell.
const AMPLITUDE: Param = Param {
    id: "amplitude",
    default: 200.0,
    min: 0.0,
    max: 200.0,
};
/// Noise lattice cells, root-space metres, coarse first.
const OCTAVES: [(f64, f32); 2] = [(4000.0, 0.65), (1000.0, 0.35)];

pub struct RegionKind;

impl MapKindModule for RegionKind {
    fn kind(&self) -> MapKind {
        REGION
    }

    fn default_settings(&self) -> MapSettings {
        MapSettings {
            kind: REGION,
            width: 8192,
            height: 8192,
            metres_per_pixel: 10.0,
        }
    }

    fn layers(&self) -> &'static [Layer] {
        &[Layer::HEIGHT, SLOPE, Layer::LOCK]
    }

    fn stages(&self) -> &'static [Stage] {
        slope::STAGES
    }

    fn render_passes(&self) -> &'static [RenderPass] {
        &[RenderPass {
            layer: Layer::HEIGHT,
            shader: include_str!("height.wgsl"),
        }]
    }

    fn tools(&self) -> &'static [Tool] {
        &[
            Tool {
                id: "brush-height",
                layer: Layer::HEIGHT,
                action: ToolAction::Brush,
            },
            Tool {
                id: "smooth-height",
                layer: Layer::HEIGHT,
                action: ToolAction::Smooth,
            },
            Tool {
                id: "flatten-height",
                layer: Layer::HEIGHT,
                action: ToolAction::Flatten,
            },
            Tool {
                id: "roughen-height",
                layer: Layer::HEIGHT,
                action: ToolAction::Roughen,
            },
            Tool {
                id: "paint-lock",
                layer: Layer::LOCK,
                action: ToolAction::Paint { on: 1.0, off: 0.0 },
            },
        ]
    }

    fn generators(&self) -> &'static [GeneratorDecl] {
        &[GeneratorDecl {
            id: "from-parent",
            params: &[AMPLITUDE],
            needs_parent: true,
        }]
    }

    fn generate(&self, project: &Project, map: MapId, run: &GenRun) -> Result<Command> {
        if run.generator != "from-parent" {
            return Err(Error::UnknownGenerator(run.generator.clone()));
        }
        let (seed, amplitude) = (run.seed, run.param(&AMPLITUDE) as f32);
        let m = project
            .maps()
            .iter()
            .find(|m| m.id == map)
            .ok_or(Error::UnknownMap(map))?;
        let parent = project
            .parent_context(map, Layer::HEIGHT)?
            .ok_or(Error::NoParentContext(map))?;
        let s = m.settings;
        let mpp = s.metres_per_pixel;
        // Global pixel indices: neighbours compute the same root-space centres bit for bit.
        let (gx, gy) = (
            (m.origin.x / mpp).round() as i64,
            (m.origin.y / mpp).round() as i64,
        );
        let rows: Vec<i64> = (0..s.height as i64).collect();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let values = par_map(threads, &rows, &|| false, |&j| {
            (0..s.width as i64)
                .map(|i| {
                    let at = RootPoint {
                        x: ((gx + i) as f64 + 0.5) * mpp,
                        y: ((gy + j) as f64 + 0.5) * mpp,
                    };
                    let n: f32 = OCTAVES
                        .iter()
                        .map(|&(cell, w)| w * rng::noise(seed, "region.height", at, cell) as f32)
                        .sum();
                    let noise = (n - 0.5) * amplitude;
                    let land = parent.sample_by(at, coast) + noise > WATER_LEVEL;
                    let h = parent.sample(at) + noise;
                    // Only ground on the wrong side of the coast moves, just across it.
                    match (land, h > WATER_LEVEL) {
                        (true, false) => WATER_LEVEL + SHORE,
                        (false, true) => WATER_LEVEL - SHORE,
                        _ => h,
                    }
                })
                .collect::<Vec<f32>>()
        })
        .expect("never cancelled")
        .concat();
        Ok(Command::Generate(Generation {
            run: run.clone(),
            entities: vec![],
            rasters: vec![(Layer::HEIGHT, values)],
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
                let tile = project.layer_tile(map, Layer::HEIGHT, tx, ty)?;
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
                        land += (tile[(y * TILE_SIZE + x) as usize] > WATER_LEVEL) as u64;
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

/// The parent's land as +`COAST_RELIEF` and sea as -`COAST_RELIEF`: with the noise, where the coast
/// goes, about one parent cell from the parent's, and never an island in flat parent sea.
fn coast(h: f32) -> f32 {
    if h > WATER_LEVEL {
        WATER_LEVEL + COAST_RELIEF
    } else {
        WATER_LEVEL - COAST_RELIEF
    }
}

/// Metres from water level of ground moved across the coast.
const SHORE: f32 = 0.5;

/// Summary sample spacing in pixels.
const SUMMARY_STEP: usize = 8;
/// Summary tint opacity.
pub const SUMMARY_ALPHA: u8 = 160;
