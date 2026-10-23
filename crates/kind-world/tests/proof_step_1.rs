//! Proof step 1 (seam 1): a root world map, paint land and sea as height, generate crude regions as entities.

use mmc_core::{
    APP_VERSION, Command, Error, GenRun, Kinds, Layer, MapId, MapKind, MapKindModule, MapSettings,
    Project, RootPoint, ToolAction,
};
use mmc_kind_world::{LAND_HEIGHT, SEA_DEPTH, WORLD, WorldKind};

const KM: f64 = 1000.0;

fn world() -> MapSettings {
    MapSettings {
        kind: WORLD,
        width: 512,
        height: 512,
        metres_per_pixel: KM,
    }
}

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint {
        x: x * KM,
        y: y * KM,
    }
}

fn paint(c: RootPoint, radius_km: f64, land: bool) -> Command {
    mmc_kind_world::paint(vec![c], radius_km * KM, land)
}

/// A round continent of radius 150 km at (200 km, 250 km), with a lake in its middle.
fn paint_continent(p: &mut Project, map: MapId) {
    p.apply(map, paint(at(200.0, 250.0), 150.0, true)).unwrap();
    p.apply(map, paint(at(200.0, 250.0), 10.0, false)).unwrap();
}

fn inside(poly: &[RootPoint], p: RootPoint) -> bool {
    let mut odd = false;
    for (a, b) in poly.iter().zip(poly.iter().cycle().skip(1)) {
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            odd = !odd;
        }
    }
    odd
}

fn kinds() -> Kinds {
    Kinds::new(vec![Box::new(WorldKind)])
}

#[test]
fn world_kind_declares_its_layers_tools_generators_and_entity_kinds() {
    let kinds = kinds();
    let w = kinds.get(WORLD).unwrap();
    assert!(kinds.get(MapKind::new("settlement")).is_none());
    assert_eq!(w.layers(), [Layer::HEIGHT], "no land layer: height decides");
    assert!(w.render_passes().iter().any(|p| p.layer == Layer::HEIGHT));
    let tool = |id: &str| *w.tools().iter().find(|t| t.id == id).unwrap();
    for (id, to, raise) in [
        ("paint-land", LAND_HEIGHT, true),
        ("paint-sea", SEA_DEPTH, false),
    ] {
        assert_eq!(tool(id).layer, Layer::HEIGHT);
        assert_eq!(tool(id).action, ToolAction::Target { to, raise });
    }
    const { assert!(LAND_HEIGHT > mmc_core::WATER_LEVEL && SEA_DEPTH < mmc_core::WATER_LEVEL) };
    assert_eq!(w.child().unwrap().context, [Layer::HEIGHT]);
    let gens: Vec<_> = w.generators().iter().map(|g| g.id).collect();
    assert_eq!(gens, ["regions"]);
    assert_eq!(w.entity_kinds(), ["region"]);
    assert_eq!(w.child().map(|c| c.kind), Some(MapKind::new("region")));
}

#[test]
fn proof_step_1_paint_land_and_generate_regions() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), world()).unwrap();
    let map = p.maps()[0].id;
    paint_continent(&mut p, map);
    let kinds = kinds();
    let w = kinds.get(WORLD).unwrap();

    let cmd = w.generate(&p, map, &GenRun::new("regions", 7)).unwrap();
    assert_eq!(
        cmd,
        w.generate(&p, map, &GenRun::new("regions", 7)).unwrap()
    );
    assert_ne!(
        cmd,
        w.generate(&p, map, &GenRun::new("regions", 8)).unwrap()
    );
    p.apply(map, cmd).unwrap();

    let regions = p.entities().unwrap();
    let outlines = p.outlines(map).unwrap();
    assert!(regions.len() >= 3, "{} regions", regions.len());
    assert!(
        regions
            .iter()
            .all(|e| e.kind == "region" && !e.name.is_empty())
    );
    assert_eq!(
        outlines.iter().map(|o| o.0).collect::<Vec<_>>(),
        regions.iter().map(|e| e.id).collect::<Vec<_>>()
    );
    // Regions split the land: each land point is in exactly one outline.
    for (x, y) in [
        (100.0, 250.0),
        (300.0, 250.0),
        (200.0, 120.0),
        (200.0, 380.0),
    ] {
        let n = outlines.iter().filter(|o| inside(&o.1, at(x, y))).count();
        assert_eq!(n, 1, "land at ({x}, {y}) km");
    }
    let g = p.generations(map).unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(
        (
            g[0].run.generator.as_str(),
            g[0].run.seed,
            g[0].app_version.as_str()
        ),
        ("regions", 7, APP_VERSION)
    );

    // One-shot: a later land edit leaves the generated regions alone.
    p.apply(map, paint(at(200.0, 250.0), 400.0, false)).unwrap();
    assert_eq!(p.outlines(map).unwrap(), outlines);

    // The generation is one command on the map's history.
    p.undo(map).unwrap();
    p.undo(map).unwrap();
    assert!(p.entities().unwrap().is_empty());
    assert!(p.outlines(map).unwrap().is_empty());
    p.redo(map).unwrap();
    assert_eq!(p.entities().unwrap(), regions);

    let file = dir.path().join("world.mmc");
    p.save(&file);
    p.close().unwrap();
    let p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(p.entities().unwrap(), regions);
    assert_eq!(p.outlines(map).unwrap(), outlines);
    assert_eq!(p.generations(map).unwrap(), g);
}

#[test]
fn an_all_sea_world_gets_no_regions_and_unknown_generators_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), world()).unwrap();
    let map = p.maps()[0].id;
    let Command::Generate(g) = WorldKind
        .generate(&p, map, &GenRun::new("regions", 1))
        .unwrap()
    else {
        panic!("not a generation");
    };
    assert!(g.entities.is_empty());
    let err = WorldKind
        .generate(&p, map, &GenRun::new("rivers", 1))
        .unwrap_err();
    assert!(matches!(err, Error::UnknownGenerator(_)), "{err}");
}
