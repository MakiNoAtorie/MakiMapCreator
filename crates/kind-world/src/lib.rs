//! Crude world map kind (Phase 0): paint land and sea, split land into Voronoi regions.

use mmc_core::{
    Command, Error, Generation, Layer, MapId, MapKind, MapKindModule, NewEntity, Project, Result,
    RootPoint, Tool, rng,
};

/// Region sites per map side.
const GRID: u32 = 8;

pub struct WorldKind;

impl MapKindModule for WorldKind {
    fn kind(&self) -> MapKind {
        MapKind::World
    }

    fn layers(&self) -> &'static [Layer] {
        &[Layer::Land]
    }

    fn render_layers(&self) -> &'static [Layer] {
        &[Layer::Land]
    }

    fn tools(&self) -> &'static [Tool] {
        &[Tool {
            id: "paint-land",
            layer: Layer::Land,
        }]
    }

    fn generators(&self) -> &'static [&'static str] {
        &["regions"]
    }

    fn generate(
        &self,
        project: &Project,
        map: MapId,
        generator: &str,
        seed: u64,
    ) -> Result<Command> {
        if generator != "regions" {
            return Err(Error::UnknownGenerator(generator.into()));
        }
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
        for j in 0..GRID {
            for i in 0..GRID {
                let cell = RootPoint {
                    x: i as f64 * w / GRID as f64,
                    y: j as f64 * h / GRID as f64,
                };
                let key = rng::Key::At(cell);
                let p = RootPoint {
                    x: cell.x + rng::unit(seed, "world.regions.x", key) * w / GRID as f64,
                    y: cell.y + rng::unit(seed, "world.regions.y", key) * h / GRID as f64,
                };
                if project.layer_value(map, Layer::Land, p)? > Some(0.5) {
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
            generator: generator.into(),
            seed,
            entities,
            rasters: vec![],
        }))
    }

    fn entity_kinds(&self) -> &'static [&'static str] {
        &["region"]
    }

    fn child_kind(&self) -> Option<MapKind> {
        Some(MapKind::Region)
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
