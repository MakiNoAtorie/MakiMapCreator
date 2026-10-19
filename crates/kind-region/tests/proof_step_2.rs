//! Proof step 2 (seam 1): select a region on the world map and create a child region map
//! from parent context; ownership by outline; neighbours agree in their shared strip.

use mmc_core::{
    APP_VERSION, BrushStroke, Command, Error, GenRun, Generation, Kinds, Layer, MapId,
    MapKindModule, MapSettings, NewEntity, PaintStroke, Project, RootPoint, StrokePoint,
};
use mmc_kind_region::{REGION, RegionKind};
use mmc_kind_world::{LAND, WORLD, WorldKind};

const KM: f64 = 1000.0;

fn at(x: f64, y: f64) -> RootPoint {
    RootPoint {
        x: x * KM,
        y: y * KM,
    }
}

fn kinds() -> Kinds {
    Kinds::new(vec![Box::new(WorldKind), Box::new(RegionKind)])
}

fn world(size: u32) -> MapSettings {
    MapSettings {
        kind: WORLD,
        width: size,
        height: size,
        metres_per_pixel: KM,
    }
}

fn paint(c: RootPoint, radius_km: f64, value: f32) -> Command {
    Command::Paint(PaintStroke {
        layer: LAND,
        points: vec![c],
        radius: radius_km * KM,
        value,
    })
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<RootPoint> {
    vec![at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)]
}

/// A world with two square regions sharing the edge x = 12 km and a coast through both.
fn two_regions(dir: &std::path::Path) -> (Project, MapId) {
    let mut p = mmc_fixtures::create(&dir.join("wc.sqlite"), world(32)).unwrap();
    let w = p.maps()[0].id;
    p.apply(w, paint(at(12.0, 0.0), 11.0, 1.0)).unwrap();
    let region = |name: &str, outline| NewEntity {
        kind: "region".into(),
        name: name.into(),
        outline,
    };
    p.apply(
        w,
        Command::Generate(Generation {
            run: GenRun::new("test", 1),
            entities: vec![
                region("West", rect(4.0, 4.0, 12.0, 12.0)),
                region("East", rect(12.0, 4.0, 20.0, 12.0)),
            ],
            rasters: vec![],
        }),
    )
    .unwrap();
    (p, w)
}

fn brush(at: RootPoint, radius: f64) -> Command {
    Command::Brush(BrushStroke {
        layer: Layer::HEIGHT,
        points: vec![StrokePoint {
            at,
            pressure: 1.0,
            radius,
        }],
        strength: 5.0,
        falloff: 1.0,
    })
}

fn height(p: &Project, map: MapId, x: f64, y: f64) -> Option<f32> {
    p.layer_value(map, Layer::HEIGHT, at(x, y)).unwrap()
}

#[test]
fn proof_step_2_child_region_map_from_parent_context() {
    let dir = tempfile::tempdir().unwrap();
    let kinds = kinds();
    let mut p = mmc_fixtures::create(&dir.path().join("wc.sqlite"), world(64)).unwrap();
    let w = p.maps()[0].id;
    assert_eq!(p.maps()[0].origin, at(0.0, 0.0));
    p.apply(w, paint(at(32.0, 32.0), 20.0, 1.0)).unwrap();
    let cmd = WorldKind
        .generate(&p, w, &GenRun::new("regions", 7))
        .unwrap();
    p.apply(w, cmd).unwrap();
    // The smallest region keeps the child small.
    let span = |o: &[RootPoint]| {
        let (xs, ys) = (o.iter().map(|q| q.x), o.iter().map(|q| q.y));
        (xs.clone().fold(f64::MIN, f64::max) - xs.fold(f64::MAX, f64::min))
            * (ys.clone().fold(f64::MIN, f64::max) - ys.fold(f64::MAX, f64::min))
    };
    let (entity, outline) = p
        .outlines(w)
        .unwrap()
        .into_iter()
        .min_by(|a, b| span(&a.1).total_cmp(&span(&b.1)))
        .unwrap();

    let child = p.create_child_map(w, entity, None).unwrap();
    let m = p.maps().iter().find(|m| m.id == child).unwrap().clone();
    assert_eq!(m.parent, Some(w));
    assert_eq!(m.settings.kind, REGION);
    assert_eq!(m.settings.metres_per_pixel, KM / 100.0);
    assert_eq!(m.outline.as_deref(), Some(outline.as_slice()));
    // The child covers the outline's bounding box plus a margin.
    let mpp = m.settings.metres_per_pixel;
    let (end_x, end_y) = (
        m.origin.x + m.settings.width as f64 * mpp,
        m.origin.y + m.settings.height as f64 * mpp,
    );
    for q in &outline {
        assert!(q.x > m.origin.x + KM && q.x < end_x - KM, "{q:?}");
        assert!(q.y > m.origin.y + KM && q.y < end_y - KM, "{q:?}");
    }

    let region = kinds.get(REGION).unwrap();
    let cmd = region
        .generate(&p, child, &GenRun::new("from-parent", 3))
        .unwrap();
    assert_eq!(
        cmd,
        region
            .generate(&p, child, &GenRun::new("from-parent", 3))
            .unwrap()
    );
    p.apply(child, cmd).unwrap();
    let g = p.generations(child).unwrap();
    assert_eq!(
        (
            g[0].run.generator.as_str(),
            g[0].run.seed,
            g[0].app_version.as_str()
        ),
        ("from-parent", 3, APP_VERSION)
    );
    // Generated tiles are not hand edits (ADR-0022).
    assert!(p.edited_tiles(child, Layer::HEIGHT).unwrap().is_empty());

    // Upsample of the parent: land well inside the continent is above sea, sea below.
    let c = outline
        .iter()
        .fold((0.0, 0.0), |a, q| (a.0 + q.x, a.1 + q.y));
    let n = outline.len() as f64;
    let centre = RootPoint {
        x: c.0 / n,
        y: c.1 / n,
    };
    let land = p.layer_value(w, LAND, centre).unwrap().unwrap();
    let h = p
        .layer_value(child, Layer::HEIGHT, centre)
        .unwrap()
        .unwrap();
    assert_eq!(h > 0.0, land > 0.5, "height {h} on land {land}");

    // The snapshot does not follow later parent edits.
    let ctx = p.parent_context(child, LAND).unwrap().unwrap();
    p.apply(w, paint(at(32.0, 32.0), 64.0, 0.0)).unwrap();
    assert_eq!(p.parent_context(child, LAND).unwrap().unwrap(), ctx);
    assert_eq!(ctx.sample(centre), land);

    // Save and reopen keep the transform, outline and snapshot.
    let before = p.maps().to_vec();
    let file = dir.path().join("w.mmc");
    p.save(&file);
    p.wait_for_save().unwrap();
    p.close().unwrap();
    let p = mmc_fixtures::open(&file, &dir.path().join("wc2.sqlite")).unwrap();
    assert_eq!(p.maps(), before.as_slice());
    assert_eq!(p.parent_context(child, LAND).unwrap().unwrap(), ctx);
    assert_eq!(
        p.layer_value(child, Layer::HEIGHT, centre).unwrap(),
        Some(h)
    );
}

#[test]
fn a_child_owns_only_its_outline() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, w) = two_regions(dir.path());
    let (west, _) = p.outlines(w).unwrap()[0].clone();
    let child = p.create_child_map(w, west, Some(100.0)).unwrap();

    assert!(p.owns(child, at(8.0, 8.0)).unwrap());
    assert!(!p.owns(child, at(13.0, 8.0)).unwrap());
    assert!(p.owns(w, at(13.0, 8.0)).unwrap());

    // Wholly outside the outline: refused, nothing written.
    let err = p.apply(child, brush(at(13.0, 8.0), 300.0)).unwrap_err();
    assert!(matches!(err, Error::OutsideOutline), "{err}");
    assert_eq!(height(&p, child, 13.0, 8.0), Some(0.0));
    assert!(!p.undo(child).unwrap());

    // Across the edge: only owned ground changes.
    p.apply(child, brush(at(12.0, 8.0), 500.0)).unwrap();
    assert!(height(&p, child, 11.8, 8.0).unwrap() > 0.0);
    assert_eq!(height(&p, child, 12.2, 8.0), Some(0.0));
}

#[test]
fn neighbouring_children_agree_in_their_shared_strip() {
    let dir = tempfile::tempdir().unwrap();
    let (mut p, w) = two_regions(dir.path());
    let kinds = kinds();
    let ids: Vec<_> = p.outlines(w).unwrap().iter().map(|o| o.0).collect();
    let west = p.create_child_map(w, ids[0], Some(100.0)).unwrap();
    let east = p.create_child_map(w, ids[1], Some(100.0)).unwrap();
    let region = kinds.get(REGION).unwrap();
    for m in [west, east] {
        let cmd = region
            .generate(&p, m, &GenRun::new("from-parent", 9))
            .unwrap();
        p.apply(m, cmd).unwrap();
    }

    // The strip both cover: x from 12 km minus the east margin to 12 km plus the west margin.
    let (mut land, mut sea, mut n) = (0, 0, 0);
    for i in 0..60 {
        for j in 0..40 {
            let (x, y) = (10.05 + i as f64 * 0.067, 3.0 + j as f64 * 0.25);
            let (a, b) = (height(&p, west, x, y), height(&p, east, x, y));
            if let (Some(a), Some(b)) = (a, b) {
                assert_eq!(a, b, "at ({x}, {y}) km");
                n += 1;
                if a > 0.0 { land += 1 } else { sea += 1 }
            }
        }
    }
    assert!(n > 1000 && land > 100 && sea > 100, "{n} {land} {sea}");
}
