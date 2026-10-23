//! Crude world map kind: paint coarse height for land and sea (ADR-0024), split land into Voronoi regions.

use mmc_core::{
    ChildDecl, Command, Error, GenRun, Generation, GeneratorDecl, Layer, MapId, MapKind,
    MapKindModule, MapSettings, NewEntity, Param, Project, RenderPass, Result, RootPoint, Tool,
    ToolAction, WATER_LEVEL, rng,
};

pub const WORLD: MapKind = MapKind::new("world");
/// Height the land tool raises to, metres.
pub const LAND_HEIGHT: f32 = 200.0;
/// Height the sea tool lowers to, metres.
pub const SEA_DEPTH: f32 = -200.0;

/// Region sites per map side.
const GRID: Param = Param {
    id: "sites-per-side",
    default: 8.0,
    min: 2.0,
    max: 32.0,
};

pub struct WorldKind;

impl MapKindModule for WorldKind {
    fn kind(&self) -> MapKind {
        WORLD
    }

    fn default_settings(&self) -> MapSettings {
        MapSettings {
            kind: WORLD,
            width: 4096,
            height: 4096,
            metres_per_pixel: 1000.0,
        }
    }

    fn layers(&self) -> &'static [Layer] {
        &[Layer::HEIGHT]
    }

    fn render_passes(&self) -> &'static [RenderPass] {
        &[RenderPass {
            layer: Layer::HEIGHT,
            shader: include_str!("height.wgsl"),
        }]
    }

    fn tools(&self) -> &'static [Tool] {
        &[Tool {
            id: "paint-land",
            layer: Layer::HEIGHT,
            action: ToolAction::Target {
                raise: LAND_HEIGHT,
                lower: SEA_DEPTH,
            },
        }]
    }

    fn generators(&self) -> &'static [GeneratorDecl] {
        &[GeneratorDecl {
            id: "regions",
            params: &[GRID],
            needs_parent: false,
        }]
    }

    fn generate(&self, project: &Project, map: MapId, run: &GenRun) -> Result<Command> {
        if run.generator != "regions" {
            return Err(Error::UnknownGenerator(run.generator.clone()));
        }
        let seed = run.seed;
        let grid = run.param(&GRID).round() as u32;
        let s = project
            .maps()
            .iter()
            .find(|m| m.id == map)
            .ok_or(Error::UnknownMap(map))?
            .settings;
        let (w, h) = (
            s.width as f64 * s.metres_per_pixel,
            s.height as f64 * s.metres_per_pixel,
        );
        // One site per grid cell, jittered; only sites on land make regions.
        let mut sites = vec![];
        for j in 0..grid {
            for i in 0..grid {
                let cell = RootPoint {
                    x: i as f64 * w / grid as f64,
                    y: j as f64 * h / grid as f64,
                };
                let key = rng::Key::At(cell);
                let p = RootPoint {
                    x: cell.x + rng::unit(seed, "world.regions.x", key) * w / grid as f64,
                    y: cell.y + rng::unit(seed, "world.regions.y", key) * h / grid as f64,
                };
                if project.layer_value(map, Layer::HEIGHT, p)? > Some(WATER_LEVEL) {
                    sites.push(p);
                }
            }
        }
        let rect = vec![
            RootPoint { x: 0.0, y: 0.0 },
            RootPoint { x: w, y: 0.0 },
            RootPoint { x: w, y: h },
            RootPoint { x: 0.0, y: h },
        ];
        // ponytail: outlines are Voronoi cells and reach into the sea; clip to the coast with the v2 generator.
        let entities = sites
            .iter()
            .enumerate()
            .map(|(n, a)| NewEntity {
                kind: "region".into(),
                name: format!("Region {}", n + 1),
                outline: sites
                    .iter()
                    .filter(|b| *b != a)
                    .fold(rect.clone(), |poly, b| closer_to(&poly, *a, *b)),
            })
            .collect();
        Ok(Command::Generate(Generation {
            run: run.clone(),
            entities,
            rasters: vec![],
        }))
    }

    fn entity_kinds(&self) -> &'static [&'static str] {
        &["region"]
    }

    fn child(&self) -> Option<ChildDecl> {
        Some(ChildDecl {
            kind: MapKind::new("region"),
            scale_factor: 100.0,
            context: &[Layer::HEIGHT],
        })
    }
}

/// The part of convex `poly` closer to `a` than to `b` (Sutherland-Hodgman, one half-plane).
fn closer_to(poly: &[RootPoint], a: RootPoint, b: RootPoint) -> Vec<RootPoint> {
    // Signed distance to the bisector; <= 0 is a's side.
    let side = |p: RootPoint| {
        (b.x - a.x) * (p.x - (a.x + b.x) / 2.0) + (b.y - a.y) * (p.y - (a.y + b.y) / 2.0)
    };
    let mut out = vec![];
    for (i, &p) in poly.iter().enumerate() {
        let q = poly[(i + 1) % poly.len()];
        let (sp, sq) = (side(p), side(q));
        if sp <= 0.0 {
            out.push(p);
        }
        if (sp <= 0.0) != (sq <= 0.0) {
            let t = sp / (sp - sq);
            out.push(RootPoint {
                x: p.x + (q.x - p.x) * t,
                y: p.y + (q.y - p.y) * t,
            });
        }
    }
    out
}

/// A hard-edged, full-pressure stroke of the land tool (`land`) or the sea tool.
pub fn paint(points: Vec<RootPoint>, radius: f64, land: bool) -> Command {
    Command::Brush(mmc_core::BrushStroke {
        layer: Layer::HEIGHT,
        points: points
            .into_iter()
            .map(|at| mmc_core::StrokePoint {
                at,
                pressure: 1.0,
                radius,
            })
            .collect(),
        strength: if land { 1.0 } else { -1.0 },
        falloff: 0.0,
        target: Some(if land { LAND_HEIGHT } else { SEA_DEPTH }),
        ..Default::default()
    })
}
