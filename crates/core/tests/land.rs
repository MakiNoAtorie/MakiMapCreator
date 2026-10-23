//! Land and sea come from height (seam 1): raise-to and lower-to strokes, and the one-time
//! migration of the old land layer on open.

use std::sync::Arc;

use mmc_core::{
    BrushStroke, ChildDecl, Command, GenRun, Generation, Kinds, Layer, MapId, MapKind,
    MapKindModule, MapSettings, NewEntity, PaintStroke, Project, RootPoint, StrokePoint,
    WATER_LEVEL,
};
use mmc_kind_world::{LAND_HEIGHT, SEA_DEPTH, WORLD};

const LEGACY_LAND: Layer = Layer::new("land");

fn world() -> MapSettings {
    MapSettings {
        kind: WORLD,
        width: 512,
        height: 512,
        metres_per_pixel: 1000.0,
    }
}

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint { x, y }
}

/// A hard-edged stroke at full pressure: `target` set, or `strength` added when `None`.
fn stroke(c: RootPoint, radius: f64, strength: f32, target: Option<f32>) -> Command {
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at: c,
            pressure: 1.0,
            radius,
        }],
        strength,
        falloff: 0.0,
        target,
    })
}

fn height(p: &Project, map: MapId, c: RootPoint) -> f32 {
    p.layer_value(map, Layer::HEIGHT, c).unwrap().unwrap()
}

#[test]
fn raise_to_and_lower_to_make_land_and_sea_from_height() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), world()).unwrap();
    let map = p.maps()[0].id;
    let land = |p: &Project, x: f64| height(p, map, at(x, 100_000.0)) > WATER_LEVEL;
    assert!(!land(&p, 100_000.0), "a new world is sea");
    let raise = |x: f64| stroke(at(x, 100_000.0), 50_000.0, 1.0, Some(LAND_HEIGHT));
    p.apply(map, raise(100_000.0)).unwrap();
    p.apply(map, raise(100_000.0)).unwrap();
    assert_eq!(height(&p, map, at(100_000.0, 100_000.0)), LAND_HEIGHT);
    assert!(!land(&p, 160_000.0), "outside the radius stays sea");

    // Raise-to never lowers a hill; lower-to never raises a deep.
    let hill = at(300_000.0, 300_000.0);
    p.apply(map, stroke(hill, 10_000.0, 900.0, None)).unwrap();
    p.apply(map, stroke(hill, 10_000.0, 1.0, Some(LAND_HEIGHT)))
        .unwrap();
    assert_eq!(height(&p, map, hill), 900.0);
    let deep = at(400_000.0, 400_000.0);
    p.apply(map, stroke(deep, 10_000.0, -900.0, None)).unwrap();
    p.apply(map, stroke(deep, 10_000.0, -1.0, Some(SEA_DEPTH)))
        .unwrap();
    assert_eq!(height(&p, map, deep), -900.0);

    // Half pressure moves half the way.
    let mut half = stroke(at(450_000.0, 50_000.0), 10_000.0, 1.0, Some(LAND_HEIGHT));
    if let Command::Brush(b) = &mut half {
        b.points[0].pressure = 0.5;
    }
    p.apply(map, half).unwrap();
    assert_eq!(height(&p, map, at(450_000.0, 50_000.0)), LAND_HEIGHT / 2.0);

    let sea = stroke(at(120_000.0, 100_000.0), 10_000.0, -1.0, Some(SEA_DEPTH));
    p.apply(map, sea).unwrap();
    assert!(!land(&p, 120_000.0));
    assert!(land(&p, 60_000.0));
    p.undo(map).unwrap();
    assert!(land(&p, 120_000.0));
}

/// The world kind of older versions: an authored land layer (1 land, 0 sea) and no height.
struct OldWorld;

impl MapKindModule for OldWorld {
    fn kind(&self) -> MapKind {
        WORLD
    }
    fn default_settings(&self) -> MapSettings {
        world()
    }
    fn layers(&self) -> &'static [Layer] {
        &[LEGACY_LAND]
    }
    fn child(&self) -> Option<ChildDecl> {
        Some(ChildDecl {
            kind: MapKind::new("region"),
            scale_factor: 100.0,
            context: &[LEGACY_LAND],
        })
    }
}

fn old_kinds() -> Arc<Kinds> {
    Arc::new(Kinds::new(vec![
        Box::new(OldWorld),
        Box::new(mmc_kind_region::RegionKind),
    ]))
}

fn paint_land(p: &mut Project, map: MapId, c: RootPoint, radius: f64, value: f32) {
    let s = PaintStroke {
        layer: LEGACY_LAND,
        points: vec![c],
        radius,
        value,
    };
    p.apply(map, Command::Paint(s)).unwrap();
}

#[test]
fn opening_an_old_file_moves_its_land_mask_into_height_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("old.mmc");
    let wc = dir.path().join("wc.sqlite");
    let mut p = mmc_fixtures::create_with(&wc, old_kinds(), world()).unwrap();
    let map = p.maps()[0].id;
    // Land at 100 km with flat height 0; a 700 m hill at 300 km stays as it is.
    let (flat, hill) = (at(100_000.0, 100_000.0), at(300_000.0, 100_000.0));
    paint_land(&mut p, map, flat, 30_000.0, 1.0);
    paint_land(&mut p, map, hill, 30_000.0, 1.0);
    p.apply(map, stroke(hill, 5_000.0, 700.0, None)).unwrap();
    // Sea with positive height at 100 km south; deep sea at 300 km south stays as it is.
    let (shoal, deep) = (at(100_000.0, 300_000.0), at(300_000.0, 300_000.0));
    p.apply(map, stroke(shoal, 5_000.0, 50.0, None)).unwrap();
    p.apply(map, stroke(deep, 5_000.0, -900.0, None)).unwrap();
    let id = p.next_entity_id().unwrap();
    let outline = vec![
        at(50_000.0, 50_000.0),
        at(150_000.0, 50_000.0),
        at(150_000.0, 150_000.0),
        at(50_000.0, 150_000.0),
    ];
    let g = Generation {
        run: GenRun::new("test", 1),
        entities: vec![NewEntity {
            kind: "region".into(),
            name: "R".into(),
            outline,
        }],
        rasters: vec![],
    };
    p.apply(map, Command::Generate(g)).unwrap();
    let child = p.create_child_map(map, id, None).unwrap();
    assert!(p.parent_context(child, LEGACY_LAND).unwrap().is_some());
    p.save(&file);
    p.close().unwrap();

    let file2 = dir.path().join("new.mmc");
    let mut p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    let check = |p: &Project| {
        assert!(height(p, map, flat) > WATER_LEVEL, "flat land is raised");
        assert_eq!(height(p, map, hill), 700.0);
        assert!(
            height(p, map, shoal) < WATER_LEVEL,
            "positive sea is lowered"
        );
        assert_eq!(height(p, map, deep), -900.0);
        assert!(height(p, map, at(450_000.0, 450_000.0)) < WATER_LEVEL);
        assert!(p.edited_tiles(map, LEGACY_LAND).unwrap().is_empty());
        assert_eq!(p.layer_value(map, LEGACY_LAND, flat).unwrap(), Some(0.0));
        assert_eq!(p.parent_context(child, LEGACY_LAND).unwrap(), None);
        let ctx = p.parent_context(child, Layer::HEIGHT).unwrap().unwrap();
        assert!(ctx.sample(flat) > WATER_LEVEL);
        assert!(ctx.sample(at(140_000.0, 140_000.0)) < WATER_LEVEL);
    };
    check(&p);
    let migrated = height(&p, map, flat);
    p.save(&file2);
    p.close().unwrap();

    // Once: a second open finds nothing to migrate and changes nothing.
    let p = mmc_fixtures::open(&file2, &dir.path().join("wc3.sqlite")).unwrap();
    check(&p);
    assert_eq!(height(&p, map, flat), migrated);
}
